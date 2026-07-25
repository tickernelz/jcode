#[test]
fn test_context_limit_error_detection() {
    assert!(is_context_limit_error(
        "OpenAI API error 400: This model's maximum context length is 200000 tokens"
    ));
    assert!(is_context_limit_error(
        "request too large: prompt is too long for context window"
    ));
    assert!(!is_context_limit_error(
        "rate limit exceeded, retry after 20s"
    ));
}

#[test]
fn test_request_payload_too_large_error_detection() {
    assert!(is_request_payload_too_large_error(
        "Anthropic API error (413 Payload Too Large): {\"error\":{\"type\":\"request_too_large\",\"message\":\"Request exceeds the maximum size\"}}"
    ));
    assert!(!is_request_payload_too_large_error(
        "rate limit exceeded, retry after 20s"
    ));
    // A plain token-context overflow is not a payload-size error.
    assert!(!is_request_payload_too_large_error(
        "This model's maximum context length is 200000 tokens"
    ));
}

#[test]
fn test_provider_image_suppression_drops_oldest_first_without_mutating_history() {
    use crate::message::ContentBlock;
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());

    let big = "a".repeat(8 * 1024 * 1024); // 8 MiB base64 image each
    for _ in 0..3 {
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: big.clone(),
            }],
        );
    }

    // 24 MiB of images, budget 12 MiB → drop the two oldest, keep the newest.
    let canonical_before = serde_json::to_vec(&app.session.messages).unwrap();
    let stripped = app
        .session
        .suppress_oversized_images_for_provider(crate::compaction::PAYLOAD_IMAGE_CHAR_BUDGET);
    assert_eq!(stripped, 2);
    assert_eq!(
        serde_json::to_vec(&app.session.messages).unwrap(),
        canonical_before
    );
    assert!(matches!(
        app.session.messages[0].content[0],
        ContentBlock::Image { .. }
    ));
    let provider = app.session.messages_for_provider();
    assert!(matches!(provider[0].content[0], ContentBlock::Text { .. }));
    assert!(matches!(provider[1].content[0], ContentBlock::Text { .. }));
    assert!(matches!(provider[2].content[0], ContentBlock::Image { .. }));
}

#[test]
fn test_rewind_archives_provider_messages_without_truncating_canonical_history() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());

    for idx in 1..=3 {
        let text = format!("msg-{}", idx);
        app.add_provider_message(Message::user(&text));
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
        );
    }
    app.provider_session_id = Some("provider-session".to_string());
    app.session.provider_session_id = Some("provider-session".to_string());

    app.input = "/rewind 2".to_string();
    app.submit_input();

    assert_eq!(app.messages.len(), 2);
    assert_eq!(app.session.messages.len(), 3);
    assert_eq!(app.session.archived_message_ids.len(), 1);
    assert!(matches!(
        &app.messages[1].content[0],
        ContentBlock::Text { text, .. } if text == "msg-2"
    ));
    assert!(app.provider_session_id.is_none());
    assert!(app.session.provider_session_id.is_none());
}

#[test]
fn test_rewind_undo_restores_truncated_messages() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());

    for idx in 1..=3 {
        let text = format!("msg-{}", idx);
        app.add_provider_message(Message::user(&text));
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
        );
    }
    app.provider_session_id = Some("provider-session".to_string());
    app.session.provider_session_id = Some("provider-session".to_string());
    let compaction = crate::session::StoredCompactionState {
        summary_text: "exact pre-rewind summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };
    app.session.compaction = Some(compaction.clone());

    app.input = "/rewind 1".to_string();
    app.submit_input();
    assert_eq!(app.session.visible_conversation_message_count(), 1);
    assert!(app.session.compaction.is_none());
    assert!(
        app.display_messages()
            .last()
            .expect("rewind notice")
            .content
            .contains("Undo anytime with /rewind undo")
    );

    app.input = "/rewind undo".to_string();
    app.submit_input();

    assert_eq!(app.session.visible_conversation_message_count(), 3);
    assert_eq!(app.messages.len(), 3);
    assert_eq!(app.session.compaction, Some(compaction));
    assert_eq!(app.provider_session_id.as_deref(), Some("provider-session"));
    assert_eq!(
        app.session.provider_session_id.as_deref(),
        Some("provider-session")
    );
    assert!(
        app.display_messages()
            .last()
            .expect("undo notice")
            .content
            .contains("✓ Undid rewind. Restored 2 messages.")
    );
}

