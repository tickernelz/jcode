#[test]
fn test_side_diagram_uses_left_splitter_instead_of_rounded_box() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    app.diagram_pane_position = crate::config::DiagramPanePosition::Side;

    crate::tui::mermaid::clear_active_diagrams();
    crate::tui::mermaid::register_active_diagram(0x444, 900, 450, Some("side".to_string()));

    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let text = render_and_snap(&app, &mut terminal);

    let diagram_area = crate::tui::ui::last_layout_snapshot()
        .and_then(|layout| layout.diagram_area)
        .expect("expected side diagram area after render");
    let buf = terminal.backend().buffer();

    assert_eq!(buf[(diagram_area.x, diagram_area.y)].symbol(), "│");
    assert_eq!(buf[(diagram_area.x, diagram_area.y + 1)].symbol(), "│");
    assert!(text.contains("pinned 1/1"), "rendered text: {text}");

    crate::tui::mermaid::clear_active_diagrams();
}

#[test]
fn test_tool_side_panel_focus_supports_horizontal_pan_keys() {
    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "hello".to_string(),
            updated_at_ms: 1,
        }],
    };

    assert!(app.handle_diagram_ctrl_key(KeyCode::Char('l'), false));
    assert!(app.diff_pane_focus);

    app.handle_key(KeyCode::Right, KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.diff_pane_scroll_x, 4);
    assert!(app.input.is_empty());

    app.handle_key(KeyCode::Left, KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.diff_pane_scroll_x, 0);
}

#[test]
fn test_tool_side_panel_focus_supports_image_zoom_keys() {
    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "hello".to_string(),
            updated_at_ms: 1,
        }],
    };

    assert!(app.handle_diagram_ctrl_key(KeyCode::Char('l'), false));
    assert!(app.diff_pane_focus);

    app.handle_key(KeyCode::Char('+'), KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.side_panel_image_zoom_percent, 110);

    app.handle_key(KeyCode::Char('-'), KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.side_panel_image_zoom_percent, 100);

    app.handle_key(KeyCode::Char('+'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('0'), KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.side_panel_image_zoom_percent, 100);
}

#[test]
fn test_mouse_horizontal_scroll_over_tool_side_panel_pans_without_focus_change() {
    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.diff_pane_scroll_x = 0;
    app.diff_pane_focus = false;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "hello".to_string(),
            updated_at_ms: 1,
        }],
    };

    crate::tui::ui::record_layout_snapshot(
        Rect::new(0, 0, 40, 20),
        None,
        Some(Rect::new(40, 0, 20, 20)),
        None,
    );

    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollRight,
        column: 45,
        row: 5,
        modifiers: KeyModifiers::empty(),
    });

    assert!(
        !scroll_only,
        "side-panel horizontal pan should request an immediate redraw"
    );
    assert_eq!(app.diff_pane_scroll_x, 1);
    assert!(!app.diff_pane_focus);
}

#[test]
fn test_ctrl_mouse_scroll_over_tool_side_panel_zooms_images() {
    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.side_panel_image_zoom_percent = 100;
    app.diff_pane_focus = false;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "hello".to_string(),
            updated_at_ms: 1,
        }],
    };

    crate::tui::ui::record_layout_snapshot(
        Rect::new(0, 0, 40, 20),
        None,
        Some(Rect::new(40, 0, 20, 20)),
        None,
    );

    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 45,
        row: 5,
        modifiers: KeyModifiers::CONTROL,
    });

    assert!(
        !scroll_only,
        "side-panel image zoom should request an immediate redraw"
    );
    assert_eq!(app.side_panel_image_zoom_percent, 110);
    assert!(!app.diff_pane_focus);
}

