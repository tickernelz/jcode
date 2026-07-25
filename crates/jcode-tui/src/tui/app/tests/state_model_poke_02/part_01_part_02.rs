#[test]
fn test_command_suggestion_render_highlights_selected_row_by_color() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.input = "/con".to_string();
    app.cursor_pos = app.input.len();
    let suggestions = app.command_suggestions();
    assert!(suggestions.len() >= 2);
    let first = suggestions[0].0.clone();
    let second = suggestions[1].0.clone();

    let selected_base = crate::tui::color_support::rgb(255, 213, 128);
    let unselected_base = crate::tui::color_support::rgb(128, 203, 196);

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20))
        .expect("failed to create test terminal");
    render_and_snap(&app, &mut terminal);
    assert_command_match_recolored(&terminal, &first, selected_base);
    assert_command_match_recolored(&terminal, &second, unselected_base);

    app.handle_key(KeyCode::Down, KeyModifiers::empty())
        .unwrap();
    render_and_snap(&app, &mut terminal);
    assert_command_match_recolored(&terminal, &first, unselected_base);
    assert_command_match_recolored(&terminal, &second, selected_base);
}

#[test]
fn test_single_command_suggestion_uses_selected_color_only() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.input = "/review".to_string();
    app.cursor_pos = app.input.len();
    let suggestions = app.command_suggestions();
    assert_eq!(suggestions.len(), 1);
    let command = suggestions[0].0.clone();

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20))
        .expect("failed to create test terminal");
    render_and_snap(&app, &mut terminal);
    // A single suggestion still uses the selected-row base color; the fuzzy
    // match recoloring dims the '/' and brightens matched characters of it.
    assert_command_match_recolored(
        &terminal,
        &command,
        crate::tui::color_support::rgb(255, 213, 128),
    );
}

#[test]
fn test_command_suggestion_render_window_scrolls_with_selection() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.input = "/".to_string();
    app.cursor_pos = app.input.len();
    let suggestions = app.command_suggestions();
    let limit = crate::tui::app::COMMAND_SUGGESTION_VISIBLE_LIMIT;
    assert!(suggestions.len() > limit);
    let first = suggestions[0].0.clone();
    let selected_after_scroll = suggestions[limit].0.clone();

    for _ in 0..limit {
        app.handle_key(KeyCode::Down, KeyModifiers::empty())
            .unwrap();
    }

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24))
        .expect("failed to create test terminal");
    let rendered = render_and_snap(&app, &mut terminal);
    assert!(
        !rendered.contains(&first),
        "the first suggestion should scroll out of the visible window:\n{rendered}"
    );
    assert!(
        rendered.contains(&selected_after_scroll),
        "the newly selected suggestion should be visible:\n{rendered}"
    );
    assert!(
        rendered.contains("↑"),
        "the scrolled window should indicate suggestions above:\n{rendered}"
    );
    assert_eq!(
        command_cell_fg(&terminal, &selected_after_scroll),
        Some(crate::tui::color_support::rgb(255, 213, 128))
    );
}

#[test]
fn test_remote_command_suggestion_arrow_and_ctrl_navigation_accepts_highlighted_row() {
    let mut app = create_test_app();
    app.input = "/con".to_string();
    app.cursor_pos = app.input.len();
    let suggestions = app.command_suggestions();
    assert!(suggestions.len() >= 2);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    rt.block_on(app.handle_remote_key(KeyCode::Down, KeyModifiers::empty(), &mut remote))
        .unwrap();
    assert_eq!(app.command_suggestion_selected, 1);
    rt.block_on(app.handle_remote_key(KeyCode::Char('k'), KeyModifiers::CONTROL, &mut remote))
        .unwrap();
    assert_eq!(app.command_suggestion_selected, 0);
    rt.block_on(app.handle_remote_key(KeyCode::Char('j'), KeyModifiers::CONTROL, &mut remote))
        .unwrap();
    assert_eq!(app.command_suggestion_selected, 1);

    let expected = suggestions[1].0.clone();
    rt.block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::empty(), &mut remote))
        .unwrap();
    assert_eq!(app.input, expected);
    assert_eq!(app.cursor_pos, app.input.len());
}

