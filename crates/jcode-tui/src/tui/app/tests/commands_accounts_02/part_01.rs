#[test]
fn test_usage_card_renders_when_loading() {
    let mut app = create_test_app();
    app.open_usage_inline_loading();

    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, &app))
        .expect("usage card draw should succeed");

    let text = buffer_to_text(&terminal);
    assert!(
        text.contains("╭"),
        "usage card should render as rounded box, got:\n{text}"
    );
    assert!(
        text.contains("Refreshing usage"),
        "usage card should be visible while loading, got:\n{text}"
    );
    assert!(
        text.contains("Checking connected provider limits"),
        "usage card should include loading details, got:\n{text}"
    );
}

#[test]
fn test_usage_card_does_not_capture_typing() {
    let mut app = create_test_app();
    app.open_usage_inline_loading();
    assert!(app.usage_overlay.is_none());

    app.handle_key(KeyCode::Char('h'), KeyModifiers::empty())
        .expect("type after usage card");

    assert!(app.usage_overlay.is_none());
    assert_eq!(app.input(), "h");
}

#[test]
fn test_usage_report_updates_display_only_card_without_system_message() {
    let mut app = create_test_app();
    app.usage_report_refreshing = true;
    // App::new seeds the provider transcript with the immutable session-context
    // reminder, so assert the usage report adds nothing on top of it rather
    // than expecting an empty transcript.
    let provider_messages_before = app.materialized_provider_messages().len();
    app.handle_usage_report(vec![crate::usage::ProviderUsage {
        provider_name: "OpenAI (ChatGPT)".to_string(),
        limits: vec![crate::usage::UsageLimit {
            name: "5h".to_string(),
            usage_percent: 82.0,
            resets_at: None,
        }],
        extra_info: vec![("plan".to_string(), "pro".to_string())],
        hard_limit_reached: false,
        error: None,
        last_used_unix_secs: None,
    }]);

    assert!(!app.usage_report_refreshing);
    assert!(app.inline_view_state.is_none());
    assert!(app.usage_overlay.is_none());
    let msg = app.display_messages().last().expect("missing usage card");
    assert_eq!(msg.role, "usage");
    assert!(msg.content.contains("OpenAI (ChatGPT)"));
    assert!(msg.content.contains("5h"));
    assert!(msg.content.contains("82%"));
    assert!(msg.content.contains("plan: pro"));
    let leaked = app.materialized_provider_messages();
    assert_eq!(
        leaked.len(),
        provider_messages_before,
        "usage report must not add provider-visible messages: {leaked:#?}"
    );
}

#[test]
fn test_usage_progress_updates_card_incrementally() {
    let mut app = create_test_app();
    app.open_usage_inline_loading();

    app.handle_usage_report_progress(crate::usage::ProviderUsageProgress {
        results: vec![crate::usage::ProviderUsage {
            provider_name: "Anthropic (Claude)".to_string(),
            limits: vec![crate::usage::UsageLimit {
                name: "5-hour window".to_string(),
                usage_percent: 41.0,
                resets_at: None,
            }],
            extra_info: Vec::new(),
            hard_limit_reached: false,
            error: None,
            last_used_unix_secs: None,
        }],
        completed: 1,
        total: 2,
        done: false,
        from_cache: false,
    });

    assert!(app.usage_report_refreshing);
    assert_eq!(
        app.display_messages()
            .iter()
            .filter(|message| message.role == "usage")
            .count(),
        1
    );
    let detail = &app
        .display_messages()
        .last()
        .expect("missing usage card")
        .content;
    assert!(detail.contains("5-hour window") || detail.contains("Refreshing usage (1/2)"));
}

#[test]
fn test_usage_with_suffix_does_not_open_picker_preview() {
    let mut app = create_test_app();

    for c in "/usage open".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }

    assert!(app.inline_interactive_state.is_none());
    assert_eq!(app.input(), "/usage open");
}