#[test]
fn test_normal_user_turn_invalidates_rewind_undo_snapshot() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());
    app.messages.clear();
    for index in 1..=3 {
        let text = format!("old-branch-{index}");
        app.add_provider_message(Message::user(&text));
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
        );
    }

    app.input = "/rewind 1".to_string();
    app.submit_input();
    assert!(app.rewind_undo_snapshot.is_some());

    app.input = "new branch prompt".to_string();
    app.submit_input();

    assert!(app.rewind_undo_snapshot.is_none());
    assert!(app.session.messages.iter().any(|message| {
        matches!(
            message.content.first(),
            Some(ContentBlock::Text { text, .. }) if text == "new branch prompt"
        )
    }));
    assert!(app.session.messages.iter().any(|message| {
        matches!(
            message.content.first(),
            Some(ContentBlock::Text { text, .. }) if text == "old-branch-3"
        )
    }));
    assert!(
        app.session
            .messages_for_provider_uncached()
            .iter()
            .all(|message| {
                !matches!(
                    message.content.first(),
                    Some(ContentBlock::Text { text, .. }) if text == "old-branch-3"
                )
            })
    );
}

#[test]
fn test_rewind_and_undo_save_failures_keep_tui_session_unchanged() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());
    for idx in 1..=3 {
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
    app.session.save().expect("save transcript");

    let failure = crate::storage::inject_nth_write_failures(None, 1, 2);
    app.input = "/rewind 1".to_string();
    app.submit_input();
    assert_eq!(app.session.visible_conversation_message_count(), 3);
    drop(failure);

    app.input = "/rewind 1".to_string();
    app.submit_input();
    assert_eq!(app.session.visible_conversation_message_count(), 1);
    let _failure = crate::storage::inject_nth_write_failures(None, 1, 2);
    app.input = "/rewind undo".to_string();
    app.submit_input();
    assert_eq!(app.session.visible_conversation_message_count(), 1);
}

#[test]
fn test_clear_close_failure_keeps_current_tui_session() {
    let mut app = create_test_app();
    app.session.save().expect("save current session");
    let session_id = app.session.id.clone();
    let path = crate::session::session_path(&session_id).expect("session path");
    let _failure = crate::storage::inject_write_failure(Some(path));

    assert!(super::commands_review::reset_current_session(&mut app).is_err());
    assert_eq!(app.session.id, session_id);
    assert_eq!(
        crate::session::Session::load(&session_id)
            .expect("load current session")
            .status,
        crate::session::SessionStatus::Active
    );
}

#[test]
fn test_restore_close_failure_keeps_current_tui_session() {
    let mut app = create_test_app();
    app.session.save().expect("save current session");
    let session_id = app.session.id.clone();
    let mut target = crate::session::Session::create(None, None);
    target.save().expect("save restore target");
    App::save_startup_submission_for_session(&target.id, "retry me".to_string(), Vec::new());
    let path = crate::session::session_path(&session_id).expect("session path");
    let _failure = crate::storage::inject_write_failure(Some(path));

    app.restore_session(&target.id);
    assert_eq!(app.session.id, session_id);
    assert_eq!(
        crate::session::Session::load(&session_id)
            .expect("load current session")
            .status,
        crate::session::SessionStatus::Active
    );
    assert!(
        app.display_messages()
            .last()
            .is_some_and(|message| message.content.contains("Failed to restore exact durable"))
    );
    assert_eq!(
        App::restore_input_for_reload(&target.id)
            .expect("failed restore must retain staged input")
            .input,
        "retry me"
    );
    assert!(
        !crate::session::session_path(&session_id)
            .expect("source path")
            .with_extension("handoff")
            .exists()
    );
}

