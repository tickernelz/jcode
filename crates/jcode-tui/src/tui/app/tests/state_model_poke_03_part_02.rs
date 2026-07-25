#[test]
fn test_tui_openai_compatible_empty_catalog_does_not_switch_to_profile_default() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let refreshes = StdArc::new(AtomicUsize::new(0));
    let set_model_attempts = StdArc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(EmptyPostLoginCatalogProvider {
        refreshes: StdArc::clone(&refreshes),
        set_model_attempts: StdArc::clone(&set_model_attempts),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    let mut bus_rx = crate::bus::Bus::global().subscribe();
    while bus_rx.try_recv().is_ok() {}

    let _guard = rt.enter();
    app.start_openai_compatible_post_login_activation(
        "cerebras".to_string(),
        "Cerebras".to_string(),
    );

    let activity = rt.block_on(async {
        loop {
            match tokio::time::timeout(Duration::from_secs(2), bus_rx.recv()).await {
                Ok(Ok(crate::bus::BusEvent::ProviderModelActivated { .. })) => {
                    panic!("empty catalog must not activate a provider model")
                }
                Ok(Ok(crate::bus::BusEvent::LoginCompleted(login))) => {
                    panic!("empty local catalog must not publish final login failure: {login:?}")
                }
                Ok(Ok(crate::bus::BusEvent::UiActivity(activity)))
                    if activity.message.contains("Model Discovery Still Updating") =>
                {
                    break activity;
                }
                Ok(Ok(_)) => continue,
                other => panic!("expected pending catalog activity, got {other:?}"),
            }
        }
    });

    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        set_model_attempts.load(Ordering::SeqCst),
        0,
        "post-login activation must not try the metadata default when the catalog has no selectable route"
    );
    assert!(activity.message.contains("Saved credentials are active"));
    assert!(activity.message.contains("Jcode is still processing"));
    assert!(!activity.message.contains("did not switch models"));
    assert!(!activity.message.contains("documented default"));
    assert!(!activity.message.contains("qwen-3-coder-480b"));
}

#[test]
fn test_tui_openai_compatible_local_refresh_failure_is_pending_not_final_failure() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let refreshes = StdArc::new(AtomicUsize::new(0));
    let set_model_attempts = StdArc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(FailingPostLoginCatalogProvider {
        refreshes: StdArc::clone(&refreshes),
        set_model_attempts: StdArc::clone(&set_model_attempts),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    let mut bus_rx = crate::bus::Bus::global().subscribe();
    while bus_rx.try_recv().is_ok() {}

    let _guard = rt.enter();
    app.start_openai_compatible_post_login_activation(
        "cerebras".to_string(),
        "Cerebras".to_string(),
    );

    let activity = rt.block_on(async {
        loop {
            match tokio::time::timeout(Duration::from_secs(2), bus_rx.recv()).await {
                Ok(Ok(crate::bus::BusEvent::ProviderModelActivated { .. })) => {
                    panic!("failing local refresh must not activate a provider model")
                }
                Ok(Ok(crate::bus::BusEvent::LoginCompleted(login))) => {
                    panic!(
                        "local refresh failure must not publish a final login failure while server auth-change recovery can still finish: {login:?}"
                    )
                }
                Ok(Ok(crate::bus::BusEvent::UiActivity(activity)))
                    if activity.message.contains("Model Discovery Still Updating") =>
                {
                    break activity;
                }
                Ok(Ok(_)) => continue,
                other => panic!("expected pending catalog activity, got {other:?}"),
            }
        }
    });

    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        set_model_attempts.load(Ordering::SeqCst),
        0,
        "local refresh failure must not try to switch models from an unavailable catalog"
    );
    assert!(activity.message.contains("Saved credentials are active"));
    assert!(
        activity
            .message
            .contains("server auth-change catalog refresh")
    );
    assert!(activity.message.contains("fixture refresh failed"));
    assert!(!activity.message.contains("Login: failed"));
    assert!(!activity.message.contains("Unable to sign in"));
    assert!(!activity.message.contains("did not switch models"));
}

