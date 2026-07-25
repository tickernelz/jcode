#[tokio::test]
async fn streaming_generation_change_reconciles_before_tool_and_continuation() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let identity = Arc::new(std::sync::Mutex::new(account_transition_identity(
        "account-a",
        "stable-a",
    )));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = Arc::new(StreamingGenerationProvider {
        identity: Arc::clone(&identity),
        requests: Arc::clone(&requests),
    });
    let registry = Registry::new(provider.clone()).await;
    let stale_executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    registry
        .register(
            "identity_check".to_string(),
            Arc::new(IdentityCheckingTool {
                expected_generation: 2,
                stale_executions: Arc::clone(&stale_executions),
            }),
        )
        .await;
    let mut agent = Agent::new(provider.fork(), registry);
    agent.bind_provider_session_id("generation-one-resume".to_string());
    agent.session.save().expect("save generation-one binding");
    let (tx, _rx) = tokio_mpsc::unbounded_channel();

    agent
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("stream turn");

    assert_eq!(
        stale_executions.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        agent
            .session
            .exact_runtime_identity
            .as_ref()
            .and_then(|value| value.account_generation),
        Some(2)
    );
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.session.provider_session_identity.is_none());
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn blocking_generation_change_reconciles_before_tool_and_continuation() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let identity = Arc::new(std::sync::Mutex::new(account_transition_identity(
        "account-a",
        "stable-a",
    )));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = Arc::new(StreamingGenerationProvider {
        identity: Arc::clone(&identity),
        requests: Arc::clone(&requests),
    });
    let registry = Registry::new(provider.clone()).await;
    let stale_executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    registry
        .register(
            "identity_check".to_string(),
            Arc::new(IdentityCheckingTool {
                expected_generation: 2,
                stale_executions: Arc::clone(&stale_executions),
            }),
        )
        .await;
    let mut agent = Agent::new(provider.fork(), registry);
    agent.bind_provider_session_id("generation-one-resume".to_string());
    agent.session.save().expect("save generation-one binding");

    agent
        .run_once_capture("verify blocking stream identity")
        .await
        .expect("blocking turn");

    assert_eq!(
        stale_executions.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(
        agent
            .session
            .exact_runtime_identity
            .as_ref()
            .and_then(|value| value.account_generation),
        Some(2)
    );
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.session.provider_session_identity.is_none());
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn cross_process_oauth_refresh_reconciles_identity_before_request_open() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let concrete = Arc::new(AccountTransitionProvider {
        identity: Arc::new(std::sync::Mutex::new(account_transition_identity(
            "account-a",
            "stable-a",
        ))),
        switched: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        seen_resume_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
        refresh_identity: Arc::new(std::sync::Mutex::new(Some(account_transition_identity(
            "account-b",
            "stable-b",
        )))),
        credential_refreshes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let registry = Registry::new(concrete.clone()).await;
    let mut agent = Agent::new(concrete.fork(), registry);
    agent.bind_provider_session_id("resume-owned-by-account-a".to_string());
    agent.session.save().expect("persist old account binding");

    agent
        .run_once("refresh before request")
        .await
        .expect("request should open only after exact account reconciliation");

    // Credential reload and reconciliation happen once, under the shared
    // admission lease, so no pre-admission save can race an account switch.
    assert_eq!(
        concrete
            .credential_refreshes
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(*concrete.seen_resume_ids.lock().unwrap(), [None]);
    assert_eq!(
        agent
            .session
            .exact_runtime_identity
            .as_ref()
            .and_then(|identity| identity.account_id.as_deref()),
        Some("stable-b")
    );
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.session.compaction.is_none());
    assert!(agent.session.context_nodes.is_empty());

    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn provider_resume_binding_rejects_each_exact_identity_dimension() {
    let mut effective = account_transition_identity("account-a", "stable-a");
    effective.route.runtime_key = jcode_provider_core::RuntimeKey::OpenAiCompatible {
        profile_id: Some("profile-a".to_string()),
    };
    effective.route.detail = "endpoint-a".to_string();
    let concrete = Arc::new(AccountTransitionProvider {
        identity: Arc::new(std::sync::Mutex::new(effective.clone())),
        switched: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        seen_resume_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
        refresh_identity: Arc::new(std::sync::Mutex::new(None)),
        credential_refreshes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let provider: Arc<dyn Provider> = concrete;
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let mut mismatches = Vec::new();
    let mut identity = effective.clone();
    identity.provider_key = "different-provider".to_string();
    mismatches.push(("provider", identity));
    let mut identity = effective.clone();
    identity.route.model = "different-model".to_string();
    mismatches.push(("model", identity));
    let mut identity = effective.clone();
    identity.route.api_method = "different-api-method".to_string();
    mismatches.push(("api_method", identity));
    let mut identity = effective.clone();
    identity.route.runtime_key = jcode_provider_core::RuntimeKey::OpenAIApiKey;
    mismatches.push(("runtime", identity));
    let mut identity = effective.clone();
    identity.route.runtime_key = jcode_provider_core::RuntimeKey::OpenAiCompatible {
        profile_id: Some("profile-b".to_string()),
    };
    mismatches.push(("profile", identity));
    let mut identity = effective.clone();
    identity.route.detail = "endpoint-b".to_string();
    mismatches.push(("route_detail", identity));
    let mut identity = effective.clone();
    identity.account_label = Some("account-b".to_string());
    mismatches.push(("account_label", identity));
    let mut identity = effective.clone();
    identity.account_id = Some("stable-b".to_string());
    mismatches.push(("account_id", identity));
    let mut identity = effective.clone();
    identity.account_generation = Some(2);
    mismatches.push(("account_generation", identity));
    let mut identity = effective.clone();
    identity.reasoning_effort = Some("high".to_string());
    mismatches.push(("reasoning_effort", identity));

    for (dimension, mismatched) in mismatches {
        let resume_id = format!("resume-for-{dimension}");
        agent.provider_session_id = Some(resume_id.clone());
        agent.session.provider_session_id = Some(resume_id);
        agent.session.provider_session_identity = Some(mismatched);
        agent.sync_exact_runtime_identity_from_provider();
        assert!(
            agent.provider_session_id.is_none(),
            "runtime resume binding survived {dimension} mismatch"
        );
        assert!(
            agent.session.provider_session_id.is_none(),
            "durable resume binding survived {dimension} mismatch"
        );
        assert!(agent.session.provider_session_identity.is_none());
    }
}

#[tokio::test]
async fn provider_resume_binding_uses_stream_identity_not_mutable_provider_identity() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let stream_identity = account_transition_identity("account-a", "stable-a");
    let mutable_identity = Arc::new(std::sync::Mutex::new(stream_identity.clone()));
    let provider: Arc<dyn Provider> = Arc::new(AccountTransitionProvider {
        identity: Arc::clone(&mutable_identity),
        switched: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        seen_resume_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
        refresh_identity: Arc::new(std::sync::Mutex::new(None)),
        credential_refreshes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    *mutable_identity.lock().unwrap() = account_transition_identity("account-b", "stable-b");
    assert!(agent.bind_provider_session_id_for_identity(
        "response-owned-by-account-a".to_string(),
        Some(&stream_identity),
    ));
    assert_eq!(
        agent.session.provider_session_identity.as_ref(),
        Some(&stream_identity),
        "a response resume id must never be stamped with the provider's later mutable identity"
    );

    agent
        .reconcile_exact_runtime_identity_after_request_open(Some(&stream_identity))
        .expect("final stream reconciliation");
    assert_eq!(
        agent
            .session
            .exact_runtime_identity
            .as_ref()
            .and_then(|identity| identity.account_label.as_deref()),
        Some("account-b")
    );
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.session.provider_session_identity.is_none());
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn opaque_runtime_identity_cannot_execute_tools() {
    let provider: Arc<dyn Provider> = Arc::new(DelayedProvider {
        open_delay: Duration::ZERO,
        first_event_delay: Duration::ZERO,
    });
    let registry = Registry::new(provider.clone()).await;
    let agent = Agent::new(provider, registry);

    let error = agent
        .execute_tool("definitely_missing", serde_json::json!({}))
        .await
        .expect_err("opaque identity must fail before tool dispatch");
    assert!(
        error
            .to_string()
            .contains("exact provider/account identity is opaque")
    );
}
struct ProviderSessionEventProvider;

#[async_trait]
impl Provider for ProviderSessionEventProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (tx, rx) = tokio_mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::SessionId("provider-resume".to_string())))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "provider-session-event"
    }

    fn model(&self) -> String {
        "provider-session-model".to_string()
    }

    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(agent_test_identity(self.name(), &self.model()))
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[tokio::test]
async fn provider_resume_id_is_not_forwarded_as_canonical_remote_session_id() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let provider: Arc<dyn Provider> = Arc::new(ProviderSessionEventProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let canonical_session_id = agent.session.id.clone();
    let (tx, mut rx) = tokio_mpsc::unbounded_channel();

    agent
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("streaming turn");

    assert_eq!(agent.provider_session_id.as_deref(), Some("provider-resume"));
    assert_eq!(agent.session.id, canonical_session_id);
    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(event, crate::protocol::ServerEvent::SessionId { .. }),
            "provider resume IDs must not replace canonical Jcode session ownership"
        );
    }
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn verified_serving_model_fallback_reconciles_after_request_open() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let mut previous = account_transition_identity("account-a", "stable-a");
    previous.route.runtime_key = jcode_provider_core::RuntimeKey::OpenRouter;
    previous.route.provider_label = "pinned-provider".to_string();
    let identity = Arc::new(std::sync::Mutex::new(previous.clone()));
    let concrete = Arc::new(AccountTransitionProvider {
        identity: Arc::clone(&identity),
        switched: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        seen_resume_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
        refresh_identity: Arc::new(std::sync::Mutex::new(None)),
        credential_refreshes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let registry = Registry::new(concrete.clone()).await;
    let mut agent = Agent::new(concrete, registry);
    agent.bind_provider_session_id("old-model-resume".to_string());
    agent.session.save().expect("save old model binding");

    let mut served = previous.clone();
    served.route.model = "fallback-model".to_string();
    let persisted_model = crate::provider::persisted_session_model_for_route(
        &served.route,
        &served.route.model,
    );
    *identity.lock().unwrap() = served.clone();

    agent
        .reconcile_exact_runtime_identity_after_request_open(Some(&previous))
        .expect("verified serving-model fallback should reconcile");

    assert_eq!(agent.session.exact_runtime_identity.as_ref(), Some(&served));
    assert_eq!(agent.persisted_serving_model("fallback-model"), persisted_model);
    assert_eq!(agent.session.model.as_deref(), Some(persisted_model.as_str()));
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.session.provider_session_identity.is_none());
    assert_eq!(
        crate::session::Session::load(agent.session_id())
            .expect("load fallback identity")
            .model
            .as_deref(),
        Some(persisted_model.as_str())
    );
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn automatic_identity_reconciliation_does_not_overwrite_newer_cas_winner() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let mut previous = account_transition_identity("account-a", "stable-a");
    previous.account_generation = Some(1);
    let identity = Arc::new(std::sync::Mutex::new(previous.clone()));
    let concrete = Arc::new(AccountTransitionProvider {
        identity: Arc::clone(&identity),
        switched: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        seen_resume_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
        refresh_identity: Arc::new(std::sync::Mutex::new(None)),
        credential_refreshes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let registry = Registry::new(concrete.clone()).await;
    let mut agent = Agent::new(concrete, registry);
    agent.session.exact_runtime_identity = Some(previous.clone());
    agent.session.save().expect("save previous identity");

    let mut current = previous.clone();
    current.account_generation = Some(2);
    *identity.lock().unwrap() = current;
    let mut peer = crate::session::Session::load(agent.session_id()).unwrap();
    let mut newer = previous.clone();
    newer.account_generation = Some(3);
    peer.exact_runtime_identity = Some(newer.clone());
    peer.provider_session_id = None;
    peer.provider_session_identity = None;
    peer.reset_context_graph_for_identity_transition();
    peer.save().expect("persist newer peer identity");

    let error = agent
        .reconcile_exact_runtime_identity_after_request_open(Some(&previous))
        .expect_err("stale automatic transition must lose to newer durable identity");
    assert!(error.to_string().contains("lost CAS"));
    assert_eq!(
        crate::session::Session::load(agent.session_id())
            .unwrap()
            .exact_runtime_identity,
        Some(newer)
    );
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn model_switch_fails_before_publication_when_compaction_is_busy() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let provider: Arc<dyn Provider> = Arc::new(SwitchingProvider {
        model: std::sync::Mutex::new("old-model".to_string()),
        reasoning_effort: std::sync::Mutex::new(None),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.session.model = Some("old-model".to_string());
    agent.session.provider_session_id = Some("old-resume".to_string());
    agent.provider_session_id = Some("old-resume".to_string());
    agent.session.save().expect("save old model");
    let compaction = agent.registry.compaction();
    let busy = compaction.write().await;

    assert!(agent.set_model("new-model").is_err());

    drop(busy);
    assert_eq!(agent.provider_model(), "old-model");
    assert_eq!(agent.session.model.as_deref(), Some("old-model"));
    assert_eq!(agent.provider_session_id.as_deref(), Some("old-resume"));
    assert_eq!(
        crate::session::Session::load(agent.session_id())
            .expect("load durable session")
            .model
            .as_deref(),
        Some("old-model")
    );
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn reasoning_switch_fails_before_publication_when_compaction_is_busy() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let provider = Arc::new(SwitchingProvider {
        model: std::sync::Mutex::new("effort-model".to_string()),
        reasoning_effort: std::sync::Mutex::new(Some("low".to_string())),
    });
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_trait.clone()).await;
    let mut agent = Agent::new(provider_trait, registry);
    agent.session.reasoning_effort = Some("low".to_string());
    agent.session.provider_session_id = Some("old-resume".to_string());
    agent.provider_session_id = Some("old-resume".to_string());
    agent.session.save().expect("save old reasoning");
    let compaction = agent.registry.compaction();
    let busy = compaction.write().await;

    assert!(agent.set_reasoning_effort("high").is_err());

    drop(busy);
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    assert_eq!(agent.session.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(agent.provider_session_id.as_deref(), Some("old-resume"));
    assert_eq!(
        crate::session::Session::load(agent.session_id())
            .expect("load durable session")
            .reasoning_effort
            .as_deref(),
        Some("low")
    );
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn route_switch_fails_before_publication_when_compaction_is_busy() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let provider: Arc<dyn Provider> = Arc::new(SwitchingProvider {
        model: std::sync::Mutex::new("old-model".to_string()),
        reasoning_effort: std::sync::Mutex::new(None),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.session.model = Some("old-model".to_string());
    agent.session.save().expect("save old route");
    let mut route = agent_test_identity("switching", "new-model").route;
    route.api_method = "new-api-method".to_string();
    let compaction = agent.registry.compaction();
    let busy = compaction.write().await;

    assert!(agent.set_route_selection(&route).is_err());

    drop(busy);
    assert_eq!(agent.provider_model(), "old-model");
    assert_eq!(agent.session.model.as_deref(), Some("old-model"));
    assert_eq!(
        crate::session::Session::load(agent.session_id())
            .expect("load durable session")
            .model
            .as_deref(),
        Some("old-model")
    );
    crate::env::remove_var("JCODE_HOME");
}
