#[test]
fn test_handle_server_event_token_usage_uses_per_call_deltas() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.streaming.streaming_tps_collect_output = true;

    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 10,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );
    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 30,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );
    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 30,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );

    assert_eq!(app.streaming.streaming_output_tokens, 30);
    assert_eq!(app.streaming.streaming_total_output_tokens, 30);
    assert_eq!(app.token_accounting.total_input_tokens, 100);
    assert_eq!(app.token_accounting.total_output_tokens, 30);
}

#[test]
fn test_handle_server_event_tool_exec_pauses_tps_but_collects_final_tool_usage() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.streaming.streaming_tps_elapsed = Duration::from_secs(2);

    app.handle_server_event(
        crate::protocol::ServerEvent::ToolStart {
            id: "tool-1".to_string(),
            name: "read".to_string(),
        },
        &mut remote,
    );

    assert!(app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_some());

    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(3));

    app.handle_server_event(
        crate::protocol::ServerEvent::ToolExec {
            id: "tool-1".to_string(),
            name: "read".to_string(),
        },
        &mut remote,
    );

    assert!(app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_none());
    assert!(app.streaming.streaming_tps_elapsed >= Duration::from_secs(5));

    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 25,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );

    assert_eq!(app.streaming.streaming_total_output_tokens, 25);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 25);

    app.handle_server_event(
        crate::protocol::ServerEvent::TextDelta {
            text: "hello".to_string(),
        },
        &mut remote,
    );

    assert!(app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_some());
}

#[test]
fn test_handle_server_event_kv_cache_request_resets_tps_output_watermark_for_next_api_call() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.streaming.streaming_tps_collect_output = true;

    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 40,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );

    app.handle_server_event(
        crate::protocol::ServerEvent::KvCacheRequest {
            system_static_hash: 1,
            tools_hash: 2,
            messages_hash: 3,
            message_hashes: vec![11, 22],
            message_count: 2,
            tool_count: 1,
            system_static_chars: 10,
            tools_json_chars: 20,
            messages_json_chars: 30,
            ephemeral_hash: None,
            ephemeral_chars: 0,
            ephemeral_message_count: 0,
        },
        &mut remote,
    );

    assert!(!app.streaming.streaming_tps_collect_output);

    app.handle_server_event(
        crate::protocol::ServerEvent::ConnectionPhase {
            phase: "streaming".to_string(),
        },
        &mut remote,
    );

    assert!(app.streaming.streaming_tps_collect_output);

    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 120,
            output: 15,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );

    assert_eq!(app.streaming.streaming_total_output_tokens, 55);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 55);
}

#[test]
fn test_handle_server_event_message_end_marks_stream_as_finalizing_without_stall_mode() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    app.streaming.streaming_tps_collect_output = true;

    let needs_redraw =
        app.handle_server_event(crate::protocol::ServerEvent::MessageEnd, &mut remote);

    assert!(needs_redraw);
    assert!(app.stream_message_ended);
    assert!(matches!(app.status, ProcessingStatus::Streaming));
    assert!(app.streaming.streaming_tps_collect_output);
}

#[test]
fn test_remote_done_waits_for_paced_backlog_and_one_live_frame() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.current_message_id = Some(42);
    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    let response = "paced text";
    let ops = app.stream_buffer.push_text(response);
    app.apply_stream_ops(ops);
    assert!(!app.stream_buffer.is_empty());

    app.handle_server_event(crate::protocol::ServerEvent::MessageEnd, &mut remote);
    app.handle_server_event(crate::protocol::ServerEvent::Done { id: 42 }, &mut remote);

    assert!(app.is_processing, "Done must not force-flush the backlog");
    assert_eq!(app.deferred_stream_done_id, Some(42));
    assert!(app.display_messages.iter().all(|message| {
        message.role != "assistant" || !message.content.contains(response)
    }));

    // The first tick drains the short backlog, but deliberately leaves the live
    // streaming representation visible for one frame before committing it.
    std::thread::sleep(Duration::from_millis(60));
    rt.block_on(crate::tui::app::remote::handle_tick(&mut app, &mut remote));
    assert!(app.stream_buffer.is_empty());
    assert!(app.is_processing);
    assert_eq!(app.deferred_stream_done_id, Some(42));
    assert_eq!(app.streaming.streaming_text, response);

    // The following tick replays Done now that the preceding live frame was
    // eligible to render, committing exactly the text that was paced out.
    rt.block_on(crate::tui::app::remote::handle_tick(&mut app, &mut remote));
    assert!(!app.is_processing);
    assert_eq!(app.deferred_stream_done_id, None);
    assert!(app.display_messages.iter().any(|message| {
        message.role == "assistant" && message.content == response
    }));
}

