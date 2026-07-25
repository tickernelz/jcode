#[tokio::test]
async fn auth_model_first_prompt_e2e_state_space_is_bounded_by_selection_source() {
    let scenarios = [
        AuthModelE2eScenario {
            name: "auth auto-selects matching route when user does not intervene",
            manual_pick_after_first_snapshot: None,
            prompt_immediately_after_model_pick: false,
            expected_first_prompt_model: "logged-in-model",
        },
        AuthModelE2eScenario {
            name: "manual picker selection during auth refresh wins first prompt",
            manual_pick_after_first_snapshot: Some("user-picked-model"),
            prompt_immediately_after_model_pick: true,
            expected_first_prompt_model: "user-picked-model",
        },
    ];

    for scenario in scenarios {
        let _guard = EnvGuard::save(&[
            "JCODE_OPENROUTER_API_BASE",
            "JCODE_OPENROUTER_API_KEY_NAME",
            "JCODE_OPENROUTER_ENV_FILE",
            "JCODE_OPENROUTER_CACHE_NAMESPACE",
            "JCODE_OPENROUTER_PROVIDER_FEATURES",
            "JCODE_OPENROUTER_TRANSPORT_STATE",
            "JCODE_OPENROUTER_MODEL_CATALOG",
            "JCODE_OPENROUTER_AUTH_HEADER",
            "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
            "JCODE_OPENROUTER_MODEL",
            "JCODE_RUNTIME_PROVIDER",
            "JCODE_ACTIVE_PROVIDER",
            "JCODE_FORCE_PROVIDER",
        ]);

        crate::bus::reset_models_updated_publish_state_for_tests();
        let provider_concrete = Arc::new(AuthChangeMockProvider::new());
        provider_concrete
            .state
            .auth_refresh_delay_ms
            .store(80, Ordering::Release);
        *provider_concrete.state.selected_model.write().unwrap() = Some("stale-model".to_string());
        *provider_concrete.state.route_provider.write().unwrap() = "Cerebras".to_string();
        *provider_concrete.state.route_api_method.write().unwrap() =
            "openai-compatible:cerebras".to_string();
        *provider_concrete
            .state
            .expose_selected_model_in_routes
            .write()
            .unwrap() = false;
        let provider: Arc<dyn Provider> = provider_concrete.clone();
        let registry = Registry::empty();
        let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
        let session_id = { agent.lock().await.session_id().to_string() };
        let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([(
            format!("test-session-{}", scenario.name),
            Arc::clone(&agent),
        )])));
        let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

        let mut auth = crate::protocol::AuthChanged::new("cerebras");
        auth.credential_source = Some(crate::protocol::AuthCredentialSource::ApiKeyFile);
        auth.auth_method = Some(crate::protocol::AuthMethod::RemoteTuiPasteApiKey);
        auth.expected_runtime = Some(crate::protocol::RuntimeProviderKey::new(
            "openai-compatible",
        ));
        auth.expected_catalog_namespace = Some(crate::protocol::CatalogNamespace::new("cerebras"));

        handle_notify_auth_changed(
            148,
            None,
            Some(auth),
            false,
            &provider,
            &provider,
            &sessions,
            session_id.as_str(),
            &agent,
            &client_event_tx,
        )
        .await;

        assert!(
            matches!(
                client_event_rx.recv().await,
                Some(ServerEvent::Done { id: 148 })
            ),
            "{}: expected auth Done",
            scenario.name
        );

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if matches!(
                    client_event_rx.recv().await,
                    Some(ServerEvent::AvailableModelsUpdated { .. })
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{}: expected immediate auth model snapshot", scenario.name));

        let mut first_prompt_output = None;
        if let Some(model) = scenario.manual_pick_after_first_snapshot {
            handle_set_model(248, model.to_string(), &agent, &client_event_tx).await;
            loop {
                match client_event_rx.recv().await {
                    Some(ServerEvent::ModelChanged {
                        id: 248,
                        model: changed,
                        error,
                        ..
                    }) => {
                        assert_eq!(error, None, "{}: manual model switch failed", scenario.name);
                        assert_eq!(
                            changed, model,
                            "{}: manual model switch mismatch",
                            scenario.name
                        );
                        break;
                    }
                    Some(_) => continue,
                    None => panic!("{}: model switch channel closed", scenario.name),
                }
            }

            if scenario.prompt_immediately_after_model_pick {
                let agent_for_prompt = Arc::clone(&agent);
                let scenario_name = scenario.name;
                first_prompt_output = Some(
                    tokio::time::timeout(std::time::Duration::from_secs(2), async move {
                        let mut agent_guard = agent_for_prompt.lock().await;
                        agent_guard
                            .run_once_capture("first prompt immediately after model selection")
                            .await
                    })
                    .await
                    .unwrap_or_else(|_| panic!("{}: first prompt stalled", scenario_name))
                    .unwrap_or_else(|error| {
                        panic!("{}: first prompt failed: {error:?}", scenario_name)
                    }),
                );
            }
        }

        let final_message = recv_final_catalog_notification(&mut client_event_rx).await;
        assert!(
            final_message.contains(&format!(
                "**Model ready:** `{}`",
                scenario.expected_first_prompt_model
            )),
            "{}: final activity selected wrong model: {}",
            scenario.name,
            final_message
        );

        let first_prompt_output = if let Some(output) = first_prompt_output {
            output
        } else {
            let mut agent_guard = agent.lock().await;
            agent_guard
                .run_once_capture("first prompt after auth/model selection")
                .await
                .unwrap_or_else(|error| panic!("{}: first prompt failed: {error:?}", scenario.name))
        };
        assert!(
            first_prompt_output.contains("ok"),
            "{}: fake provider response not observed: {}",
            scenario.name,
            first_prompt_output
        );
        let completed_models = provider_concrete
            .state
            .complete_models
            .lock()
            .unwrap()
            .clone();
        assert_eq!(
            completed_models.last().map(String::as_str),
            Some(scenario.expected_first_prompt_model),
            "{}: first provider request used wrong model; all completions: {:?}",
            scenario.name,
            completed_models
        );
    }
}

