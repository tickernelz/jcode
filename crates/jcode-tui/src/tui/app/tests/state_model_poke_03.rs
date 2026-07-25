#[test]
fn test_model_picker_preview_arrow_keys_navigate() {
    let mut app = create_test_app();
    configure_test_remote_models(&mut app);

    // Type /model to open preview
    for c in "/model".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker preview should be open");
    assert!(picker.preview);
    let initial_selected = picker.selected;

    // Down arrow should navigate in preview mode
    app.handle_key(KeyCode::Down, KeyModifiers::empty())
        .unwrap();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("picker should still be open");
    assert!(picker.preview, "should remain in preview mode");
    assert_eq!(picker.selected, initial_selected + 1);

    // Up arrow should navigate back
    app.handle_key(KeyCode::Up, KeyModifiers::empty()).unwrap();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("picker should still be open");
    assert!(picker.preview, "should remain in preview mode");
    assert_eq!(picker.selected, initial_selected);

    // Opening the preview should place the cursor in the model filter argument.
    assert_eq!(app.input(), "/model ");
}

#[test]
fn test_open_model_picker_without_routes_shows_actionable_guidance() {
    let mut app = create_test_app();

    app.open_model_picker();
    wait_for_model_picker_load(&mut app);

    assert!(app.inline_interactive_state.is_none());
    assert_eq!(app.status_notice(), Some("No models available".to_string()));

    let last = app.display_messages.last().expect("display message");
    assert_eq!(last.role, "system");
    assert!(last.content.contains("/login"));
    assert!(last.content.contains("/account"));
    assert!(last.content.contains("/model"));
}

#[derive(Clone)]
struct CountingModelRoutesProvider {
    calls: StdArc<AtomicUsize>,
    route_count: usize,
    delay: Duration,
}

#[derive(Clone)]
struct MixedModelRoutesProvider {
    model: StdArc<StdMutex<String>>,
}

#[derive(Clone)]
struct AuthUxStateSpaceProvider {
    authed: StdArc<AtomicBool>,
    refreshes: StdArc<AtomicUsize>,
    model: StdArc<StdMutex<String>>,
    set_model_requests: StdArc<StdMutex<Vec<String>>>,
    provider_id: &'static str,
    provider_label: &'static str,
    models: &'static [&'static str],
    include_wrong_profile_first: bool,
    include_generic_profile_duplicate: bool,
}

#[derive(Clone)]
struct EmptyPostLoginCatalogProvider {
    refreshes: StdArc<AtomicUsize>,
    set_model_attempts: StdArc<AtomicUsize>,
}

#[derive(Clone)]
struct FailingPostLoginCatalogProvider {
    refreshes: StdArc<AtomicUsize>,
    set_model_attempts: StdArc<AtomicUsize>,
}

impl AuthUxStateSpaceProvider {
    fn routes(&self) -> Vec<crate::provider::ModelRoute> {
        let authed = self.authed.load(Ordering::SeqCst);
        let mut routes = Vec::new();
        if self.include_wrong_profile_first {
            routes.push(crate::provider::ModelRoute {
                model: "wrong-profile-first".to_string(),
                provider: self.provider_label.to_string(),
                api_method: "openai-compatible:other-provider".to_string(),
                available: authed,
                detail: if authed {
                    "fresh wrong-profile catalog route".to_string()
                } else {
                    "no API key".to_string()
                },
                cheapness: None,
            });
        }
        for model in self.models {
            routes.push(crate::provider::ModelRoute {
                model: (*model).to_string(),
                provider: self.provider_label.to_string(),
                api_method: format!("openai-compatible:{}", self.provider_id),
                available: authed,
                detail: if authed {
                    "fresh catalog route".to_string()
                } else {
                    "no API key".to_string()
                },
                cheapness: None,
            });
            if self.include_generic_profile_duplicate {
                routes.push(crate::provider::ModelRoute {
                    model: (*model).to_string(),
                    provider: self.provider_label.to_string(),
                    api_method: "openai-compatible".to_string(),
                    available: authed,
                    detail: if authed {
                        "duplicate generic direct route".to_string()
                    } else {
                        "no API key".to_string()
                    },
                    cheapness: None,
                });
            }
        }
        routes
    }
}