#[test]
fn test_handle_server_event_tps_connection_phase_streaming_starts_collection_only_for_streaming() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::ConnectionPhase {
            phase: "waiting for response".to_string(),
        },
        &mut remote,
    );

    assert!(!app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_none());

    app.handle_server_event(
        crate::protocol::ServerEvent::ConnectionPhase {
            phase: "streaming".to_string(),
        },
        &mut remote,
    );

    assert!(app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_some());
    assert!(matches!(app.status, ProcessingStatus::Streaming));
}

#[test]
fn test_connection_phase_elapsed_resets_per_attempt_not_per_turn() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    // Simulate a long-running turn: the whole-turn timer has been ticking for
    // well over the "suspiciously long" yellow threshold.
    app.is_processing = true;
    app.processing_started = Some(Instant::now() - Duration::from_secs(120));
    assert!(crate::tui::TuiState::elapsed(&app).unwrap() > Duration::from_secs(60));

    // A later round-trip enters the connecting phase. The per-attempt timer
    // must start fresh, so it reads as a brief connect (well under 10s) instead
    // of inheriting the 120s whole-turn elapsed and rendering yellow.
    app.handle_server_event(
        crate::protocol::ServerEvent::ConnectionPhase {
            phase: "connecting".to_string(),
        },
        &mut remote,
    );

    assert!(matches!(
        app.status,
        ProcessingStatus::Connecting(crate::message::ConnectionPhase::Connecting)
    ));
    let phase_elapsed = crate::tui::TuiState::connection_phase_elapsed(&app)
        .expect("connection phase elapsed should be tracked");
    assert!(
        phase_elapsed < Duration::from_secs(5),
        "per-attempt connection elapsed should be fresh, got {:?}",
        phase_elapsed
    );

    // Sub-phase transitions within the same attempt must not restart the timer.
    let started = app.connection_phase_started;
    app.handle_server_event(
        crate::protocol::ServerEvent::ConnectionPhase {
            phase: "waiting for response".to_string(),
        },
        &mut remote,
    );
    assert_eq!(
        app.connection_phase_started, started,
        "sub-phase transitions should keep the same per-attempt start"
    );

    // Streaming clears the per-attempt timer.
    app.handle_server_event(
        crate::protocol::ServerEvent::ConnectionPhase {
            phase: "streaming".to_string(),
        },
        &mut remote,
    );
    assert!(app.connection_phase_started.is_none());
}

#[test]
fn test_handle_server_event_tps_message_end_counts_late_usage_without_timer_running() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::ConnectionPhase {
            phase: "streaming".to_string(),
        },
        &mut remote,
    );
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(4));

    app.handle_server_event(crate::protocol::ServerEvent::MessageEnd, &mut remote);

    assert!(app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_none());
    assert!(app.streaming.streaming_tps_elapsed >= Duration::from_secs(4));

    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 20,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );

    assert_eq!(app.streaming.streaming_total_output_tokens, 20);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 20);
    assert!(app.streaming.streaming_tps_observed_elapsed >= Duration::from_secs(4));
    assert!(app.streaming.streaming_tps_start.is_none());
}

#[test]
fn test_handle_server_event_tps_redundant_late_usage_after_message_end_does_not_double_count() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::ConnectionPhase {
            phase: "streaming".to_string(),
        },
        &mut remote,
    );
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(5));

    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 10,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );
    app.handle_server_event(crate::protocol::ServerEvent::MessageEnd, &mut remote);
    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 30,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );
    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 100,
            output: 30,
            cache_read_input: None,
            cache_creation_input: None,
        },
        &mut remote,
    );

    assert_eq!(app.streaming.streaming_total_output_tokens, 30);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 30);
    assert_eq!(*remote.call_output_tokens_seen(), 30);
}