#[tokio::test]
async fn notify_auth_changed_switches_only_current_session_model() {
    let _guard = EnvGuard::save(&[
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_TRANSPORT_STATE",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_AUTH_HEADER",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "JCODE_OPENROUTER_MODEL",
        "JCODE_RUNTIME_PROVIDER",
        "JCODE_ACTIVE_PROVIDER",
        "JCODE_FORCE_PROVIDER",
    ]);

    crate::bus::reset_models_updated_publish_state_for_tests();
    let current_provider = Arc::new(AuthChangeMockProvider::new());
    let current_state = Arc::clone(&current_provider.state);
    *current_state.selected_model.write().unwrap() = Some("gpt-5.5".to_string());
    *current_state.route_provider.write().unwrap() = "Groq".to_string();
    *current_state.route_api_method.write().unwrap() = "openai-compatible:groq".to_string();
    let peer_provider = Arc::new(AuthChangeMockProvider::new());
    let peer_state = Arc::clone(&peer_provider.state);
    *peer_state.selected_model.write().unwrap() = Some("gpt-5.5".to_string());
    *peer_state.route_provider.write().unwrap() = "Groq".to_string();
    *peer_state.route_api_method.write().unwrap() = "openai-compatible:groq".to_string();

    let current_provider: Arc<dyn Provider> = current_provider;
    let peer_provider: Arc<dyn Provider> = peer_provider;
    let registry = Registry::empty();
    let current_agent = Arc::new(Mutex::new(Agent::new(
        Arc::clone(&current_provider),
        registry.clone(),
    )));
    let current_session_id = { current_agent.lock().await.session_id().to_string() };
    let peer_agent = Arc::new(Mutex::new(Agent::new(peer_provider, registry)));
    let sessions: SessionAgents = Arc::new(RwLock::new(HashMap::from([
        ("current-session".to_string(), Arc::clone(&current_agent)),
        ("peer-session".to_string(), Arc::clone(&peer_agent)),
    ])));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    let mut auth = crate::protocol::AuthChanged::new("groq");
    auth.credential_source = Some(crate::protocol::AuthCredentialSource::ApiKeyFile);
    auth.auth_method = Some(crate::protocol::AuthMethod::RemoteTuiPasteApiKey);
    auth.expected_runtime = Some(crate::protocol::RuntimeProviderKey::new(
        "openai-compatible",
    ));
    auth.expected_catalog_namespace = Some(crate::protocol::CatalogNamespace::new("groq"));

    handle_notify_auth_changed(
        47,
        Some("openai".to_string()),
        Some(auth),
        false,
        &current_provider,
        &current_provider,
        &sessions,
        current_session_id.as_str(),
        &current_agent,
        &client_event_tx,
    )
    .await;

    assert!(matches!(
        client_event_rx.recv().await,
        Some(ServerEvent::Done { id: 47 })
    ));

    let expected = "llama-3.1-8b-instant";
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let current = current_state.selected_model.read().unwrap().clone();
        let peer = peer_state.selected_model.read().unwrap().clone();
        let peer_refreshed = *peer_state.logged_in.read().unwrap();
        if current.as_deref() == Some(expected)
            && peer.as_deref() == Some("gpt-5.5")
            && peer_refreshed
        {
            let peer_snapshot = available_models_updated_event(&peer_agent).await;
            let ServerEvent::AvailableModelsUpdated {
                provider_name,
                provider_model,
                available_model_routes,
                ..
            } = peer_snapshot
            else {
                panic!("expected available models snapshot for peer session");
            };
            assert_eq!(provider_name.as_deref(), Some("mock-auth"));
            assert_eq!(provider_model.as_deref(), Some("gpt-5.5"));
            assert!(available_model_routes.iter().any(|route| {
                route.model == "gpt-5.5"
                    && route.provider == "Groq"
                    && route.api_method == "openai-compatible:groq"
            }));
            assert!(
                available_model_routes
                    .iter()
                    .all(|route| route.model != expected),
                "auth-triggered Groq model leaked into peer session routes: {:?}",
                available_model_routes
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!(
        "auth change did not keep model switch session-local: current={:?}, peer={:?}, peer_refreshed={}",
        current_state.selected_model.read().unwrap().clone(),
        peer_state.selected_model.read().unwrap().clone(),
        *peer_state.logged_in.read().unwrap()
    );
}

#[tokio::test]
async fn refresh_models_emits_available_models_updated_after_prefetch() {
    crate::bus::reset_models_updated_publish_state_for_tests();
    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let registry = Registry::empty();
    let agent = Arc::new(Mutex::new(Agent::new(provider.clone(), registry)));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();

    handle_refresh_models(7, &provider, &agent, &client_event_tx).await;

    let mut saw_done = false;
    let mut saw_models = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, client_event_rx.recv())
            .await
            .expect("receive server event before timeout");
        match event.expect("channel open") {
            ServerEvent::Done { id } => {
                assert_eq!(id, 7);
                saw_done = true;
            }
            ServerEvent::AvailableModelsUpdated {
                provider_name,
                provider_model,
                available_models,
                available_model_routes,
            } => {
                saw_models = Some((
                    provider_name,
                    provider_model,
                    available_models,
                    available_model_routes,
                ));
                break;
            }
            _ => {}
        }
    }

    assert!(saw_done, "expected immediate Done ack");
    let (provider_name, provider_model, available_models, available_model_routes) =
        saw_models.expect("expected AvailableModelsUpdated event");
    assert_eq!(provider_name.as_deref(), Some("mock-auth"));
    assert_eq!(provider_model.as_deref(), Some("logged-out-model"));
    assert_eq!(available_models, vec!["logged-out-model".to_string()]);
    assert!(available_model_routes.iter().any(|route| {
        route.model == "logged-out-model"
            && route.provider == "MockAuth"
            && route.api_method == "mock-auth"
    }));
}