impl MixedModelRoutesProvider {
    fn routes() -> Vec<crate::provider::ModelRoute> {
        vec![
            crate::provider::ModelRoute {
                model: "gpt-5.5".to_string(),
                provider: "OpenAI".to_string(),
                api_method: "openai-oauth".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            },
            crate::provider::ModelRoute {
                model: "claude-opus-4-6".to_string(),
                provider: "Anthropic".to_string(),
                api_method: "claude-oauth".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            },
            crate::provider::ModelRoute {
                model: "Qwen/Qwen3-Coder-480B-A35B-Instruct".to_string(),
                provider: "Chutes".to_string(),
                api_method: "openai-compatible:chutes".to_string(),
                available: true,
                detail: "https://llm.chutes.ai/v1".to_string(),
                cheapness: None,
            },
            crate::provider::ModelRoute {
                model: "deepseek/deepseek-v4-pro".to_string(),
                provider: "auto".to_string(),
                api_method: "openrouter".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            },
        ]
    }
}

#[async_trait::async_trait]
impl Provider for AuthUxStateSpaceProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("AuthUxStateSpaceProvider")
    }

    fn name(&self) -> &str {
        "openrouter"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn available_models_display(&self) -> Vec<String> {
        self.routes()
            .into_iter()
            .filter(|route| route.available)
            .map(|route| route.model)
            .collect()
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        self.routes()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        self.set_model_requests
            .lock()
            .unwrap()
            .push(model.to_string());
        let model = model
            .strip_prefix(&format!("{}:", self.provider_id))
            .unwrap_or(model);
        let found = self
            .routes()
            .into_iter()
            .any(|route| route.available && route.model == model);
        if !found {
            anyhow::bail!("model {model} is not available in the refreshed catalog");
        }
        *self.model.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn on_auth_changed(&self) {
        self.authed.store(true, Ordering::SeqCst);
    }

    async fn refresh_model_catalog(&self) -> Result<crate::provider::ModelCatalogRefreshSummary> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        Ok(crate::provider::ModelCatalogRefreshSummary {
            model_count_before: 0,
            model_count_after: 2,
            models_added: 2,
            models_removed: 0,
            models_added_names: Vec::new(),
            models_removed_names: Vec::new(),
            route_count_before: 0,
            route_count_after: 2,
            routes_added: 2,
            routes_removed: 0,
            routes_changed: 0,
        })
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[async_trait::async_trait]
impl Provider for MixedModelRoutesProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("MixedModelRoutesProvider")
    }

    fn name(&self) -> &str {
        "mixed"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn available_models_display(&self) -> Vec<String> {
        Self::routes()
            .into_iter()
            .map(|route| route.model)
            .collect()
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        Self::routes()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        let model = model.strip_prefix("chutes:").unwrap_or(model);
        if !Self::routes().iter().any(|route| route.model == model) {
            anyhow::bail!("model {model} is not available in the mixed catalog");
        }
        *self.model.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[async_trait::async_trait]
impl Provider for EmptyPostLoginCatalogProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("EmptyPostLoginCatalogProvider")
    }

    fn name(&self) -> &str {
        "empty-catalog"
    }

    fn model(&self) -> String {
        "pre-auth-model".to_string()
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        vec![]
    }

    fn set_model(&self, model: &str) -> Result<()> {
        self.set_model_attempts.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("unexpected attempt to switch to {model}")
    }

    async fn refresh_model_catalog(&self) -> Result<crate::provider::ModelCatalogRefreshSummary> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        Ok(crate::provider::ModelCatalogRefreshSummary {
            model_count_before: 0,
            model_count_after: 0,
            models_added: 0,
            models_removed: 0,
            models_added_names: Vec::new(),
            models_removed_names: Vec::new(),
            route_count_before: 0,
            route_count_after: 0,
            routes_added: 0,
            routes_removed: 0,
            routes_changed: 0,
        })
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[async_trait::async_trait]
impl Provider for FailingPostLoginCatalogProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("FailingPostLoginCatalogProvider")
    }

    fn name(&self) -> &str {
        "failing-catalog"
    }

    fn model(&self) -> String {
        "pre-auth-model".to_string()
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        vec![]
    }

    fn set_model(&self, model: &str) -> Result<()> {
        self.set_model_attempts.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("unexpected attempt to switch to {model}")
    }

    async fn refresh_model_catalog(&self) -> Result<crate::provider::ModelCatalogRefreshSummary> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("fixture refresh failed before server auth-change catalog refresh")
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[async_trait::async_trait]
impl Provider for CountingModelRoutesProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<crate::provider::EventStream> {
        unimplemented!("CountingModelRoutesProvider")
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn model(&self) -> String {
        "counting-a".to_string()
    }

    fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        (0..self.route_count)
            .map(|idx| crate::provider::ModelRoute {
                model: format!("counting-{}", (b'a' + idx as u8) as char),
                provider: "Counting".to_string(),
                api_method: "test".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            })
            .collect()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

#[test]
fn test_model_picker_reuses_cached_entries_until_invalidated() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let calls = StdArc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CountingModelRoutesProvider {
        calls: StdArc::clone(&calls),
        route_count: 2,
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
    assert!(app.model_picker_cache.is_some());

    app.open_model_picker();
    wait_for_model_picker_load(&mut app);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "second open should reuse cached picker entries"
    );

    app.invalidate_model_picker_cache();
    app.open_model_picker();
    wait_for_model_picker_load(&mut app);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "invalidating should force rebuilding provider routes"
    );
}

