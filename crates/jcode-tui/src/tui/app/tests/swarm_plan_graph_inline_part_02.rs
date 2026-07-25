/// registered diagram hash.
fn seed_rendered_plan_graph(
    app: &mut App,
    remote: &mut crate::tui::backend::RemoteConnection,
) -> u64 {
    crate::tui::mermaid::clear_active_diagrams();
    app.handle_server_event(
        swarm_plan_event(1, vec![swarm_plan_graph_item("haiku-1", "write a haiku")]),
        remote,
    );
    let plan_msg = app
        .display_messages()
        .iter()
        .rev()
        .find(|m| m.role == "swarm")
        .expect("plan graph message")
        .clone();
    let _ =
        crate::tui::ui::render_swarm_message(&plan_msg, 80, crate::config::DiffDisplayMode::Inline);
    assert_eq!(
        crate::tui::mermaid::active_diagram_count(),
        1,
        "seed: plan render registers exactly one active diagram"
    );
    assert!(
        !app.swarm_plan_items.is_empty(),
        "seed: plan snapshot applied"
    );
    crate::tui::mermaid::get_active_diagrams()[0].hash
}

/// Shared post-clear assertions: the plan-graph transcript message is gone,
/// but the ACTIVE_DIAGRAMS registry still holds the orphaned diagram (the
/// pinned pane keeps showing it) and the swarm plan snapshot fields survive
/// untouched.
fn assert_transcript_clear_leaks_diagram_and_plan_state(
    app: &mut App,
    stale_hash: u64,
    path: &str,
) {
    assert!(
        plan_graph_titles(app).is_empty(),
        "{path}: transcript wiped, no plan-graph message remains"
    );
    let diagrams = crate::tui::mermaid::get_active_diagrams();
    assert_eq!(
        diagrams.len(),
        1,
        "{path}: LEAK CONFIRMED - ACTIVE_DIAGRAMS still holds the cleared transcript's diagram"
    );
    assert_eq!(
        diagrams[0].hash, stale_hash,
        "{path}: the surviving entry is exactly the stale plan graph"
    );
    assert!(
        !app.swarm_plan_items.is_empty(),
        "{path}: STALE STATE CONFIRMED - swarm_plan_items persist after the transcript is wiped"
    );
    assert_eq!(
        app.swarm_plan_version,
        Some(1),
        "{path}: stale swarm_plan_version persists"
    );
    assert_eq!(
        app.swarm_plan_swarm_id.as_deref(),
        Some("test-swarm"),
        "{path}: stale swarm_plan_swarm_id persists"
    );
    // The pinned pane still anchors on the orphaned diagram even though no
    // transcript message backs it anymore.
    app.diagram_index = 0;
    app.sync_diagram_fit_context();
    assert_eq!(
        app.last_visible_diagram_hash,
        Some(stale_hash),
        "{path}: pinned pane still shows the orphaned plan graph"
    );
}

/// Shared post-clear assertions for the FULL-DISCARD paths: the transcript
/// and the diagram registry are both wiped (no orphaned diagram can be shown
/// by the pinned pane or the Margin info widget), while the swarm plan
/// snapshot fields still survive (a separate pinned staleness).
fn assert_full_discard_clears_diagrams_but_keeps_plan_state(app: &mut App, path: &str) {
    assert!(
        plan_graph_titles(app).is_empty(),
        "{path}: transcript wiped, no plan-graph message remains"
    );
    assert!(
        crate::tui::mermaid::get_active_diagrams().is_empty(),
        "{path}: FIX - full transcript discard re-scopes ACTIVE_DIAGRAMS \
         (no orphaned diagram survives)"
    );
    assert!(
        !app.swarm_plan_items.is_empty(),
        "{path}: STALE STATE (still pinned) - swarm_plan_items persist after the discard"
    );
    // With an empty registry the pinned pane has nothing to anchor on.
    app.diagram_index = 0;
    app.sync_diagram_fit_context();
    assert_eq!(
        app.last_visible_diagram_hash, None,
        "{path}: pinned pane no longer anchors on a discarded diagram"
    );
}