#[tokio::test]
async fn openai_account_switch_reloads_and_retries_a_stale_session_revision() {
    let _guard = EnvGuard::save(&[]);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let account = |label: &str| crate::auth::codex::OpenAiAccount {
        label: label.to_string(),
        access_token: format!("access-{label}"),
        refresh_token: format!("refresh-{label}"),
        id_token: None,
        account_id: Some(format!("acct-{label}")),
        expires_at: Some(now_ms + 60_000),
        email: None,
    };
    let first = crate::auth::codex::upsert_account(account("first")).unwrap();
    let second = crate::auth::codex::upsert_account(account("second")).unwrap();
    crate::auth::codex::set_active_account(&first).unwrap();

    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let mut session = crate::session::Session::create(None, None);
    let identity = provider
        .exact_runtime_identity()
        .expect("active account identity");
    session.exact_runtime_identity = Some(identity.clone());
    session.provider_session_id = Some("durable-resume".to_string());
    session.provider_session_identity = Some(identity);
    session.save().unwrap();
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider,
        Registry::empty(),
        session,
        None,
    )));
    let mut concurrent = crate::session::Session::load(agent.lock().await.session_id()).unwrap();
    concurrent.title = Some("advance CAS revision".to_string());
    concurrent.save().unwrap();
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        agent.lock().await.session_id().to_string(),
        Arc::clone(&agent),
    )])));

    handle_switch_openai_account(41, second.clone(), &sessions, &agent, &client_event_tx).await;

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match client_event_rx.recv().await {
                Some(ServerEvent::Done { id: 41 }) => break,
                Some(ServerEvent::Error {
                    id: 41, message, ..
                }) => panic!("account switch failed instead of retrying stale revision: {message}"),
                Some(_) => {}
                None => panic!("account switch event channel closed"),
            }
        }
    })
    .await
    .expect("account switch completion");
    assert_eq!(
        crate::auth::codex::active_account_label().as_deref(),
        Some(second.as_str())
    );
    let agent = agent.lock().await;
    assert_eq!(agent.provider_session_id(), None);
    assert_eq!(agent.durable_provider_session_id(), None);
    assert!(!account_reconciliation_path().unwrap().exists());
}