#[test]
fn test_shift_tab_model_favorite_hotkey_preserves_input_line() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let calls = StdArc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(CountingModelRoutesProvider {
        calls: StdArc::clone(&calls),
        route_count: 2,
        delay: Duration::ZERO,
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    app.set_input_for_test("do not drop this draft");
    let cursor = app.cursor_pos();

    app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT)
        .unwrap();
    wait_for_model_picker_load(&mut app);

    assert_eq!(app.input(), "do not drop this draft");
    assert_eq!(app.cursor_pos(), cursor);
}

#[test]
fn test_tui_api_key_auth_refreshes_catalog_shows_diff_without_opening_picker() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let provider = AuthUxStateSpaceProvider {
        authed: StdArc::new(AtomicBool::new(false)),
        refreshes: StdArc::new(AtomicUsize::new(0)),
        model: StdArc::new(StdMutex::new("pre-auth-model".to_string())),
        set_model_requests: StdArc::new(StdMutex::new(Vec::new())),
        provider_id: "state-space",
        provider_label: "StateSpace",
        models: &["state-space-alpha", "state-space-beta"],
        include_wrong_profile_first: true,
        include_generic_profile_duplicate: false,
    };
    let refreshes = provider.refreshes.clone();
    let provider: Arc<dyn Provider> = Arc::new(provider);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    let mut bus_rx = crate::bus::Bus::global().subscribe();
    while bus_rx.try_recv().is_ok() {}

    let _guard = rt.enter();
    app.start_openai_compatible_post_login_activation(
        "state-space".to_string(),
        "StateSpace".to_string(),
    );
    assert_eq!(
        app.status_notice(),
        Some("StateSpace: fetching models...".to_string())
    );
    assert!(
        app.inline_interactive_state.is_none(),
        "auth-triggered discovery should not open /model automatically"
    );

    let activation = rt.block_on(async {
        loop {
            match tokio::time::timeout(Duration::from_secs(2), bus_rx.recv()).await {
                Ok(Ok(event @ crate::bus::BusEvent::ProviderModelActivated { .. })) => break event,
                Ok(Ok(_)) => continue,
                other => panic!("expected ProviderModelActivated event, got {other:?}"),
            }
        }
    });
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "auth completion must refresh the model catalog exactly once"
    );

    super::local::handle_bus_event(&mut app, Ok(activation));
    assert!(
        app.inline_interactive_state.is_none(),
        "activation completion should still not open /model automatically"
    );
    assert_eq!(app.session.model.as_deref(), Some("state-space-alpha"));
    let last = app.display_messages.last().expect("activation message");
    assert!(last.content.contains("Added models:"));
    assert!(last.content.contains("state-space-alpha"));
    assert!(last.content.contains("state-space-beta"));
    assert!(last.content.contains("Use /model"));
    assert!(!last.content.contains("model picker is open"));

    assert!(super::model_context::handle_model_command(
        &mut app,
        "/model state-space-beta"
    ));
    assert_eq!(app.session.model.as_deref(), Some("state-space-beta"));
    assert_eq!(
        app.status_notice(),
        Some("Model → state-space-beta".to_string())
    );
}

