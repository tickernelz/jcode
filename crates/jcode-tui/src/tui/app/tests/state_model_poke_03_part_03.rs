#[test]
fn test_login_smoke_model_picker_renders_unstacked_provider_rows() {
    let mut app = create_login_smoke_model_app();
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
        let backend = ratatui::backend::TestBackend::new(180, 48);
        let mut terminal =
            ratatui::Terminal::new(backend).expect("failed to create test terminal");
        render_and_snap(app, &mut terminal)
    };

    // Effort-capable routes now expand into multiple rows. Render focused
    // slices so each provider remains observable without assuming the complete
    // catalog fits in one terminal viewport.
    let openai_text = render_filtered(&mut app, "gpt-5.4");
    let comtegra_text = render_filtered(&mut app, "glm-51-nvfp4");
    let copilot_text = render_filtered(&mut app, "claude-opus-4.6");
    let deepseek_text = render_filtered(&mut app, "deepseek/deepseek-v4-pro");
    let kimi_text = render_filtered(&mut app, "moonshotai/kimi-k2.5");
    let openrouter_openai_text = render_filtered(&mut app, "openai/gpt-5.5");

    assert!(
        openai_text.contains("MODEL")
            && openai_text.contains("PROVIDER")
            && openai_text.contains("METHOD"),
        "rendered /model view should include user-visible picker columns, got:\n{}",
        openai_text
    );
    assert!(
        openai_text.contains("gpt-5.4")
            && openai_text.contains("OpenAI")
            && openai_text.contains("oauth")
            && openai_text.contains("api key"),
        "OpenAI OAuth and API-key routes should be separately visible, got:\n{}",
        openai_text
    );
    let glm_row = comtegra_text
        .lines()
        .find(|line| line.contains("glm-51-nvfp4"))
        .unwrap_or("");
    assert!(
        glm_row.contains("Comtegra GPU Cloud")
            && glm_row.contains("api key")
            && !glm_row.contains("copilot"),
        "Comtegra GLM row should show its provider and API-key method, got row `{}` in:\n{}",
        glm_row,
        comtegra_text
    );
    assert!(
        comtegra_text.contains("glm-51-nvfp4")
            && comtegra_text.contains("Comtegra GPU Cloud")
            && comtegra_text.contains("new"),
        "Comtegra login route should be visible and marked new, got:\n{}",
        comtegra_text
    );
    assert!(
        copilot_text.contains("claude-opus-4.6") && copilot_text.contains("Copilot"),
        "Copilot route should be visible, got:\n{}",
        copilot_text
    );
    assert!(
        deepseek_text.contains("deepseek/deepseek-v4-pro")
            && deepseek_text.contains("openrouter"),
        "OpenRouter route should be visible, got:\n{}",
        deepseek_text
    );
    let deepseek_auto_row = deepseek_text
        .lines()
        .find(|line| line.contains("deepseek/deepseek-v4-pro") && line.contains("auto"))
        .unwrap_or("");
    let deepseek_provider_row = deepseek_text
        .lines()
        .find(|line| line.contains("deepseek/deepseek-v4-pro") && line.contains("DeepSeek"))
        .unwrap_or("");
    assert!(
        !deepseek_auto_row.contains('★'),
        "OpenRouter auto route should not carry the recommended marker, got row `{}` in:\n{}",
        deepseek_auto_row,
        deepseek_text
    );
    assert!(
        !deepseek_provider_row.contains('★'),
        "OpenRouter provider-specific routes should not carry the recommended marker, got row `{}` in:\n{}",
        deepseek_provider_row,
        deepseek_text
    );
    let kimi25_row = kimi_text
        .lines()
        .find(|line| line.contains("moonshotai/kimi-k2.5"))
        .unwrap_or("");
    assert!(
        !kimi25_row.contains('★'),
        "Kimi K2.5 should not be recommended, got row `{}` in:\n{}",
        kimi25_row,
        kimi_text
    );
    let openrouter_openai_row = openrouter_openai_text
        .lines()
        .find(|line| line.contains("openai/gpt-5.5"))
        .unwrap_or("");
    assert!(
        openrouter_openai_row.contains("OpenRou")
            && openrouter_openai_row.contains("openrouter")
            && !openrouter_openai_row.contains("api key"),
        "OpenRouter endpoint routes should not look like native OpenAI API-key rows, got row `{}` in:\n{}",
        openrouter_openai_row,
        openrouter_openai_text
    );
    for text in [
        &openai_text,
        &comtegra_text,
        &copilot_text,
        &deepseek_text,
        &kimi_text,
        &openrouter_openai_text,
    ] {
        assert!(
            !text.contains("(2)"),
            "provider routes should not be hidden behind stacked option counts, got:\n{}",
            text
        );
    }
}