#[test]
fn account_reconciliation_lock_path_ignores_per_process_runtime_dir() {
    let _guard = EnvGuard::save(&["JCODE_RUNTIME_DIR"]);
    let runtime_a = tempfile::tempdir().expect("runtime a");
    let runtime_b = tempfile::tempdir().expect("runtime b");
    crate::env::set_var("JCODE_RUNTIME_DIR", runtime_a.path());
    let first = account_reconciliation_path().expect("shared marker path");
    crate::env::set_var("JCODE_RUNTIME_DIR", runtime_b.path());
    let second = account_reconciliation_path().expect("shared marker path");

    assert_eq!(first, second);
    assert!(first.starts_with(crate::storage::jcode_dir().unwrap()));
    assert_eq!(
        first
            .parent()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str()),
        Some("state")
    );
}

#[tokio::test]
async fn legacy_oauth_session_is_enumerated_and_derived_context_is_cleared() {
    let _guard = EnvGuard::save(&[]);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let account = |label: &str| crate::auth::codex::OpenAiAccount {
        label: label.to_string(),
        access_token: format!("access-{label}"),
        refresh_token: format!("refresh-{label}"),
        id_token: None,
        account_id: Some(format!("acct-{label}")),
        expires_at: Some(now_ms + 60_000),
        email: None,
    };
    let first = crate::auth::codex::upsert_account(account("first")).unwrap();
    let second = crate::auth::codex::upsert_account(account("second")).unwrap();
    crate::auth::codex::set_active_account(&first).unwrap();

    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let current = Arc::new(Mutex::new(Agent::new_with_session(
        Arc::clone(&provider),
        Registry::empty(),
        crate::session::Session::create(None, None),
        None,
    )));
    let sessions = Arc::new(RwLock::new(HashMap::new()));

    let legacy_id = crate::id::new_id("legacy_oauth_reconciliation");
    let mut legacy = crate::session::Session::create_with_id(legacy_id.clone(), None, None);
    legacy.provider_key = Some("openai".to_string());
    legacy.model = Some("legacy-model".to_string());
    legacy.provider_session_id = Some("old-account-resume".to_string());
    legacy.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "old account rolling summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    });
    legacy.add_message(
        crate::message::Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "canonical legacy raw history".to_string(),
            cache_control: None,
        }],
    );
    legacy.save().unwrap();

    let targets = managed_target_session_ids(
        &sessions,
        &current,
        &jcode_provider_core::RuntimeKey::OpenAIOAuth,
    )
    .await
    .unwrap();
    assert!(targets.contains(&legacy_id));
    let marker = marker_for_target_account(
        jcode_provider_core::RuntimeKey::OpenAIOAuth,
        &second,
        targets,
    )
    .unwrap();
    persist_pending_account_reconciliation(&marker).unwrap();
    reconcile_pending_account_marker(&sessions, &current)
        .await
        .unwrap();

    let cleaned = crate::session::Session::load(&legacy_id).unwrap();
    assert!(cleaned.exact_runtime_identity.is_none());
    assert!(cleaned.provider_session_id.is_none());
    assert!(cleaned.provider_session_identity.is_none());
    assert!(cleaned.compaction.is_none());
    assert!(cleaned.context_nodes.is_empty());
    assert_eq!(cleaned.messages.len(), 1);
    assert!(!account_reconciliation_path().unwrap().exists());
}