#[test]
fn test_restore_target_activation_failure_rolls_back_current_tui_session() {
    let mut app = create_test_app();
    app.session.save().expect("save current session");
    let session_id = app.session.id.clone();
    let mut target = crate::session::Session::create(None, None);
    target.mark_closed();
    target.save().expect("save closed restore target");
    App::save_startup_submission_for_session(
        &target.id,
        "retry activation".to_string(),
        Vec::new(),
    );
    let target_path = crate::session::session_path(&target.id).expect("target path");
    let _failure = crate::storage::inject_write_failure(Some(target_path));

    app.restore_session(&target.id);

    assert_eq!(app.session.id, session_id);
    assert_eq!(app.session.status, crate::session::SessionStatus::Active);
    assert_eq!(
        crate::session::Session::load(&session_id)
            .expect("load rolled-back current session")
            .status,
        crate::session::SessionStatus::Active
    );
    assert_eq!(
        crate::session::Session::load(&target.id)
            .expect("load unactivated target")
            .status,
        crate::session::SessionStatus::Closed
    );
    assert_eq!(
        App::restore_input_for_reload(&target.id)
            .expect("failed activation must retain staged input")
            .input,
        "retry activation"
    );
    assert!(
        !crate::session::session_path(&session_id)
            .expect("source path")
            .with_extension("handoff")
            .exists()
    );
}

#[test]
fn test_local_model_switch_stale_cas_rolls_back_exact_runtime_identity() {
    let _guard = crate::storage::lock_test_env();
    let (mut app, model, _) = create_durable_identity_test_app();
    app.session.model = Some("old-model".to_string());
    app.session.provider_key = None;
    app.session.route_api_method = None;
    app.session.provider_session_id = Some("durable-resume".to_string());
    app.provider_session_id = Some("runtime-resume".to_string());
    app.session.save().expect("save initial model identity");

    let mut concurrent =
        crate::session::Session::load(&app.session.id).expect("load concurrent session");
    concurrent.title = Some("advance model CAS".to_string());
    concurrent.save().expect("advance durable model revision");

    app.provider.set_model("new-model").expect("apply model");
    assert!(app.finalize_model_switch("new-model").is_err());
    assert_eq!(model.lock().unwrap().as_str(), "old-model");
    assert_eq!(app.session.model.as_deref(), Some("old-model"));
    assert_eq!(
        app.session.provider_session_id.as_deref(),
        Some("durable-resume")
    );
    assert_eq!(app.provider_session_id.as_deref(), Some("runtime-resume"));
    assert_eq!(
        crate::session::Session::load(&app.session.id)
            .expect("load durable model")
            .model
            .as_deref(),
        Some("old-model")
    );
}

#[test]
fn test_local_effort_switch_stale_cas_rolls_back_absent_override() {
    let _guard = crate::storage::lock_test_env();
    let (mut app, _, effort) = create_durable_identity_test_app();
    app.session.reasoning_effort = None;
    app.session.save().expect("save initial effort identity");

    let mut concurrent =
        crate::session::Session::load(&app.session.id).expect("load concurrent session");
    concurrent.title = Some("advance effort CAS".to_string());
    concurrent.save().expect("advance durable effort revision");

    assert!(app.set_reasoning_effort_transactional("high").is_err());
    assert_eq!(*effort.lock().unwrap(), None);
    assert_eq!(app.session.reasoning_effort, None);
    assert_eq!(
        crate::session::Session::load(&app.session.id)
            .expect("load durable effort")
            .reasoning_effort,
        None
    );
}

#[test]
fn test_tui_restore_clears_previous_reasoning_override_when_target_is_none() {
    let _guard = crate::storage::lock_test_env();
    let (mut app, _, effort) = create_durable_identity_test_app();
    *effort.lock().unwrap() = Some("high".to_string());
    app.session.model = Some("old-model".to_string());
    app.session.reasoning_effort = Some("high".to_string());
    app.session.save().expect("save source effort");

    let mut target = crate::session::Session::create(None, None);
    target.model = Some("old-model".to_string());
    target.reasoning_effort = None;
    target.status = crate::session::SessionStatus::Closed;
    target.save().expect("save target effort");

    app.restore_session(&target.id);
    assert_eq!(*effort.lock().unwrap(), None);
    assert_eq!(app.session.id, target.id);
    assert_eq!(app.session.reasoning_effort, None);
    assert_eq!(
        crate::session::Session::load(&target.id)
            .expect("load restored target")
            .reasoning_effort,
        None
    );
}