#[test]
fn test_show_accounts_includes_masked_email_column() {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let accounts = vec![crate::auth::claude::AnthropicAccount {
        label: "work".to_string(),
        access: "acc".to_string(),
        refresh: "ref".to_string(),
        expires: now_ms + 60000,
        email: Some("user@example.com".to_string()),
        scopes: Vec::new(),
        subscription_type: Some("max".to_string()),
    }];

    let mut lines = vec!["**Anthropic Accounts:**\n".to_string()];
    lines.push("| Account | Email | Status | Subscription | Active |".to_string());
    lines.push("|---------|-------|--------|-------------|--------|".to_string());

    for account in &accounts {
        let status = if account.expires > now_ms {
            "✓ valid"
        } else {
            "⚠ expired"
        };
        let email = account
            .email
            .as_deref()
            .map(mask_email)
            .unwrap_or_else(|| "unknown".to_string());
        let sub = account.subscription_type.as_deref().unwrap_or("unknown");
        lines.push(format!(
            "| {} | {} | {} | {} | {} |",
            account.label, email, status, sub, "◉"
        ));
    }

    let output = lines.join("\n");
    assert!(output.contains("| Account | Email | Status | Subscription | Active |"));
    assert!(output.contains("u***r@example.com"));
}

#[test]
fn test_account_openai_command_opens_account_picker() {
    with_temp_jcode_home(|| {
        let now_ms = chrono::Utc::now().timestamp_millis();

        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "work".to_string(),
            access_token: "acc".to_string(),
            refresh_token: "ref".to_string(),
            id_token: None,
            account_id: Some("acct_work".to_string()),
            expires_at: Some(now_ms + 60_000),
            email: Some("user@example.com".to_string()),
        })
        .unwrap();

        let mut app = create_test_app();
        app.input = "/account openai".to_string();
        app.submit_input();

        assert!(app.account_picker_overlay.is_none());
        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("/account openai should open the inline account picker");
        assert_eq!(picker.kind, crate::tui::PickerKind::Account);
        assert!(picker.entries.iter().any(|entry| {
            matches!(
                entry.action,
                crate::tui::PickerAction::Account(crate::tui::AccountPickerAction::Switch {
                    ref provider_id,
                    ..
                }) if provider_id == "openai"
            )
        }));
        assert!(
            picker
                .entries
                .iter()
                .any(|entry| entry.name == "new account")
        );
        assert!(
            picker
                .entries
                .iter()
                .any(|entry| entry.name == "replace account")
        );
        assert!(
            picker
                .entries
                .iter()
                .any(|entry| entry.name == "account center")
        );
    });
}

#[test]
fn test_account_command_opens_account_picker() {
    with_temp_jcode_home(|| {
        let now_ms = chrono::Utc::now().timestamp_millis();

        crate::auth::claude::upsert_account(crate::auth::claude::AnthropicAccount {
            label: "claude-1".to_string(),
            access: "claude_acc".to_string(),
            refresh: "claude_ref".to_string(),
            expires: now_ms + 60_000,
            email: Some("claude@example.com".to_string()),
            scopes: Vec::new(),
            subscription_type: Some("pro".to_string()),
        })
        .unwrap();

        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "work".to_string(),
            access_token: "acc".to_string(),
            refresh_token: "ref".to_string(),
            id_token: None,
            account_id: Some("acct_work".to_string()),
            expires_at: Some(now_ms + 60_000),
            email: Some("user@example.com".to_string()),
        })
        .unwrap();

        let mut app = create_test_app();
        app.input = "/account".to_string();
        app.submit_input();

        assert!(app.account_picker_overlay.is_none());
        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("/account should open the inline account picker");
        assert!(picker.entries.iter().any(|entry| {
            matches!(
                entry.action,
                crate::tui::PickerAction::Account(crate::tui::AccountPickerAction::Switch {
                    ref provider_id,
                    ref label
                }) if provider_id == "claude" && label == "claude-1"
            )
        }));
        assert!(picker.entries.iter().any(|entry| {
            matches!(
                entry.action,
                crate::tui::PickerAction::Account(crate::tui::AccountPickerAction::Switch {
                    ref provider_id,
                    ..
                }) if provider_id == "openai"
            )
        }));
        assert!(
            picker
                .entries
                .iter()
                .any(|entry| entry.name == "new Claude account")
        );
        assert!(
            picker
                .entries
                .iter()
                .any(|entry| entry.name == "new OpenAI account")
        );
        assert!(
            picker
                .entries
                .iter()
                .any(|entry| entry.name == "account center")
        );
    });
}