#[test]
fn test_handle_server_event_interrupted_clears_stream_state_and_sets_idle() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    app.processing_started = Some(Instant::now());
    app.current_message_id = Some(42);
    app.streaming.streaming_text = "partial".to_string();
    app.streaming_tool_calls.push(crate::message::ToolCall {
        id: "tool_1".to_string(),
        name: "bash".to_string(),
        input: serde_json::Value::Null,
        intent: None, thought_signature: None, });
    app.interleave_message = Some("queued interrupt".to_string());
    app.pending_soft_interrupts
        .push("pending soft interrupt".to_string());
    app.pending_soft_interrupt_requests
        .push((77, "pending soft interrupt".to_string()));

    remote.handle_tool_start("tool_1", "bash");
    remote.handle_tool_input("{\"command\":\"sleep 10\"}");
    remote.handle_tool_exec("tool_1", "edit");

    app.handle_server_event(crate::protocol::ServerEvent::Interrupted, &mut remote);

    assert!(!app.is_processing);
    assert!(matches!(app.status, ProcessingStatus::Idle));
    assert!(app.processing_started.is_none());
    assert!(app.current_message_id.is_none());
    assert!(app.streaming.streaming_text.is_empty());
    assert!(app.streaming_tool_calls.is_empty());
    assert!(app.interleave_message.is_none());
    assert_eq!(app.queued_messages(), &["queued interrupt"]);
    assert_eq!(app.pending_soft_interrupts, vec!["pending soft interrupt"]);
    assert_eq!(
        app.pending_soft_interrupt_requests,
        vec![(77, "pending soft interrupt".to_string())]
    );

    let last = app
        .display_messages()
        .last()
        .expect("missing interrupted message");
    assert_eq!(last.role, "system");
    assert_eq!(last.content, "Interrupted");
}

#[test]
fn test_remote_interrupted_defers_queued_followup_dispatch_by_one_cycle() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    app.current_message_id = Some(42);
    app.queued_messages.push("queued later".to_string());

    app.handle_server_event(crate::protocol::ServerEvent::Interrupted, &mut remote);

    assert!(app.pending_queued_dispatch);
    assert_eq!(app.queued_messages(), &["queued later"]);
    assert!(!app.is_processing);

    rt.block_on(remote::process_remote_followups(&mut app, &mut remote));
    assert_eq!(app.queued_messages(), &["queued later"]);
    assert!(!app.is_processing);

    app.pending_queued_dispatch = false;
    rt.block_on(remote::process_remote_followups(&mut app, &mut remote));
    assert!(app.queued_messages().is_empty());
    assert!(app.is_processing);
    assert!(matches!(app.status, ProcessingStatus::Sending));
    assert!(app.current_message_id.is_some());
}

#[test]
fn test_remote_interrupted_recovers_pending_interleaves_in_order() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    app.current_message_id = Some(42);
    app.interleave_message = Some("unsent interleave".to_string());
    app.pending_soft_interrupts = vec!["acked interleave".to_string()];
    app.pending_soft_interrupt_requests = vec![(55, "acked interleave".to_string())];
    app.queued_messages.push("queued later".to_string());

    app.handle_server_event(crate::protocol::ServerEvent::Interrupted, &mut remote);

    assert!(app.pending_queued_dispatch);
    assert_eq!(
        app.queued_messages(),
        &["unsent interleave", "queued later"]
    );
    assert_eq!(app.pending_soft_interrupts, vec!["acked interleave"]);

    rt.block_on(remote::process_remote_followups(&mut app, &mut remote));
    assert!(app.pending_soft_interrupts.is_empty());
    assert!(app.pending_soft_interrupt_requests.is_empty());
    assert_eq!(
        app.queued_messages(),
        &["acked interleave", "unsent interleave", "queued later"]
    );
    assert!(!app.is_processing);

    app.pending_queued_dispatch = false;
    rt.block_on(remote::process_remote_followups(&mut app, &mut remote));

    assert!(app.pending_soft_interrupts.is_empty());
    assert!(app.pending_soft_interrupt_requests.is_empty());
    assert!(app.queued_messages().is_empty());
    assert!(app.is_processing);
    assert!(matches!(app.status, ProcessingStatus::Sending));

    let user_messages: Vec<&str> = app
        .display_messages()
        .iter()
        .filter(|msg| msg.role == "user")
        .map(|msg| msg.content.as_str())
        .collect();
    assert_eq!(
        user_messages,
        vec!["acked interleave", "unsent interleave", "queued later"]
    );
}