/// Path 1: local `/clear` (commands.rs -> reset_current_session at
/// commands_review.rs). A full transcript discard: it now also clears the
/// process-global ACTIVE_DIAGRAMS registry, so neither the pinned pane nor
/// the Margin info widget can keep showing a diagram from the old
/// transcript. The swarm plan snapshot fields remain stale (separate pin).
#[test]
fn test_local_clear_command_clears_active_diagrams_but_keeps_swarm_plan_state() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::pinned();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    let _stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);

    assert!(super::commands::handle_session_command(&mut app, "/clear"));

    assert_full_discard_clears_diagrams_but_keeps_plan_state(
        &mut app,
        "local /clear (reset_current_session)",
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Margin-mode counterpart of path 1, the exact user-visible bug the fix
/// targets: immediately after a local `/clear`, the Margin info widget
/// (which draws `get_active_diagrams()[0]`, info_widget.rs) must no longer
/// list a diagram from the discarded transcript.
#[test]
fn test_local_clear_command_empties_margin_info_widget_diagram_list() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::margin();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Margin;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    let stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);
    assert_eq!(
        crate::tui::TuiState::info_widget_data(&app)
            .diagrams
            .first()
            .map(|d| d.hash),
        Some(stale_hash),
        "seed: the margin widget lists the rendered plan graph"
    );

    assert!(super::commands::handle_session_command(&mut app, "/clear"));

    assert!(
        crate::tui::TuiState::info_widget_data(&app).diagrams.is_empty(),
        "FIX: after local /clear the Margin info widget lists no diagram \
         from the discarded transcript"
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Path 2: local `/rewind N` (commands.rs ~2004) and `/rewind undo`
/// (commands.rs ~1933). Both rebuild the transcript via
/// `clear_display_messages` + re-render; neither unregisters the plan-graph
/// diagram nor resets the swarm plan snapshot. This survival is DELIBERATE
/// (see the comments at the /rewind handlers): retained/restored messages
/// are served from body-cache prefix reuse without re-rendering, so their
/// diagrams would never re-register if the registry were cleared.
#[test]
fn test_local_rewind_and_undo_leave_stale_active_diagram_and_swarm_plan_state() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::pinned();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    // `/rewind N` needs rewindable stored messages.
    app.session.replace_messages(Vec::new());
    for idx in 1..=2 {
        let text = format!("msg-{idx}");
        app.add_provider_message(Message::user(&text));
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
        );
    }

    let stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);

    // Truncating rewind (commands.rs ~2004).
    assert!(super::commands::handle_session_command(
        &mut app,
        "/rewind 1"
    ));
    assert_transcript_clear_leaks_diagram_and_plan_state(&mut app, stale_hash, "local /rewind N");

    // Rewind undo (commands.rs ~1933) restores the transcript from the
    // snapshot; the plan-graph display message was never stored, so it does
    // not come back, but the diagram and plan state stay stale.
    assert!(super::commands::handle_session_command(
        &mut app,
        "/rewind undo"
    ));
    assert_transcript_clear_leaks_diagram_and_plan_state(
        &mut app,
        stale_hash,
        "local /rewind undo",
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Path 3: local session recovery (`recover_session_without_tools`,
/// conversation_state.rs ~809). It rebuilds the session into a fresh one but
/// KEEPS every text block, so the registry deliberately survives (the
/// retained messages' diagrams stay backed); the plan state stays stale the
/// same way.
#[test]
fn test_recover_session_without_tools_leaves_stale_active_diagram_and_swarm_plan_state() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::pinned();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    let stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);

    app.recover_session_without_tools();

    assert_transcript_clear_leaks_diagram_and_plan_state(
        &mut app,
        stale_hash,
        "local Ctrl+R recovery (recover_session_without_tools)",
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Path 4: remote `/clear` (remote/key_handling.rs). A full transcript
/// discard like path 1: the server session is cleared, so the registry is
/// re-scoped too. The swarm plan snapshot fields remain stale (unlike the
/// session-changing History event, which resets them).
#[test]
fn test_remote_clear_command_clears_active_diagrams_but_keeps_swarm_plan_state() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::pinned();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    app.is_remote = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    let _stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);

    app.input = "/clear".to_string();
    app.cursor_pos = app.input.len();
    rt.block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::empty(), &mut remote))
        .expect("remote /clear should succeed");
    assert_eq!(
        crate::tui::TuiState::status_notice(&app).as_deref(),
        Some("Session cleared"),
        "remote /clear path executed"
    );

    assert_full_discard_clears_diagrams_but_keeps_plan_state(&mut app, "remote /clear");

    crate::tui::mermaid::clear_active_diagrams();
}