#[test]
fn test_account_picker_supports_arrow_and_vim_navigation() {
    with_temp_jcode_home(|| {
        let now_ms = chrono::Utc::now().timestamp_millis();

        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "first".to_string(),
            access_token: "acc1".to_string(),
            refresh_token: "ref1".to_string(),
            id_token: None,
            account_id: Some("acct_1".to_string()),
            expires_at: Some(now_ms + 60_000),
            email: Some("first@example.com".to_string()),
        })
        .unwrap();
        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "second".to_string(),
            access_token: "acc2".to_string(),
            refresh_token: "ref2".to_string(),
            id_token: None,
            account_id: Some("acct_2".to_string()),
            expires_at: Some(now_ms + 60_000),
            email: Some("second@example.com".to_string()),
        })
        .unwrap();

        let mut app = create_test_app();
        app.input = "/account openai".to_string();
        app.submit_input();

        let initial_selected = app
            .inline_interactive_state
            .as_ref()
            .expect("inline account picker should open")
            .selected;

        app.handle_key(KeyCode::Down, KeyModifiers::empty())
            .unwrap();
        let after_arrow = app.inline_interactive_state.as_ref().unwrap().selected;
        assert_eq!(after_arrow, initial_selected + 1);

        app.handle_key(KeyCode::Char('j'), KeyModifiers::empty())
            .unwrap();
        let after_vim = app.inline_interactive_state.as_ref().unwrap().selected;
        assert_eq!(after_vim, after_arrow + 1);

        app.handle_key(KeyCode::Char('k'), KeyModifiers::empty())
            .unwrap();
        assert_eq!(
            app.inline_interactive_state.as_ref().unwrap().selected,
            after_arrow
        );
    });
}

#[test]
fn test_account_picker_preview_from_input_filters_accounts() {
    with_temp_jcode_home(|| {
        let now_ms = chrono::Utc::now().timestamp_millis();

        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "first".to_string(),
            access_token: "acc1".to_string(),
            refresh_token: "ref1".to_string(),
            id_token: None,
            account_id: Some("acct_1".to_string()),
            expires_at: Some(now_ms + 60_000),
            email: Some("first@example.com".to_string()),
        })
        .unwrap();
        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "second".to_string(),
            access_token: "acc2".to_string(),
            refresh_token: "ref2".to_string(),
            id_token: None,
            account_id: Some("acct_2".to_string()),
            expires_at: Some(now_ms + 60_000),
            email: Some("second@example.com".to_string()),
        })
        .unwrap();

        let mut app = create_test_app();
        for c in "/account openai sec".chars() {
            app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
                .unwrap();
        }

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("account preview should open");
        assert!(picker.preview, "account picker should stay in preview mode");
        assert_eq!(picker.kind, crate::tui::PickerKind::Account);
        assert_eq!(picker.filter, "sec");
        assert!(app.account_picker_overlay.is_none());
        assert_eq!(app.input(), "/account openai sec");
    });
}

#[test]
fn test_account_picker_preview_stays_closed_for_explicit_subcommands() {
    let mut app = create_test_app();

    for c in "/account openai settings".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }

    assert!(app.inline_interactive_state.is_none());
    assert_eq!(app.input(), "/account openai settings");
}

#[test]
fn test_account_command_combines_claude_and_openai_accounts() {
    with_temp_jcode_home(|| {
        let now_ms = chrono::Utc::now().timestamp_millis();

        crate::auth::claude::upsert_account(crate::auth::claude::AnthropicAccount {
            label: "claude-1".to_string(),
            access: "claude_acc".to_string(),
            refresh: "claude_ref".to_string(),
            expires: now_ms + 60_000,
            email: Some("claude@example.com".to_string()),
            scopes: Vec::new(),
            subscription_type: Some("pro".to_string()),
        })
        .unwrap();
        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "openai-1".to_string(),
            access_token: "acc".to_string(),
            refresh_token: "ref".to_string(),
            id_token: None,
            account_id: Some("acct_openai_1".to_string()),
            expires_at: Some(now_ms + 60_000),
            email: Some("openai@example.com".to_string()),
        })
        .unwrap();

        let mut app = create_test_app();
        app.input = "/account".to_string();
        app.submit_input();

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("inline account picker should open");
        assert!(picker.entries.iter().any(|entry| {
            matches!(
                entry.action,
                crate::tui::PickerAction::Account(crate::tui::AccountPickerAction::Switch {
                    ref provider_id,
                    ref label
                }) if provider_id == "claude" && label == "claude-1"
            )
        }));
        assert!(picker.entries.iter().any(|entry| {
            matches!(
                entry.action,
                crate::tui::PickerAction::Account(crate::tui::AccountPickerAction::Switch {
                    ref provider_id,
                    ref label
                }) if provider_id == "openai" && label == "openai-1"
            )
        }));
        assert!(
            picker
                .entries
                .iter()
                .any(|entry| entry.name == "account center")
        );
    });
}