#[tokio::test]
async fn openai_account_switch_invalidates_every_live_agent_provider() {
    let _guard = EnvGuard::save(&[]);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let account = |label: &str| crate::auth::codex::OpenAiAccount {
        label: label.to_string(),
        access_token: format!("access-{label}"),
        refresh_token: format!("refresh-{label}"),
        id_token: None,
        account_id: Some(format!("acct-{label}")),
        expires_at: Some(now_ms + 60_000),
        email: None,
    };
    let first = crate::auth::codex::upsert_account(account("invalidate-first")).unwrap();
    let second = crate::auth::codex::upsert_account(account("invalidate-second")).unwrap();
    crate::auth::codex::set_active_account(&first).unwrap();

    let current_provider = Arc::new(AuthChangeMockProvider::new());
    let current_state = Arc::clone(&current_provider.state);
    let current_trait: Arc<dyn Provider> = current_provider;
    let current_agent = Arc::new(Mutex::new(Agent::new(current_trait, Registry::empty())));

    let peer_provider = Arc::new(AuthChangeMockProvider::new());
    let peer_state = Arc::clone(&peer_provider.state);
    let peer_trait: Arc<dyn Provider> = peer_provider;
    let mut peer_session = crate::session::Session::create(None, None);
    let peer_identity = peer_trait
        .exact_runtime_identity()
        .expect("active peer account identity");
    peer_session.exact_runtime_identity = Some(peer_identity.clone());
    peer_session.provider_session_id = Some("peer-resume".to_string());
    peer_session.provider_session_identity = Some(peer_identity);
    peer_session.save().unwrap();
    let peer_agent = Arc::new(Mutex::new(Agent::new_with_session(
        peer_trait,
        Registry::empty(),
        peer_session,
        None,
    )));
    let (client_event_tx, mut client_event_rx) = mpsc::unbounded_channel();
    let current_id = current_agent.lock().await.session_id().to_string();
    let peer_id = peer_agent.lock().await.session_id().to_string();
    let sessions = Arc::new(RwLock::new(HashMap::from([
        (current_id, Arc::clone(&current_agent)),
        (peer_id.clone(), Arc::clone(&peer_agent)),
    ])));

    handle_switch_openai_account(
        42,
        second.clone(),
        &sessions,
        &current_agent,
        &client_event_tx,
    )
    .await;

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match client_event_rx.recv().await {
                Some(ServerEvent::Done { id: 42 }) => break,
                Some(ServerEvent::Error {
                    id: 42, message, ..
                }) => {
                    panic!("account switch failed: {message}")
                }
                Some(_) => {}
                None => panic!("account switch event channel closed"),
            }
        }
    })
    .await
    .expect("account switch completion");

    assert_eq!(
        crate::auth::codex::active_account_label().as_deref(),
        Some(second.as_str())
    );
    assert_eq!(
        current_state
            .credential_invalidations
            .load(Ordering::SeqCst),
        2,
        "active provider must be invalidated directly and through the live Agent registry"
    );
    assert_eq!(
        peer_state.credential_invalidations.load(Ordering::SeqCst),
        1,
        "peer live Agent must not retain credentials from the previous account"
    );
    assert_eq!(peer_agent.lock().await.provider_session_id(), None);
    assert_eq!(
        crate::session::Session::load(&peer_id)
            .unwrap()
            .provider_session_id,
        None,
        "peer provider-session binding must be cleared durably before the account changes"
    );
}

#[tokio::test]
async fn pending_account_marker_survives_partial_failure_and_restart_retry() {
    let _guard = EnvGuard::save(&[]);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let account = |label: &str| crate::auth::codex::OpenAiAccount {
        label: label.to_string(),
        access_token: format!("access-{label}"),
        refresh_token: format!("refresh-{label}"),
        id_token: None,
        account_id: Some(format!("acct-{label}")),
        expires_at: Some(now_ms + 60_000),
        email: None,
    };
    let first = crate::auth::codex::upsert_account(account("reconcile-first")).unwrap();
    let second = crate::auth::codex::upsert_account(account("reconcile-second")).unwrap();
    crate::auth::codex::set_active_account(&first).unwrap();

    let make_session = || {
        let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
        let mut session = crate::session::Session::create(None, None);
        let identity = provider.exact_runtime_identity().expect("old identity");
        session.exact_runtime_identity = Some(identity.clone());
        session.provider_session_id = Some(format!("resume-{}", session.id));
        session.provider_session_identity = Some(identity);
        session.compaction = Some(crate::session::StoredCompactionState {
            summary_text: "old-account summary".to_string(),
            openai_encrypted_content: None,
            covers_up_to_turn: 0,
            original_turn_count: 1,
            compacted_count: 1,
        });
        session.save().unwrap();
        session.id
    };
    let failed_id = make_session();
    let reconciled_id = make_session();
    let marker = marker_for_target_account(
        jcode_provider_core::RuntimeKey::OpenAIOAuth,
        &second,
        vec![reconciled_id.clone(), failed_id.clone()],
    )
    .unwrap();
    persist_pending_account_reconciliation(&marker).unwrap();
    let marker_json = std::fs::read_to_string(account_reconciliation_path().unwrap()).unwrap();
    assert!(marker_json.contains(&failed_id));
    assert!(marker_json.contains(&marker.account_id));
    assert!(!marker_json.contains("access-"));
    assert!(!marker_json.contains("refresh-"));

    crate::auth::codex::set_active_account(&second).unwrap();
    *FAIL_ACCOUNT_RECONCILIATION_SESSION
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap() = Some(failed_id.clone());
    let startup_provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let error = reconcile_pending_account_transition_on_startup_sync(&startup_provider)
        .expect_err("injected target must retain marker");
    assert!(error.to_string().contains(&failed_id));
    assert!(account_reconciliation_path().unwrap().exists());
    let reconciled = crate::session::Session::load(&reconciled_id).unwrap();
    assert!(reconciled.provider_session_id.is_none());
    assert!(reconciled.compaction.is_none());
    let failed = crate::session::Session::load(&failed_id).unwrap();
    assert!(failed.provider_session_id.is_some());
    assert!(failed.compaction.is_some());

    let provider = AuthChangeMockProvider::new();
    *provider.state.selected_model.write().unwrap() = Some("logged-out-model".to_string());
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let mut blocked_agent = Agent::new_with_session(provider, Registry::empty(), failed, None);
    let message_count_before = blocked_agent.message_count();
    let blocked = blocked_agent
        .run_once("must remain blocked without transcript mutation")
        .await
        .expect_err("model turn must fail closed while marker exists");
    assert!(
        blocked
            .to_string()
            .contains("pending durable session reconciliation")
    );
    assert_eq!(blocked_agent.message_count(), message_count_before);
    let tool_blocked = blocked_agent
        .ensure_tool_identity_ready()
        .expect_err("tool turn must fail closed while marker exists");
    assert!(
        tool_blocked
            .to_string()
            .contains("pending durable session reconciliation")
    );

    // Remove the transient fault without restarting. The next model admission
    // owns a retry, reloads the concurrently advanced durable revision, and may
    // proceed only after the marker is cleared.
    *FAIL_ACCOUNT_RECONCILIATION_SESSION
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap() = None;
    blocked_agent
        .run_once("retry reconciliation in the same server lifetime")
        .await
        .expect("later model admission should reconcile and then proceed");
    assert!(!account_reconciliation_path().unwrap().exists());
    assert!(!reconcile_pending_account_transition_on_startup_sync(&startup_provider).unwrap());
    for session_id in [&failed_id, &reconciled_id] {
        let session = crate::session::Session::load(session_id).unwrap();
        assert!(session.provider_session_id.is_none());
        assert!(session.provider_session_identity.is_none());
        assert!(session.compaction.is_none());
        assert!(session.context_frontier.as_ref().is_none_or(|frontier| {
            frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
        }));
        assert_eq!(
            session
                .exact_runtime_identity
                .as_ref()
                .and_then(|identity| identity.account_id.as_deref()),
            Some(marker.account_id.as_str())
        );
    }
}