#[test]
fn test_model_picker_filter_text_includes_provider_and_method() {
    let entry = crate::tui::PickerEntry {
        name: "glm-51-nvfp4".to_string(),
        options: vec![crate::tui::PickerOption {
            provider: "Comtegra GPU Cloud".to_string(),
            api_method: "openai-compatible:comtegra".to_string(),
            available: true,
            detail: "https://llm.comtegra.cloud/v1".to_string(),
            estimated_reference_cost_micros: None,
        }],
        action: crate::tui::PickerAction::Model,
        selected_option: 0,
        is_current: false,
        is_default: false,
        is_favorite: false,
        recommended: false,
        recommendation_rank: usize::MAX,
        usage_score: 0,
        old: false,
        created_date: None,
        effort: None,
    };

    let filter_text = crate::tui::PickerKind::Model.filter_text(&entry);
    assert!(filter_text.contains("glm-51-nvfp4"));
    assert!(filter_text.contains("Comtegra GPU Cloud"));
    assert!(filter_text.contains("openai-compatible:comtegra"));
}

#[test]
fn test_login_picker_preview_stays_open_and_updates_filter() {
    let mut app = create_test_app();

    for c in "/login za".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("login picker preview should be open");
    assert!(picker.preview);
    assert_eq!(picker.kind, crate::tui::PickerKind::Login);
    assert_eq!(picker.filter, "za");
    assert!(
        picker
            .filtered
            .iter()
            .any(|&i| picker.entries[i].name == "Z.AI")
    );
    assert_eq!(app.input(), "/login za");
}

#[test]
fn test_login_picker_preview_enter_starts_login_flow() {
    let mut app = create_test_app();

    for c in "/login zai".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    assert!(app.inline_interactive_state.is_none());
    match app.pending_login {
        Some(crate::tui::app::auth::PendingLogin::ApiKeyProfile {
            provider,
            openai_compatible_profile: Some(profile),
            ..
        }) => {
            assert_eq!(provider, "Z.AI");
            assert_eq!(profile.id, crate::provider_catalog::ZAI_PROFILE.id);
        }
        ref other => panic!("unexpected pending login state: {other:?}"),
    }
}

#[test]
fn test_typing_login_auto_inserts_filter_space() {
    let mut app = create_test_app();

    for c in "/login".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }

    // The trailing space arms provider filtering immediately, so the next
    // keystrokes filter the login picker instead of extending the command.
    assert_eq!(app.input(), "/login ");
    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("login picker preview should be open");
    assert!(picker.preview);
    assert_eq!(picker.kind, crate::tui::PickerKind::Login);
    assert_eq!(picker.filter, "");

    // A habitual manually-typed space is swallowed instead of doubling up.
    app.handle_key(KeyCode::Char(' '), KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.input(), "/login ");

    for c in "za".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }
    assert_eq!(app.input(), "/login za");
    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("login picker preview should stay open");
    assert_eq!(picker.filter, "za");
}

#[test]
fn test_login_preview_enter_without_selection_focuses_picker_instead_of_logging_in() {
    let mut app = create_test_app();

    for c in "/login".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    // No filter and no explicit selection: Enter must not launch the first
    // provider's login flow. It focuses the picker for a deliberate choice.
    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("login picker should stay open after bare Enter");
    assert!(!picker.preview, "picker should be focused (not preview)");
    assert_eq!(picker.kind, crate::tui::PickerKind::Login);
    assert!(app.pending_login.is_none());
    assert_eq!(app.input(), "");
}