#[test]
fn test_rewind_lists_visible_messages_when_initial_session_context_is_hidden() {
    let mut app = create_test_app();

    for idx in 1..=2 {
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("msg-{}", idx),
                cache_control: None,
            }],
        );
    }

    app.input = "/rewind".to_string();
    app.submit_input();

    let last = app.display_messages().last().expect("history message");
    assert!(last.content.contains("Conversation history:"));
    assert!(last.content.contains("1 👤 User - msg-1"));
    assert!(last.content.contains("2 👤 User - msg-2"));
    assert!(!last.content.contains("Session Context"));
    assert!(!last.content.contains("No messages in conversation"));
}

#[test]
fn test_rewind_autocomplete_does_not_fuzzy_rewrite_numeric_targets() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());

    for idx in 1..=3 {
        app.session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("msg-{}", idx),
                cache_control: None,
            }],
        );
    }

    app.input = "/rewind 10".to_string();
    assert!(!app.autocomplete());
    assert_eq!(app.input, "/rewind 10");

    app.input = "/rewind 2".to_string();
    assert!(!app.autocomplete());
    assert_eq!(app.input, "/rewind 2");
}

#[test]
fn test_rewind_autocomplete_uses_visible_message_count() {
    let mut app = create_test_app();
    app.session.replace_messages(Vec::new());

    app.session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "<system-reminder>hidden</system-reminder>".to_string(),
            cache_control: None,
        }],
    );
    app.session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "visible".to_string(),
            cache_control: None,
        }],
    );

    assert_eq!(app.session.messages.len(), 2);
    assert_eq!(app.session.visible_conversation_message_count(), 1);

    app.input = "/rewind ".to_string();
    let suggestions = app.get_suggestions_for(&app.input);
    assert_eq!(
        suggestions,
        vec![("/rewind 1".to_string(), "Rewind to this message")]
    );
}

#[test]
fn test_accumulate_streaming_output_tokens_uses_deltas() {
    let mut app = create_test_app();
    let mut seen = 0;

    app.streaming.streaming_tps_collect_output = true;
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(10));

    app.accumulate_streaming_output_tokens(10, &mut seen);
    app.accumulate_streaming_output_tokens(30, &mut seen);
    app.accumulate_streaming_output_tokens(30, &mut seen);

    assert_eq!(app.streaming.streaming_total_output_tokens, 30);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 30);
    assert!(app.streaming.streaming_tps_observed_elapsed >= Duration::from_secs(9));
    assert_eq!(seen, 30);
}

#[test]
fn test_accumulate_streaming_output_tokens_ignores_hidden_output_phase() {
    let mut app = create_test_app();
    let mut seen = 0;

    app.accumulate_streaming_output_tokens(20, &mut seen);
    assert_eq!(app.streaming.streaming_total_output_tokens, 0);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 0);
    assert_eq!(seen, 20);

    app.streaming.streaming_tps_collect_output = true;
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(10));
    app.accumulate_streaming_output_tokens(60, &mut seen);

    assert_eq!(app.streaming.streaming_total_output_tokens, 40);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 40);
    assert_eq!(seen, 60);
}

#[test]
fn test_compute_streaming_tps_uses_latest_observed_snapshot_instead_of_current_repaint_time() {
    let mut app = create_test_app();
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(20));
    app.streaming.streaming_tps_observed_output_tokens = 40;
    app.streaming.streaming_tps_observed_elapsed = Duration::from_secs(10);

    let tps = app.compute_streaming_tps().expect("tps");
    assert!(tps > 3.9 && tps < 4.1, "unexpected tps: {tps}");
}

