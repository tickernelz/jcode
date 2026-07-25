
#[test]
fn update_command_reloads_stale_remote_server_before_client_update_check() {
    use tokio::io::AsyncBufReadExt;

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_server_has_update = Some(true);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut line = String::new();
    let reloaded = rt.block_on(async {
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        let peer = remote
            .take_dummy_peer()
            .expect("dummy remote should retain peer stream");
        let (reader, _writer) = peer.into_split();
        let mut reader = tokio::io::BufReader::new(reader);

        let reloaded =
            super::remote::reload_stale_remote_server_before_update(&mut app, &mut remote)
                .await
                .expect("stale server reload request should send");
        reader
            .read_line(&mut line)
            .await
            .expect("reload request should be readable by peer");
        reloaded
    });

    assert!(reloaded);
    assert!(matches!(
        serde_json::from_str::<crate::protocol::Request>(&line)
            .expect("reload request should deserialize"),
        crate::protocol::Request::Reload { id: 1, force: true }
    ));
    let content = app.display_messages().last().unwrap().content.clone();
    assert!(content.contains("Reloading stale server"), "{content}");
}

#[test]
fn stale_server_history_is_deferred_before_remote_state_is_applied() {
    crate::env::remove_var("JCODE_ALLOW_SERVER_VERSION_MISMATCH");
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.remote_session_id = Some("session_existing".to_string());
    app.connection_type = Some("websocket".to_string());

    let redraw = app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: "session_from_stale_server".to_string(),
            messages: vec![crate::protocol::HistoryMessage {
                role: "assistant".to_string(),
                content: "stale answer".to_string(),
                tool_calls: None,
                tool_data: None,
            }],
            images: vec![],
            provider_name: Some("stale-provider".to_string()),
            provider_model: Some("stale-model".to_string()),
            exact_runtime_identity: None,
            subagent_model: Some("stale-subagent".to_string()),
            autoreview_enabled: Some(true),
            autojudge_enabled: Some(true),
            available_models: vec!["stale-model".to_string()],
            available_model_routes: vec![],
            mcp_servers: vec!["stale-mcp:1".to_string()],
            skills: vec!["stale-skill".to_string()],
            total_tokens: Some((99, 100)),
            token_usage_totals: None,
            all_sessions: vec!["session_from_stale_server".to_string()],
            client_count: Some(42),
            is_canary: Some(false),
            reload_recovery: None,
            server_version: Some("v0.0.1-stale".to_string()),
            server_name: Some("stale-server".to_string()),
            server_icon: Some("🧟".to_string()),
            server_has_update: Some(true),
            was_interrupted: None,
            connection_type: Some("stale-connection".to_string()),
            status_detail: Some("stale-status".to_string()),
            upstream_provider: Some("stale-upstream".to_string()),
            resolved_credential: None,
            reasoning_effort: Some("high".to_string()),
            service_tier: Some("stale-tier".to_string()),
            compaction_mode: crate::config::CompactionMode::Reactive,
            activity: None,
            side_panel: crate::side_panel::SidePanelSnapshot::default(),
        },
        &mut remote,
    );

    assert!(!redraw);
    assert!(app.pending_server_reload);
    assert_eq!(app.remote_server_has_update, Some(true));
    assert_eq!(app.remote_server_version.as_deref(), Some("v0.0.1-stale"));
    assert_eq!(app.remote_session_id.as_deref(), Some("session_existing"));
    assert_eq!(remote.session_id(), None);
    assert_eq!(app.connection_type.as_deref(), Some("websocket"));
    assert!(app.remote_skills.is_empty());
    assert!(app.remote_sessions.is_empty());
    assert_eq!(app.remote_client_count, None);
    assert_eq!(app.remote_total_tokens, None);
    assert_ne!(
        app.session.subagent_model.as_deref(),
        Some("stale-subagent")
    );
    let content = app.display_messages().last().unwrap().content.clone();
    assert!(
        content.contains("Reloading the server before applying remote session state"),
        "{content}"
    );
}