#[test]
fn test_mouse_scroll_events_are_classified_as_scroll_only() {
    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::File;

    crate::tui::ui::record_layout_snapshot(
        Rect::new(0, 0, 40, 20),
        None,
        Some(Rect::new(40, 0, 20, 20)),
        None,
    );

    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 45,
        row: 5,
        modifiers: KeyModifiers::empty(),
    });

    assert!(
        scroll_only,
        "scroll wheel events should be deferrable during streaming"
    );

    let non_scroll = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 5,
        modifiers: KeyModifiers::empty(),
    });

    assert!(!non_scroll, "clicks should still redraw immediately");
}

#[test]
fn test_handterm_native_scroll_command_updates_chat_offset() {
    // Use an app with real scrollable content and draw it, so the renderer
    // records a non-zero max scroll. Since the phantom-offset fix,
    // scroll_down treats a rendered max of 0 (e.g. an undrawn or empty
    // transcript) as "already at the bottom" and snaps back to follow mode.
    let (mut app, mut terminal) = create_scroll_test_app(50, 12, 0, 24);
    app.auto_scroll_paused = true;
    app.scroll_offset = 6;
    terminal
        .draw(|f| crate::tui::ui::draw(f, &app))
        .expect("draw failed");
    crate::tui::ui::record_layout_snapshot(Rect::new(0, 0, 50, 12), None, None, None);
    assert!(
        crate::tui::ui::last_max_scroll() > 7,
        "scroll test content should exceed the viewport"
    );

    app.apply_handterm_native_scroll(super::handterm_native_scroll::HostToApp::Scroll {
        pane: super::handterm_native_scroll::PaneKind::Chat,
        delta: -2,
    });
    assert_eq!(
        app.scroll_offset, 5,
        "the first row should render immediately"
    );
    assert_eq!(
        app.mouse_scroll_queue, -1,
        "the second row should remain queued"
    );
    app.progress_mouse_scroll_animation();
    assert_eq!(app.scroll_offset, 4);

    app.apply_handterm_native_scroll(super::handterm_native_scroll::HostToApp::Scroll {
        pane: super::handterm_native_scroll::PaneKind::Chat,
        delta: 3,
    });
    assert_eq!(
        app.scroll_offset, 5,
        "the first row should render immediately"
    );
    assert_eq!(
        app.mouse_scroll_queue, 2,
        "later rows should animate on ticks"
    );
    app.progress_mouse_scroll_animation();
    assert_eq!(
        app.scroll_offset, 6,
        "the queued rows should be revealed separately"
    );
    assert_eq!(app.mouse_scroll_queue, 1);
    app.progress_mouse_scroll_animation();
    assert_eq!(app.scroll_offset, 7);
}

#[cfg(unix)]
#[test]
fn test_handterm_native_scroll_client_roundtrips_over_socket() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    let _lock = crate::storage::lock_test_env();
    let _render_lock = scroll_render_test_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("handterm-scroll.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind unix listener");
    unsafe {
        std::env::set_var("HANDTERM_NATIVE_SCROLL_SOCKET", &socket_path);
    }

    let mut client = super::handterm_native_scroll::HandtermNativeScrollClient::connect_from_env()
        .expect("native scroll client should connect from env");
    let (mut server, _) = listener.accept().expect("accept client");
    server
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set read timeout");

    let (mut app, mut terminal) = create_scroll_test_app(50, 12, 0, 24);
    app.auto_scroll_paused = true;
    app.scroll_offset = 6;
    let _ = render_and_snap(&app, &mut terminal);

    client.sync_from_app(&app);

    let mut buf = [0u8; 4096];
    let n = server.read(&mut buf).expect("read pane snapshot");
    let line = std::str::from_utf8(&buf[..n]).expect("utf8 snapshot");
    assert!(line.contains("pane_snapshot"));
    assert!(line.contains("chat"));
    assert!(line.contains("\"position\":6"));

    server
        .write_all(b"{\"type\":\"scroll\",\"pane\":\"chat\",\"delta\":-2}\n")
        .expect("write host scroll command");

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let command = runtime
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(1), client.recv())
                .await
                .expect("timeout waiting for scroll command")
        })
        .expect("scroll command should arrive");

    app.apply_handterm_native_scroll(command);
    assert_eq!(app.scroll_offset, 5);
    assert_eq!(app.mouse_scroll_queue, -1);
    app.progress_mouse_scroll_animation();
    assert_eq!(app.scroll_offset, 4);

    unsafe {
        std::env::remove_var("HANDTERM_NATIVE_SCROLL_SOCKET");
    }
}