#[test]
fn test_registered_command_suggestions_include_aliases_and_hide_secret_commands() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/");
    let commands: Vec<&str> = suggestions.iter().map(|(cmd, _)| cmd.as_str()).collect();

    assert_eq!(commands.iter().filter(|cmd| **cmd == "/cancel").count(), 1);
    assert!(commands.contains(&"/models"));
    assert!(commands.contains(&"/sessions"));
    assert!(commands.contains(&"/dictation"));
    assert!(commands.contains(&"/feedback"));
    assert!(commands.contains(&"/plan"));
    assert!(!commands.contains(&"/z"));
    assert!(!commands.contains(&"/zz"));
    assert!(!commands.contains(&"/zzz"));
}

#[test]
fn test_cancel_command_is_available_for_prefix_autocomplete() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/can");

    assert!(suggestions.iter().any(|(cmd, help)| {
        cmd == "/cancel" && *help == "Cancel the current prompt or operation"
    }));
}

#[test]
fn test_auth_doctor_command_suggestion_is_not_shadowed_by_provider_suggestions() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/auth d");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/auth doctor"));
}

#[test]
fn test_top_level_command_suggestions_include_config_and_subscription() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/con");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/config"));
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/context"));

    let suggestions = app.get_suggestions_for("/ali");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/alignment"));

    let suggestions = app.get_suggestions_for("/sub");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/subscription"));
}

#[test]
fn test_top_level_command_suggestions_include_project_local_skills() {
    let mut app = create_test_app();

    // Hermetic project-local skill: the suggestion list must surface skills
    // found under <working_dir>/.jcode/skills, independent of the skills
    // installed on the machine running the tests.
    let temp = tempfile::tempdir().expect("tempdir");
    let skill_dir = temp
        .path()
        .join(".jcode")
        .join("skills")
        .join("optimization");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: optimization\ndescription: Project-local test skill\n---\n# Optimization\n",
    )
    .expect("write SKILL.md");
    app.session.working_dir = Some(temp.path().to_string_lossy().to_string());
    app.refresh_skills_snapshot();

    let suggestions = app.get_suggestions_for("/optim");

    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/optimization"));
}

#[test]
fn test_top_level_command_suggestions_include_catchup_and_back() {
    let app = create_test_app();

    let suggestions = app.get_suggestions_for("/cat");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/catchup"));

    let suggestions = app.get_suggestions_for("/bac");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/back"));

    let suggestions = app.get_suggestions_for("/gi");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/git"));

    let suggestions = app.get_suggestions_for("/comm");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/commit"));

    let suggestions = app.get_suggestions_for("/tran");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/transcript"));
}

#[test]
fn test_top_level_command_suggestions_include_all_non_hidden_commands() {
    let app = create_test_app();

    let suggestions = app.get_suggestions_for("/logo");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/logout"));

    let suggestions = app.get_suggestions_for("/client");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/client-reload"));

    let suggestions = app.get_suggestions_for("/z");
    assert!(!suggestions.iter().any(|(cmd, _)| cmd == "/z"));
    assert!(!suggestions.iter().any(|(cmd, _)| cmd == "/zz"));
}

#[test]
fn test_logout_clear_anthropic_accounts_removes_all_accounts_once() {
    with_temp_jcode_home(|| {
        for index in 1..=3 {
            crate::auth::claude::upsert_account(crate::auth::claude::AnthropicAccount {
                label: format!("requested-{index}"),
                access: format!("access-{index}"),
                refresh: format!("refresh-{index}"),
                expires: 100 + index,
                email: None,
                subscription_type: None,
                scopes: Vec::new(),
            })
            .unwrap();
        }
        crate::auth::claude::set_active_account("claude-3").unwrap();

        let labels: Vec<_> = crate::auth::claude::list_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.label)
            .collect();
        assert_eq!(labels, vec!["claude-1", "claude-2", "claude-3"]);

        assert_eq!(crate::auth::claude::clear_accounts().unwrap(), 3);
        assert!(crate::auth::claude::list_accounts().unwrap().is_empty());
        assert!(crate::auth::claude::active_account_label().is_none());
    });
}