#[test]
fn test_model_picker_opens_simplified_state_before_async_routes_complete() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let calls = StdArc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CountingModelRoutesProvider {
        calls: StdArc::clone(&calls),
        route_count: 2,
        delay: Duration::from_millis(75),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("loading picker should open immediately");
    assert_eq!(picker.entries.len(), 1);
    assert_eq!(picker.entries[0].name, "counting-a");
    assert_eq!(picker.entries[0].options[0].detail, "simplified catalog");
    assert!(app.pending_model_picker_load.is_some());
    assert_eq!(
        app.status_notice(),
        Some("Updating model routes…".to_string())
    );

    wait_for_model_picker_load(&mut app);
    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("hydrated picker should still be open");
    assert!(picker.entries.len() >= 2);
    assert_eq!(app.status_notice(), Some("Model list updated".to_string()));
}

#[test]
fn test_model_picker_state_space_preserves_provider_labels_after_route_hydration() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let provider: Arc<dyn Provider> = Arc::new(MixedModelRoutesProvider {
        model: StdArc::new(StdMutex::new("gpt-5.5".to_string())),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.recent_authenticated_provider = Some(("chutes".to_string(), Instant::now()));

    app.open_model_picker();
    wait_for_model_picker_load(&mut app);

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("hydrated mixed-provider model picker should be open");
    let mut routes_by_model = std::collections::BTreeMap::new();
    for entry in &picker.entries {
        let route = entry
            .active_option()
            .expect("model picker entry should have an active route");
        routes_by_model.insert(
            entry.name.clone(),
            (route.provider.clone(), route.api_method.clone()),
        );
    }

    // Models with reasoning-effort support expand into effort rows (issue
    // #458); the hydrated route must be preserved on each variant.
    assert_eq!(
        routes_by_model.get("gpt-5.5 (high)"),
        Some(&("OpenAI".to_string(), "openai-oauth".to_string()))
    );
    assert_eq!(
        routes_by_model.get("claude-opus-4-6 (high)"),
        Some(&("Anthropic".to_string(), "claude-oauth".to_string()))
    );
    assert_eq!(
        routes_by_model.get("Qwen/Qwen3-Coder-480B-A35B-Instruct"),
        Some(&("Chutes".to_string(), "openai-compatible:chutes".to_string()))
    );
    assert_eq!(
        routes_by_model.get("deepseek/deepseek-v4-pro (high)"),
        Some(&("auto".to_string(), "openrouter".to_string()))
    );

    let chutes_rows = picker
        .entries
        .iter()
        .filter(|entry| {
            entry
                .active_option()
                .map(|route| route.provider == "Chutes")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        chutes_rows, 1,
        "opening the model list must not collapse every route to the recently authenticated direct provider: {:?}",
        routes_by_model
    );
}

#[test]
fn test_model_picker_does_not_cache_single_model_fallback() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let calls = StdArc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CountingModelRoutesProvider {
        calls: StdArc::clone(&calls),
        route_count: 1,
        delay: Duration::ZERO,
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    app.open_model_picker();
    wait_for_model_picker_load(&mut app);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        app.model_picker_cache.is_none(),
        "single-model fallback results should not be retained"
    );

    app.open_model_picker();
    wait_for_model_picker_load(&mut app);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "single-model fallback should be rebuilt so a later full catalog can surface"
    );
}

#[test]
fn test_local_model_picker_selection_failure_keeps_picker_open_and_shows_next_steps() {
    let mut app = create_failing_model_switch_test_app();

    app.open_model_picker();
    wait_for_model_picker_load(&mut app);
    assert!(app.inline_interactive_state.is_some());

    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .expect("enter should be handled");

    assert!(
        app.inline_interactive_state.is_some(),
        "picker should remain open so the user can choose another model"
    );
    assert_eq!(app.status_notice(), Some("Model switch failed".to_string()));

    let last = app.display_messages.last().expect("display message");
    assert_eq!(last.role, "error");
    assert!(last.content.contains("credentials expired"));
    assert!(last.content.contains("/model"));
    assert!(last.content.contains("/login"));
    assert!(last.content.contains("/account"));
}