#[test]
fn local_account_switch_preparation_failure_keeps_original_account_and_resume_state() {
    let _guard = EnvGuard::save(&[]);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let account = |label: &str| crate::auth::codex::OpenAiAccount {
        label: label.to_string(),
        access_token: format!("access-{label}"),
        refresh_token: format!("refresh-{label}"),
        id_token: None,
        account_id: Some(format!("acct-{label}")),
        expires_at: Some(now_ms + 60_000),
        email: None,
    };
    let first = crate::auth::codex::upsert_account(account("local-first")).unwrap();
    let second = crate::auth::codex::upsert_account(account("local-second")).unwrap();
    crate::auth::codex::set_active_account(&first).unwrap();

    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let mut session = crate::session::Session::create(None, None);
    let identity = provider.exact_runtime_identity().expect("old identity");
    session.exact_runtime_identity = Some(identity.clone());
    session.provider_session_id = Some("old-durable-resume".to_string());
    session.provider_session_identity = Some(identity);
    session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "old-account summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 0,
        original_turn_count: 1,
        compacted_count: 1,
    });
    session.save().unwrap();
    let session_id = session.id.clone();
    *FAIL_ACCOUNT_RECONCILIATION_SESSION
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap() = Some(session_id.clone());

    let error = match prepare_local_account_switch(
        provider,
        jcode_provider_core::RuntimeKey::OpenAIOAuth,
        &second,
        &session_id,
    ) {
        Ok(_) => panic!("injected durable reset failure must abort before credential activation"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("durable transition marker was retained"));
    assert_eq!(
        crate::auth::codex::active_account_label().as_deref(),
        Some(first.as_str()),
        "new credentials must not become active before every durable session is safe"
    );
    let unchanged = crate::session::Session::load(&session_id).unwrap();
    assert_eq!(
        unchanged.provider_session_id.as_deref(),
        Some("old-durable-resume")
    );
    assert!(unchanged.provider_session_identity.is_some());
    assert!(unchanged.compaction.is_some());
    assert!(account_reconciliation_path().unwrap().exists());
    assert!(ensure_no_pending_account_reconciliation().is_err());

    *FAIL_ACCOUNT_RECONCILIATION_SESSION
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .unwrap() = None;
    clear_pending_account_reconciliation().unwrap();
}