/// Path 5: disconnected Ctrl+L (remote.rs ~1670) clears the display
/// transcript and queued messages, again without touching the diagram
/// registry or the swarm plan snapshot. (The connected-remote Ctrl+L branch
/// at key_handling.rs ~611 and the local Ctrl+L branch at input.rs ~1922 are
/// deliberate no-ops, so the disconnected handler is the only Ctrl+L
/// transcript clear.)
#[test]
fn test_disconnected_ctrl_l_clear_leaves_stale_active_diagram_and_swarm_plan_state() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::pinned();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    let stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);
    app.queued_messages.push("queued".to_string());

    super::remote::handle_disconnected_key(&mut app, KeyCode::Char('l'), KeyModifiers::CONTROL)
        .expect("disconnected Ctrl+L should succeed");
    assert!(
        app.queued_messages.is_empty(),
        "disconnected Ctrl+L clears queued messages (proves the clear branch ran)"
    );

    assert_transcript_clear_leaks_diagram_and_plan_state(
        &mut app,
        stale_hash,
        "disconnected Ctrl+L",
    );

    crate::tui::mermaid::clear_active_diagrams();
}

// ---------------------------------------------------------------------------
// wiring-audit.margin-streaming-preview-verify.margin-stale-entries: Margin
// mode reads the SAME process-global ACTIVE_DIAGRAMS registry as the pinned
// pane, but through a different consumer: `info_widget_data` (tui_state.rs
// ~1456) copies `get_active_diagrams()` into `InfoWidgetData.diagrams` only
// when `self.diagram_mode == DiagramDisplayMode::Margin`, and the margin
// widget renders `data.diagrams[0]` only (info_widget.rs ~1361). The tests
// below pin (a) the mode gate, (b) that plan-graph version bumps accumulate
// the same stale entries in the Margin list as in the pinned pane, and
// (c) Margin-mode selection semantics: `diagram_index` is force-reset and
// keyboard cycling is unreachable, so the widget always shows the newest
// diagram regardless of any previously parked selection.
// ---------------------------------------------------------------------------