#[test]
fn test_login_completed_spawns_auth_refresh_when_runtime_is_available() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let started = StdArc::new(AtomicBool::new(false));
    let completed = StdArc::new(AtomicBool::new(false));
    let provider: Arc<dyn Provider> = Arc::new(AsyncAuthRefreshingMockProvider {
        started: StdArc::clone(&started),
        completed: StdArc::clone(&completed),
        delay: Duration::from_millis(150),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    let _guard = rt.enter();
    let start = Instant::now();
    app.handle_login_completed(crate::bus::LoginCompleted {
        provider: "openrouter".to_string(),
        success: true,
        message: "OpenRouter ready".to_string(),
    });
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "login completion should not block on auth refresh, took {:?}",
        elapsed
    );

    let wait_start = Instant::now();
    while !started.load(Ordering::SeqCst) || !completed.load(Ordering::SeqCst) {
        assert!(
            wait_start.elapsed() < Duration::from_secs(2),
            "background auth refresh did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn test_model_picker_waits_for_async_post_login_catalog_activation() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let logged_in = StdArc::new(StdMutex::new(false));
    let provider: Arc<dyn Provider> = Arc::new(AuthRefreshingMockProvider {
        logged_in: StdArc::clone(&logged_in),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    let mut bus_rx = crate::bus::Bus::global().subscribe();

    {
        let _guard = rt.enter();
        app.handle_login_completed(crate::bus::LoginCompleted {
            provider: "auto-import".to_string(),
            success: true,
            message: "Imported existing logins".to_string(),
        });
        app.open_model_picker();
    }

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("loading model picker should be open");
    assert_eq!(picker.entries.len(), 1);
    assert!(
        picker.entries[0].options[0]
            .detail
            .contains("updating model list")
    );
    // The single loading row labels the *current* model (which may legitimately
    // still be the pre-import one until async activation lands), so check the
    // route metadata: the stale pre-import catalog route must not be shown as a
    // selectable ready entry.
    assert!(
        !picker
            .entries
            .iter()
            .flat_map(|entry| entry.options.iter())
            .any(|option| option.api_method == "openai-oauth"),
        "the stale pre-import catalog must not be presented as ready"
    );

    let ready = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = bus_rx.recv().await.expect("auth catalog event");
                if matches!(event, crate::bus::BusEvent::AuthCatalogRefreshReady) {
                    break event;
                }
            }
        })
        .await
        .expect("post-login activation should finish")
    });
    assert!(crate::tui::app::local::handle_bus_event(
        &mut app,
        Ok(ready)
    ));
    assert!(*logged_in.lock().unwrap());
    wait_for_model_picker_load(&mut app);

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should refresh in place");
    assert!(
        picker
            .entries
            .iter()
            .any(|entry| entry.name == "claude-opus-4.6")
    );
    assert!(
        picker
            .entries
            .iter()
            .any(|entry| entry.name == "grok-code-fast-1")
    );
}

#[test]
fn test_login_completed_surfaces_new_provider_models_in_local_model_picker() {
    let mut app = create_auth_refresh_test_app();

    app.handle_login_completed(crate::bus::LoginCompleted {
        provider: "copilot".to_string(),
        success: true,
        message: "Authenticated as **octocat** via GitHub Copilot.\n\nCopilot models are now available in `/model`."
            .to_string(),
    });

    app.open_model_picker();
    wait_for_model_picker_load(&mut app);

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");

    let copilot_entry = picker
        .entries
        .iter()
        .find(|entry| entry.name == "claude-opus-4.6")
        .expect("copilot model should be shown after login");

    assert!(
        picker
            .entries
            .iter()
            .any(|entry| entry.name == "grok-code-fast-1"),
        "all newly available Copilot models should appear in /model"
    );
    assert!(copilot_entry.options.iter().any(|route| {
        route.provider == "Copilot" && route.api_method == "copilot" && route.available
    }));

    assert!(
        picker.entries[0]
            .options
            .iter()
            .any(|route| route.provider == "Copilot" && route.detail.contains("recently added")),
        "recently authenticated provider should be prioritized and marked in /model"
    );
}

#[derive(Clone)]
struct AzureLoginMockProvider {
    model: StdArc<StdMutex<String>>,
    auth_changed: StdArc<AtomicUsize>,
    complete_calls: StdArc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Provider for AzureLoginMockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        let stream = futures::stream::empty::<Result<crate::message::StreamEvent>>();
        Ok(Box::pin(stream) as crate::provider::EventStream)
    }

    fn name(&self) -> &str {
        "OpenRouter"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        let model = model
            .trim()
            .strip_prefix("openrouter:")
            .unwrap_or_else(|| model.trim())
            .trim();
        if model.is_empty() {
            anyhow::bail!("model cannot be empty");
        }
        *self.model.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn available_models_display(&self) -> Vec<String> {
        vec![self.model()]
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        vec![crate::provider::ModelRoute {
            model: self.model(),
            provider: "Azure OpenAI".to_string(),
            api_method: "openai-compatible".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }]
    }

    fn on_auth_changed(&self) {
        self.auth_changed.fetch_add(1, Ordering::SeqCst);
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

struct AzureLoginEnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl AzureLoginEnvGuard {
    fn save(keys: &[&'static str]) -> Self {
        let saved = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        for key in keys {
            crate::env::remove_var(key);
        }
        Self { saved }
    }
}

impl Drop for AzureLoginEnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            if let Some(value) = value {
                crate::env::set_var(key, value);
            } else {
                crate::env::remove_var(key);
            }
        }
    }
}