#[test]
fn test_remote_done_recovers_stranded_soft_interrupt_as_queued_followup() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    remote.mark_history_loaded();

    app.is_processing = true;
    app.status = ProcessingStatus::Streaming;
    app.current_message_id = Some(42);
    app.pending_soft_interrupts = vec!["late interleave".to_string()];
    app.pending_soft_interrupt_requests = vec![(55, "late interleave".to_string())];
    app.queued_messages.push("queued later".to_string());

    app.handle_server_event(crate::protocol::ServerEvent::Done { id: 42 }, &mut remote);

    assert!(!app.is_processing);
    assert_eq!(app.pending_soft_interrupts, vec!["late interleave"]);
    assert_eq!(
        app.pending_soft_interrupt_requests,
        vec![(55, "late interleave".to_string())]
    );
    assert_eq!(app.queued_messages(), &["queued later"]);

    rt.block_on(remote::process_remote_followups(&mut app, &mut remote));

    assert!(app.pending_soft_interrupts.is_empty());
    assert!(app.pending_soft_interrupt_requests.is_empty());
    assert!(app.queued_messages().is_empty());
    assert!(app.is_processing);
    assert!(matches!(app.status, ProcessingStatus::Sending));
    assert!(app.current_message_id.is_some());

    let user_messages: Vec<&str> = app
        .display_messages()
        .iter()
        .filter(|msg| msg.role == "user")
        .map(|msg| msg.content.as_str())
        .collect();
    assert_eq!(user_messages, vec!["late interleave", "queued later"]);
}

#[test]
fn test_remote_done_auto_pokes_again_when_todos_remain() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let mut remote = crate::tui::backend::RemoteConnection::dummy();

        crate::todo::save_todos(
            &app.session.id,
            &[crate::todo::TodoItem {
                group: None,
                id: "todo-1".to_string(),
                content: "Continue working".to_string(),
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

        app.is_remote = true;
        app.auto_poke_incomplete_todos = true;
        app.is_processing = true;
        app.status = ProcessingStatus::Streaming;
        app.current_message_id = Some(42);

        let needs_redraw =
            app.handle_server_event(crate::protocol::ServerEvent::Done { id: 42 }, &mut remote);

        assert!(needs_redraw);
        assert!(app.pending_queued_dispatch);
        assert_eq!(app.queued_messages().len(), 1);
        assert!(app.queued_messages()[0].contains("Continue working, or update the todo tool."));
    });
}

#[test]
fn test_handle_server_event_side_pane_images_populates_pane_live() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.side_panel = crate::side_panel::SidePanelSnapshot::default();
    app.remote_session_id = Some("session_active".to_string());
    assert!(app.remote_side_pane_images.is_empty());

    let needs_redraw = app.handle_server_event(
        crate::protocol::ServerEvent::SidePaneImages {
            session_id: "session_active".to_string(),
            images: vec![crate::session::RenderedImage {
                media_type: "image/png".to_string(),
                data: "image-data".to_string(),
                label: Some("openclaw.png".to_string()),
                source: crate::session::RenderedImageSource::ToolResult {
                    tool_name: "read".to_string(),
                },
                anchor: None,
            }],
        },
        &mut remote,
    );

    assert!(needs_redraw, "live side-pane image should request a redraw");
    assert_eq!(app.remote_side_pane_images.len(), 1);
    // Images render inline in the transcript now, so a live image must not flip
    // the side panel or arm the old auto-hide timer.
    assert!(!app.side_panel_user_hidden);
    assert!(<App as crate::tui::TuiState>::pin_images(&app));
    assert!(app.pinned_images_auto_hide_deadline.is_none());
}

#[test]
fn test_native_generated_image_renders_inline_without_opening_side_panel() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.side_panel = crate::side_panel::SidePanelSnapshot::default();
    app.remote_session_id = Some("session_active".to_string());

    let generated_redraw = app.handle_server_event(
        crate::protocol::ServerEvent::GeneratedImage {
            id: "image_call_123".to_string(),
            path: "/tmp/generated.png".to_string(),
            metadata_path: Some("/tmp/generated.json".to_string()),
            output_format: "png".to_string(),
            revised_prompt: Some("a green square".to_string()),
        },
        &mut remote,
    );
    let image_redraw = app.handle_server_event(
        crate::protocol::ServerEvent::SidePaneImages {
            session_id: "session_active".to_string(),
            images: vec![crate::session::RenderedImage {
                media_type: "image/png".to_string(),
                data: "image-data".to_string(),
                label: Some("/tmp/generated.png".to_string()),
                source: crate::session::RenderedImageSource::ToolResult {
                    tool_name: crate::message::GENERATED_IMAGE_TOOL_NAME.to_string(),
                },
                anchor: Some(crate::session::RenderedImageAnchor::ToolCall {
                    id: "image_call_123".to_string(),
                }),
            }],
        },
        &mut remote,
    );

    assert!(generated_redraw);
    assert!(image_redraw);
    assert!(!app.side_panel.has_pages());
    assert_eq!(app.remote_side_pane_images.len(), 1);
    assert_eq!(
        app.remote_side_pane_images[0].anchor,
        Some(crate::session::RenderedImageAnchor::ToolCall {
            id: "image_call_123".to_string(),
        })
    );
    let generated_row = app
        .display_messages
        .iter()
        .find(|message| {
            message
                .tool_data
                .as_ref()
                .is_some_and(|tool| tool.id == "image_call_123")
        })
        .expect("generated image tool row");
    assert_eq!(generated_row.title.as_deref(), Some("Generated image"));
}

