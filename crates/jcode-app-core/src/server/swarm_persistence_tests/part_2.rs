#[test]
fn persisted_swarm_state_without_plan_still_restores_coordinator_and_members() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let _env = test_env(&dir);

    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let members = vec![SwarmMember {
        session_id: "coord-1".to_string(),
        event_tx,
        event_txs: HashMap::new(),
        working_dir: Some(PathBuf::from("/tmp/swarm-gamma")),
        swarm_id: Some("swarm-gamma".to_string()),
        swarm_enabled: true,
        status: "ready".to_string(),
        detail: None,
        friendly_name: Some("owl".to_string()),
        report_back_to_session_id: None,
        latest_completion_report: None,
        role: "coordinator".to_string(),
        joined_at: Instant::now(),
        last_status_change: Instant::now(),
        is_headless: false,
        output_tail: None,
        todo_progress: None,
        todo_items: Vec::new(),
        runtime: crate::protocol::SwarmMemberRuntime::default(),
        task_label: None,
    }];

    persist_swarm_state("swarm-gamma", None, Some("coord-1"), &members);

    let loaded = load_runtime_state();
    assert!(!loaded.plans.contains_key("swarm-gamma"));
    assert_eq!(
        loaded.coordinators.get("swarm-gamma"),
        Some(&"coord-1".to_string())
    );
    assert_eq!(
        loaded
            .members
            .get("coord-1")
            .and_then(|member| member.friendly_name.as_deref()),
        Some("owl")
    );
    assert_eq!(
        loaded.swarms_by_id.get("swarm-gamma"),
        Some(&HashSet::from(["coord-1".to_string()]))
    );
}

#[test]
fn remove_swarm_state_removes_backup_and_cannot_resurrect() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let _env = test_env(&dir);

    // First persist creates the primary; the second overwrite makes
    // write_json_fast hard-link the previous (coord-v1) snapshot to `.bak`.
    persist_swarm_state("swarm-zombie", None, Some("coord-v1"), &[]);
    persist_swarm_state("swarm-zombie", None, Some("coord-v2"), &[]);
    let bak_path = state_path("swarm-zombie").with_extension("bak");
    assert!(bak_path.exists(), "write_json_fast leaves a .bak hard link");

    remove_swarm_state("swarm-zombie");
    assert!(!state_path("swarm-zombie").exists());
    assert!(
        !bak_path.exists(),
        "logical deletion must remove the recovery backup too"
    );

    let loaded = load_runtime_state();
    assert!(
        !loaded.coordinators.contains_key("swarm-zombie"),
        "a deleted swarm must not be restored on the next load"
    );
}

#[test]
fn empty_persist_dissolution_removes_backup_and_cannot_resurrect() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let _env = test_env(&dir);

    persist_swarm_state("swarm-dissolve", None, Some("coord-v1"), &[]);
    persist_swarm_state("swarm-dissolve", None, Some("coord-v2"), &[]);
    let bak_path = state_path("swarm-dissolve").with_extension("bak");
    assert!(bak_path.exists(), "write_json_fast leaves a .bak hard link");

    // Dissolution: no plan, no coordinator, no members hits the
    // remove_file branch instead of writing a snapshot.
    persist_swarm_state("swarm-dissolve", None, None, &[]);
    assert!(!state_path("swarm-dissolve").exists());
    assert!(
        !bak_path.exists(),
        "empty-state persistence must remove the recovery backup too"
    );

    let loaded = load_runtime_state();
    assert!(
        !loaded.coordinators.contains_key("swarm-dissolve"),
        "a dissolved swarm must not be restored on the next load"
    );
}

/// Delete-vs-write interleaving between `remove_persisted_swarm_state_for`
/// and a concurrent persist (wiring-audit.bak-resurrection, part b).
///
/// `remove_persisted_swarm_state_for` (server.rs:120) is `load_runtime()
/// .await` followed by an unserialized `remove_swarm_state`. Like the
/// persist inversion race above, `load_runtime` observes the four state
/// maps across multiple await points, so a remover that saw an all-empty
/// (dissolved) runtime can park, lose the race to a swarm re-creation plus
/// persist, then resume and delete the FRESH snapshot the re-creation just
/// wrote. Two failures compound:
///   1. Orphaned live swarm: the recreated swarm (coordinator registered
///      in memory) has no primary snapshot, so a clean restart loses it.
///   2. Zombie resurrection: the persist that the remover clobbered
///      hard-linked the PRE-dissolution snapshot to `.bak`, and
///      `load_runtime_state` reads `.bak` files, so restart restores the
///      stale pre-dissolution state instead.
///
/// Same gate technique as
/// `stale_persist_cannot_regress_newer_plan_version`:
/// park A inside `load_runtime` at the contended `members.read()`, run
/// mutator B's re-creation and persist while A is parked, release A.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn stale_remove_cannot_delete_fresh_snapshot_or_restore_backup() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let _env = test_env(&dir);

    // The previous incarnation's snapshot is on disk; the swarm has since
    // been dissolved, so the in-memory runtime is empty.
    persist_swarm_state("swarm-del-race", None, Some("coord-stale"), &[]);
    let swarm_state = crate::server::SwarmState::new(
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );

    // Gate: hold members.write() so remover A parks inside load_runtime at
    // the final members.read(), AFTER it has already observed the
    // dissolved (all-empty) plans/coordinators/swarms_by_id state.
    let gate = swarm_state.members.write().await;

    let a = tokio::spawn({
        let swarm_state = swarm_state.clone();
        async move {
            crate::server::remove_persisted_swarm_state_for("swarm-del-race", &swarm_state).await;
        }
    });
    // Current-thread test runtime: yielding runs A until it parks on the
    // contended members.read().await.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    // Mutator B: the swarm is recreated while A is parked. B registers a
    // new coordinator in memory ...
    {
        let mut coordinators = swarm_state.coordinators.write().await;
        coordinators.insert("swarm-del-race".to_string(), "coord-new".to_string());
    }
    // ... and B's persist half runs to completion (in production this is
    // B's own persist_swarm_state_for on another worker thread, whose
    // uncontended lock reads resolve without suspending). This overwrite
    // also hard-links the stale pre-dissolution snapshot to `.bak`.
    persist_swarm_state("swarm-del-race", None, Some("coord-new"), &[]);
    let on_disk = storage::read_json::<PersistedSwarmState>(&state_path("swarm-del-race"))
        .expect("fresh snapshot");
    assert_eq!(
        on_disk.coordinator_session_id.as_deref(),
        Some("coord-new"),
        "fresh snapshot must be durably on disk before A resumes"
    );

    // Release A: its stale all-empty runtime passes has_any_state(), but the
    // compare-and-delete guard must notice that the durable snapshot changed.
    drop(gate);
    a.await.expect("remove task");

    assert!(
        state_path("swarm-del-race").exists(),
        "a stale remove must not delete a freshly persisted snapshot"
    );
    let loaded = load_runtime_state();
    assert_eq!(
        loaded.coordinators.get("swarm-del-race"),
        Some(&"coord-new".to_string()),
        "restart must restore the fresh incarnation, not its stale backup"
    );
}