#[cfg(unix)]
#[test]
fn test_account_command_uses_fast_auth_snapshot_without_running_cursor_status() {
    use std::os::unix::fs::PermissionsExt;

    with_temp_jcode_home(|| {
        let prev_cursor_cli_path = std::env::var_os("JCODE_CURSOR_CLI_PATH");
        let temp = tempfile::TempDir::new().expect("create temp dir");
        let marker = temp.path().join("cursor-status-ran");
        let script = temp.path().join("cursor-agent-mock");

        std::fs::write(
            &script,
            format!("#!/bin/sh\necho ran > \"{}\"\nexit 0\n", marker.display()),
        )
        .expect("write mock cursor agent");
        let mut permissions = std::fs::metadata(&script)
            .expect("stat mock cursor agent")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("chmod mock cursor agent");

        let mut app = create_test_app();

        crate::env::set_var("JCODE_CURSOR_CLI_PATH", &script);
        crate::auth::AuthStatus::invalidate_cache();
        let _ = std::fs::remove_file(&marker);

        app.input = "/account".to_string();
        app.submit_input();

        assert!(app.inline_interactive_state.is_some());
        assert!(
            !marker.exists(),
            "/account should not execute `cursor-agent status` on open"
        );

        match prev_cursor_cli_path {
            Some(value) => crate::env::set_var("JCODE_CURSOR_CLI_PATH", value),
            None => crate::env::remove_var("JCODE_CURSOR_CLI_PATH"),
        }
        crate::auth::AuthStatus::invalidate_cache();
    });
}

#[test]
fn test_account_switch_shorthand_switches_openai_account_by_label() {
    with_temp_jcode_home(|| {
        let now_ms = chrono::Utc::now().timestamp_millis();

        crate::auth::codex::upsert_account(crate::auth::codex::OpenAiAccount {
            label: "openai2".to_string(),
            access_token: "acc".to_string(),
            refresh_token: "ref".to_string(),
            id_token: None,
            account_id: Some("acct_openai2".to_string()),
            expires_at: Some(now_ms + 60_000),
            email: Some("user2@example.com".to_string()),
        })
        .unwrap();

        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            app.input = "/account switch openai2".to_string();
            app.submit_input();

            assert_eq!(
                crate::auth::codex::active_account_label().as_deref(),
                Some("openai-1")
            );
        });
    });
}

fn local_oauth_identity(
    runtime_key: jcode_provider_core::RuntimeKey,
    provider_key: &str,
    account: (String, String, u64),
) -> jcode_provider_core::ExactRuntimeIdentity {
    jcode_provider_core::ExactRuntimeIdentity {
        provider_key: provider_key.to_string(),
        route: jcode_provider_core::RouteSelection {
            model: "account-switch-model".to_string(),
            runtime_key,
            api_method: "oauth-test".to_string(),
            provider_label: provider_key.to_string(),
            detail: "exact account-switch route".to_string(),
        },
        account_label: Some(account.0),
        account_id: Some(account.1),
        account_generation: Some(account.2),
        reasoning_effort: Some("high".to_string()),
    }
}

#[derive(Clone)]
struct RefreshingLocalAccountProvider {
    identity: std::sync::Arc<std::sync::Mutex<jcode_provider_core::ExactRuntimeIdentity>>,
}

#[async_trait::async_trait]
impl Provider for RefreshingLocalAccountProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<crate::provider::EventStream> {
        unimplemented!("identity admission test does not open a provider request")
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> String {
        "account-switch-model".to_string()
    }

    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(self.identity.lock().unwrap().clone())
    }

    async fn ensure_credentials_current(&self) -> anyhow::Result<()> {
        *self.identity.lock().unwrap() = local_oauth_identity(
            jcode_provider_core::RuntimeKey::OpenAIOAuth,
            "openai",
            crate::auth::codex::active_account_identity()
                .ok_or_else(|| anyhow::anyhow!("active OpenAI test account is missing"))?,
        );
        Ok(())
    }

    fn fork(&self) -> std::sync::Arc<dyn Provider> {
        std::sync::Arc::new(self.clone())
    }
}