#[test]
fn test_transcript_command_suggestions_include_path_variant() {
    let app = create_test_app();

    let suggestions = app.get_suggestions_for("/transcript p");

    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/transcript path"));
}

#[test]
fn test_help_topic_suggestions_are_contextual() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/help fi");
    assert_eq!(
        suggestions.first().map(|(cmd, _)| cmd.as_str()),
        Some("/help fix")
    );
}

#[test]
fn test_help_topic_suggestions_include_catchup_topics() {
    let app = create_test_app();

    let suggestions = app.get_suggestions_for("/help cat");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/help catchup"));

    let suggestions = app.get_suggestions_for("/help bac");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/help back"));
}

#[test]
fn test_context_command_reports_session_context_snapshot() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.memory_enabled = true;
        app.swarm_enabled = true;
        app.queue_mode = true;
        app.active_skill = Some("debug".to_string());
        app.queued_messages.push("queued follow-up".to_string());
        app.pending_images
            .push(("image/png".to_string(), "abc".to_string()));
        app.side_panel = crate::side_panel::SidePanelSnapshot {
            focused_page_id: Some("goals".to_string()),
            pages: vec![crate::side_panel::SidePanelPage {
                id: "goals".to_string(),
                title: "Goals".to_string(),
                file_path: "".to_string(),
                format: crate::side_panel::SidePanelPageFormat::Markdown,
                source: crate::side_panel::SidePanelPageSource::Managed,
                content: "goal details".to_string(),
                updated_at_ms: 0,
            }],
        };
        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "one".to_string(),
                content: "Inspect context summary".to_string(),
                status: "pending".to_string(),
                priority: "high".to_string(),
                blocked_by: Vec::new(),
                assigned_to: None,
                confidence: Some(77),
                completion_confidence: None,
                confidence_history: Vec::new(),
            }],
        )
        .expect("save todos");

        app.input = "/context".to_string();
        app.submit_input();

        let msg = app
            .display_messages()
            .last()
            .expect("missing context report");
        assert_eq!(msg.title.as_deref(), Some("Context"));
        assert!(msg.content.contains("Session Context"));
        assert!(msg.content.contains("Prompt / Context Composition"));
        assert!(msg.content.contains("Compaction"));
        assert!(msg.content.contains("Session State"));
        assert!(msg.content.contains("Todos"));
        assert!(msg.content.contains("Side Panel"));
        assert!(msg.content.contains("Inspect context summary"));
        assert!(msg.content.contains("[pending|high|confidence 77%]"));
        assert!(msg.content.contains("active skill: debug"));
        assert!(msg.content.contains("queue mode: on"));
    });
}

#[test]
fn test_nested_command_suggestions_filter_partial_suffixes() {
    let app = create_test_app();

    let suggestions = app.get_suggestions_for("/config ed");
    assert_eq!(
        suggestions.first().map(|(cmd, _)| cmd.as_str()),
        Some("/config edit")
    );

    let suggestions = app.get_suggestions_for("/alignment ce");
    assert_eq!(
        suggestions.first().map(|(cmd, _)| cmd.as_str()),
        Some("/alignment centered")
    );

    let suggestions = app.get_suggestions_for("/compact mo se");
    assert_eq!(
        suggestions.first().map(|(cmd, _)| cmd.as_str()),
        Some("/compact mode semantic")
    );

    let suggestions = app.get_suggestions_for("/memory st");
    assert_eq!(
        suggestions.first().map(|(cmd, _)| cmd.as_str()),
        Some("/memory status")
    );

    let suggestions = app.get_suggestions_for("/improve st");
    assert!(
        suggestions.iter().any(|(cmd, _)| cmd == "/improve status"),
        "expected /improve status suggestion"
    );

    let suggestions = app.get_suggestions_for("/refactor st");
    assert!(
        suggestions.iter().any(|(cmd, _)| cmd == "/refactor status"),
        "expected /refactor status suggestion"
    );
}