#[test]
fn deferred_stale_server_history_captures_session_id_for_reload_handoff() {
    // Issue #328: when a fresh client connects to a still-running older server
    // (e.g. right after an auto-update), the History payload is deferred because
    // of the version mismatch and the handler returns BEFORE assigning
    // `remote_session_id`. On a fresh client that id is `None`, so the later
    // client reload handoff used to fabricate a `ses_<ts>_<rand>` id that no
    // store can resolve, leaving the user stuck at "No session found matching
    // ...". We must stash the real session id so the re-exec resumes the actual
    // server session instead.
    let _env_guard = crate::storage::lock_test_env();
    crate::env::remove_var("JCODE_ALLOW_SERVER_VERSION_MISMATCH");
    crate::env::set_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE", "v0.21.0 (deadbeef)");

    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    // Fresh client: no session id learned yet (the #328 reproduction).
    app.remote_session_id = None;
    assert!(app.pending_reload_session_id.is_none());

    let redraw = app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: "session_real_server_owned".to_string(),
            messages: vec![crate::protocol::HistoryMessage {
                role: "assistant".to_string(),
                content: "stale answer".to_string(),
                tool_calls: None,
                tool_data: None,
            }],
            images: vec![],
            provider_name: Some("stale-provider".to_string()),
            provider_model: Some("stale-model".to_string()),
            exact_runtime_identity: None,
            subagent_model: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            available_models: vec![],
            available_model_routes: vec![],
            mcp_servers: vec![],
            skills: vec![],
            total_tokens: None,
            token_usage_totals: None,
            all_sessions: vec![],
            client_count: None,
            is_canary: None,
            reload_recovery: None,
            // Ancient server that predates self-reported staleness; the client's
            // own release-version comparison drives the deferral.
            server_version: Some("v0.20.4".to_string()),
            server_name: Some("stale-server".to_string()),
            server_icon: Some("🧟".to_string()),
            server_has_update: None,
            was_interrupted: None,
            connection_type: None,
            status_detail: None,
            upstream_provider: None,
            resolved_credential: None,
            reasoning_effort: None,
            service_tier: None,
            compaction_mode: crate::config::CompactionMode::Reactive,
            activity: None,
            side_panel: crate::side_panel::SidePanelSnapshot::default(),
        },
        &mut remote,
    );

    // History is deferred (no redraw, reload pending, remote_session_id still
    // unset) but the real session id is captured for the reload handoff.
    assert!(!redraw);
    assert!(app.pending_server_reload);
    assert_eq!(app.remote_session_id.as_deref(), None);
    assert_eq!(
        app.pending_reload_session_id.as_deref(),
        Some("session_real_server_owned")
    );

    crate::env::remove_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE");
}

#[test]
fn ancient_server_history_is_deferred_via_client_side_release_check() {
    // Issue #295: a server old enough to predate the self-reported staleness
    // machinery sends `server_has_update: None`, so it can never tell the client
    // it is stale. The client must independently compare release versions and
    // defer + reload anyway, instead of attaching to the ancient daemon (which
    // would then reject newer protocol requests like `set_route`).
    let _env_guard = crate::storage::lock_test_env();
    crate::env::remove_var("JCODE_ALLOW_SERVER_VERSION_MISMATCH");
    // The test binary's own version is dev/dirty (unorderable), so use the
    // test-only override to give the client a clean release version newer than
    // the simulated ancient server.
    crate::env::set_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE", "v0.17.0 (d741696f)");

    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.remote_session_id = Some("session_existing".to_string());
    app.connection_type = Some("websocket".to_string());

    let redraw = app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: "session_from_ancient_server".to_string(),
            messages: vec![crate::protocol::HistoryMessage {
                role: "assistant".to_string(),
                content: "ancient answer".to_string(),
                tool_calls: None,
                tool_data: None,
            }],
            images: vec![],
            provider_name: Some("ancient-provider".to_string()),
            provider_model: Some("ancient-model".to_string()),
            exact_runtime_identity: None,
            subagent_model: Some("ancient-subagent".to_string()),
            autoreview_enabled: Some(true),
            autojudge_enabled: Some(true),
            available_models: vec!["ancient-model".to_string()],
            available_model_routes: vec![],
            mcp_servers: vec!["ancient-mcp:1".to_string()],
            skills: vec!["ancient-skill".to_string()],
            total_tokens: Some((99, 100)),
            token_usage_totals: None,
            all_sessions: vec!["session_from_ancient_server".to_string()],
            client_count: Some(42),
            is_canary: Some(false),
            reload_recovery: None,
            // Clean older release, and crucially server_has_update is None: the
            // ancient daemon does not know how to self-assess.
            server_version: Some("v0.14.2 (38452185)".to_string()),
            server_name: Some("ancient-server".to_string()),
            server_icon: Some("🦖".to_string()),
            server_has_update: None,
            was_interrupted: None,
            connection_type: Some("ancient-connection".to_string()),
            status_detail: Some("ancient-status".to_string()),
            upstream_provider: Some("ancient-upstream".to_string()),
            resolved_credential: None,
            reasoning_effort: Some("high".to_string()),
            service_tier: Some("ancient-tier".to_string()),
            compaction_mode: crate::config::CompactionMode::Reactive,
            activity: None,
            side_panel: crate::side_panel::SidePanelSnapshot::default(),
        },
        &mut remote,
    );

    crate::env::remove_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE");

    assert!(!redraw);
    assert!(app.pending_server_reload);
    // Remote session state must NOT have been applied from the ancient server.
    assert_eq!(app.remote_session_id.as_deref(), Some("session_existing"));
    assert_eq!(remote.session_id(), None);
    assert!(app.remote_skills.is_empty());
    assert!(app.remote_sessions.is_empty());
    assert_ne!(
        app.session.subagent_model.as_deref(),
        Some("ancient-subagent")
    );
    let content = app.display_messages().last().unwrap().content.clone();
    assert!(
        content.contains("older release") && content.contains("jcode server stop"),
        "{content}"
    );
}

