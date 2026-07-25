#[tokio::test]
async fn fallback_resume_preserves_shared_source_and_registers_events_before_replay() -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let _runtime = setup_runtime_dir()?;

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let target_session_id = format!("session_restore_target_{suffix}");
    let temp_session_id = format!("session_restore_temp_{suffix}");

    let mut persisted = crate::session::Session::create_with_id(
        target_session_id.clone(),
        None,
        Some("Resume Registration Ordering".to_string()),
    );
    persisted.save()?;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        registry.clone(),
        &temp_session_id,
        Vec::new(),
    )));

    let sessions = Arc::new(RwLock::new(HashMap::from([(
        temp_session_id.clone(),
        Arc::clone(&agent),
    )])));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let now = Instant::now();
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        (
            "conn_restore".to_string(),
            ClientConnectionInfo {
                client_id: "conn_restore".to_string(),
                session_id: temp_session_id.clone(),
                client_instance_id: None,
                debug_client_id: Some("debug_restore".to_string()),
                connected_at: now,
                last_seen: now,
                is_processing: false,
                current_tool_name: None,
                terminal_env: Vec::new(),
                disconnect_tx: mpsc::unbounded_channel().0,
            },
        ),
        (
            "conn_source_peer".to_string(),
            ClientConnectionInfo {
                client_id: "conn_source_peer".to_string(),
                session_id: temp_session_id.clone(),
                client_instance_id: None,
                debug_client_id: Some("debug_source_peer".to_string()),
                connected_at: now,
                last_seen: now,
                is_processing: false,
                current_tool_name: None,
                terminal_env: Vec::new(),
                disconnect_tx: mpsc::unbounded_channel().0,
            },
        ),
    ])));
    let client_debug_state = Arc::new(RwLock::new(ClientDebugState::default()));
    let (placeholder_event_tx, _placeholder_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        temp_session_id.clone(),
        SwarmMember {
            session_id: temp_session_id.clone(),
            event_tx: placeholder_event_tx,
            event_txs: HashMap::new(),
            working_dir: None,
            swarm_id: None,
            swarm_enabled: false,
            status: "ready".to_string(),
            detail: None,
            task_label: None,
            friendly_name: Some("restore".to_string()),
            report_back_to_session_id: None,
            latest_completion_report: None,
            role: "agent".to_string(),
            joined_at: now,
            last_status_change: now,
            is_headless: false,
            output_tail: None,
            todo_progress: None,
            todo_items: Vec::new(),
            runtime: crate::protocol::SwarmMemberRuntime::default(),
        },
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::<String, HashSet<String>>::new()));
    let file_touch = FileTouchService::new();
    let channel_subscriptions = Arc::new(RwLock::new(HashMap::<
        String,
        HashMap<String, HashSet<String>>,
    >::new()));
    let channel_subscriptions_by_session = Arc::new(RwLock::new(HashMap::<
        String,
        HashMap<String, HashSet<String>>,
    >::new()));
    let swarm_plans = Arc::new(RwLock::new(HashMap::<String, VersionedPlan>::new()));
    let swarm_coordinators = Arc::new(RwLock::new(HashMap::<String, String>::new()));
    let client_count = Arc::new(RwLock::new(2usize));
    let (writer, _peer_stream) = test_writer()?;
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let event_history = Arc::new(RwLock::new(VecDeque::<SwarmEvent>::new()));
    let event_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel::<SwarmEvent>(8);
    let mcp_pool = Arc::new(crate::mcp::SharedMcpPool::from_default_config());

    let mut client_selfdev = false;
    let expected_temp_session_id = temp_session_id.clone();
    let mut client_session_id = temp_session_id;
    let writer_guard = writer.lock().await;
    // Model cleanup after it has removed the target Agent but before terminal
    // publication. The persisted target exists, while the managed map contains
    // only the incoming connection's temporary source Agent.
    let cleanup_lease =
        crate::server::client_session_lifecycle::acquire_session_lifecycle_lease(
            &target_session_id,
        )
        .await;
    let resume_waiting_for_cleanup =
        crate::server::client_session_lifecycle::observe_next_session_lifecycle_acquire(
            &target_session_id,
        );

    let resume_task = tokio::spawn({
        let agent = Arc::clone(&agent);
        let provider = Arc::clone(&provider);
        let registry = registry.clone();
        let sessions = Arc::clone(&sessions);
        let shutdown_signals = Arc::clone(&shutdown_signals);
        let soft_interrupt_queues = Arc::clone(&soft_interrupt_queues);
        let client_connections = Arc::clone(&client_connections);
        let client_debug_state = Arc::clone(&client_debug_state);
        let swarm_members = Arc::clone(&swarm_members);
        let swarms_by_id = Arc::clone(&swarms_by_id);
        let file_touch = file_touch.clone();
        let channel_subscriptions = Arc::clone(&channel_subscriptions);
        let channel_subscriptions_by_session = Arc::clone(&channel_subscriptions_by_session);
        let swarm_plans = Arc::clone(&swarm_plans);
        let swarm_coordinators = Arc::clone(&swarm_coordinators);
        let client_count = Arc::clone(&client_count);
        let writer = Arc::clone(&writer);
        let client_event_tx = client_event_tx.clone();
        let mcp_pool = Arc::clone(&mcp_pool);
        let event_history = Arc::clone(&event_history);
        let event_counter = Arc::clone(&event_counter);
        let swarm_event_tx = swarm_event_tx.clone();
        let resume_target_session_id = target_session_id.clone();
        async move {
            handle_resume_session(
                46,
                resume_target_session_id,
                None,
                None,
                false,
                false,
                &mut client_selfdev,
                &mut client_session_id,
                "conn_restore",
                &agent,
                &provider,
                &registry,
                &sessions,
                &shutdown_signals,
                &soft_interrupt_queues,
                &client_connections,
                &client_debug_state,
                &swarm_members,
                &swarms_by_id,
                &file_touch,
                &channel_subscriptions,
                &channel_subscriptions_by_session,
                &swarm_plans,
                &swarm_coordinators,
                &client_count,
                &writer,
                "test-server",
                "🌿",
                &client_event_tx,
                &mcp_pool,
                &event_history,
                &event_counter,
                &swarm_event_tx,
            )
            .await
        }
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        resume_waiting_for_cleanup,
    )
    .await
    .map_err(|_| anyhow!("fallback resume did not reach target lifecycle acquisition"))?
    .map_err(|_| anyhow!("fallback resume lifecycle acquisition observer dropped"))?;
    assert_eq!(
        client_connections.read().await["conn_restore"].session_id,
        expected_temp_session_id,
        "fallback resume must not publish connection ownership before terminal cleanup releases the target"
    );
    assert!(
        !sessions.read().await.contains_key(&target_session_id),
        "fallback resume must not restore the target Agent while cleanup owns its lifecycle"
    );
    assert!(
        !resume_task.is_finished(),
        "fallback resume must wait after cleanup claim and before terminal publication"
    );
    drop(cleanup_lease);

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let registered = {
                let members = swarm_members.read().await;
                members
                    .get(&target_session_id)
                    .map(|member| member.event_txs.contains_key("conn_restore"))
                    .unwrap_or(false)
            };
            if registered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| anyhow!("live event sender should register before history replay completes"))?;

    assert!(
        !resume_task.is_finished(),
        "resume should still be blocked on history replay while writer is locked"
    );

    drop(writer_guard);

    resume_task
        .await
        .map_err(|e| anyhow!("resume task join: {e}"))??;

    let events = collect_events_until_done(&mut client_event_rx, 46).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ServerEvent::Done { id } if *id == 46)),
        "expected Done event for restore resume, got {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ServerEvent::Error { .. })),
        "restore resume should not emit error events: {events:?}"
    );

    let sessions_guard = sessions.read().await;
    let restored_agent = sessions_guard
        .get(&target_session_id)
        .ok_or_else(|| anyhow!("fallback resume should publish a restored target Agent"))?;
    assert!(
        !Arc::ptr_eq(restored_agent, &agent),
        "fallback resume must not rebind a source Agent shared by another connection"
    );
    let source_agent = sessions_guard
        .get(&expected_temp_session_id)
        .ok_or_else(|| anyhow!("shared source Agent must remain published"))?;
    assert!(Arc::ptr_eq(source_agent, &agent));
    drop(sessions_guard);

    assert_eq!(
        agent.lock().await.session_id(),
        expected_temp_session_id,
        "fallback resume must not mutate the shared source Agent identity"
    );
    assert_eq!(
        client_connections.read().await["conn_source_peer"].session_id,
        expected_temp_session_id,
        "fallback resume must not move the source peer connection"
    );
    assert!(
        swarm_members
            .read()
            .await
            .contains_key(&expected_temp_session_id),
        "fallback resume must preserve support state for a still-live source session"
    );

    Ok(())
}