/// Mode gate: `info_widget_data().diagrams` is populated from the global
/// registry ONLY in Margin mode (tui_state.rs:1456-1460); Pinned mode (which
/// uses the dedicated pane) gets an empty list.
#[test]
fn test_info_widget_diagram_list_populated_only_in_margin_mode() {
    let _render_lock = scroll_render_test_lock();
    let mut app = create_test_app();

    crate::tui::mermaid::clear_active_diagrams();
    crate::tui::mermaid::register_active_diagram(0xA1, 100, 80, None);
    crate::tui::mermaid::register_active_diagram(0xA2, 120, 90, None);

    app.diagram_mode = crate::config::DiagramDisplayMode::Margin;
    let margin_data = crate::tui::TuiState::info_widget_data(&app);
    assert_eq!(
        margin_data
            .diagrams
            .iter()
            .map(|d| d.hash)
            .collect::<Vec<_>>(),
        vec![0xA2, 0xA1],
        "Margin mode copies the registry (newest-first) into the info widget"
    );

    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    let pinned_data = crate::tui::TuiState::info_widget_data(&app);
    assert!(
        pinned_data.diagrams.is_empty(),
        "Pinned mode must NOT feed the margin info widget (dedicated pane instead)"
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Stale accumulation reproduces in Margin mode: an in-place plan-graph
/// version bump with changed content registers a second content hash and the
/// Margin info-widget list keeps BOTH versions (nothing unregisters the old
/// one). The margin widget itself renders `diagrams[0]`, so the panel shows
/// the fresh version, but the stale entry inflates the list exactly as in
/// the pinned pane (see test_upsert_in_place_plan_bump_accumulates_stale_active_diagrams).
#[test]
fn test_margin_mode_plan_bump_accumulates_stale_diagram_in_info_widget_list() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::margin();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Margin;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    crate::tui::mermaid::clear_active_diagrams();

    // v1: task running. Render through the real swarm-message markdown path;
    // in Margin mode `mermaid_should_register_active()` is true (only None
    // opts out, jcode-tui-markdown/src/lib.rs mermaid_should_register_active),
    // so the diagram registers like a transcript render would.
    app.handle_server_event(
        swarm_plan_event(1, vec![swarm_plan_graph_item("haiku-1", "write a haiku")]),
        &mut remote,
    );
    let v1_msg = app
        .display_messages()
        .iter()
        .rev()
        .find(|m| m.role == "swarm")
        .expect("plan graph message")
        .clone();
    let _ =
        crate::tui::ui::render_swarm_message(&v1_msg, 80, crate::config::DiffDisplayMode::Inline);
    let v1_list = crate::tui::TuiState::info_widget_data(&app).diagrams;
    assert_eq!(
        v1_list.len(),
        1,
        "Margin mode: first plan render lands in the info-widget diagram list"
    );
    let v1_hash = v1_list[0].hash;

    // v2: status flip changes the graph content; the transcript message is
    // replaced in place, but the registry gains a second entry.
    let mut done = swarm_plan_graph_item("haiku-1", "write a haiku");
    done.status = "completed".to_string();
    app.handle_server_event(swarm_plan_event(2, vec![done.clone()]), &mut remote);
    assert_eq!(
        plan_graph_titles(&app),
        vec!["Plan graph · v2".to_string()],
        "upsert keeps a single transcript plan-graph message"
    );
    let v2_msg = app
        .display_messages()
        .iter()
        .rev()
        .find(|m| m.role == "swarm")
        .expect("plan graph message")
        .clone();
    assert_ne!(
        v1_msg.content, v2_msg.content,
        "status flip changes graph source"
    );
    let _ =
        crate::tui::ui::render_swarm_message(&v2_msg, 80, crate::config::DiffDisplayMode::Inline);

    let diagrams = crate::tui::TuiState::info_widget_data(&app).diagrams;
    assert_eq!(
        diagrams.len(),
        2,
        "STALE ACCUMULATION CONFIRMED in Margin mode: the info-widget list \
         holds both plan-graph versions after an in-place bump"
    );
    assert_ne!(
        diagrams[0].hash, v1_hash,
        "newest-first: index 0 is the fresh v2 diagram (the one the margin \
         widget renders, info_widget.rs render_diagrams_widget)"
    );
    assert_eq!(
        diagrams[1].hash, v1_hash,
        "the replaced v1 diagram is still listed (stale)"
    );

    // Refinement (same as pinned): a version-only bump with identical items
    // produces identical mermaid source and does NOT add a third entry.
    app.handle_server_event(swarm_plan_event(3, vec![done]), &mut remote);
    let v3_msg = app
        .display_messages()
        .iter()
        .rev()
        .find(|m| m.role == "swarm")
        .expect("plan graph message")
        .clone();
    assert_eq!(
        v2_msg.content, v3_msg.content,
        "version-only bump keeps identical graph content"
    );
    let _ =
        crate::tui::ui::render_swarm_message(&v3_msg, 80, crate::config::DiffDisplayMode::Inline);
    assert_eq!(
        crate::tui::TuiState::info_widget_data(&app).diagrams.len(),
        2,
        "accumulation is per distinct graph content, not per version number"
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Margin-mode selection semantics: there is no per-diagram selection at all.
/// `diagram_available()` requires Pinned mode (navigation.rs:336-340), so
/// Ctrl+arrow cycling is unreachable; `normalize_diagram_state` force-resets
/// `diagram_index` to 0 in any non-Pinned mode (navigation.rs:342-349); and
/// the margin widget always renders `diagrams[0]` (info_widget.rs:1361). So
/// after the list changes, the "selection" is always the newest diagram:
/// a stale index can never be pointed at a stale entry in Margin mode.
#[test]
fn test_margin_mode_has_no_diagram_selection_and_always_shows_newest() {
    let _render_lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Margin;
    app.diagram_pane_enabled = true;

    crate::tui::mermaid::clear_active_diagrams();
    crate::tui::mermaid::register_active_diagram(0xB1, 100, 80, None);
    crate::tui::mermaid::register_active_diagram(0xB2, 100, 80, None);
    crate::tui::mermaid::register_active_diagram(0xB3, 100, 80, None);

    // Cycling is unreachable: diagram_available() is Pinned-only, so the
    // Ctrl-key handler refuses the cycle keys even with diagrams present.
    assert!(
        !app.diagram_available(),
        "Margin mode reports no cyclable diagram pane"
    );
    app.diagram_focus = true; // even with focus somehow set
    assert!(
        !app.handle_diagram_ctrl_key(KeyCode::Left, app.diagram_available()),
        "Ctrl+Left does not cycle in Margin mode"
    );
    assert!(
        !app.handle_diagram_ctrl_key(KeyCode::Right, app.diagram_available()),
        "Ctrl+Right does not cycle in Margin mode"
    );

    // A parked/stale index from a previous Pinned session is force-reset by
    // normalize_diagram_state's non-Pinned branch, so it can never select a
    // stale entry after the list changes.
    app.diagram_index = 2;
    app.diagram_scroll_x = 5;
    app.diagram_scroll_y = 7;
    app.normalize_diagram_state();
    assert_eq!(
        app.diagram_index, 0,
        "non-Pinned normalize resets the index"
    );
    assert!(
        !app.diagram_focus,
        "non-Pinned normalize drops diagram focus"
    );
    assert_eq!(app.diagram_scroll_x, 0);
    assert_eq!(app.diagram_scroll_y, 0);
    assert_eq!(
        app.last_visible_diagram_hash, None,
        "no visible-diagram anchor is tracked in Margin mode"
    );

    // The widget input is newest-first, and the margin renderer draws only
    // element 0, so a new registration immediately becomes the shown diagram.
    let before = crate::tui::TuiState::info_widget_data(&app).diagrams;
    assert_eq!(before[0].hash, 0xB3, "newest diagram is the rendered one");
    crate::tui::mermaid::register_active_diagram(0xB4, 100, 80, None);
    let after = crate::tui::TuiState::info_widget_data(&app).diagrams;
    assert_eq!(
        after.iter().map(|d| d.hash).collect::<Vec<_>>(),
        vec![0xB4, 0xB3, 0xB2, 0xB1],
        "stale entries stay listed behind the newest one"
    );
    assert_eq!(
        after[0].hash, 0xB4,
        "the margin widget switches to the new diagram (index 0) with no \
         selection to go stale"
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Margin-mode counterpart of the transcript-clear leak: after a session
/// switch removes the plan-graph transcript message, the Margin info widget
/// STILL lists (and therefore renders) the orphaned diagram, because nothing
/// re-scopes ACTIVE_DIAGRAMS (mermaid_active.rs) on session change.
#[test]
fn test_margin_mode_session_switch_keeps_orphaned_diagram_in_info_widget() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::margin();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Margin;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    app.remote_session_id = Some("session_old".to_string());
    let stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);

    app.handle_server_event(history_event_for_session("session_new"), &mut remote);
    assert!(
        plan_graph_titles(&app).is_empty(),
        "session switch removes the plan-graph transcript message"
    );

    let diagrams = crate::tui::TuiState::info_widget_data(&app).diagrams;
    assert_eq!(
        diagrams.len(),
        1,
        "LEAK CONFIRMED in Margin mode: the info widget still lists the \
         previous session's diagram"
    );
    assert_eq!(
        diagrams[0].hash, stale_hash,
        "the margin widget would render exactly the orphaned session-A plan graph"
    );

    crate::tui::mermaid::clear_active_diagrams();
}

// ---------------------------------------------------------------------------
// wiring-audit.compaction-rewind-clear-verify: transcript-REPLACEMENT paths
// that bypass `clear_display_messages` entirely.
//
//   A. `apply_compacted_history_window` (state_ui_messages.rs:404) assigns
//      `self.display_messages = messages` wholesale. The window is built from
//      server-side session storage, which never contains the client-only
//      "Plan graph · vN" message, so the coalesced diagram message is DROPPED
//      from the transcript while ACTIVE_DIAGRAMS and the swarm_plan_* snapshot
//      leak (the function touches neither).
//   B. Server-driven remote `/rewind N` / `/rewind undo`: `remote.rewind()`
//      (backend.rs:608) flips `has_loaded_history=false` and the server
//      responds with a fresh History payload for the SAME session id, so
//      `session_changed` (server_events.rs:1585) is FALSE and the plan-state
//      clearing block (server_events.rs:1637-1639) never runs. The transcript
//      is replaced via `replace_display_messages` (dropping the plan graph),
//      while ACTIVE_DIAGRAMS and swarm_plan_* leak.
//   C. Local (non-remote) session picker `/resume` current-terminal switch:
//      `handle_session_picker_current_terminal_selection`
//      (inline_interactive.rs:2128) only queues the target on
//      `workspace_client.queue_resume_session`; the queued resume is consumed
//      exclusively by remote::handle_tick (app/remote.rs:136). local::handle_tick
//      (app/local.rs:63-118) never takes it, so in local mode the switch is a
//      silent no-op: no transcript clear ever happens and the plan graph,
//      plan snapshot, and registered diagram all persist trivially.
// ---------------------------------------------------------------------------

/// Path A: a CompactedHistory window replaces the transcript wholesale
/// (`apply_compacted_history_window` bypasses `clear_display_messages`). The
/// server-built window cannot contain the client-only plan-graph message, so
/// the coalesced "Plan graph · vN" message is dropped, while the process-global
/// ACTIVE_DIAGRAMS entry and the swarm plan snapshot fields survive untouched.
#[test]
fn test_compacted_history_window_drops_plan_graph_but_leaks_diagram_and_plan_state() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::pinned();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    // CompactedHistory is dropped for inactive sessions, so the app must be
    // attached to the same session the event names (server_events.rs ~1968).
    app.remote_session_id = Some("session_same".to_string());
    let stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);

    app.handle_server_event(
        crate::protocol::ServerEvent::CompactedHistory {
            id: 42,
            session_id: "session_same".to_string(),
            messages: vec![
                user_history_message("older prompt 1"),
                user_history_message("older prompt 2"),
            ],
            images: vec![],
            compacted_total: 2,
            compacted_visible: 2,
            compacted_remaining: 0,
            compacted_hidden_prompts: 0,
        },
        &mut remote,
    );

    // The window landed: transcript is exactly the server-built message list.
    assert_eq!(
        app.display_messages().len(),
        2,
        "compacted window replaces the transcript wholesale"
    );
    assert_eq!(
        crate::tui::TuiState::status_notice(&app).as_deref(),
        Some("Loaded all 2 compacted messages"),
        "apply_compacted_history_window ran"
    );
    // The coalesced plan-graph message did NOT survive the replacement: the
    // window comes from server session storage, which never holds the
    // client-side "Plan graph · vN" display message.
    assert_transcript_clear_leaks_diagram_and_plan_state(
        &mut app,
        stale_hash,
        "CompactedHistory window (apply_compacted_history_window)",
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Path B: server-driven remote `/rewind N` and `/rewind undo`. Both flip
/// `has_loaded_history=false` (backend.rs:608/622) and the server answers
/// with a History payload for the SAME session, so `session_changed` is false
/// and the swarm-plan clearing block (server_events.rs:1637-1639) is skipped.
/// The truncated payload replaces the transcript (dropping the plan graph),
/// while ACTIVE_DIAGRAMS and swarm_plan_* leak.
#[test]
fn test_remote_rewind_history_response_is_not_session_changed_and_leaks_plan_state() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::pinned();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    app.is_remote = true;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();
    remote.set_session_id("session_same".to_string());
    app.remote_session_id = Some("session_same".to_string());

    // `/rewind N` requires at least one rewindable (user/assistant) display
    // message.
    app.push_display_message(DisplayMessage::user("hello"));
    let stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);

    // --- /rewind 1 ---------------------------------------------------------
    app.input = "/rewind 1".to_string();
    app.cursor_pos = app.input.len();
    rt.block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::empty(), &mut remote))
        .expect("remote /rewind should succeed");
    assert!(
        !remote.has_loaded_history(),
        "remote.rewind() must re-open the history gate so the server's \
         truncated History payload can replace the display state"
    );

    // The server responds with a History payload for the SAME session id
    // carrying the truncated message list.
    app.handle_server_event(
        history_event_for_session_with_messages(
            "session_same",
            vec![user_history_message("hello")],
        ),
        &mut remote,
    );
    assert!(
        app.display_messages()
            .iter()
            .any(|m| m.content.starts_with("✓ Rewound to message 1")),
        "rewind notice confirms the rewind History path executed"
    );
    // session_changed was false, so the plan-clearing block was skipped.
    assert_transcript_clear_leaks_diagram_and_plan_state(
        &mut app,
        stale_hash,
        "remote /rewind N (same-session History response)",
    );

    // --- /rewind undo ------------------------------------------------------
    app.input = "/rewind undo".to_string();
    app.cursor_pos = app.input.len();
    rt.block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::empty(), &mut remote))
        .expect("remote /rewind undo should succeed");
    assert!(
        !remote.has_loaded_history(),
        "remote.rewind_undo() also re-opens the history gate"
    );
    app.handle_server_event(
        history_event_for_session_with_messages(
            "session_same",
            vec![
                user_history_message("hello"),
                user_history_message("restored"),
            ],
        ),
        &mut remote,
    );
    assert!(
        app.display_messages()
            .iter()
            .any(|m| m.content.starts_with("✓ Undid rewind")),
        "undo notice confirms the rewind-undo History path executed"
    );
    assert_transcript_clear_leaks_diagram_and_plan_state(
        &mut app,
        stale_hash,
        "remote /rewind undo (same-session History response)",
    );

    crate::tui::mermaid::clear_active_diagrams();
}