#[test]
fn test_tui_cerebras_paste_key_lifecycle_has_no_degraded_success_messages() {
    let _env_lock = crate::storage::lock_test_env();
    let _guard = AzureLoginEnvGuard::save(&[
        "CEREBRAS_API_KEY",
        "JCODE_OPENROUTER_API_BASE",
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENROUTER_ENV_FILE",
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "JCODE_OPENROUTER_PROVIDER_FEATURES",
        "JCODE_OPENROUTER_MODEL_CATALOG",
        "JCODE_OPENROUTER_AUTH_HEADER",
        "JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER",
        "JCODE_RUNTIME_PROVIDER",
        "JCODE_ACTIVE_PROVIDER",
        "JCODE_FORCE_PROVIDER",
    ]);
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let fake_provider = AuthUxStateSpaceProvider {
        authed: StdArc::new(AtomicBool::new(false)),
        refreshes: StdArc::new(AtomicUsize::new(0)),
        model: StdArc::new(StdMutex::new("gpt-5.5".to_string())),
        set_model_requests: StdArc::new(StdMutex::new(Vec::new())),
        provider_id: "cerebras",
        provider_label: "Cerebras",
        models: &["qwen-3-235b-a22b-instruct-2507", "llama3.1-8b"],
        include_wrong_profile_first: true,
        include_generic_profile_duplicate: true,
    };
    let refreshes = fake_provider.refreshes.clone();
    let set_model_requests = fake_provider.set_model_requests.clone();
    let provider: Arc<dyn Provider> = Arc::new(fake_provider);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let registry = rt.block_on(crate::tool::Registry::new(provider.clone()));
    let mut app = App::new_for_test_harness(provider, registry);
    app.queue_mode = false;
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    let mut bus_rx = crate::bus::Bus::global().subscribe();
    while bus_rx.try_recv().is_ok() {}

    app.start_login_provider(
        crate::provider_catalog::resolve_login_provider("cerebras")
            .expect("Cerebras login provider"),
    );

    let prompt = app
        .display_messages
        .last()
        .expect("login prompt")
        .content
        .clone();
    assert!(prompt.contains("Cerebras API Key"), "{prompt}");
    assert!(
        prompt.contains("Stored variable: CEREBRAS_API_KEY"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Endpoint: https://api.cerebras.ai/v1"),
        "{prompt}"
    );
    assert!(
        prompt.contains("Suggested default model: gpt-oss-120b"),
        "{prompt}"
    );
    assert!(prompt.contains("Paste your API key below"), "{prompt}");

    let pending = app
        .pending_login
        .take()
        .expect("pending Cerebras key login");
    let _runtime_guard = rt.enter();
    app.handle_login_input(pending, "test-cerebras-key".to_string());

    let mut saw_saved = false;
    let mut saw_catalog_started = false;
    let mut saw_activation = false;
    let mut saw_catalog_ready = false;
    let mut login_success_events = 0;
    let mut login_failure_events = 0;
    let mut catalog_warning_events = 0;
    let mut activation_events = 0;
    rt.block_on(async {
        while !(saw_saved && saw_catalog_started && saw_activation && saw_catalog_ready) {
            match tokio::time::timeout(Duration::from_secs(2), bus_rx.recv()).await {
                Ok(Ok(crate::bus::BusEvent::LoginCompleted(login))) => {
                    if login.success {
                        login_success_events += 1;
                    } else {
                        login_failure_events += 1;
                    }
                    assert!(login.success, "unexpected failed login event: {login:?}");
                    assert_eq!(login.provider, "Cerebras");
                    assert!(login.message.contains("Cerebras API key saved."));
                    assert!(
                        login
                            .message
                            .contains("Stored at ~/.config/jcode/cerebras.env.")
                    );
                    assert!(login.message.contains("Fetching models now."));
                    assert!(!login.message.contains("did not switch models"));
                    app.handle_login_completed(login);
                    saw_saved = true;
                }
                Ok(Ok(crate::bus::BusEvent::UiActivity(activity))) => {
                    if activity.message.contains("Auth Model Catalog Warning") {
                        catalog_warning_events += 1;
                    }
                    assert!(
                        !activity.message.contains("Auth Model Catalog Warning"),
                        "unexpected warning activity: {}",
                        activity.message
                    );
                    assert!(
                        !activity.message.contains("did not switch models"),
                        "unexpected degraded activity: {}",
                        activity.message
                    );
                    if activity.message.contains("Model Discovery Started") {
                        saw_catalog_started = true;
                    }
                    super::local::handle_bus_event(
                        &mut app,
                        Ok(crate::bus::BusEvent::UiActivity(activity)),
                    );
                }
                Ok(Ok(event @ crate::bus::BusEvent::ProviderModelActivated { .. })) => {
                    activation_events += 1;
                    if let crate::bus::BusEvent::ProviderModelActivated {
                        model,
                        provider_key,
                        message,
                        ..
                    } = &event
                    {
                        assert_eq!(model, "qwen-3-235b-a22b-instruct-2507");
                        assert_eq!(provider_key.as_deref(), Some("cerebras"));
                        assert!(message.contains("Cerebras is ready."), "{message}");
                        assert!(!message.contains("wrong-profile-first"), "{message}");
                    }
                    super::local::handle_bus_event(&mut app, Ok(event));
                    saw_activation = true;
                }
                Ok(Ok(event @ crate::bus::BusEvent::AuthCatalogRefreshReady)) => {
                    super::local::handle_bus_event(&mut app, Ok(event));
                    saw_catalog_ready = true;
                }
                Ok(Ok(_)) => {}
                other => panic!("expected local Cerebras auth lifecycle event, got {other:?}"),
            }
        }
    });

    while let Ok(event) = bus_rx.try_recv() {
        match event {
            crate::bus::BusEvent::LoginCompleted(login) => {
                if login.success {
                    login_success_events += 1;
                } else {
                    panic!("late failed login event after successful auth: {login:?}");
                }
            }
            crate::bus::BusEvent::UiActivity(activity) => {
                if activity.message.contains("Auth Model Catalog Warning") {
                    panic!(
                        "late warning activity after successful auth: {}",
                        activity.message
                    );
                }
                assert!(
                    !activity.message.contains("did not switch models"),
                    "late degraded activity after successful auth: {}",
                    activity.message
                );
            }
            crate::bus::BusEvent::ProviderModelActivated {
                model,
                provider_key,
                message,
                ..
            } => {
                activation_events += 1;
                assert_eq!(model, "qwen-3-235b-a22b-instruct-2507");
                assert_eq!(provider_key.as_deref(), Some("cerebras"));
                assert!(message.contains("Cerebras is ready."), "{message}");
            }
            event @ crate::bus::BusEvent::AuthCatalogRefreshReady => {
                super::local::handle_bus_event(&mut app, Ok(event));
            }
            _ => {}
        }
    }

    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        login_success_events, 1,
        "expected exactly one successful login event"
    );
    assert_eq!(
        login_failure_events, 0,
        "happy auth must not publish failed login events"
    );
    assert_eq!(
        catalog_warning_events, 0,
        "happy auth must not publish catalog warnings"
    );
    assert_eq!(
        activation_events, 1,
        "expected exactly one provider activation event"
    );
    assert_eq!(
        app.session.model.as_deref(),
        Some("qwen-3-235b-a22b-instruct-2507")
    );
    assert_eq!(
        app.session.provider_key.as_deref(),
        Some("cerebras")
    );
    assert_eq!(
        set_model_requests.lock().unwrap().as_slice(),
        ["cerebras:qwen-3-235b-a22b-instruct-2507"],
        "post-login activation must preserve the authenticated Cerebras route instead of switching a bare model"
    );
    let transcript = app
        .display_messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    for forbidden in [
        "Auth Model Catalog Warning",
        "did not switch models",
        "contained no selectable",
        "Saved the API key and fetched the model catalog, but",
        "Login: Cerebras failed",
        "wrong-profile-first",
    ] {
        assert!(
            !transcript.contains(forbidden),
            "transcript contained forbidden degraded-success marker `{forbidden}`:\n{transcript}"
        );
    }

    set_model_requests.lock().unwrap().clear();
    app.open_model_picker();
    wait_for_model_picker_load(&mut app);
    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should open after Cerebras auth");
    let qwen_entry = picker
        .entries
        .iter()
        .find(|entry| entry.name == "qwen-3-235b-a22b-instruct-2507")
        .expect("selected Cerebras model should be visible in /model");
    assert_eq!(
        picker
            .entries
            .iter()
            .filter(|entry| entry.name == "qwen-3-235b-a22b-instruct-2507")
            .count(),
        1,
        "Cerebras model picker should not show duplicate rows for the selected model"
    );
    assert!(qwen_entry.options.iter().any(|route| {
        route.provider == "Cerebras"
            && route.api_method == "openai-compatible:cerebras"
            && route.available
    }));
    assert!(
        !qwen_entry
            .options
            .iter()
            .any(|route| route.api_method == "openai-compatible"),
        "generic direct route should be de-duplicated in favor of the Cerebras profile route"
    );
    let llama_idx = picker
        .entries
        .iter()
        .position(|entry| entry.name == "llama3.1-8b")
        .expect("alternate Cerebras model should be visible in /model");
    let llama_entry = &picker.entries[llama_idx];
    assert_eq!(
        picker
            .entries
            .iter()
            .filter(|entry| entry.name == "llama3.1-8b")
            .count(),
        1,
        "Cerebras model picker should not show duplicate rows for alternate models"
    );
    assert!(llama_entry.options.iter().any(|route| {
        route.provider == "Cerebras"
            && route.api_method == "openai-compatible:cerebras"
            && route.available
    }));
    let llama_cerebras_option = llama_entry
        .options
        .iter()
        .position(|route| {
            route.provider == "Cerebras"
                && route.api_method == "openai-compatible:cerebras"
                && route.available
        })
        .expect("alternate model should expose its authenticated Cerebras route");
    assert!(
        !llama_entry
            .options
            .iter()
            .any(|route| route.api_method == "openai-compatible"),
        "generic direct route should be de-duplicated in favor of the Cerebras profile route"
    );
    let filtered_pos = picker
        .filtered
        .iter()
        .position(|&idx| idx == llama_idx)
        .expect("alternate Cerebras model should be selectable in filtered picker list");

    let picker = app.inline_interactive_state.as_mut().unwrap();
    picker.selected = filtered_pos;
    picker.entries[llama_idx].selected_option = llama_cerebras_option;
    picker.column = picker.max_navigable_column();
    app.handle_inline_interactive_key(KeyCode::Enter, KeyModifiers::empty())
        .expect("Cerebras picker selection should switch models");

    assert_eq!(app.session.model.as_deref(), Some("llama3.1-8b"));
    assert_eq!(
        app.session.provider_key.as_deref(),
        Some("openai-compatible:cerebras")
    );
    assert_eq!(app.provider.model(), "llama3.1-8b");
    assert_eq!(
        set_model_requests.lock().unwrap().as_slice(),
        ["cerebras:llama3.1-8b"],
        "model picker must route post-auth switches through the authenticated Cerebras profile"
    );
}