#[test]
fn model_turn_admission_lease_blocks_account_transition_exclusive_lease() {
    let _guard = EnvGuard::save(&[]);
    let admission = AccountReconciliationFileLock::acquire_shared().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let exclusive = AccountReconciliationFileLock::acquire().unwrap();
        tx.send(()).unwrap();
        drop(exclusive);
    });

    assert!(
        matches!(
            rx.recv_timeout(std::time::Duration::from_millis(150)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "an account transition must wait while a model turn holds shared admission"
    );
    drop(admission);
    rx.recv_timeout(std::time::Duration::from_secs(2))
        .expect("exclusive account transition should proceed after model turn completion");
    waiter.join().unwrap();
}

#[test]
fn account_transition_exclusive_lease_blocks_new_session_persistence() {
    let _guard = EnvGuard::save(&[]);
    let exclusive = AccountReconciliationFileLock::acquire().unwrap();
    let mut session = crate::session::Session::create(None, None);
    let old_identity = jcode_provider_core::ExactRuntimeIdentity {
        provider_key: "openai".to_string(),
        route: crate::provider::RouteSelection {
            model: "gpt-test".to_string(),
            runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
            api_method: "openai-responses".to_string(),
            provider_label: "OpenAI".to_string(),
            detail: String::new(),
        },
        account_label: Some("old-account".to_string()),
        account_id: Some("acct-old".to_string()),
        account_generation: Some(1),
        reasoning_effort: None,
    };
    session.exact_runtime_identity = Some(old_identity.clone());
    session.provider_session_id = Some("old-account-resume".to_string());
    session.provider_session_identity = Some(old_identity);
    session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "old account projection".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 0,
        original_turn_count: 1,
        compacted_count: 1,
    });
    session.add_message(
        crate::message::Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "canonical raw history".to_string(),
            cache_control: None,
        }],
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        session.save().unwrap();
        tx.send(session.id).unwrap();
    });

    assert!(
        matches!(
            rx.recv_timeout(std::time::Duration::from_millis(150)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "new session persistence must wait until the transition target snapshot is closed"
    );
    crate::session::record_completed_account_transition(
        jcode_provider_core::RuntimeKey::OpenAIOAuth,
        "new-account",
        "acct-new",
        2,
    )
    .unwrap();
    drop(exclusive);
    let session_id = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("session save should proceed after account transition completion");
    writer.join().unwrap();
    assert!(crate::session::session_path(&session_id).unwrap().exists());
    let saved = crate::session::Session::load(&session_id).unwrap();
    let exact = saved.exact_runtime_identity.unwrap();
    assert_eq!(exact.account_label.as_deref(), Some("new-account"));
    assert_eq!(exact.account_id.as_deref(), Some("acct-new"));
    assert_eq!(exact.account_generation, Some(2));
    assert!(saved.provider_session_id.is_none());
    assert!(saved.provider_session_identity.is_none());
    assert!(saved.compaction.is_none());
    assert_eq!(saved.messages.len(), 1);
}

#[test]
fn unloaded_preidentity_reconciliation_does_not_reacquire_shared_save_lease() {
    let _guard = EnvGuard::save(&[]);
    let mut session = crate::session::Session::create(None, None);
    session.provider_key = Some("openai".to_string());
    session.provider_session_id = Some("opaque-old-account-resume".to_string());
    session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "opaque old-account projection".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 0,
        original_turn_count: 1,
        compacted_count: 1,
    });
    session.save().unwrap();
    let session_id = session.id.clone();
    let marker = PendingAccountReconciliation {
        schema_version: ACCOUNT_RECONCILIATION_SCHEMA_VERSION,
        runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
        account_label: "destination".to_string(),
        account_id: "acct-destination".to_string(),
        account_generation: 2,
        target_session_ids: vec![session_id.clone()],
    };

    let exclusive = AccountReconciliationFileLock::acquire().unwrap();
    reconcile_unloaded_session(&session_id, &marker)
        .expect("pre-identity cleanup must use the exclusive transition save path");
    drop(exclusive);

    let reconciled = crate::session::Session::load(&session_id).unwrap();
    assert!(reconciled.exact_runtime_identity.is_none());
    assert!(reconciled.provider_session_id.is_none());
    assert!(reconciled.provider_session_identity.is_none());
    assert!(reconciled.compaction.is_none());
    assert!(
        reconciled
            .context_frontier
            .as_ref()
            .is_none_or(|frontier| frontier.active_node_ids.is_empty()
                && frontier.covered_message_count == 0)
    );
}

