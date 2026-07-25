#[test]
fn test_account_picker_prompt_new_openai_label_cancel_clears_prompt() {
    let mut app = create_test_app();
    app.prompt_new_account_label(crate::tui::account_picker::AccountProviderKind::OpenAi);

    assert!(matches!(
        app.pending_account_input,
        Some(super::auth::PendingAccountInput::NewAccountLabel { ref provider_id, .. }) if provider_id == "openai"
    ));

    app.input = "/cancel".to_string();
    app.submit_input();

    assert!(app.pending_account_input.is_none());
    assert!(app.pending_login.is_none());
}

#[test]
fn test_login_command_opens_inline_login_picker() {
    let mut app = create_test_app();
    app.input = "/login".to_string();
    app.submit_input();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("/login should open inline login picker");
    assert_eq!(picker.kind, crate::tui::PickerKind::Login);
    assert!(app.pending_login.is_none());
}

#[test]
fn test_account_openai_compatible_settings_renders_provider_settings() {
    let mut app = create_test_app();
    app.input = "/account openai-compatible settings".to_string();
    app.submit_input();

    let msg = app
        .display_messages()
        .last()
        .expect("missing settings output");
    assert_eq!(msg.role, "system");
    assert!(msg.content.contains("OpenAI-compatible"));
    assert!(msg.content.contains("API base"));
    assert!(msg.content.contains("default-model"));
}

#[test]
fn test_account_default_provider_command_saves_config() {
    let _guard = crate::storage::lock_test_env();
    let mut app = create_test_app();
    app.input = "/account default-provider openai".to_string();
    app.submit_input();

    let cfg = crate::config::Config::load();
    assert_eq!(cfg.provider.default_provider.as_deref(), Some("openai"));
}

#[test]
fn test_commands_alias_shows_help() {
    let mut app = create_test_app();
    app.input = "/commands".to_string();
    app.submit_input();

    assert!(
        app.help_scroll.is_some(),
        "/commands should open help overlay"
    );
}

#[test]
fn test_improve_command_starts_improvement_loop() {
    let mut app = create_test_app();
    app.input = "/improve".to_string();
    app.submit_input();

    assert_eq!(app.improve_mode, Some(ImproveMode::ImproveRun));
    assert_eq!(
        app.session.improve_mode,
        Some(crate::session::SessionImproveMode::ImproveRun)
    );
    assert!(app.is_processing());

    let msg = app.session.messages.last().expect("missing improve prompt");
    assert!(matches!(
        &msg.content[0],
        ContentBlock::Text { text, .. }
            if text.contains("You are entering improvement mode for this repository")
                && text.contains("write a concise ranked todo list using `todo`")
    ));

    let display = app
        .display_messages()
        .last()
        .expect("missing improve launch notice");
    assert!(display.content.contains("Starting improvement loop"));
}

#[test]
fn test_improve_plan_command_is_plan_only_and_accepts_focus() {
    let mut app = create_test_app();
    app.input = "/improve plan startup performance".to_string();
    app.submit_input();

    assert_eq!(app.improve_mode, Some(ImproveMode::ImprovePlan));
    assert_eq!(
        app.session.improve_mode,
        Some(crate::session::SessionImproveMode::ImprovePlan)
    );
    assert!(app.is_processing());

    let msg = app
        .session
        .messages
        .last()
        .expect("missing improve plan prompt");
    assert!(matches!(
        &msg.content[0],
        ContentBlock::Text { text, .. }
            if text.contains("improvement planning mode")
                && text.contains("This is plan-only mode")
                && text.contains("Focus area: startup performance")
    ));
}

#[test]
fn test_improve_status_summarizes_current_todos() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[
                crate::todo::TodoItem {
                    group: None,
                    id: "one".to_string(),
                    content: "Profile startup path".to_string(),
                    status: "in_progress".to_string(),
                    priority: "high".to_string(),
                    blocked_by: Vec::new(),
                    assigned_to: None,
                    confidence: Some(82),
                    completion_confidence: None,
                    confidence_history: Vec::new(),
                },
                crate::todo::TodoItem {
                    group: None,
                    id: "two".to_string(),
                    content: "Add regression test".to_string(),
                    status: "completed".to_string(),
                    priority: "medium".to_string(),
                    blocked_by: Vec::new(),
                    assigned_to: None,
                    confidence: None,
                    completion_confidence: None,
                    confidence_history: Vec::new(),
                },
            ],
        )
        .expect("save todos");

        app.improve_mode = Some(ImproveMode::ImproveRun);
        app.input = "/improve status".to_string();
        app.submit_input();

        let msg = app
            .display_messages()
            .last()
            .expect("missing improve status");
        assert!(msg.content.contains("Improve status"));
        assert!(
            msg.content
                .contains("1 incomplete · 1 completed · 0 cancelled")
        );
        assert!(msg.content.contains("Profile startup path"));
        assert!(msg.content.contains("confidence 82%"));
    });
}