/// Path C: local (non-remote) session picker current-terminal switch. Enter on
/// a session queues the target via `workspace_client.queue_resume_session`
/// (inline_interactive.rs:2128), but only remote::handle_tick (app/remote.rs:136)
/// ever consumes that queue; local::handle_tick (app/local.rs:63-118) does not.
/// So in local mode the "switch" never happens: no transcript clear, no
/// History event, and the plan graph message, swarm plan snapshot, and
/// registered diagram all persist. The stale state here is not a clear-path
/// leak but the queued switch silently never executing.
#[test]
fn test_local_session_picker_switch_is_never_consumed_and_keeps_plan_graph_state() {
    let _render_lock = scroll_render_test_lock();
    let _mode_guard = DiagramModeOverrideGuard::pinned();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    assert!(!app.is_remote, "this pins the local (non-remote) mode path");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    let stale_hash = seed_rendered_plan_graph(&mut app, &mut remote);

    // Open a picker with one target and select it in the current terminal.
    app.session_picker_mode = SessionPickerMode::Resume;
    app.session_picker_overlay = Some(RefCell::new(
        crate::tui::session_picker::SessionPicker::new(vec![
            crate::tui::session_picker::SessionInfo {
                id: "session_target_456".to_string(),
                parent_id: None,
                short_name: "target".to_string(),
                icon: "t".to_string(),
                title: "Target".to_string(),
                message_count: 1,
                user_message_count: 1,
                assistant_message_count: 0,
                created_at: chrono::Utc::now(),
                last_message_time: chrono::Utc::now(),
                last_active_at: None,
                working_dir: None,
                model: None,
                provider_key: None,
                is_canary: false,
                is_debug: false,
                saved: false,
                save_label: None,
                status: crate::session::SessionStatus::Closed,
                needs_catchup: false,
                estimated_tokens: 0,
                first_user_prompt: None,
                messages_preview: Vec::new(),
                search_index: "target".to_string(),
                server_name: None,
                server_icon: None,
                source: crate::tui::session_picker::SessionSource::Jcode,
                resume_target: crate::tui::session_picker::ResumeTarget::JcodeSession {
                    session_id: "session_target_456".to_string(),
                },
                external_path: None,
            },
        ]),
    ));
    app.handle_session_picker_key(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::empty(),
    )
    .expect("session picker enter should succeed");
    assert!(
        app.session_picker_overlay.is_none(),
        "picker closes and reports 'Switching → …'"
    );

    // The local tick loop never consumes the queued resume (only
    // remote::handle_tick at app/remote.rs:136 does), so nothing switches.
    let _ = crate::tui::app::local::handle_tick(&mut app);
    let _ = crate::tui::app::local::handle_tick(&mut app);
    assert_eq!(
        app.workspace_client
            .take_pending_resume_session()
            .as_deref(),
        Some("session_target_456"),
        "CONFIRMED: local ticks leave the queued switch unconsumed \
         (session switching is remote-only)"
    );

    // Because no switch (and therefore no transcript clear or History event)
    // ever happens locally, the plan graph message, plan snapshot, and
    // registered diagram all remain in place.
    assert_eq!(
        plan_graph_titles(&app),
        vec!["Plan graph · v1".to_string()],
        "no transcript clear happened, the plan graph message persists"
    );
    assert!(!app.swarm_plan_items.is_empty());
    assert_eq!(app.swarm_plan_version, Some(1));
    let diagrams = crate::tui::mermaid::get_active_diagrams();
    assert_eq!(diagrams.len(), 1);
    assert_eq!(diagrams[0].hash, stale_hash);

    crate::tui::mermaid::clear_active_diagrams();
}