#[test]
fn test_login_preview_enter_after_navigation_starts_selected_login() {
    let mut app = create_test_app();

    for c in "/login".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }
    // Explicit navigation makes the selection deliberate, so Enter activates.
    // Navigate to the Anthropic API key row (an offline api-key prompt flow).
    app.handle_key(KeyCode::Down, KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Down, KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    assert!(
        app.inline_interactive_state.is_none(),
        "picker should close after selecting a provider"
    );
    assert!(
        app.pending_login.is_some(),
        "selected provider login flow should start"
    );
}

#[test]
fn test_subagent_model_command_sets_and_resets_session_preference() {
    let mut app = create_test_app();

    assert!(super::commands::handle_session_command(
        &mut app,
        "/subagent-model gpt-5.4"
    ));
    assert_eq!(app.session.subagent_model.as_deref(), Some("gpt-5.4"));

    assert!(super::commands::handle_session_command(
        &mut app,
        "/subagent-model inherit"
    ));
    assert_eq!(app.session.subagent_model, None);
}

#[test]
fn test_autoreview_command_toggles_session_preference() {
    let mut app = create_test_app();

    assert!(super::commands::handle_session_command(
        &mut app,
        "/autoreview on"
    ));
    assert_eq!(app.session.autoreview_enabled, Some(true));
    assert!(app.autoreview_enabled);

    assert!(super::commands::handle_session_command(
        &mut app,
        "/autoreview off"
    ));
    assert_eq!(app.session.autoreview_enabled, Some(false));
    assert!(!app.autoreview_enabled);
}

#[test]
fn test_autojudge_command_toggles_session_preference() {
    let mut app = create_test_app();

    assert!(super::commands::handle_session_command(
        &mut app,
        "/autojudge on"
    ));
    assert_eq!(app.session.autojudge_enabled, Some(true));
    assert!(app.autojudge_enabled);

    assert!(super::commands::handle_session_command(
        &mut app,
        "/autojudge off"
    ));
    assert_eq!(app.session.autojudge_enabled, Some(false));
    assert!(!app.autojudge_enabled);
}

#[test]
fn test_transcript_path_command_reports_current_session_file() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let expected = crate::session::session_path(&app.session.id).expect("session path");

        assert!(super::commands::handle_session_command(
            &mut app,
            "/transcript path"
        ));

        assert!(app.display_messages().iter().any(|msg| {
            msg.content.contains("Transcript file:")
                && msg.content.contains(&expected.display().to_string())
        }));
    });
}

#[test]
fn test_poke_arms_auto_poke_until_todos_are_done() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "todo-1".to_string(),
                content: "Finish the remaining task".to_string(),
                status: "pending".to_string(),
                priority: "high".to_string(),
                blocked_by: Vec::new(),
                assigned_to: None,
                confidence: None,
                completion_confidence: None,
                confidence_history: Vec::new(),
            }],
        )
        .expect("save todos");

        assert!(super::commands::handle_session_command(&mut app, "/poke"));

        assert!(app.auto_poke_incomplete_todos);
        assert!(app.pending_turn);
        assert!(app.display_messages().iter().any(|msg| {
            msg.content.contains("Poking model: 1 incomplete todo")
                && msg.content.contains("/poke off")
        }));
    });
}

