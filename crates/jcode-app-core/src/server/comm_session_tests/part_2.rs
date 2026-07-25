#[tokio::test]
async fn nested_agent_cannot_spawn_when_root_is_light_or_normal() {
    // Both explicit light-swarm effort and ordinary ad hoc swarm use are
    // one-level fan-out. A spawned child cannot grow another generation.
    for (root_id, effort) in [
        ("light-root-no-recursion", Some("swarm")),
        ("normal-root-no-recursion", None),
    ] {
        crate::session_effort::forget_session_effort(root_id);
        crate::session_effort::record_session_effort(root_id, effort);
        let swarm_id = format!("swarm-{root_id}");
        let child_id = format!("child-{root_id}");
        let swarm_members = Arc::new(RwLock::new(HashMap::new()));
        let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
            swarm_id.clone(),
            HashSet::from([child_id.clone(), root_id.to_string()]),
        )])));
        let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
            swarm_id.clone(),
            root_id.to_string(),
        )])));
        let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
        let (mut child_member, _child_rx) = member(&child_id, Some(&swarm_id), "agent");
        child_member.report_back_to_session_id = Some(root_id.to_string());
        let (root_member, _root_rx) = member(root_id, Some(&swarm_id), "coordinator");
        let mut members = swarm_members.write().await;
        members.insert(child_id.clone(), child_member);
        members.insert(root_id.to_string(), root_member);
        drop(members);
        let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

        let refused = ensure_spawn_coordinator_swarm(
            2,
            &child_id,
            &client_event_tx,
            &swarm_members,
            &swarms_by_id,
            &swarm_coordinators,
            &swarm_plans,
            32,
        )
        .await;

        crate::session_effort::forget_session_effort(root_id);
        assert!(refused.is_none());
        assert_eq!(
            swarm_coordinators
                .read()
                .await
                .get(&swarm_id)
                .map(String::as_str),
            Some(root_id)
        );
        assert_eq!(
            swarm_members
                .read()
                .await
                .get(&child_id)
                .map(|member| member.role.as_str()),
            Some("agent")
        );
        assert!(matches!(
            client_event_rx.recv().await,
            Some(ServerEvent::Error { message, .. })
                if message.contains("Recursive swarm spawning is disabled")
                    && message.contains(&format!("Only the root session ({root_id}) may spawn agents"))
        ));
    }
}

#[tokio::test]
async fn nested_agent_can_spawn_when_root_is_deep() {
    let root_id = "deep-root-recursive";
    crate::session_effort::record_session_effort(root_id, Some("swarm-deep"));

    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        "swarm-deep".to_string(),
        HashSet::from(["deep-child".to_string(), root_id.to_string()]),
    )])));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-deep".to_string(),
        root_id.to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    let (mut child_member, _child_rx) = member("deep-child", Some("swarm-deep"), "agent");
    child_member.report_back_to_session_id = Some(root_id.to_string());
    let (root_member, _root_rx) = member(root_id, Some("swarm-deep"), "coordinator");
    let mut members = swarm_members.write().await;
    members.insert("deep-child".to_string(), child_member);
    members.insert(root_id.to_string(), root_member);
    drop(members);
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let allowed = ensure_spawn_coordinator_swarm(
        3,
        "deep-child",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        32,
    )
    .await;

    crate::session_effort::forget_session_effort(root_id);
    assert_eq!(allowed.as_deref(), Some("swarm-deep"));
    assert!(client_event_rx.try_recv().is_err());
}

#[tokio::test]
async fn spawn_allowed_at_arbitrary_depth_without_depth_cap() {
    // Deep-swarm mode still allows recursive decomposition at arbitrary depth.
    let root_id = "deep-root-arbitrary-depth";
    crate::session_effort::record_session_effort(root_id, Some("swarm-deep"));
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        root_id.to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    {
        let mut members = swarm_members.write().await;
        let (root, _rx) = member(root_id, Some("swarm-1"), "coordinator");
        members.insert(root_id.to_string(), root);
        let chain = [
            ("a", root_id),
            ("b", "a"),
            ("c", "b"),
            ("d", "c"),
            ("e", "d"),
            ("f", "e"),
        ];
        for (id, parent) in chain {
            let (mut m, _rx) = member(id, Some("swarm-1"), "agent");
            m.report_back_to_session_id = Some(parent.to_string());
            members.insert(id.to_string(), m);
        }
    }
    let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel();

    // `f` is deeply nested but the swarm is far below the member cap, so spawning
    // is allowed.
    let allowed = ensure_spawn_coordinator_swarm(
        7,
        "f",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        32,
    )
    .await;
    crate::session_effort::forget_session_effort(root_id);
    assert_eq!(allowed.as_deref(), Some("swarm-1"));
}