// ---------------------------------------------------------------------------
// wiring-audit.margin-streaming-preview-verify.local-paths-preview-leak:
// unlike ACTIVE_DIAGRAMS (whose survival across transcript clears is pinned
// above as a known leak), the ephemeral STREAMING_PREVIEW_DIAGRAM slot
// (mermaid_active.rs) must NOT survive local transcript mutations. The
// typed-command paths (/clear, /rewind) are already protected in practice
// because submit_input commits pending streaming text first
// (input.rs commit_pending_streaming_assistant_message -> take_streaming_text
// clears the slot), but direct dispatch must not rely on that: Ctrl+R
// (recover_session_without_tools) is reachable mid-stream from the turn.rs
// key loops with a live preview and no commit. These tests pin that all three
// local transcript-mutation paths clear the preview slot themselves.
// ---------------------------------------------------------------------------

/// Simulates a mid-stream mermaid preview exactly like the streaming
/// markdown renderer would create it (markdown_render_full.rs
/// set_streaming_preview_diagram on a complete fenced block).
fn seed_streaming_preview(app: &mut App, hash: u64) {
    crate::tui::mermaid::clear_active_diagrams();
    app.streaming.streaming_text = "```mermaid\ngraph TD; A-->B\n```".to_string();
    app.is_processing = true;
    crate::tui::mermaid::set_streaming_preview_diagram(hash, 320, 240, Some("preview".to_string()));
    assert_eq!(
        crate::tui::mermaid::get_active_diagrams()
            .first()
            .map(|d| d.hash),
        Some(hash),
        "seed: streaming preview occupies index 0 (what Margin mode draws)"
    );
}