#[test]
fn test_poke_status_reports_current_state() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "todo-1".to_string(),
                content: "Finish the remaining task".to_string(),
                status: "pending".to_string(),
                priority: "high".to_string(),
                blocked_by: Vec::new(),
                assigned_to: None,
                confidence: None,
                completion_confidence: None,
                confidence_history: Vec::new(),
            }],
        )
        .expect("save todos");

        assert!(super::commands::handle_session_command(
            &mut app,
            "/poke status"
        ));
        assert!(
            app.display_messages()
                .iter()
                .any(|msg| { msg.content.contains("Auto-poke: ON. 1 incomplete todo.") })
        );

        app.auto_poke_incomplete_todos = true;
        app.is_processing = true;
        app.queued_messages
            .push(super::commands::build_poke_message(
                &super::commands::incomplete_poke_todos(&app),
            ));
        app.hidden_queued_system_messages.push(
            "All todos are done. Todo confidence summary:\n- Weighted completion confidence: 80%."
                .to_string(),
        );

        assert!(super::commands::handle_session_command(
            &mut app,
            "/poke status"
        ));
        assert!(app.display_messages().iter().any(|msg| {
            msg.content.contains("Auto-poke: ON. 1 incomplete todo.")
                && msg.content.contains("A follow-up poke is queued.")
                && msg.content.contains("A turn is currently running.")
        }));
    });
}

#[test]
fn test_poke_off_disarms_and_clears_queued_followup() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "todo-1".to_string(),
                content: "Keep going".to_string(),
                status: "pending".to_string(),
                priority: "high".to_string(),
                blocked_by: Vec::new(),
                assigned_to: None,
                confidence: None,
                completion_confidence: None,
                confidence_history: Vec::new(),
            }],
        )
        .expect("save todos");

        app.auto_poke_incomplete_todos = true;
        app.pending_queued_dispatch = true;
        app.queued_messages
            .push(super::commands::build_poke_message(
                &super::commands::incomplete_poke_todos(&app),
            ));
        app.hidden_queued_system_messages.push(
            "All todos are done. Todo confidence summary:\n- Weighted completion confidence: 80%."
                .to_string(),
        );

        assert!(super::commands::handle_session_command(
            &mut app,
            "/poke off"
        ));

        assert!(!app.auto_poke_incomplete_todos);
        assert!(!app.pending_queued_dispatch);
        assert!(app.queued_messages().is_empty());
        assert!(app.hidden_queued_system_messages.is_empty());
        assert_eq!(app.status_notice(), Some("Poke: OFF".to_string()));
        assert!(app.display_messages().iter().any(|msg| {
            msg.content.contains("Auto-poke disabled.")
                && msg.content.contains("Cleared 2 queued poke follow-ups")
        }));
    });
}

#[test]
fn test_poke_queues_when_turn_is_in_progress() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "todo-1".to_string(),
                content: "Finish the remaining task".to_string(),
                status: "pending".to_string(),
                priority: "high".to_string(),
                blocked_by: Vec::new(),
                assigned_to: None,
                confidence: None,
                completion_confidence: None,
                confidence_history: Vec::new(),
            }],
        )
        .expect("save todos");

        app.is_processing = true;

        assert!(super::commands::handle_session_command(&mut app, "/poke"));

        assert!(app.auto_poke_incomplete_todos);
        assert!(app.is_processing);
        assert!(!app.cancel_requested);
        assert!(!app.pending_turn);
        assert_eq!(
            app.status_notice(),
            Some("Poke queued after current turn".to_string())
        );
        assert!(app.queued_messages().is_empty());
        assert!(app.display_messages().iter().any(|msg| {
            msg.content
                .contains("/poke queued. Re-checking incomplete todos after this turn")
        }));

        crate::todo::save_todos(
            &app.session.id,
            &[
                crate::todo::TodoItem {
                    group: None,
                    id: "todo-1".to_string(),
                    content: "Finish the remaining task".to_string(),
                    status: "pending".to_string(),
                    priority: "high".to_string(),
                    blocked_by: Vec::new(),
                    assigned_to: None,
                    confidence: None,
                    completion_confidence: None,
                    confidence_history: Vec::new(),
                },
                crate::todo::TodoItem {
                    group: None,
                    id: "todo-2".to_string(),
                    content: "Pick up the newly discovered task".to_string(),
                    status: "pending".to_string(),
                    priority: "medium".to_string(),
                    blocked_by: Vec::new(),
                    assigned_to: None,
                    confidence: None,
                    completion_confidence: None,
                    confidence_history: Vec::new(),
                },
            ],
        )
        .expect("save updated todos");

        super::local::finish_turn(&mut app);

        assert!(app.pending_queued_dispatch);
        assert_eq!(app.queued_messages().len(), 1);
        assert!(app.queued_messages()[0].contains("You have 2 incomplete todos"));
        assert!(!app.queued_messages()[0].contains("Pick up the newly discovered task"));
        assert!(!app.queued_messages()[0].contains("/poke off"));
    });
}