#[test]
fn older_server_reporting_no_update_is_still_deferred_via_client_check() {
    // The "current client, stale server" report: the daemon self-reports
    // `server_has_update: Some(false)` (its own shared-server channel still
    // points at its old binary, so locally it sees nothing newer), but the
    // client can PROVE it is an older release. Before this fix, Some(false)
    // short-circuited and the client trusted the old server forever. Now the
    // client's release-order check wins: defer + reload (after repairing the
    // shared-server channel client-side).
    let _env_guard = crate::storage::lock_test_env();
    crate::env::remove_var("JCODE_ALLOW_SERVER_VERSION_MISMATCH");
    crate::env::set_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE", "v0.22.0 (abcd1234)");

    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.remote_session_id = Some("session_existing".to_string());

    let redraw = app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: "session_from_old_server".to_string(),
            messages: vec![],
            images: vec![],
            provider_name: Some("p".to_string()),
            provider_model: Some("m".to_string()),
            exact_runtime_identity: None,
            subagent_model: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            available_models: vec!["m".to_string()],
            available_model_routes: vec![],
            mcp_servers: vec![],
            skills: vec![],
            total_tokens: None,
            token_usage_totals: None,
            all_sessions: vec![],
            client_count: Some(1),
            is_canary: Some(false),
            reload_recovery: None,
            // Older clean release than the client, but the daemon insists it has
            // no newer binary to reload into.
            server_version: Some("v0.14.6 (deadbeef)".to_string()),
            server_name: Some("old-server".to_string()),
            server_icon: Some("🕰".to_string()),
            server_has_update: Some(false),
            was_interrupted: None,
            connection_type: Some("websocket".to_string()),
            status_detail: None,
            upstream_provider: None,
            resolved_credential: None,
            reasoning_effort: None,
            service_tier: None,
            compaction_mode: crate::config::CompactionMode::Reactive,
            activity: None,
            side_panel: crate::side_panel::SidePanelSnapshot::default(),
        },
        &mut remote,
    );

    crate::env::remove_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE");

    assert!(!redraw);
    assert!(
        app.pending_server_reload,
        "client-proven-older server must defer + reload even when it reports Some(false)"
    );
    assert_eq!(app.remote_server_has_update, Some(false));
    // Remote session state must NOT have been applied from the old server.
    assert_eq!(app.remote_session_id.as_deref(), Some("session_existing"));
    assert_eq!(remote.session_id(), None);
    let content = app.display_messages().last().unwrap().content.clone();
    assert!(
        content.contains("older release") && content.contains("jcode server stop"),
        "{content}"
    );
}