fn assert_streaming_preview_cleared(hash: u64, path: &str) {
    assert!(
        !crate::tui::mermaid::get_active_diagrams()
            .iter()
            .any(|d| d.hash == hash),
        "{path}: streaming preview diagram must not survive the transcript mutation"
    );
}

/// Local `/clear` -> reset_current_session (commands_review.rs) now clears
/// the streaming render state, including the preview slot.
#[test]
fn test_local_clear_command_clears_streaming_preview_diagram() {
    let _render_lock = scroll_render_test_lock();
    let mut app = create_test_app();
    let hash: u64 = 0x0005_17EA_11ED_0001;
    seed_streaming_preview(&mut app, hash);

    assert!(super::commands::handle_session_command(&mut app, "/clear"));

    assert_streaming_preview_cleared(hash, "local /clear");
    assert!(
        app.streaming.streaming_text.is_empty(),
        "local /clear: in-flight streaming text is dropped with the transcript"
    );
    crate::tui::mermaid::clear_active_diagrams();
}

/// Local `/rewind N` and `/rewind undo` (commands.rs) rebuild the transcript;
/// both must drop the streaming preview slot.
#[test]
fn test_local_rewind_and_undo_clear_streaming_preview_diagram() {
    let _render_lock = scroll_render_test_lock();
    let mut app = create_test_app();

    app.session.replace_messages(Vec::new());
    for idx in 1..=2 {
        let text = format!("msg-{idx}");
        app.add_provider_message(Message::user(&text));
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
        );
    }

    let hash: u64 = 0x0005_17EA_11ED_0002;
    seed_streaming_preview(&mut app, hash);
    assert!(super::commands::handle_session_command(
        &mut app,
        "/rewind 1"
    ));
    assert_streaming_preview_cleared(hash, "local /rewind N");

    seed_streaming_preview(&mut app, hash);
    assert!(super::commands::handle_session_command(
        &mut app,
        "/rewind undo"
    ));
    assert_streaming_preview_cleared(hash, "local /rewind undo");
    crate::tui::mermaid::clear_active_diagrams();
}