#[test]
fn test_btw_forks_even_when_turn_is_in_progress() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.is_processing = true;

        assert!(super::commands::handle_session_command(
            &mut app,
            "/btw should this fork context?"
        ));

        assert!(app.is_processing, "parent turn should keep running");
        assert!(app.queued_messages().is_empty());
        assert!(app.hidden_queued_system_messages.is_empty());
        assert!(app.display_messages().iter().any(|msg| {
            msg.content.contains("created for the next prompt")
                || msg.content.contains("Next prompt launched in")
        }));
    });
}

#[test]
fn test_finish_turn_auto_pokes_again_when_todos_remain() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "todo-1".to_string(),
                content: "Keep going".to_string(),
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

        app.auto_poke_incomplete_todos = true;
        app.is_processing = true;
        super::local::finish_turn(&mut app);

        assert!(app.pending_queued_dispatch);
        assert_eq!(app.queued_messages().len(), 1);
        assert!(app.queued_messages()[0].contains("Continue working, or update the todo tool."));
    });
}

#[test]
fn test_finish_turn_auto_poke_queues_confidence_summary_when_todos_done() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[
                crate::todo::TodoItem {
                    group: None,
                    id: "todo-1".to_string(),
                    content: "Finish risky provider path".to_string(),
                    status: "completed".to_string(),
                    priority: "high".to_string(),
                    blocked_by: Vec::new(),
                    assigned_to: None,
                    confidence: Some(70),
                    completion_confidence: Some(80),
                    confidence_history: Vec::new(),
                },
                crate::todo::TodoItem {
                    group: None,
                    id: "todo-2".to_string(),
                    content: "Document straightforward behavior".to_string(),
                    status: "completed".to_string(),
                    priority: "medium".to_string(),
                    blocked_by: Vec::new(),
                    assigned_to: None,
                    confidence: Some(90),
                    completion_confidence: Some(95),
                    confidence_history: Vec::new(),
                },
            ],
        )
        .expect("save todos");

        app.auto_poke_incomplete_todos = true;
        app.is_processing = true;
        super::local::finish_turn(&mut app);

        assert!(app.auto_poke_incomplete_todos);
        assert!(app.pending_queued_dispatch);
        assert!(app.queued_messages().is_empty());
        assert_eq!(app.hidden_queued_system_messages.len(), 1);
        let summary = &app.hidden_queued_system_messages[0];
        assert!(super::commands::is_poke_message(summary));
        assert!(super::commands::is_todo_confidence_summary_message(summary));
        assert_eq!(summary, crate::todo::TODO_COMPLETION_CONTINUATION_MESSAGE);
        assert!(!summary.chars().any(|ch| ch.is_ascii_digit()));
        assert!(summary.contains("completion confidence"));
        assert!(!summary.to_ascii_lowercase().contains("gate"));
        assert!(!summary.to_ascii_lowercase().contains("threshold"));
        assert!(!summary.contains("Finish risky provider path"));
        assert!(
            app.display_messages()
                .iter()
                .any(|msg| msg.content.contains(
                    "Todo completion gate: completion confidence needs stronger validation."
                ))
        );

        // Dispatching the follow-up does not disarm the gate. If the model
        // finishes another turn without improving completion confidence, the
        // same validation follow-up is queued again.
        app.hidden_queued_system_messages.clear();
        app.pending_queued_dispatch = false;
        app.is_processing = true;
        super::local::finish_turn(&mut app);
        assert!(app.auto_poke_incomplete_todos);
        assert!(app.pending_queued_dispatch);
        assert_eq!(app.hidden_queued_system_messages.len(), 1);

        // Once the model records sufficient completion confidence through the
        // todo tool, the next completion check passes and disarms auto-poke.
        let mut validated = crate::todo::load_todos(&app.session.id).expect("load todos");
        for todo in &mut validated {
            todo.completion_confidence = Some(100);
            todo.confidence_history = match todo.id.as_str() {
                "todo-1" => vec![70, 80, 90, 100],
                _ => vec![90, 100],
            };
        }
        crate::todo::save_todos(&app.session.id, &validated).expect("save validated todos");
        app.hidden_queued_system_messages.clear();
        app.pending_queued_dispatch = false;
        app.is_processing = true;
        super::local::finish_turn(&mut app);
        assert!(!app.auto_poke_incomplete_todos);
        assert!(!app.pending_queued_dispatch);
        assert!(app.hidden_queued_system_messages.is_empty());
        assert!(app.display_messages().iter().any(|msg| {
            msg.content
                .contains("Todos complete. Completion confidence: 100%.")
        }));
    });
}