#[test]
fn test_improve_stop_without_active_run_reports_idle() {
    let mut app = create_test_app();
    app.session.improve_mode = None;
    app.input = "/improve stop".to_string();
    app.submit_input();

    let msg = app
        .display_messages()
        .last()
        .expect("missing improve stop idle message");
    assert!(msg.content.contains("No active improve loop to stop"));
}

#[test]
fn test_improve_stop_queues_stop_prompt_and_clears_mode() {
    let mut app = create_test_app();
    app.improve_mode = Some(ImproveMode::ImproveRun);
    app.session.improve_mode = Some(crate::session::SessionImproveMode::ImproveRun);
    app.input = "/improve stop".to_string();
    app.submit_input();

    assert_eq!(app.improve_mode, None);
    assert_eq!(app.session.improve_mode, None);
    assert!(app.is_processing());

    let msg = app
        .session
        .messages
        .last()
        .expect("missing improve stop prompt");
    assert!(matches!(
        &msg.content[0],
        ContentBlock::Text { text, .. }
            if text.contains("Stop improvement mode after the current safe point")
    ));
}

#[test]
fn test_improve_resume_requires_saved_mode() {
    let mut app = create_test_app();
    app.input = "/improve resume".to_string();
    app.submit_input();

    let msg = app
        .display_messages()
        .last()
        .expect("missing improve resume idle message");
    assert!(msg.content.contains("No saved improve run found"));
}

#[test]
fn test_improve_resume_uses_saved_mode_and_current_todos() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.session.improve_mode = Some(crate::session::SessionImproveMode::ImproveRun);
        app.session.save().expect("save session");
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "resume1".to_string(),
                content: "Refactor command parsing".to_string(),
                status: "in_progress".to_string(),
                priority: "high".to_string(),
                blocked_by: Vec::new(),
                assigned_to: None,
                confidence: None,
                completion_confidence: None,
                confidence_history: Vec::new(),
            }],
        )
        .expect("save todos");

        app.input = "/improve resume".to_string();
        app.submit_input();

        assert_eq!(app.improve_mode, Some(ImproveMode::ImproveRun));
        assert_eq!(
            app.session.improve_mode,
            Some(crate::session::SessionImproveMode::ImproveRun)
        );
        assert!(app.is_processing());

        let msg = app
            .session
            .messages
            .last()
            .expect("missing improve resume prompt");
        assert!(matches!(
            &msg.content[0],
            ContentBlock::Text { text, .. }
                if text.contains("Resume improvement mode")
                    && text.contains("Refactor command parsing")
        ));
    });
}

#[test]
fn test_local_oauth_generation_refresh_reconciles_after_request_open() {
    with_temp_jcode_home(|| {
        let old_identity = local_oauth_identity(
            jcode_provider_core::RuntimeKey::OpenAIOAuth,
            "openai",
            ("openai-1".to_string(), "stable-openai-1".to_string(), 1),
        );
        let identity = std::sync::Arc::new(std::sync::Mutex::new(old_identity.clone()));
        let mut app = create_test_app();
        app.provider = std::sync::Arc::new(RefreshingLocalAccountProvider {
            identity: std::sync::Arc::clone(&identity),
        });
        let (_, _, immutable_nodes, canonical_raw) =
            install_local_account_projection(&mut app, old_identity.clone());
        let session_id = app.session.id.clone();

        let mut refreshed = old_identity;
        refreshed.account_generation = Some(2);
        *identity.lock().unwrap() = refreshed.clone();

        app.reconcile_verified_local_provider_identity_transition()
            .expect("verified same-account refresh must reconcile");

        assert_eq!(app.session.exact_runtime_identity, Some(refreshed.clone()));
        assert!(app.provider_session_id.is_none());
        assert!(app.session.provider_session_id.is_none());
        assert!(app.session.provider_session_identity.is_none());
        assert!(app.session.compaction.is_none());
        assert!(app.session.context_frontier.as_ref().is_some_and(|frontier| {
            frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
        }));
        assert_eq!(
            serde_json::to_vec(&app.session.context_nodes).unwrap(),
            immutable_nodes,
            "refresh must deactivate rather than delete forensic context nodes"
        );
        assert_eq!(serde_json::to_vec(&app.session.messages).unwrap(), canonical_raw);
        assert_eq!(
            serde_json::to_vec(&app.messages).unwrap(),
            serde_json::to_vec(&app.session.messages_for_provider_uncached()).unwrap()
        );
        let persisted = crate::session::Session::load(&session_id).unwrap();
        assert_eq!(persisted.exact_runtime_identity, Some(refreshed));
        assert!(persisted.provider_session_id.is_none());
        app.ensure_local_provider_identity_matches_admission()
            .expect("reconciled identity must remain admissible");
    });
}