/// Ctrl+R recovery (recover_session_without_tools, conversation_state.rs) is
/// reachable mid-stream from the turn.rs key loops with a live preview and no
/// prior commit, so it must clear the preview slot itself.
#[test]
fn test_recover_session_without_tools_clears_streaming_preview_diagram() {
    let _render_lock = scroll_render_test_lock();
    let mut app = create_test_app();
    let hash: u64 = 0x0005_17EA_11ED_0003;
    seed_streaming_preview(&mut app, hash);

    app.recover_session_without_tools();

    assert_streaming_preview_cleared(hash, "local Ctrl+R recovery");
    assert!(
        app.streaming.streaming_text.is_empty(),
        "recovery: in-flight streaming text is dropped with the transcript"
    );
    crate::tui::mermaid::clear_active_diagrams();
}

/// `commit_pending_streaming_assistant_message` early-returns when the live
/// buffer is empty (tool-only boundary). The buffer can become empty *after*
/// a preview was rendered only via `replace_streaming_text` (remote
/// TextReplace, server_events.rs:644, and debug snapshot restore,
/// debug.rs:539), which does not touch the preview slot. The commit boundary
/// is the mirror point: an empty buffer means any surviving preview is stale,
/// so the early return must clear the slot instead of leaking it
/// (input.rs commit_pending_streaming_assistant_message).
#[test]
fn test_commit_with_emptied_stream_buffer_clears_streaming_preview_diagram() {
    let _render_lock = scroll_render_test_lock();
    let mut app = create_test_app();
    let hash: u64 = 0x0005_17EA_11ED_0004;
    seed_streaming_preview(&mut app, hash);

    // Simulate a TextReplace-style rewrite that drops the fenced block the
    // preview was rendered from, leaving the buffer empty while the preview
    // slot is still occupied.
    app.replace_streaming_text(String::new());
    assert_eq!(
        crate::tui::mermaid::get_active_diagrams()
            .first()
            .map(|d| d.hash),
        Some(hash),
        "precondition: replace_streaming_text alone leaves the preview live"
    );

    let committed = app.commit_pending_streaming_assistant_message();

    assert!(!committed, "empty buffer commits nothing");
    assert_streaming_preview_cleared(hash, "commit with emptied stream buffer");
    crate::tui::mermaid::clear_active_diagrams();
}