fn install_local_account_projection(
    app: &mut App,
    identity: jcode_provider_core::ExactRuntimeIdentity,
) -> (String, Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut parent = crate::session::Session::create(None, None);
    parent.exact_runtime_identity = Some(identity.clone());
    parent.add_message(
        crate::message::Role::User,
        vec![ContentBlock::Text {
            text: "canonical raw account history".to_string(),
            cache_control: None,
        }],
    );
    parent.save().unwrap();
    let parent_id = parent.id.clone();
    let canonical_raw = serde_json::to_vec(&parent.messages).unwrap();

    let mut session = crate::session::Session::create(Some(parent_id.clone()), None);
    session.provider_key = Some(identity.provider_key.clone());
    session.model = Some(identity.route.model.clone());
    session.route_api_method = Some(identity.route.api_method.clone());
    session.reasoning_effort = identity.reasoning_effort.clone();
    session.exact_runtime_identity = Some(identity.clone());
    session.provider_session_id = Some("old-durable-resume".to_string());
    session.provider_session_identity = Some(identity.clone());
    session
        .install_imported_context_root(
            &parent,
            crate::session::StoredCompactionState {
                summary_text: "old-account imported projection".to_string(),
                openai_encrypted_content: None,
                covers_up_to_turn: 0,
                original_turn_count: 1,
                compacted_count: 1,
            },
            &identity,
        )
        .unwrap();
    session.add_message(
        crate::message::Role::User,
        vec![ContentBlock::Text {
            text: "current session raw bytes must survive account transition".to_string(),
            cache_control: None,
        }],
    );
    session.save().unwrap();
    let immutable_nodes = serde_json::to_vec(&session.context_nodes).unwrap();
    let current_raw = serde_json::to_vec(&session.messages).unwrap();
    let old_projected_provider_view = session.messages_for_provider_uncached();
    app.session = session;
    app.messages = old_projected_provider_view;
    app.provider_session_id = Some("old-runtime-resume".to_string());
    (parent_id, canonical_raw, immutable_nodes, current_raw)
}

async fn wait_for_local_account_switch_completion() {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if crate::server::ensure_no_pending_account_reconciliation().is_ok()
                && crate::provider::ensure_no_account_transition().is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("local account switch completion");
}

#[test]
fn test_openai_account_switch_rebinds_exact_identity_and_deactivates_old_projection() {
    with_temp_jcode_home(|| {
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
        let old_identity = local_oauth_identity(
            jcode_provider_core::RuntimeKey::OpenAIOAuth,
            "openai",
            crate::auth::codex::active_account_identity().unwrap(),
        );

        let mut app = create_test_app();
        let (parent_id, canonical_raw, immutable_nodes, current_raw) =
            install_local_account_projection(&mut app, old_identity.clone());
        let session_id = app.session.id.clone();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            app.switch_openai_account(&second);
            wait_for_local_account_switch_completion().await;
        });

        let target = crate::auth::codex::active_account_identity().unwrap();
        assert_eq!(target.0, second);
        let rebound = app
            .session
            .exact_runtime_identity
            .as_ref()
            .expect("destination exact identity");
        assert_eq!(rebound.route, old_identity.route);
        assert_eq!(rebound.account_label.as_deref(), Some(target.0.as_str()));
        assert_eq!(rebound.account_id.as_deref(), Some(target.1.as_str()));
        assert_eq!(rebound.account_generation, Some(target.2));
        assert!(app.provider_session_id.is_none());
        assert!(
            app.provider_session_id_for_next_request().is_none(),
            "the next provider request must not reuse the old account's resume ID"
        );
        assert!(app.session.provider_session_id.is_none());
        assert!(app.session.provider_session_identity.is_none());
        assert!(app.session.compaction.is_none());
        assert!(app.session.context_frontier.as_ref().is_some_and(|frontier| {
            frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
        }));
        assert!(
            runtime
                .block_on(async { app.registry.compaction().read().await.persisted_state() })
                .is_none()
        );
        assert_eq!(
            serde_json::to_vec(&app.session.context_nodes).unwrap(),
            immutable_nodes,
            "account switch must deactivate, not delete, immutable forensic nodes"
        );
        assert_eq!(
            serde_json::to_vec(
                &crate::session::Session::load(&parent_id)
                    .unwrap()
                    .messages
            )
            .unwrap(),
            canonical_raw,
            "account switch must not mutate canonical ancestor history"
        );
        assert_eq!(
            serde_json::to_vec(&app.session.messages).unwrap(),
            current_raw,
            "account switch must preserve current-session canonical raw bytes"
        );
        assert_eq!(
            serde_json::to_vec(&app.messages).unwrap(),
            serde_json::to_vec(&app.session.messages_for_provider_uncached()).unwrap(),
            "the next request must use the raw canonical view, not the old account projection"
        );
        let persisted = crate::session::Session::load(&session_id).unwrap();
        assert_eq!(persisted.exact_runtime_identity, app.session.exact_runtime_identity);
        assert!(persisted.provider_session_id.is_none());
        assert_eq!(serde_json::to_vec(&persisted.messages).unwrap(), current_raw);
    });
}