#[test]
fn test_autocomplete_adds_space_for_nested_argument_commands() {
    let mut app = create_test_app();
    app.input = "/goals sh".to_string();
    app.cursor_pos = app.input.len();

    assert!(app.autocomplete());
    assert_eq!(app.input(), "/goals show ");
}

#[test]
fn test_goals_show_suggestions_include_goal_ids() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let goal = crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: crate::goal::GoalScope::Project,
            ..crate::goal::GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");

    let mut app = create_test_app();
    app.session.working_dir = Some(project.display().to_string());

    let suggestions = app.get_suggestions_for("/goals show ");
    assert!(
        suggestions
            .iter()
            .any(|(cmd, _)| cmd == &format!("/goals show {}", goal.id))
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

fn configure_test_remote_models(app: &mut App) {
    app.is_remote = true;
    app.remote_provider_model = Some("gpt-5.3-codex".to_string());
    app.remote_available_entries = vec!["gpt-5.3-codex".to_string(), "gpt-5.2-codex".to_string()];
}

fn configure_test_remote_models_with_openai_recommendations(app: &mut App) {
    app.is_remote = true;
    app.remote_provider_model = Some("gpt-5.2".to_string());
    app.remote_available_entries = vec![
        "gpt-5.2".to_string(),
        "gpt-5.5".to_string(),
        "gpt-5.4".to_string(),
        "gpt-5.4-pro".to_string(),
        "gpt-5.3-codex-spark".to_string(),
        "gpt-5.3-codex".to_string(),
        "claude-opus-4-8".to_string(),
    ];
    app.remote_model_options = app
        .remote_available_entries
        .iter()
        .filter(|model| model.as_str() != "claude-opus-4-8")
        .cloned()
        .map(|model| crate::provider::ModelRoute {
            model,
            provider: "OpenAI".to_string(),
            api_method: "openai-oauth".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        })
        .collect();
    app.remote_model_options.push(crate::provider::ModelRoute {
        model: "claude-opus-4-8".to_string(),
        provider: "Anthropic".to_string(),
        api_method: "claude-oauth".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
    });
    app.remote_model_options.push(crate::provider::ModelRoute {
        model: "claude-opus-4-8".to_string(),
        provider: "Anthropic".to_string(),
        api_method: "claude-api".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
    });
}

fn configure_test_remote_openrouter_provider_routes(app: &mut App) {
    app.is_remote = true;
    app.remote_provider_name = Some("openrouter".to_string());
    app.remote_provider_model = Some("anthropic/claude-sonnet-4".to_string());
    app.remote_available_entries = vec!["anthropic/claude-sonnet-4".to_string()];
    app.remote_model_options = vec![
        crate::provider::ModelRoute {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: "auto".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: "→ Fireworks".to_string(),
            cheapness: None,
        },
        crate::provider::ModelRoute {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: "Fireworks".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        },
        crate::provider::ModelRoute {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: "OpenAI".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        },
    ];
}

#[test]
fn test_model_picker_preview_filter_parsing() {
    assert_eq!(
        App::model_picker_preview_filter("/model"),
        Some(String::new())
    );
    assert_eq!(
        App::model_picker_preview_filter("/model   gpt-5"),
        Some("gpt-5".to_string())
    );
    assert_eq!(
        App::model_picker_preview_filter("   /models codex"),
        Some("codex".to_string())
    );
    assert_eq!(App::model_picker_preview_filter("/modelx"), None);
    assert_eq!(App::model_picker_preview_filter("hello /model"), None);
}

#[test]
fn test_login_picker_preview_filter_parsing() {
    assert_eq!(
        App::login_picker_preview_filter("/login"),
        Some(String::new())
    );
    assert_eq!(
        App::login_picker_preview_filter("/login   zai"),
        Some("zai".to_string())
    );
    assert_eq!(App::login_picker_preview_filter("/loginx"), None);
    assert_eq!(App::login_picker_preview_filter("hello /login"), None);
}

#[test]
fn test_agents_command_opens_agent_picker() {
    let mut app = create_test_app();
    app.input = "/agents".to_string();

    app.submit_input();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("/agents should open the agent picker");
    assert!(
        picker
            .entries
            .iter()
            .any(|entry| entry.name == "Code review")
    );
    assert!(picker.entries.iter().any(|entry| matches!(
        entry.action,
        crate::tui::PickerAction::AgentTarget(crate::tui::AgentModelTarget::Swarm)
    )));
    let swarm_entry = picker
        .entries
        .iter()
        .find(|entry| {
            matches!(
                entry.action,
                crate::tui::PickerAction::AgentTarget(crate::tui::AgentModelTarget::Swarm)
            )
        })
        .expect("swarm entry");
    assert!(swarm_entry.options[0].detail.contains("/swarm-prompt"));
    assert_eq!(picker.filtered.len(), picker.entries.len());
    let compaction = picker
        .entries
        .iter()
        .find(|entry| {
            matches!(
                entry.action,
                crate::tui::PickerAction::AgentTarget(crate::tui::AgentModelTarget::Compaction)
            )
        })
        .expect("compaction entry");
    assert_eq!(compaction.name, "LCM compactor");
    assert_eq!(compaction.options[0].api_method, "compaction.model");
}

#[test]
fn test_agents_command_suggestions_include_targets() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/agents re");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/agents review"));
    let suggestions = app.get_suggestions_for("/agents comp");
    assert!(
        suggestions
            .iter()
            .any(|(cmd, _)| cmd == "/agents compaction")
    );
    let help = app.command_help("agents").expect("agents help");
    assert!(help.contains("compaction"));
    assert!(help.contains("/agents lcm"));
}