#[test]
fn test_todo_completion_gate_detects_abrupt_confidence_increase() {
    let summary = super::commands::todo_confidence_summary(&[crate::todo::TodoItem {
        status: "completed".to_string(),
        priority: "high".to_string(),
        confidence: Some(0),
        completion_confidence: Some(100),
        confidence_history: vec![0, 100],
        ..Default::default()
    }]);

    assert_eq!(summary.completion_average, Some(100));
    assert!(!summary.completion_confidence_needs_validation);
    assert!(summary.confidence_spike_detected);
    assert!(summary.needs_more_work);
}

#[test]
fn test_todo_completion_gate_allows_evidence_backed_confidence_steps() {
    let summary = super::commands::todo_confidence_summary(&[crate::todo::TodoItem {
        status: "completed".to_string(),
        priority: "high".to_string(),
        confidence: Some(100),
        completion_confidence: Some(100),
        confidence_history: vec![70, 80, 90, 100],
        ..Default::default()
    }]);

    assert_eq!(summary.completion_average, Some(100));
    assert!(!summary.completion_confidence_needs_validation);
    assert!(!summary.confidence_spike_detected);
    assert!(!summary.needs_more_work);
}

#[test]
fn test_finish_turn_challenges_confidence_spike_once() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                id: "todo-1".to_string(),
                content: "Validate provider result".to_string(),
                status: "completed".to_string(),
                priority: "high".to_string(),
                confidence: Some(100),
                completion_confidence: Some(100),
                confidence_history: vec![70, 100],
                ..Default::default()
            }],
        )
        .expect("save todos");

        app.auto_poke_incomplete_todos = true;
        app.is_processing = true;
        super::local::finish_turn(&mut app);

        assert!(app.auto_poke_incomplete_todos);
        assert!(app.todo_confidence_spike_challenged);
        assert!(app.pending_queued_dispatch);
        assert_eq!(
            app.hidden_queued_system_messages,
            vec![crate::todo::TODO_CONFIDENCE_SPIKE_CONTINUATION_MESSAGE]
        );
        assert!(app.display_messages().iter().any(|msg| {
            msg.content
                .contains("abrupt confidence increase needs independent validation")
        }));

        app.hidden_queued_system_messages.clear();
        app.pending_queued_dispatch = false;
        app.is_processing = true;
        super::local::finish_turn(&mut app);

        assert!(!app.auto_poke_incomplete_todos);
        assert!(!app.todo_confidence_spike_challenged);
        assert!(!app.pending_queued_dispatch);
    });
}

#[test]
fn test_todo_confidence_summary_hidden_queue_is_not_user_prompt() {
    let summary =
        "All todos are done. Todo confidence summary:\n- Weighted completion confidence: 94%."
            .to_string();

    let (user_messages, reminder, display_system_messages) =
        super::helpers::partition_queued_messages(Vec::new(), vec![summary.clone()]);

    assert!(user_messages.is_empty());
    assert!(display_system_messages.is_empty());
    assert_eq!(reminder.as_deref(), Some(summary.as_str()));
}