#[tokio::test]
async fn spawn_rejected_when_member_limit_reached() {
    use crate::server::swarm::MAX_SWARM_MEMBERS;

    // Fill the swarm to the member cap; the next spawn must be refused.
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        "root".to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    {
        let mut members = swarm_members.write().await;
        let (root, _rx) = member("root", Some("swarm-1"), "coordinator");
        members.insert("root".to_string(), root);
        // Add filler members so the swarm holds exactly MAX_SWARM_MEMBERS total.
        for idx in 1..MAX_SWARM_MEMBERS {
            let id = format!("agent-{idx}");
            let (mut m, _rx) = member(&id, Some("swarm-1"), "agent");
            m.report_back_to_session_id = Some("root".to_string());
            members.insert(id, m);
        }
    }
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let refused = ensure_spawn_coordinator_swarm(
        7,
        "root",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        0,
    )
    .await;
    assert!(refused.is_none());
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error { message, .. })
            if message.contains("Swarm member limit reached")
    ));
}

#[tokio::test]
async fn terminal_members_do_not_consume_spawn_capacity() {
    use crate::server::swarm::MAX_SWARM_MEMBERS;

    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        "root".to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    {
        let mut members = swarm_members.write().await;
        let (root, _rx) = member("root", Some("swarm-1"), "coordinator");
        members.insert("root".to_string(), root);
        for idx in 0..MAX_SWARM_MEMBERS {
            let id = format!("historical-{idx}");
            let (mut historical, _rx) = member(&id, Some("swarm-1"), "agent");
            historical.status = if idx % 2 == 0 {
                "completed".to_string()
            } else {
                "stopped".to_string()
            };
            historical.latest_completion_report = Some(format!("report {idx}"));
            historical.report_back_to_session_id = Some("root".to_string());
            members.insert(id, historical);
        }
    }
    let (client_event_tx, _client_event_rx) = mpsc::unbounded_channel();

    let allowed = ensure_spawn_coordinator_swarm(
        7,
        "root",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        32,
    )
    .await;

    assert_eq!(allowed.as_deref(), Some("swarm-1"));
}

#[tokio::test]
async fn spawn_rejected_at_configured_live_agent_limit() {
    let swarm_members = Arc::new(RwLock::new(HashMap::new()));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::from([(
        "swarm-1".to_string(),
        "root".to_string(),
    )])));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    {
        let mut members = swarm_members.write().await;
        let (root, _rx) = member("root", Some("swarm-1"), "coordinator");
        members.insert("root".to_string(), root);
        for idx in 0..2 {
            let id = format!("agent-{idx}");
            let (mut worker, _rx) = member(&id, Some("swarm-1"), "agent");
            worker.report_back_to_session_id = Some("root".to_string());
            members.insert(id, worker);
        }
    }
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let refused = ensure_spawn_coordinator_swarm(
        7,
        "root",
        &client_event_tx,
        &swarm_members,
        &swarms_by_id,
        &swarm_coordinators,
        &swarm_plans,
        2,
    )
    .await;

    assert!(refused.is_none());
    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Error { message, .. })
            if message.contains("Swarm live-agent limit reached (max 2")
    ));
}

#[tokio::test]
async fn spawn_admission_lock_serializes_per_swarm_only() {
    use std::time::Duration;

    let key = format!("lock-test-{}", std::process::id());
    let same_a = spawn_admission_lock(&key);
    let same_b = spawn_admission_lock(&key);
    let other = spawn_admission_lock(&format!("{key}-other"));

    let held = same_a.lock().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), same_b.lock())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), other.lock())
            .await
            .is_ok()
    );
    drop(held);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), same_b.lock())
            .await
            .is_ok()
    );
}