#[test]
fn test_compute_streaming_tps_does_not_decay_on_redundant_usage_snapshots() {
    let mut app = create_test_app();
    let mut seen = 0;

    app.streaming.streaming_tps_collect_output = true;
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(10));
    app.accumulate_streaming_output_tokens(40, &mut seen);
    let initial_tps = app.compute_streaming_tps().expect("initial tps");

    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(30));
    app.accumulate_streaming_output_tokens(40, &mut seen);

    let tps = app.compute_streaming_tps().expect("tps");
    assert!(
        initial_tps > 3.9 && initial_tps < 4.1,
        "unexpected initial tps: {initial_tps}"
    );
    assert!(
        tps > 3.9 && tps < 4.1,
        "unexpected tps after redundant snapshot: {tps}"
    );
}

#[test]
fn test_compute_streaming_tps_bursty_stream_simulation_stays_constant_between_real_updates() {
    let mut app = create_test_app();
    let mut seen = 0;

    app.streaming.streaming_tps_collect_output = true;

    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(2));
    app.accumulate_streaming_output_tokens(10, &mut seen);
    let tps_after_first_burst = app.compute_streaming_tps().expect("tps after first burst");

    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(5));
    app.accumulate_streaming_output_tokens(10, &mut seen);
    let tps_after_idle_gap = app.compute_streaming_tps().expect("tps after idle gap");

    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(6));
    app.accumulate_streaming_output_tokens(30, &mut seen);
    let tps_after_second_burst = app.compute_streaming_tps().expect("tps after second burst");

    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(9));
    app.accumulate_streaming_output_tokens(30, &mut seen);
    let tps_after_second_idle_gap = app
        .compute_streaming_tps()
        .expect("tps after second idle gap");

    assert!(
        tps_after_first_burst > 4.9 && tps_after_first_burst < 5.1,
        "unexpected first burst tps: {tps_after_first_burst}"
    );
    assert!(
        (tps_after_idle_gap - tps_after_first_burst).abs() < 0.01,
        "tps changed without new tokens: first={tps_after_first_burst} idle={tps_after_idle_gap}"
    );
    assert!(
        tps_after_second_burst > 4.9 && tps_after_second_burst < 5.1,
        "unexpected second burst tps: {tps_after_second_burst}"
    );
    assert!(
        (tps_after_second_idle_gap - tps_after_second_burst).abs() < 0.01,
        "tps changed without new tokens: second={tps_after_second_burst} idle={tps_after_second_idle_gap}"
    );
}

#[test]
fn test_streaming_tps_timer_resume_pause_reset_lifecycle() {
    let mut app = create_test_app();

    assert_eq!(app.current_streaming_tps_elapsed(), Duration::ZERO);
    assert!(!app.streaming.streaming_tps_collect_output);

    app.resume_streaming_tps();
    assert!(app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_some());

    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(2));
    app.pause_streaming_tps(true);
    assert!(app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_none());
    assert!(app.streaming.streaming_tps_elapsed >= Duration::from_secs(2));

    let elapsed_after_pause = app.streaming.streaming_tps_elapsed;
    app.pause_streaming_tps(false);
    assert!(!app.streaming.streaming_tps_collect_output);
    assert_eq!(app.streaming.streaming_tps_elapsed, elapsed_after_pause);

    app.streaming.streaming_total_output_tokens = 42;
    app.streaming.streaming_tps_observed_output_tokens = 42;
    app.streaming.streaming_tps_observed_elapsed = elapsed_after_pause;
    app.reset_streaming_tps();

    assert_eq!(app.streaming.streaming_tps_elapsed, Duration::ZERO);
    assert_eq!(app.streaming.streaming_total_output_tokens, 0);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 0);
    assert_eq!(app.streaming.streaming_tps_observed_elapsed, Duration::ZERO);
    assert!(!app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_none());
}

