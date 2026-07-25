#[tokio::test]
async fn automatic_account_transition_clears_old_resume_id_before_the_next_turn() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let concrete = Arc::new(AccountTransitionProvider {
        identity: Arc::new(std::sync::Mutex::new(account_transition_identity(
            "account-a",
            "stable-a",
        ))),
        switched: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        seen_resume_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
        refresh_identity: Arc::new(std::sync::Mutex::new(None)),
        credential_refreshes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let registry_provider: Arc<dyn Provider> = concrete.clone();
    let registry = Registry::new(registry_provider).await;
    // The Agent owns a distinct provider fork. Credential refresh state and
    // request observations must still be shared so reconciliation reads the
    // identity of the instance that actually opened the request.
    let mut agent = Agent::new(concrete.fork(), registry);
    let account_a_identity = agent
        .session
        .exact_runtime_identity
        .clone()
        .expect("account-a identity");
    let mut source = crate::session::Session::create_with_id(
        crate::id::new_id("automatic_transition_graph_source"),
        None,
        None,
    );
    source.exact_runtime_identity = Some(account_a_identity.clone());
    source.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "old account canonical context".to_string(),
            cache_control: None,
        }],
    );
    source.save().expect("save old-account source");
    agent.session.replace_messages(Vec::new());
    agent.session.parent_id = Some(source.id.clone());
    agent
        .session
        .install_imported_context_root(
            &source,
            crate::session::StoredCompactionState {
                summary_text: "old account portable context".to_string(),
                openai_encrypted_content: None,
                covers_up_to_turn: 1,
                original_turn_count: 1,
                compacted_count: 0,
            },
            &account_a_identity,
        )
        .expect("install old-account graph");
    agent.session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "child canonical history".to_string(),
            cache_control: None,
        }],
    );
    assert!(!agent.session.context_nodes.is_empty());
    let forensic_nodes = agent.session.context_nodes.clone();
    let canonical_history_len = agent.session.messages.len();
    let canonical_history = serde_json::to_vec(&agent.session.messages).expect("serialize history");
    agent.bind_provider_session_id("resume-owned-by-account-a".to_string());
    agent.session.save().expect("save account-a binding");

    agent
        .run_once("trigger account failover")
        .await
        .expect("turn 1");
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.session.provider_session_identity.is_none());
    assert_eq!(agent.session.context_nodes, forensic_nodes);
    assert!(
        agent
            .session
            .context_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.active_node_ids.is_empty()
                && frontier.covered_message_count == 0)
    );
    assert!(agent.session.compaction.is_none());
    assert!(!agent.session.has_owned_native_lcm_projection());
    assert_eq!(
        serde_json::to_vec(&agent.session.messages[..canonical_history_len])
            .expect("serialize transitioned history prefix"),
        canonical_history
    );
    let transitioned_history =
        serde_json::to_vec(&agent.session.messages).expect("serialize transitioned history");
    assert_eq!(
        agent
            .session
            .exact_runtime_identity
            .as_ref()
            .and_then(|identity| identity.account_id.as_deref()),
        Some("stable-b")
    );
    let loaded =
        crate::session::Session::load(agent.session_id()).expect("load transitioned state");
    assert!(loaded.provider_session_id.is_none());
    assert_eq!(loaded.context_nodes, forensic_nodes);
    assert!(loaded.context_frontier.as_ref().is_some_and(|frontier| {
        frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
    }));
    assert!(loaded.compaction.is_none());
    assert!(!loaded.has_owned_native_lcm_projection());
    assert_eq!(
        serde_json::to_vec(&loaded.messages).expect("serialize reloaded history"),
        transitioned_history
    );
    assert_eq!(
        loaded
            .exact_runtime_identity
            .as_ref()
            .and_then(|identity| identity.account_id.as_deref()),
        Some("stable-b")
    );

    agent.run_once("next turn").await.expect("turn 2");
    assert_eq!(
        *concrete.seen_resume_ids.lock().unwrap(),
        [Some("resume-owned-by-account-a".to_string()), None]
    );

    crate::env::remove_var("JCODE_HOME");
}

