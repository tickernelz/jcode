#[tokio::test]
async fn handle_resume_session_allows_reconnect_takeover_with_local_history() -> Result<()> {
    assert_reconnect_takeover_behavior(false, true).await
}

#[tokio::test]
async fn handle_resume_session_does_not_take_over_processing_owner() -> Result<()> {
    assert_reconnect_takeover_behavior(true, false).await
}

#[tokio::test]
async fn reconnect_takeover_waits_for_stale_dispatch_and_revokes_future_dispatch() -> Result<()> {
    let client_connections = Arc::new(RwLock::new(HashMap::new()));
    let now = Instant::now();
    let (disconnect_tx, _disconnect_rx) = mpsc::unbounded_channel();
    client_connections.write().await.insert(
        "conn-stale".to_string(),
        ClientConnectionInfo {
            client_id: "conn-stale".to_string(),
            session_id: "session-takeover-barrier".to_string(),
            client_instance_id: None,
            debug_client_id: None,
            connected_at: now,
            last_seen: now,
            is_processing: false,
            current_tool_name: None,
            terminal_env: Vec::new(),
            disconnect_tx,
        },
    );
    let barrier = crate::server::client_lifecycle::connection_dispatch_barrier("conn-stale");
    let stale_dispatch = barrier.lock().await;

    let takeover_connections = Arc::clone(&client_connections);
    let takeover_barrier = Arc::clone(&barrier);
    let takeover = tokio::spawn(async move {
        let _revocation = takeover_barrier.lock().await;
        takeover_connections.write().await.remove("conn-stale");
    });
    tokio::task::yield_now().await;
    assert!(
        !takeover.is_finished(),
        "takeover must wait for admitted dispatch"
    );

    drop(stale_dispatch);
    takeover.await.expect("takeover task");

    assert!(
        crate::server::client_lifecycle::acquire_connection_dispatch_lease(
            &client_connections,
            "conn-stale",
            "session-takeover-barrier",
        )
        .await
        .is_none(),
        "production admission must reject stale owner after takeover returns"
    );
    crate::server::client_lifecycle::remove_connection_dispatch_barrier("conn-stale");
    Ok(())
}

async fn assert_reconnect_takeover_behavior(
    existing_is_processing: bool,
    expect_takeover: bool,
) -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let _runtime = setup_runtime_dir()?;

    let (target_session_id, temp_session_id) = if existing_is_processing {
        (
            "session_existing_live_processing_takeover",
            "session_temp_connecting_processing_takeover",
        )
    } else {
        (
            "session_existing_live_takeover_guarded",
            "session_temp_connecting_takeover_guarded",
        )
    };

    let mut persisted = crate::session::Session::create_with_id(
        target_session_id.to_string(),
        None,
        Some("Reconnect Takeover".to_string()),
    );
    persisted.save()?;

    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let existing_registry = Registry::new(provider.clone()).await;
    let existing_agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider.clone(),
        existing_registry,
        persisted,
        None,
    )));

    let new_registry = Registry::new(provider.clone()).await;
    let new_agent = Arc::new(Mutex::new(build_test_agent_with_id(
        provider.clone(),
        new_registry.clone(),
        temp_session_id,
        Vec::new(),
    )));

    let sessions = Arc::new(RwLock::new(HashMap::from([
        (target_session_id.to_string(), Arc::clone(&existing_agent)),
        (temp_session_id.to_string(), Arc::clone(&new_agent)),
    ])));
    let shutdown_signals = Arc::new(RwLock::new(HashMap::<String, InterruptSignal>::new()));
    let soft_interrupt_queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let now = Instant::now();
    let (disconnect_tx, mut disconnect_rx) = mpsc::unbounded_channel();
    let client_connections = Arc::new(RwLock::new(HashMap::from([
        (
            "conn_existing".to_string(),
            ClientConnectionInfo {
                client_id: "conn_existing".to_string(),
                session_id: target_session_id.to_string(),
                client_instance_id: None,
                debug_client_id: Some("debug_existing".to_string()),
                connected_at: now,
                last_seen: now,
                is_processing: existing_is_processing,
                current_tool_name: existing_is_processing.then(|| "bash".to_string()),
                terminal_env: Vec::new(),
                disconnect_tx,
            },
        ),
        (
            "conn_new".to_string(),
            ClientConnectionInfo {
                client_id: "conn_new".to_string(),
                session_id: temp_session_id.to_string(),
                client_instance_id: None,
                debug_client_id: Some("debug_new".to_string()),
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
    let swarm_members = Arc::new(RwLock::new(HashMap::<String, SwarmMember>::new()));
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
    let mut client_session_id = temp_session_id.to_string();

    handle_resume_session(
        43,
        target_session_id.to_string(),
        None,
        None,
        true,
        true,
        &mut client_selfdev,
        &mut client_session_id,
        "conn_new",
        &new_agent,
        &provider,
        &new_registry,
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
    .await?;

    while let Ok(event) = client_event_rx.try_recv() {
        assert!(
            !matches!(event, ServerEvent::Error { .. }),
            "resume takeover should not queue an error event: {event:?}"
        );
    }
    assert_eq!(client_session_id, target_session_id);

    let connections = client_connections.read().await;
    if expect_takeover {
        let disconnect_signal = disconnect_rx.recv().await;
        assert!(
            disconnect_signal.is_some(),
            "idle old client should be told to disconnect"
        );
        assert!(!connections.contains_key("conn_existing"));
    } else {
        assert!(
            disconnect_rx.try_recv().is_err(),
            "processing old client must retain ownership of its task"
        );
        let owner = connections
            .get("conn_existing")
            .expect("processing owner must stay connected");
        assert!(owner.is_processing);
        assert_eq!(owner.current_tool_name.as_deref(), Some("bash"));
    }
    assert_eq!(
        connections
            .get("conn_new")
            .map(|info| info.session_id.as_str()),
        Some(target_session_id)
    );

    Ok(())
}