#[test]
fn test_local_oauth_generation_refresh_reconciles_during_admission() {
    with_temp_jcode_home(|| {
        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "openai-1".to_string(),
            access_token: "fresh-access".to_string(),
            refresh_token: "fresh-refresh".to_string(),
            id_token: None,
            account_id: Some("stable-openai-1".to_string()),
            expires_at: Some(chrono::Utc::now().timestamp_millis() + 60_000),
            email: None,
        })
        .unwrap();
        let target = crate::auth::codex::active_account_identity().unwrap();
        let old_generation = target.2.saturating_sub(1);
        assert!(old_generation < target.2, "fixture requires a newer generation");
        let old_identity = local_oauth_identity(
            jcode_provider_core::RuntimeKey::OpenAIOAuth,
            "openai",
            (target.0.clone(), target.1.clone(), old_generation),
        );
        let identity = std::sync::Arc::new(std::sync::Mutex::new(old_identity.clone()));
        let mut app = create_test_app();
        app.provider = std::sync::Arc::new(RefreshingLocalAccountProvider {
            identity: std::sync::Arc::clone(&identity),
        });
        install_local_account_projection(&mut app, old_identity);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let admission = runtime
            .block_on(app.acquire_local_model_turn_admission())
            .expect("proactive refresh must reconcile before request admission");
        drop(admission);

        let reconciled = app.session.exact_runtime_identity.as_ref().unwrap();
        assert_eq!(reconciled.account_generation, Some(target.2));
        assert_eq!(reconciled.account_label.as_deref(), Some(target.0.as_str()));
        assert_eq!(reconciled.account_id.as_deref(), Some(target.1.as_str()));
        assert!(app.provider_session_id.is_none());
        assert!(app.session.provider_session_id.is_none());
        assert!(app.session.compaction.is_none());
    });
}

#[test]
fn test_local_oauth_generation_reconciliation_does_not_overwrite_newer_cas_winner() {
    with_temp_jcode_home(|| {
        let old_identity = local_oauth_identity(
            jcode_provider_core::RuntimeKey::OpenAIOAuth,
            "openai",
            ("openai-1".to_string(), "stable-openai-1".to_string(), 1),
        );
        let mut current_identity = old_identity.clone();
        current_identity.account_generation = Some(2);
        let identity = std::sync::Arc::new(std::sync::Mutex::new(current_identity));
        let mut app = create_test_app();
        app.provider = std::sync::Arc::new(RefreshingLocalAccountProvider {
            identity: std::sync::Arc::clone(&identity),
        });
        install_local_account_projection(&mut app, old_identity.clone());

        let mut peer = crate::session::Session::load(&app.session.id).unwrap();
        let mut newer_identity = old_identity;
        newer_identity.account_generation = Some(3);
        peer.exact_runtime_identity = Some(newer_identity.clone());
        peer.provider_session_id = None;
        peer.provider_session_identity = None;
        peer.reset_context_graph_for_identity_transition();
        peer.save().expect("persist newer peer identity");

        let error = app
            .reconcile_verified_local_provider_identity_transition()
            .expect_err("stale automatic transition must lose to newer durable identity");
        assert!(error.to_string().contains("lost CAS"));
        assert_eq!(
            app.session
                .exact_runtime_identity
                .as_ref()
                .and_then(|value| value.account_generation),
            Some(1),
            "failed in-memory transition remains retryable rather than claiming the newer state"
        );
        let persisted = crate::session::Session::load(&app.session.id).unwrap();
        assert_eq!(persisted.exact_runtime_identity, Some(newer_identity));
    });
}