#[test]
fn test_mouse_scroll_help_overlay_updates_help_scroll() {
    let mut app = create_test_app();
    app.help_scroll = Some(5);

    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::empty(),
    });

    assert!(
        scroll_only,
        "help overlay mouse wheel should be scroll-only"
    );
    assert_eq!(app.help_scroll, Some(8));

    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::empty(),
    });

    assert!(scroll_only);
    assert_eq!(app.help_scroll, Some(5));
}

#[test]
fn test_mouse_scroll_changelog_overlay_updates_changelog_scroll() {
    let mut app = create_test_app();
    app.changelog_scroll = Some(2);

    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::empty(),
    });

    assert!(
        scroll_only,
        "changelog overlay mouse wheel should be scroll-only"
    );
    assert_eq!(app.changelog_scroll, Some(0));

    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 5,
        modifiers: KeyModifiers::empty(),
    });

    assert!(scroll_only);
    assert_eq!(app.changelog_scroll, Some(3));
}

#[test]
fn test_mouse_scroll_over_unfocused_diagram_scrolls_chat_without_resizing_pane() {
    let _render_lock = scroll_render_test_lock();
    let (mut app, mut terminal) = create_scroll_test_app(120, 30, 0, 80);
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    app.diagram_pane_position = crate::config::DiagramPanePosition::Side;
    app.diagram_pane_ratio = 40;
    app.diagram_pane_ratio_from = 40;
    app.diagram_pane_ratio_target = 40;
    app.diagram_pane_anim_start = None;
    app.diagram_focus = false;

    crate::tui::mermaid::clear_active_diagrams();
    crate::tui::mermaid::register_active_diagram(0x444, 900, 450, None);
    let _ = render_and_snap(&app, &mut terminal);
    let max_scroll = crate::tui::ui::last_max_scroll();
    assert!(max_scroll > 2, "expected scrollable chat content");
    crate::tui::ui::record_layout_snapshot(
        Rect::new(0, 0, 80, 30),
        Some(Rect::new(80, 0, 40, 30)),
        None,
        None,
    );

    for (column, row) in [(80, 0), (90, 10), (119, 29)] {
        app.auto_scroll_paused = false;
        app.scroll_offset = 0;
        app.mouse_scroll_queue = 0;
        app.mouse_scroll_target = None;
        app.diagram_focus = false;

        let scroll_only = app.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        });

        assert!(
            !scroll_only,
            "unfocused diagram wheel at ({column},{row}) should request chat redraw"
        );
        assert!(
            app.auto_scroll_paused,
            "unfocused diagram wheel at ({column},{row}) should pause chat auto-scroll"
        );
        assert_ne!(
            app.scroll_offset, 0,
            "unfocused diagram wheel at ({column},{row}) should move chat scroll offset"
        );
        assert_eq!(app.diagram_pane_ratio, 40);
        assert_eq!(app.diagram_pane_ratio_from, 40);
        assert_eq!(app.diagram_pane_ratio_target, 40);
        assert!(app.diagram_pane_anim_start.is_none());
    }

    crate::tui::mermaid::clear_active_diagrams();
}

#[test]
fn test_mouse_scroll_over_focused_diagram_can_noop_at_top() {
    let _render_lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    app.diagram_pane_position = crate::config::DiagramPanePosition::Side;
    app.diagram_focus = true;
    app.diagram_scroll_y = 0;

    crate::tui::mermaid::clear_active_diagrams();
    crate::tui::mermaid::register_active_diagram(0x446, 900, 450, None);
    crate::tui::ui::record_layout_snapshot(
        Rect::new(0, 0, 80, 30),
        Some(Rect::new(80, 0, 40, 30)),
        None,
        None,
    );

    let before = app.diagram_scroll_y;
    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 90,
        row: 10,
        modifiers: KeyModifiers::empty(),
    });

    assert!(
        scroll_only,
        "focused diagram still owns plain wheel events over the diagram"
    );
    assert_eq!(
        app.diagram_scroll_y, before,
        "this test documents the remaining user-visible no-op case for trace diagnostics"
    );

    crate::tui::mermaid::clear_active_diagrams();
}