#[test]
fn test_finish_turn_without_auto_poke_does_not_queue_confidence_summary() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "todo-1".to_string(),
                content: "Done without poke".to_string(),
                status: "completed".to_string(),
                priority: "high".to_string(),
                blocked_by: Vec::new(),
                assigned_to: None,
                confidence: Some(90),
                completion_confidence: Some(90),
                confidence_history: Vec::new(),
            }],
        )
        .expect("save todos");

        app.auto_poke_incomplete_todos = false;
        app.is_processing = true;
        super::local::finish_turn(&mut app);

        assert!(!app.pending_queued_dispatch);
        assert!(app.queued_messages().is_empty());
        assert!(
            !app.display_messages()
                .iter()
                .any(|msg| msg.content.contains("confidence summary"))
        );
    });
}

#[test]
fn test_finish_turn_auto_poke_preserves_visible_turn_started() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "todo-1".to_string(),
                content: "Keep going".to_string(),
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

        let started = Instant::now() - Duration::from_secs(45);
        app.auto_poke_incomplete_todos = true;
        app.is_processing = true;
        app.visible_turn_started = Some(started);

        super::local::finish_turn(&mut app);

        assert_eq!(app.visible_turn_started, Some(started));
        assert!(app.pending_queued_dispatch);
    });
}

#[test]
fn test_help_topic_shows_overnight_command_details() {
    let mut app = create_test_app();
    app.input = "/help overnight".to_string();
    app.submit_input();

    let msg = app
        .display_messages()
        .last()
        .expect("missing help response");
    assert_eq!(msg.role, "system");
    assert!(msg.content.contains("/overnight <hours>[h|m] [mission]"));
    assert!(msg.content.contains("review HTML page"));
    assert!(msg.content.contains("/overnight status"));
}

#[test]
fn test_overnight_status_without_runs_is_handled() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        assert!(super::commands::handle_session_command(
            &mut app,
            "/overnight status"
        ));

        let msg = app
            .display_messages()
            .last()
            .expect("missing overnight status response");
        assert_eq!(msg.role, "system");
        assert!(msg.content.contains("No overnight runs found"));
    });
}

#[test]
fn test_overnight_help_command_is_handled() {
    let mut app = create_test_app();
    assert!(super::commands::handle_session_command(
        &mut app,
        "/overnight help"
    ));

    let msg = app
        .display_messages()
        .last()
        .expect("missing overnight help response");
    assert_eq!(msg.role, "system");
    assert!(msg.content.contains("/overnight <hours>[h|m] [mission]"));
    assert!(msg.content.contains("/overnight review"));
}

#[test]
fn test_overnight_start_runs_as_visible_local_turn() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        assert!(super::commands::handle_session_command(
            &mut app,
            "/overnight 1m hi"
        ));

        assert!(
            app.pending_turn,
            "local overnight should start a visible turn"
        );
        assert!(
            app.is_processing,
            "local overnight should enter processing state"
        );
        assert!(
            app.queued_messages.is_empty(),
            "local overnight should not use remote queue"
        );
        let last_message = app
            .session
            .messages
            .last()
            .expect("overnight prompt message");
        assert!(last_message.content.iter().any(|block| matches!(
            block,
            crate::message::ContentBlock::Text { text, .. }
                if text.contains("visible Overnight Coordinator")
        )));
    });
}

#[test]
fn test_overnight_start_queues_remote_turn_without_stuck_sending() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.is_remote = true;
        assert!(super::commands::handle_session_command(
            &mut app,
            "/overnight 1m hi"
        ));

        assert!(
            !app.pending_turn,
            "remote overnight should not set local pending_turn"
        );
        assert!(
            !app.is_processing,
            "remote overnight should not get stuck in local Sending"
        );
        assert_eq!(app.queued_messages.len(), 1);
        assert!(app.queued_messages[0].contains("visible Overnight Coordinator"));
    });
}