#[test]
fn test_agents_lcm_alias_opens_compaction_model_picker() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        configure_test_remote_models(&mut app);
        app.input = "/agents lcm".to_string();
        app.submit_input();

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("alias should open model picker");
        assert!(matches!(
            picker.entries[0].action,
            crate::tui::PickerAction::AgentModelChoice {
                target: crate::tui::AgentModelTarget::Compaction,
                clear_override: true,
            }
        ));
        assert!(picker.entries[0].name.starts_with("inherit ("));
    });
}

#[test]
fn test_swarm_prompt_command_is_discoverable_in_suggestions_and_help() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/swarm-pro");
    assert!(
        suggestions
            .iter()
            .any(|(command, _)| command == "/swarm-prompt")
    );

    let help = app
        .command_help("swarm-prompt")
        .expect("/swarm-prompt should have detailed help");
    assert!(help.contains("/swarm-prompt"));
    assert!(help.contains(".jcode/swarm-prompt.md"));
    assert!(help.contains("Restart or reload Jcode"));
}

#[test]
fn test_agents_picker_uses_provider_default_when_inherited_model_is_unknown() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.open_agents_picker();

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("/agents should open the agent picker");
        let swarm_entry = picker
            .entries
            .iter()
            .find(|entry| {
                matches!(
                    entry.action,
                    crate::tui::PickerAction::AgentTarget(crate::tui::AgentModelTarget::Swarm)
                )
            })
            .expect("swarm entry should exist");

        assert_eq!(swarm_entry.options[0].provider, "provider default");
    });
}

#[test]
fn test_agent_model_picker_inherit_row_uses_provider_default_when_inherited_model_is_unknown() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        configure_test_remote_models(&mut app);
        app.open_agent_model_picker(crate::tui::AgentModelTarget::Swarm);

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("agent model picker should open");
        let inherit_entry = picker.entries.first().expect("inherit row should exist");

        assert_eq!(inherit_entry.name, "inherit (provider default)");
        assert!(matches!(
            inherit_entry.action,
            crate::tui::PickerAction::AgentModelChoice {
                target: crate::tui::AgentModelTarget::Swarm,
                clear_override: true,
            }
        ));
    });
}