#[test]
fn test_dragging_diagram_border_resizes_immediately_without_animation() {
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    app.diagram_pane_position = crate::config::DiagramPanePosition::Side;
    app.diagram_pane_ratio = 40;
    app.diagram_pane_ratio_from = 40;
    app.diagram_pane_ratio_target = 40;
    app.diagram_pane_anim_start = Some(Instant::now());
    app.diagram_pane_dragging = false;

    crate::tui::mermaid::clear_active_diagrams();
    crate::tui::mermaid::register_active_diagram(0x445, 900, 450, None);
    crate::tui::ui::record_layout_snapshot(
        Rect::new(0, 0, 80, 30),
        Some(Rect::new(80, 0, 40, 30)),
        None,
        None,
    );

    app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 80,
        row: 10,
        modifiers: KeyModifiers::empty(),
    });
    assert!(app.diagram_pane_dragging);

    app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 72,
        row: 10,
        modifiers: KeyModifiers::empty(),
    });

    assert_eq!(app.diagram_pane_ratio, 40);
    assert_eq!(app.diagram_pane_ratio_from, 40);
    assert_eq!(app.diagram_pane_ratio_target, 40);
    assert!(app.diagram_pane_anim_start.is_none());

    crate::tui::mermaid::clear_active_diagrams();
}

#[test]
fn test_is_scroll_only_key_detects_navigation_inputs() {
    let mut app = create_test_app();

    let (up_code, up_mods) = scroll_up_key(&app);
    assert!(super::input::is_scroll_only_key(&app, up_code, up_mods));

    let (down_code, down_mods) = scroll_down_key(&app);
    assert!(super::input::is_scroll_only_key(&app, down_code, down_mods));

    app.diff_pane_focus = true;
    assert!(super::input::is_scroll_only_key(
        &app,
        KeyCode::Char('j'),
        KeyModifiers::empty()
    ));

    assert!(super::input::is_scroll_only_key(
        &app,
        KeyCode::Char('g'),
        KeyModifiers::ALT
    ));

    assert!(!super::input::is_scroll_only_key(
        &app,
        KeyCode::Char('a'),
        KeyModifiers::empty()
    ));
    assert!(!super::input::is_scroll_only_key(
        &app,
        KeyCode::Enter,
        KeyModifiers::empty()
    ));
}

#[test]
fn test_fuzzy_command_suggestions() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/mdl");
    assert!(suggestions.iter().any(|(cmd, _)| cmd == "/model"));
}

#[test]
fn test_refresh_model_list_command_suggestions() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/refresh");
    assert!(
        suggestions
            .iter()
            .any(|(cmd, _)| cmd == "/refresh-model-list")
    );
    assert!(!suggestions.iter().any(|(cmd, _)| cmd == "/refresh-models"));

    let spaced = app.get_suggestions_for("/refresh ");
    assert!(spaced.is_empty());
}

#[test]
fn test_command_suggestion_arrow_and_ctrl_navigation_accepts_highlighted_row() {
    let mut app = create_test_app();
    app.input = "/con".to_string();
    app.cursor_pos = app.input.len();
    let suggestions = app.command_suggestions();
    assert!(suggestions.len() >= 2);

    app.handle_key(KeyCode::Down, KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.command_suggestion_selected, 1);
    app.handle_key(KeyCode::Char('k'), KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.command_suggestion_selected, 0);
    app.handle_key(KeyCode::Char('j'), KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.command_suggestion_selected, 1);

    let expected = suggestions[1].0.clone();
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.input, expected);
    assert_eq!(app.cursor_pos, app.input.len());
}