#[test]
fn test_azure_login_completion_switches_local_model_without_completion() {
    let _env_lock = crate::storage::lock_test_env();
    let _guard = AzureLoginEnvGuard::save(&[
        "AZURE_OPENAI_ENDPOINT",
        "AZURE_OPENAI_MODEL",
        "AZURE_OPENAI_API_KEY",
        "AZURE_OPENAI_USE_ENTRA",
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_AUTH_HEADER",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "JCODE_OPENROUTER_MODEL",
        "JCODE_RUNTIME_PROVIDER",
        "JCODE_ACTIVE_PROVIDER",
        "JCODE_FORCE_PROVIDER",
    ]);
    crate::env::set_var("AZURE_OPENAI_ENDPOINT", "https://example.openai.azure.com");
    crate::env::set_var("AZURE_OPENAI_MODEL", "azure-deployment");
    crate::env::set_var("AZURE_OPENAI_API_KEY", "test-key");
    crate::env::set_var("AZURE_OPENAI_USE_ENTRA", "0");

    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let model = StdArc::new(StdMutex::new("old-model".to_string()));
    let auth_changed = StdArc::new(AtomicUsize::new(0));
    let complete_calls = StdArc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(AzureLoginMockProvider {
        model: StdArc::clone(&model),
        auth_changed: StdArc::clone(&auth_changed),
        complete_calls: StdArc::clone(&complete_calls),
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    // This test asserts the login-completed status notice; a brand-new-install
    // classification would let first-run onboarding overwrite it with the
    // StartChoice prompt. Pre-commit the onboarding guard so the flow never
    // starts.
    app.onboarding_startup_checked = true;
    app.onboarding_flow = Some(crate::tui::app::onboarding_flow::OnboardingFlow {
        phase: crate::tui::app::onboarding_flow::OnboardingPhase::Done,
    });
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.provider_session_id = Some("stale-upstream".to_string());
    app.session.provider_session_id = Some("stale-upstream".to_string());
    app.session.model = Some("old-model".to_string());

    app.handle_login_completed(crate::bus::LoginCompleted {
        provider: "Azure OpenAI".to_string(),
        success: true,
        message: "Azure OpenAI ready".to_string(),
    });

    assert_eq!(&*model.lock().unwrap(), "azure-deployment");
    assert_eq!(app.session.model.as_deref(), Some("azure-deployment"));
    assert_eq!(app.provider_session_id, None);
    assert_eq!(app.session.provider_session_id, None);
    assert_eq!(auth_changed.load(Ordering::SeqCst), 1);
    assert_eq!(complete_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::env::var("JCODE_RUNTIME_PROVIDER").as_deref(),
        Ok("azure-openai")
    );
    assert_eq!(
        app.status_notice(),
        Some("Login: Azure OpenAI ready (azure-deployment)".to_string())
    );
}

#[test]
fn test_local_model_picker_surfaces_antigravity_models_from_multiprovider() {
    let mut app = create_antigravity_picker_test_app();
    app.open_model_picker();
    wait_for_model_picker_load(&mut app);

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");

    let antigravity_entry = picker
        .entries
        .iter()
        .find(|entry| entry.name == "claude-sonnet-4-6")
        .expect("antigravity model should be shown after login");

    assert!(antigravity_entry.options.iter().any(|route| {
        route.provider == "Antigravity" && route.api_method == "cli" && route.available
    }));
}

#[test]
fn test_local_antigravity_model_picker_selection_preserves_antigravity_provider() {
    let mut app = create_antigravity_picker_test_app();
    app.open_model_picker();
    wait_for_model_picker_load(&mut app);

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");

    let model_idx = picker
        .entries
        .iter()
        .position(|entry| entry.name == "claude-sonnet-4-6")
        .expect("antigravity model should be in picker");
    let filtered_pos = picker
        .filtered
        .iter()
        .position(|&i| i == model_idx)
        .expect("antigravity model should be in filtered list");

    app.inline_interactive_state.as_mut().unwrap().selected = filtered_pos;
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    assert_eq!(app.provider.name(), "Antigravity");
    assert_eq!(app.provider.model(), "claude-sonnet-4-6");
    assert!(app.inline_interactive_state.is_none());
}

#[test]
fn test_local_model_picker_openrouter_bare_openai_route_uses_openai_catalog_prefix() {
    let (mut app, set_model_calls) = create_openrouter_spec_capture_test_app();
    app.open_model_picker();
    wait_for_model_picker_load(&mut app);

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");
    let model_idx = picker
        .entries
        .iter()
        .position(|entry| entry.name == "gpt-5.4 (high)")
        .expect("openrouter-backed OpenAI effort entry should be in picker");
    let filtered_pos = picker
        .filtered
        .iter()
        .position(|&i| i == model_idx)
        .expect("entry should be in filtered list");

    app.inline_interactive_state.as_mut().unwrap().selected = filtered_pos;
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .expect("model picker selection should succeed");

    assert_eq!(
        set_model_calls.lock().unwrap().as_slice(),
        ["openai/gpt-5.4@OpenAI"]
    );
    assert_eq!(
        app.session.model.as_deref(),
        Some("openai/gpt-5.4@OpenAI"),
        "session persistence must retain the exact OpenRouter provider pin"
    );
}

#[test]
fn test_agent_model_picker_openrouter_bare_openai_route_saves_openai_catalog_prefix() {
    let (mut app, _set_model_calls) = create_openrouter_spec_capture_test_app();

    app.open_agent_model_picker(crate::tui::AgentModelTarget::Swarm);

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("agent model picker should be open");
    let model_idx = picker
        .entries
        .iter()
        .position(|entry| entry.name == "gpt-5.4 (high)")
        .expect("openrouter-backed OpenAI effort entry should be in picker");
    let filtered_pos = picker
        .filtered
        .iter()
        .position(|&i| i == model_idx)
        .expect("entry should be in filtered list");

    app.inline_interactive_state.as_mut().unwrap().selected = filtered_pos;
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .expect("agent model picker selection should succeed");

    let last = app.display_messages.last().expect("display message");
    assert_eq!(last.role, "system");
    assert!(
        last.content.contains("openai/gpt-5.4@OpenAI"),
        "message should show normalized saved spec, got: {}",
        last.content
    );
}

#[test]
fn test_local_model_picker_render_shows_antigravity_models_exactly_as_user_sees_them() {
    let mut app = create_antigravity_picker_test_app();
    app.display_messages = vec![DisplayMessage::system("seed render state")];
    app.bump_display_messages_version();
    app.open_model_picker();
    wait_for_model_picker_load(&mut app);

    let render_filtered = |app: &mut App, filter: &str| {
        let picker = app
            .inline_interactive_state
            .as_mut()
            .expect("model picker should be open");
        picker.filter = filter.to_string();
        App::apply_inline_interactive_filter(picker);
        let _render_lock = scroll_render_test_lock();
        let backend = ratatui::backend::TestBackend::new(90, 14);
        let mut terminal =
            ratatui::Terminal::new(backend).expect("failed to create test terminal");
        render_and_snap(app, &mut terminal)
    };
    let claude_text = render_filtered(&mut app, "claude-sonnet-4-6");
    let gpt_text = render_filtered(&mut app, "gpt-oss-120b-medium");

    assert!(
        claude_text.contains("MODEL")
            && claude_text.contains("PROVIDER")
            && claude_text.contains("METHOD"),
        "rendered /model view should include picker columns, got:
{}",
        claude_text
    );
    assert!(
        claude_text.contains("claude-sonnet-4-6"),
        "rendered /model view should show the Antigravity Claude row, got:
{}",
        claude_text
    );
    assert!(
        gpt_text.contains("gpt-oss-120b-medium"),
        "rendered /model view should show the Antigravity GPT row, got:
{}",
        gpt_text
    );
    assert!(
        claude_text.contains("Antigravity") && gpt_text.contains("Antigravity"),
        "rendered /model view should show the Antigravity provider column, got:
Claude:
{}
GPT:
{}",
        claude_text,
        gpt_text
    );
    assert!(
        claude_text.contains("cli") && gpt_text.contains("cli"),
        "rendered /model view should show the route transport column, got:
Claude:
{}
GPT:
{}",
        claude_text,
        gpt_text
    );
}