#[test]
fn older_server_history_repairs_stale_shared_server_channel_end_to_end() {
    // Full-path sandbox: a real temp JCODE_HOME set up in the exact field state
    // (shared-server pinned to an OLD build, stable advanced to a NEW release by
    // a previous install). When the current client attaches to a server that
    // self-reports an older release with `server_has_update: Some(false)`, the
    // production History handler must repair the shared-server channel so the
    // forced reload it queues has a strictly-newer binary to exec into.
    use std::time::{Duration, SystemTime};
    let _env_guard = crate::storage::lock_test_env();
    crate::env::remove_var("JCODE_ALLOW_SERVER_VERSION_MISMATCH");
    crate::env::set_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE", "v0.22.0 (abcd1234)");
    let temp = tempfile::TempDir::new().expect("temp home");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    // Build the field state: shared-server -> OLD, stable -> NEW (newer mtime).
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let write_version = |version: &str, mtime: SystemTime| {
        let dir = crate::build::builds_dir()
            .unwrap()
            .join("versions")
            .join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(crate::build::binary_name());
        std::fs::write(&path, format!("bin {version}")).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    };
    let old = "0.14.6";
    let new = "0.22.0";
    write_version(old, base);
    write_version(new, base + Duration::from_secs(60));
    crate::build::update_shared_server_symlink(old).expect("pin shared-server old");
    crate::build::update_stable_symlink(new).expect("stable new");

    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    app.is_remote = true;
    app.remote_session_id = Some("session_existing".to_string());

    let _redraw = app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: "session_from_old_server".to_string(),
            messages: vec![],
            images: vec![],
            provider_name: Some("p".to_string()),
            provider_model: Some("m".to_string()),
            exact_runtime_identity: None,
            subagent_model: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            available_models: vec!["m".to_string()],
            available_model_routes: vec![],
            mcp_servers: vec![],
            skills: vec![],
            total_tokens: None,
            token_usage_totals: None,
            all_sessions: vec![],
            client_count: Some(1),
            is_canary: Some(false),
            reload_recovery: None,
            server_version: Some("v0.14.6 (deadbeef)".to_string()),
            server_name: Some("old-server".to_string()),
            server_icon: Some("🕰".to_string()),
            server_has_update: Some(false),
            was_interrupted: None,
            connection_type: Some("websocket".to_string()),
            status_detail: None,
            upstream_provider: None,
            resolved_credential: None,
            reasoning_effort: None,
            service_tier: None,
            compaction_mode: crate::config::CompactionMode::Reactive,
            activity: None,
            side_panel: crate::side_panel::SidePanelSnapshot::default(),
        },
        &mut remote,
    );

    let repaired = crate::build::read_shared_server_version().ok().flatten();
    let pending = app.pending_server_reload;

    // Restore env before asserting so a panic cannot leak global state.
    crate::env::remove_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE");
    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }

    assert!(pending, "older server must queue a reload");
    assert_eq!(
        repaired.as_deref(),
        Some(new),
        "the History handler must repair the stale shared-server channel to the newer stable \
         release so the queued reload upgrades the server instead of re-execing the old binary"
    );
}

#[test]
fn current_release_server_history_is_not_deferred_by_client_check() {
    // A server on the SAME or NEWER clean release as the client, with
    // server_has_update: None, must be trusted and attached normally. This
    // guards against the client-side check over-firing and looping reloads.
    let _env_guard = crate::storage::lock_test_env();
    crate::env::remove_var("JCODE_ALLOW_SERVER_VERSION_MISMATCH");
    crate::env::set_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE", "v0.17.0 (d741696f)");

    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.remote_session_id = Some("session_existing".to_string());

    let redraw = app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: "session_current".to_string(),
            messages: vec![],
            images: vec![],
            provider_name: Some("p".to_string()),
            provider_model: Some("m".to_string()),
            exact_runtime_identity: None,
            subagent_model: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            available_models: vec!["m".to_string()],
            available_model_routes: vec![],
            mcp_servers: vec![],
            skills: vec![],
            total_tokens: None,
            token_usage_totals: None,
            all_sessions: vec!["session_current".to_string()],
            client_count: Some(1),
            is_canary: Some(false),
            reload_recovery: None,
            server_version: Some("v0.17.0 (d741696f)".to_string()),
            server_name: Some("current-server".to_string()),
            server_icon: Some("🟢".to_string()),
            server_has_update: None,
            was_interrupted: None,
            connection_type: Some("websocket".to_string()),
            status_detail: None,
            upstream_provider: None,
            resolved_credential: None,
            reasoning_effort: None,
            service_tier: None,
            compaction_mode: crate::config::CompactionMode::Reactive,
            activity: None,
            side_panel: crate::side_panel::SidePanelSnapshot::default(),
        },
        &mut remote,
    );

    crate::env::remove_var("JCODE_TEST_CLIENT_VERSION_OVERRIDE");

    // Attached normally: session id applied, no pending reload triggered by the
    // client-side staleness check. (The History arm always returns false for
    // redraw; the meaningful signal is that state was actually applied.)
    let _ = redraw;
    assert!(!app.pending_server_reload);
    assert_eq!(app.remote_session_id.as_deref(), Some("session_current"));
}