#[test]
fn test_anthropic_account_switch_rebinds_exact_identity_and_deactivates_old_projection() {
    with_temp_jcode_home(|| {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let account = |label: &str| crate::auth::claude::AnthropicAccount {
            label: label.to_string(),
            access: format!("access-{label}"),
            refresh: format!("refresh-{label}"),
            expires: now_ms + 60_000,
            email: None,
            scopes: Vec::new(),
            subscription_type: Some("max".to_string()),
        };
        let first = crate::auth::claude::upsert_account(account("first")).unwrap();
        let second = crate::auth::claude::upsert_account(account("second")).unwrap();
        crate::auth::claude::set_active_account(&first).unwrap();
        let old_identity = local_oauth_identity(
            jcode_provider_core::RuntimeKey::ClaudeOAuth,
            "anthropic",
            crate::auth::claude::active_account_identity().unwrap(),
        );

        let mut app = create_test_app();
        let (parent_id, canonical_raw, immutable_nodes, current_raw) =
            install_local_account_projection(&mut app, old_identity.clone());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            app.switch_account(&second);
            wait_for_local_account_switch_completion().await;
        });

        let target = crate::auth::claude::active_account_identity().unwrap();
        assert_eq!(target.0, second);
        let rebound = app
            .session
            .exact_runtime_identity
            .as_ref()
            .expect("destination exact identity");
        assert_eq!(rebound.route, old_identity.route);
        assert_eq!(rebound.account_label.as_deref(), Some(target.0.as_str()));
        assert_eq!(rebound.account_id.as_deref(), Some(target.1.as_str()));
        assert_eq!(rebound.account_generation, Some(target.2));
        assert!(app.provider_session_id.is_none());
        assert!(
            app.provider_session_id_for_next_request().is_none(),
            "the next provider request must not reuse the old account's resume ID"
        );
        assert!(app.session.provider_session_id.is_none());
        assert!(app.session.provider_session_identity.is_none());
        assert!(app.session.compaction.is_none());
        assert!(app.session.context_frontier.as_ref().is_some_and(|frontier| {
            frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
        }));
        assert!(
            runtime
                .block_on(async { app.registry.compaction().read().await.persisted_state() })
                .is_none()
        );
        assert_eq!(serde_json::to_vec(&app.session.context_nodes).unwrap(), immutable_nodes);
        assert_eq!(
            serde_json::to_vec(
                &crate::session::Session::load(&parent_id)
                    .unwrap()
                    .messages
            )
            .unwrap(),
            canonical_raw
        );
        assert_eq!(serde_json::to_vec(&app.session.messages).unwrap(), current_raw);
        assert_eq!(
            serde_json::to_vec(&app.messages).unwrap(),
            serde_json::to_vec(&app.session.messages_for_provider_uncached()).unwrap()
        );
    });
}