#[test]
fn ordinary_session_save_fails_closed_while_reconciliation_marker_is_pending() {
    let _guard = EnvGuard::save(&[]);
    let marker = PendingAccountReconciliation {
        schema_version: ACCOUNT_RECONCILIATION_SCHEMA_VERSION,
        runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
        account_label: "pending-account".to_string(),
        account_id: "acct-pending".to_string(),
        account_generation: 2,
        target_session_ids: Vec::new(),
    };
    persist_pending_account_reconciliation(&marker).unwrap();
    let mut session = crate::session::Session::create(None, None);

    let error = session
        .save()
        .expect_err("ordinary persistence must not pass a crash-left transition marker");
    assert!(error.to_string().contains("reconciliation is pending"));
    assert!(!crate::session::session_path(&session.id).unwrap().exists());
    clear_pending_account_reconciliation().unwrap();
}

#[test]
fn corrupt_completed_transition_primary_never_recovers_previous_account_backup() {
    let _guard = EnvGuard::save(&[]);
    crate::session::record_completed_account_transition(
        jcode_provider_core::RuntimeKey::OpenAIOAuth,
        "previous-account",
        "acct-previous",
        1,
    )
    .unwrap();
    crate::session::record_completed_account_transition(
        jcode_provider_core::RuntimeKey::OpenAIOAuth,
        "current-account",
        "acct-current",
        2,
    )
    .unwrap();
    let state_path = crate::storage::jcode_dir()
        .unwrap()
        .join("state/provider-account-transitions.json");
    std::fs::write(&state_path, b"{corrupt completed transition state").unwrap();

    let mut session = crate::session::Session::create(None, None);
    session.exact_runtime_identity = Some(jcode_provider_core::ExactRuntimeIdentity {
        provider_key: "openai".to_string(),
        route: crate::provider::RouteSelection {
            model: "gpt-test".to_string(),
            runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
            api_method: "openai-responses".to_string(),
            provider_label: "OpenAI".to_string(),
            detail: String::new(),
        },
        account_label: Some("previous-account".to_string()),
        account_id: Some("acct-previous".to_string()),
        account_generation: Some(1),
        reasoning_effort: None,
    });

    let error = session
        .save()
        .expect_err("a corrupt primary watermark must block persistence");
    assert!(!error.to_string().is_empty());
    assert!(!crate::session::session_path(&session.id).unwrap().exists());
}

#[tokio::test]
async fn account_switch_marker_is_durable_before_first_session_mutation() {
    let _guard = EnvGuard::save(&[]);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let account = |label: &str| crate::auth::codex::OpenAiAccount {
        label: label.to_string(),
        access_token: format!("access-{label}"),
        refresh_token: format!("refresh-{label}"),
        id_token: None,
        account_id: Some(format!("acct-{label}")),
        expires_at: Some(now_ms + 60_000),
        email: None,
    };
    let first = crate::auth::codex::upsert_account(account("order-first")).unwrap();
    let second = crate::auth::codex::upsert_account(account("order-second")).unwrap();
    crate::auth::codex::set_active_account(&first).unwrap();
    let provider: Arc<dyn Provider> = Arc::new(AuthChangeMockProvider::new());
    let mut session = crate::session::Session::create(None, None);
    let identity = provider.exact_runtime_identity().unwrap();
    session.exact_runtime_identity = Some(identity.clone());
    session.provider_session_id = Some("must-survive-marker-write-failure".to_string());
    session.provider_session_identity = Some(identity);
    session.save().unwrap();
    let session_id = session.id.clone();
    let agent = Arc::new(Mutex::new(Agent::new_with_session(
        provider,
        Registry::empty(),
        session,
        None,
    )));
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        session_id.clone(),
        Arc::clone(&agent),
    )])));
    let failure =
        crate::storage::inject_write_failure(Some(account_reconciliation_path().unwrap()));
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_switch_openai_account(91, second, &sessions, &agent, &tx).await;
    drop(failure);
    assert!(matches!(
        rx.recv().await,
        Some(ServerEvent::Error { id: 91, .. })
    ));
    assert_eq!(
        crate::auth::codex::active_account_label().as_deref(),
        Some(first.as_str())
    );
    assert_eq!(
        crate::session::Session::load(&session_id)
            .unwrap()
            .provider_session_id
            .as_deref(),
        Some("must-survive-marker-write-failure")
    );
}

#[test]
fn stale_marker_backup_is_never_recovered_after_primary_clear() {
    let _guard = EnvGuard::save(&[]);
    let marker = PendingAccountReconciliation {
        schema_version: ACCOUNT_RECONCILIATION_SCHEMA_VERSION,
        runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
        account_label: "openai-stale".to_string(),
        account_id: "non-secret-stale-id".to_string(),
        account_generation: 1,
        target_session_ids: vec!["session-stale".to_string()],
    };
    let path = account_reconciliation_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path.with_extension("bak"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    assert_eq!(load_pending_account_reconciliation().unwrap(), None);
    clear_pending_account_reconciliation().unwrap();
    assert!(!path.with_extension("bak").exists());
}