#[test]
fn test_compute_streaming_tps_requires_tokens_and_minimum_elapsed() {
    let mut app = create_test_app();

    app.streaming.streaming_tps_observed_elapsed = Duration::from_secs(10);
    assert!(app.compute_streaming_tps().is_none());

    app.streaming.streaming_tps_observed_output_tokens = 10;
    app.streaming.streaming_tps_observed_elapsed = Duration::from_millis(100);
    assert!(app.compute_streaming_tps().is_none());

    app.streaming.streaming_tps_observed_elapsed = Duration::from_millis(250);
    let tps = app.compute_streaming_tps().expect("tps above threshold");
    assert!(tps > 35.0 && tps <= 40.0, "unexpected tps: {tps}");
}

#[test]
fn test_accumulate_streaming_output_tokens_counts_provider_usage_reset_once() {
    let mut app = create_test_app();
    let mut seen = 80;

    app.streaming.streaming_tps_collect_output = true;
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(10));

    app.accumulate_streaming_output_tokens(20, &mut seen);
    assert_eq!(app.streaming.streaming_total_output_tokens, 20);
    assert_eq!(seen, 20);

    app.accumulate_streaming_output_tokens(25, &mut seen);
    assert_eq!(app.streaming.streaming_total_output_tokens, 25);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 25);
    assert_eq!(seen, 25);
}

#[test]
fn test_streaming_tps_late_final_usage_after_pause_uses_paused_elapsed() {
    let mut app = create_test_app();
    let mut seen = 0;

    app.streaming.streaming_tps_collect_output = true;
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(10));
    app.pause_streaming_tps(true);

    assert!(app.streaming.streaming_tps_start.is_none());
    assert!(app.streaming.streaming_tps_elapsed >= Duration::from_secs(10));

    app.accumulate_streaming_output_tokens(40, &mut seen);

    assert_eq!(app.streaming.streaming_total_output_tokens, 40);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 40);
    assert!(app.streaming.streaming_tps_observed_elapsed >= Duration::from_secs(10));
    let tps = app.compute_streaming_tps().expect("late tps");
    assert!(tps > 3.0 && tps <= 4.0, "unexpected late tps: {tps}");
}

#[test]
fn test_begin_kv_cache_request_stops_tps_collection_until_output_resumes() {
    let mut app = create_test_app();
    let mut seen = 0;

    app.streaming.streaming_tps_collect_output = true;
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(3));

    app.begin_kv_cache_request(&[Message::user("next")], &[], "system", "dynamic");

    assert!(!app.streaming.streaming_tps_collect_output);
    assert!(app.streaming.streaming_tps_start.is_none());
    assert!(app.streaming.streaming_tps_elapsed >= Duration::from_secs(3));

    app.accumulate_streaming_output_tokens(20, &mut seen);
    assert_eq!(app.streaming.streaming_total_output_tokens, 0);
    assert_eq!(seen, 20);

    app.resume_streaming_tps();
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(2));
    app.accumulate_streaming_output_tokens(50, &mut seen);

    assert_eq!(app.streaming.streaming_total_output_tokens, 30);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 30);
    assert!(app.streaming.streaming_tps_observed_elapsed >= Duration::from_secs(5));
}

#[test]
fn test_streaming_tps_accumulates_multiple_generation_segments_excluding_paused_gap() {
    let mut app = create_test_app();
    let mut seen = 0;

    app.resume_streaming_tps();
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(2));
    app.accumulate_streaming_output_tokens(10, &mut seen);

    app.pause_streaming_tps(true);
    let elapsed_after_first_segment = app.streaming.streaming_tps_elapsed;
    assert!(elapsed_after_first_segment >= Duration::from_secs(2));

    app.resume_streaming_tps();
    app.streaming.streaming_tps_start = Some(Instant::now() - Duration::from_secs(3));
    app.accumulate_streaming_output_tokens(30, &mut seen);

    assert_eq!(app.streaming.streaming_total_output_tokens, 30);
    assert_eq!(app.streaming.streaming_tps_observed_output_tokens, 30);
    assert!(app.streaming.streaming_tps_observed_elapsed >= Duration::from_secs(5));
    let tps = app.compute_streaming_tps().expect("segmented tps");
    assert!(tps > 5.0 && tps <= 6.0, "unexpected segmented tps: {tps}");
}