#[test]
fn remote_done_finalizes_resumed_activity_without_current_message_id() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.is_processing = true;
    app.status = ProcessingStatus::RunningTool("bg".to_string());
    app.remote_resume_activity = Some(RemoteResumeActivity {
        session_id: "session_resume_cache_stats".to_string(),
        observed_at: Instant::now(),
        current_tool_name: Some("bg".to_string()),
    });
    app.streaming.streaming_input_tokens = 63_762;
    app.streaming.streaming_output_tokens = 153;
    app.streaming.streaming_cache_read_tokens = Some(0);
    app.stream_message_ended = true;

    app.handle_server_event(crate::protocol::ServerEvent::Done { id: 99 }, &mut remote);

    assert!(!app.is_processing);
    assert!(matches!(app.status, ProcessingStatus::Idle));
    assert!(app.remote_resume_activity.is_none());
    assert!(app.last_api_completed.is_some());
}

#[test]
fn oversized_pasted_submit_is_rejected_and_preserves_input() {
    let mut app = create_test_app();
    let pasted = format!(
        "{}tail",
        "x\n".repeat(crate::tui::app::input::MAX_SUBMITTED_TEXT_BYTES / 2 + 1)
    );

    crate::tui::app::input::handle_text_paste(&mut app, pasted);
    let placeholder = app.input.clone();
    assert!(placeholder.starts_with("[pasted "));

    app.submit_input();

    assert!(
        !app.is_processing,
        "oversized input must not enter sending state"
    );
    assert_eq!(
        app.input, placeholder,
        "placeholder input should be preserved"
    );
    assert_eq!(
        app.pasted_contents.len(),
        1,
        "expanded paste should remain recoverable"
    );
    assert!(
        app.display_messages()
            .iter()
            .any(|message| message.role == "system"
                && message.content.contains("Message is too large to send"))
    );
}

fn seed_stale_clear_usage(app: &mut App) {
    app.streaming.streaming_input_tokens = 40_000;
    app.streaming.streaming_output_tokens = 2_000;
    app.streaming.streaming_cache_read_tokens = Some(30_000);
    app.streaming.streaming_cache_creation_tokens = Some(5_000);
    app.streaming.streaming_context_stale = true;
    app.streaming.streaming_usage_call_reset_pending = true;
    app.kv_cache.current_api_usage_recorded = true;
}

fn assert_clear_usage_reset(app: &App) {
    assert_eq!(app.current_stream_context_tokens(), None);
    assert_eq!(app.streaming.streaming_input_tokens, 0);
    assert_eq!(app.streaming.streaming_output_tokens, 0);
    assert_eq!(app.streaming.streaming_cache_read_tokens, None);
    assert_eq!(app.streaming.streaming_cache_creation_tokens, None);
    assert!(!app.streaming.streaming_context_stale);
    assert!(!app.streaming.streaming_usage_call_reset_pending);
    assert!(!app.kv_cache.current_api_usage_recorded);
}

#[test]
fn local_clear_resets_provider_reported_context_usage() {
    let mut app = create_test_app();
    seed_stale_clear_usage(&mut app);

    assert!(super::commands::handle_session_command(&mut app, "/clear"));

    assert_clear_usage_reset(&app);
}

#[test]
fn remote_clear_resets_provider_reported_context_usage() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();
    app.is_remote = true;
    seed_stale_clear_usage(&mut app);
    app.input = "/clear".to_string();
    app.cursor_pos = app.input.len();

    rt.block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::empty(), &mut remote))
        .expect("remote /clear should succeed");

    assert_clear_usage_reset(&app);
}