#[test]
fn test_command_suggestion_navigation_moves_through_all_rows_and_allows_shift_arrow_noise() {
    let mut app = create_test_app();
    app.input = "/".to_string();
    app.cursor_pos = app.input.len();
    let suggestion_count = app.command_suggestions().len();
    assert!(suggestion_count > crate::tui::app::COMMAND_SUGGESTION_VISIBLE_LIMIT);

    for expected in 1..=crate::tui::app::COMMAND_SUGGESTION_VISIBLE_LIMIT {
        app.handle_key(KeyCode::Down, KeyModifiers::empty())
            .unwrap();
        assert_eq!(app.command_suggestion_selected, expected);
    }

    app.handle_key(KeyCode::Down, KeyModifiers::SHIFT).unwrap();
    assert_eq!(
        app.command_suggestion_selected,
        crate::tui::app::COMMAND_SUGGESTION_VISIBLE_LIMIT + 1
    );
    app.handle_key(KeyCode::Up, KeyModifiers::SHIFT).unwrap();
    assert_eq!(
        app.command_suggestion_selected,
        crate::tui::app::COMMAND_SUGGESTION_VISIBLE_LIMIT
    );

    for _ in 0..suggestion_count {
        app.handle_key(KeyCode::Down, KeyModifiers::empty())
            .unwrap();
    }
    assert_eq!(
        app.command_suggestion_selected,
        crate::tui::app::COMMAND_SUGGESTION_VISIBLE_LIMIT
    );
}

fn command_cell_fg(
    terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
    command: &str,
) -> Option<ratatui::style::Color> {
    command_cell_at(terminal, command, 0).map(|cell| cell.fg)
}

/// Find the rendered suggestion row for `command` in the terminal buffer and
/// return the cell at `offset` characters into the command (0 is the leading
/// '/'). Suggestion rows render as `{command}  {description}`, which
/// distinguishes them from the echoed input line that can also contain the
/// typed command text.
fn command_cell_at(
    terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
    command: &str,
    offset: u16,
) -> Option<ratatui::buffer::Cell> {
    let buf = terminal.backend().buffer();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        if let Some(x) = line.find(command) {
            let after = &line[x + command.len()..];
            let is_suggestion_row = after
                .strip_prefix("  ")
                .is_some_and(|desc| desc.starts_with(|c: char| !c.is_whitespace()));
            if is_suggestion_row {
                return Some(buf[(x as u16 + offset, y)].clone());
            }
        }
    }
    None
}

/// Expected fg for characters of a suggestion command that the fuzzy matcher
/// did NOT align with the typed query (dimmed toward black).
fn unmatched_command_fg(base: ratatui::style::Color) -> ratatui::style::Color {
    crate::tui::ui::input_ui::dim_command_color(Some(base))
}

/// Expected fg for characters of a suggestion command that the fuzzy matcher
/// aligned with the typed query (brightened toward white, rendered bold).
fn matched_command_fg(base: ratatui::style::Color) -> ratatui::style::Color {
    crate::tui::ui::input_ui::brighten_command_color(Some(base))
}

/// Assert the fuzzy-match recoloring of one rendered suggestion command:
/// the leading '/' is never part of the highlight, so it must be dimmed,
/// while the first command character (matched by the query) must be the
/// brightened base color and bold.
#[track_caller]
fn assert_command_match_recolored(
    terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
    command: &str,
    base: ratatui::style::Color,
) {
    let slash = command_cell_at(terminal, command, 0).expect("command not rendered");
    assert_eq!(
        slash.fg,
        unmatched_command_fg(base),
        "leading '/' of {command} should be dimmed base color"
    );
    let matched = command_cell_at(terminal, command, 1).expect("command not rendered");
    assert_eq!(
        matched.fg,
        matched_command_fg(base),
        "matched char of {command} should be brightened base color"
    );
    assert!(
        matched
            .style()
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD),
        "matched char of {command} should be bold"
    );
}