#[test]
fn test_handle_server_event_side_pane_images_ignores_inactive_session() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.remote_session_id = Some("session_active".to_string());

    let needs_redraw = app.handle_server_event(
        crate::protocol::ServerEvent::SidePaneImages {
            session_id: "session_other".to_string(),
            images: vec![crate::session::RenderedImage {
                media_type: "image/png".to_string(),
                data: "image-data".to_string(),
                label: None,
                source: crate::session::RenderedImageSource::ToolResult {
                    tool_name: "read".to_string(),
                },
                anchor: None,
            }],
        },
        &mut remote,
    );

    assert!(!needs_redraw);
    assert!(app.remote_side_pane_images.is_empty());
}

#[test]
fn test_handle_server_event_mcp_status_updates_tools_without_status_notice() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_server_event(
        crate::protocol::ServerEvent::McpStatus {
            servers: vec!["agentcard:8".to_string()],
        },
        &mut remote,
    );

    assert_eq!(app.mcp_server_names, vec![("agentcard".to_string(), 8)]);
    assert_eq!(app.status_notice(), None);
}

#[test]
fn test_handle_server_event_reasoning_delta_shows_thinking_status() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_processing = true;
    // Server emits ConnectionPhase::Streaming when reasoning starts (to kick the
    // TPS timer), so the status arrives as Streaming.
    app.status = ProcessingStatus::Streaming;

    app.handle_server_event(
        crate::protocol::ServerEvent::ReasoningDelta {
            text: "weighing options".to_string(),
        },
        &mut remote,
    );

    // Live reasoning should read as "thinking", not "streaming".
    assert!(matches!(app.status, ProcessingStatus::Thinking(_)));

    // Real output text flips the status back to streaming.
    app.handle_server_event(
        crate::protocol::ServerEvent::TextDelta {
            text: "Here is the answer".to_string(),
        },
        &mut remote,
    );
    assert!(matches!(app.status, ProcessingStatus::Streaming));
}

#[test]
fn test_handle_server_event_reasoning_delta_keeps_tool_status() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_processing = true;
    app.status = ProcessingStatus::RunningTool("bash".to_string());

    app.handle_server_event(
        crate::protocol::ServerEvent::ReasoningDelta {
            text: "post-tool reflection".to_string(),
        },
        &mut remote,
    );

    // A running tool must not be masked by reasoning text.
    assert!(matches!(app.status, ProcessingStatus::RunningTool(_)));
}

#[test]
fn test_pending_startup_notice_survives_history_bootstrap_for_fresh_session() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    // A fresh client has no remote session yet; the startup notice card is
    // pushed before the History bootstrap arrives.
    app.remote_session_id = None;
    app.set_pending_startup_notice("Launch hotkeys", "cmd+; -> home\ncmd+' -> last project");
    assert!(
        app.display_messages()
            .iter()
            .any(|m| m.content.contains("cmd+;")),
        "card should be visible before bootstrap"
    );

    // The bootstrap for a brand-new session clears the transcript.
    app.handle_server_event(
        crate::protocol::ServerEvent::History {
            id: 1,
            session_id: "session_new".to_string(),
            messages: vec![],
            images: vec![],
            provider_name: Some("claude".to_string()),
            provider_model: Some("claude-sonnet-4-20250514".to_string()),
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
            server_version: None,
            server_name: None,
            server_icon: None,
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

    // The card must still be present on the idle screen after the bootstrap.
    let card_count = app
        .display_messages()
        .iter()
        .filter(|m| m.content.contains("cmd+;"))
        .count();
    assert_eq!(
        card_count, 1,
        "startup notice should be re-applied exactly once after bootstrap"
    );
}