#[test]
fn test_external_account_reconciliation_clears_old_resume_before_next_local_request() {
    with_temp_jcode_home(|| {
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
        let old_identity = local_oauth_identity(
            jcode_provider_core::RuntimeKey::OpenAIOAuth,
            "openai",
            crate::auth::codex::active_account_identity().unwrap(),
        );
        let mut app = create_test_app();
        app.provider = std::sync::Arc::new(RefreshingLocalAccountProvider {
            identity: std::sync::Arc::new(std::sync::Mutex::new(old_identity.clone())),
        });
        install_local_account_projection(&mut app, old_identity);
        let old_revision = app.session.persistence_revision;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (_, current_is_target, completion) = crate::server::prepare_local_account_switch(
                app.provider.clone(),
                jcode_provider_core::RuntimeKey::OpenAIOAuth,
                &second,
                &app.session.id,
            )
            .unwrap();
            assert!(current_is_target);
            completion.finish().await.unwrap();
        });

        assert_eq!(
            app.provider_session_id.as_deref(),
            Some("old-runtime-resume"),
            "fixture must retain the other process's stale in-memory resume before admission"
        );
        let admission = runtime
            .block_on(app.acquire_local_model_turn_admission())
            .expect("peer TUI admission must refresh credentials and adopt durable identity");
        assert!(app.session.persistence_revision > old_revision);
        assert!(app.provider_session_id_for_next_request().is_none());
        assert!(app.session.provider_session_id.is_none());
        assert!(app.session.provider_session_identity.is_none());
        assert!(app.session.compaction.is_none());
        let target = crate::auth::codex::active_account_identity().unwrap();
        assert_eq!(
            app.session
                .exact_runtime_identity
                .as_ref()
                .and_then(|identity| identity.account_id.as_deref()),
            Some(target.1.as_str())
        );
        assert_eq!(app.provider.exact_runtime_identity(), app.session.exact_runtime_identity);
        drop(admission);
    });
}

#[test]
fn test_openai_account_switch_preflight_failure_keeps_original_account_and_resume() {
    with_temp_jcode_home(|| {
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

        let mut app = create_test_app();
        app.provider_session_id = Some("runtime-resume".to_string());
        app.session.provider_key = Some("openai".to_string());
        app.session.provider_session_id = Some("durable-resume".to_string());
        app.session.save().unwrap();
        let mut concurrent = crate::session::Session::load(&app.session.id).unwrap();
        concurrent.title = Some("advance CAS revision".to_string());
        concurrent.save().unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            app.switch_openai_account(&second);
        });

        assert_eq!(crate::auth::codex::active_account_label().as_deref(), Some(first.as_str()));
        assert_eq!(app.provider_session_id.as_deref(), Some("runtime-resume"));
        assert_eq!(app.session.provider_session_id.as_deref(), Some("durable-resume"));
        assert_eq!(
            crate::session::Session::load(&app.session.id)
                .unwrap()
                .title
                .as_deref(),
            Some("advance CAS revision")
        );
        assert!(crate::server::ensure_no_pending_account_reconciliation().is_ok());
    });
}

#[test]
fn test_openai_account_switch_during_admitted_turn_fails_fast_without_mutation() {
    with_temp_jcode_home(|| {
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
        let first = crate::auth::codex::upsert_account(account("turn-first")).unwrap();
        let second = crate::auth::codex::upsert_account(account("turn-second")).unwrap();
        crate::auth::codex::set_active_account(&first).unwrap();
        let old_identity = local_oauth_identity(
            jcode_provider_core::RuntimeKey::OpenAIOAuth,
            "openai",
            crate::auth::codex::active_account_identity().unwrap(),
        );
        let mut app = create_test_app();
        install_local_account_projection(&mut app, old_identity);
        let old_session = serde_json::to_vec(&app.session).unwrap();
        let old_runtime_resume = app.provider_session_id.clone();
        let admission = crate::session::AccountTransitionFileLock::acquire_shared().unwrap();
        app.is_processing = true;

        let started = std::time::Instant::now();
        let error = app
            .prepare_local_account_switch(jcode_provider_core::RuntimeKey::OpenAIOAuth, &second)
            .expect_err("in-turn account switch must fail fast");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.to_string().contains("retry after it finishes"));
        assert_eq!(
            crate::auth::codex::active_account_label().as_deref(),
            Some(first.as_str())
        );
        assert_eq!(serde_json::to_vec(&app.session).unwrap(), old_session);
        assert_eq!(app.provider_session_id, old_runtime_resume);
        drop(admission);
    });
}
