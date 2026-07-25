#[test]
fn test_save_and_restore_reload_state_preserves_queued_messages() {
    let mut app = create_test_app();
    let session_id = format!("test-reload-{}", std::process::id());

    app.input = "draft".to_string();
    app.cursor_pos = 3;
    app.queued_messages.push("queued one".to_string());
    app.queued_messages.push("queued two".to_string());
    app.hidden_queued_system_messages
        .push("continue silently".to_string());
    app.save_input_for_reload(&session_id);

    let restored = App::restore_input_for_reload(&session_id).expect("reload state should exist");
    assert_eq!(restored.input, "draft");
    assert_eq!(restored.cursor, 3);
    assert_eq!(restored.queued_messages, vec!["queued one", "queued two"]);
    assert_eq!(
        restored.hidden_queued_system_messages,
        vec!["continue silently"]
    );

    assert!(App::restore_input_for_reload(&session_id).is_none());
}

#[test]
fn test_new_for_remote_restored_queued_messages_stay_queued_until_remote_idle() {
    let mut app = create_test_app();
    let session_id = format!("test-remote-queued-restore-{}", std::process::id());

    app.queued_messages.push("queued one".to_string());
    app.queued_messages.push("queued two".to_string());
    app.hidden_queued_system_messages
        .push("continue silently".to_string());
    app.save_input_for_reload(&session_id);

    let restored = App::new_for_remote(Some(session_id));
    assert_eq!(restored.queued_messages(), &["queued one", "queued two"]);
    assert_eq!(
        restored.hidden_queued_system_messages,
        vec!["continue silently"]
    );
    assert!(!restored.pending_queued_dispatch);
    assert!(!restored.is_processing);
    assert!(matches!(restored.status, ProcessingStatus::Idle));
}

#[test]
fn test_save_and_restore_startup_submission_preserves_pending_images() {
    with_temp_jcode_home(|| {
        let session_id = "session_startup_prompt";
        App::save_startup_submission_for_session(
            session_id,
            "describe this".to_string(),
            vec![("image/png".to_string(), "abc123".to_string())],
        );

        let restored =
            App::restore_input_for_reload(session_id).expect("startup submission should restore");
        assert_eq!(restored.input, "describe this");
        assert!(restored.submit_on_restore);
        assert_eq!(restored.pending_images.len(), 1);
        assert_eq!(restored.pending_images[0].0, "image/png");
        assert_eq!(restored.pending_images[0].1, "abc123");
    });
}

#[test]
fn test_save_and_restore_reload_state_preserves_interleave_and_pending_retry() {
    let mut app = create_test_app();
    let session_id = format!("test-reload-pending-{}", std::process::id());

    app.input = "draft".to_string();
    app.cursor_pos = 5;
    app.interleave_message = Some("urgent now".to_string());
    app.pending_soft_interrupts = vec![
        "already sent one".to_string(),
        "already sent two".to_string(),
    ];
    app.pending_soft_interrupt_requests = vec![(17, "already sent two".to_string())];
    app.rate_limit_pending_message = Some(PendingRemoteMessage {
        content: "retry me".to_string(),
        images: vec![("image/png".to_string(), "abc123".to_string())],
        is_system: true,
        system_reminder: Some("continue silently".to_string()),
        auto_retry: true,
        retry_attempts: 2,
        retry_at: None,
    });
    app.rate_limit_reset = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
    app.save_input_for_reload(&session_id);

    let restored = App::restore_input_for_reload(&session_id).expect("reload state should exist");
    assert_eq!(restored.interleave_message.as_deref(), Some("urgent now"));
    assert_eq!(
        restored.pending_soft_interrupts,
        vec!["already sent one", "already sent two"]
    );
    assert_eq!(
        restored.pending_soft_interrupt_resend,
        Some(vec!["already sent two".to_string()])
    );

    let pending = restored
        .rate_limit_pending_message
        .expect("pending retry should restore");
    assert_eq!(pending.content, "retry me");
    assert_eq!(
        pending.images,
        vec![("image/png".to_string(), "abc123".to_string())]
    );
    assert!(pending.is_system);
    assert_eq!(
        pending.system_reminder.as_deref(),
        Some("continue silently")
    );
    assert!(pending.auto_retry);
    assert_eq!(pending.retry_attempts, 2);
    assert!(pending.retry_at.is_some());
    assert!(restored.rate_limit_reset.is_some());
}

#[test]
fn test_save_and_restore_reload_state_promotes_inflight_prompt_to_startup_submission() {
    let mut app = create_test_app();
    let session_id = format!("test-reload-inflight-prompt-{}", std::process::id());

    app.rate_limit_pending_message = Some(PendingRemoteMessage {
        content: "finish the refactor".to_string(),
        images: vec![("image/png".to_string(), "abc123".to_string())],
        is_system: false,
        system_reminder: None,
        auto_retry: false,
        retry_attempts: 0,
        retry_at: None,
    });
    app.rate_limit_reset = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
    app.save_input_for_reload(&session_id);

    let restored = App::restore_input_for_reload(&session_id).expect("reload state should exist");
    assert_eq!(restored.input, "finish the refactor");
    assert_eq!(restored.cursor, "finish the refactor".len());
    assert!(
        restored.submit_on_restore,
        "in-flight prompt should resume automatically"
    );
    assert_eq!(restored.pending_images.len(), 1);
    assert!(
        restored.rate_limit_pending_message.is_none(),
        "promoted startup submission should not linger as a passive pending retry"
    );
}

#[test]
fn test_save_and_restore_reload_state_preserves_observe_mode() {
    let mut app = create_test_app();
    let session_id = format!("test-reload-observe-{}", std::process::id());

    app.set_observe_mode_enabled(true, true);
    app.observe_page_markdown = "# Observe\n\nPersist me through reload.".to_string();
    app.observe_page_updated_at_ms = 42;
    app.save_input_for_reload(&session_id);

    let restored = App::restore_input_for_reload(&session_id).expect("reload state should exist");
    assert!(restored.observe_mode_enabled);
    assert_eq!(
        restored.observe_page_markdown,
        "# Observe\n\nPersist me through reload."
    );
    assert_eq!(restored.observe_page_updated_at_ms, 42);
}

#[test]
fn test_save_and_restore_reload_state_preserves_split_view_mode() {
    let mut app = create_test_app();
    let session_id = format!("test-reload-splitview-{}", std::process::id());

    app.set_split_view_enabled(true, true);
    app.save_input_for_reload(&session_id);

    let restored = App::restore_input_for_reload(&session_id).expect("reload state should exist");
    assert!(restored.split_view_enabled);
}

#[test]
fn test_new_for_remote_restores_observe_mode_from_reload_state() {
    let mut app = create_test_app();
    let session_id = format!("test-remote-observe-{}", std::process::id());

    app.set_observe_mode_enabled(true, true);
    app.observe_page_markdown = "# Observe\n\nRestored after reload.".to_string();
    app.observe_page_updated_at_ms = 99;
    app.save_input_for_reload(&session_id);

    let restored = App::new_for_remote(Some(session_id));
    assert!(restored.observe_mode_enabled());
    let page = restored
        .side_panel()
        .focused_page()
        .expect("observe page should be focused");
    assert_eq!(page.id, "observe");
    assert!(page.content.contains("Restored after reload."));
}

#[test]
fn test_new_for_remote_restores_split_view_from_reload_state() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let session_id = "test-remote-splitview";

        app.set_split_view_enabled(true, true);
        app.save_input_for_reload(session_id);

        let restored = App::new_for_remote(Some(session_id.to_string()));
        assert!(restored.split_view_enabled());
        let page = restored
            .side_panel()
            .focused_page()
            .expect("split view page should be focused");
        assert_eq!(page.id, "split_view");
        assert!(page.content.contains("Split View"));
    });
}

#[test]
fn test_restore_reload_state_supports_legacy_input_format() {
    let session_id = format!("test-reload-legacy-{}", std::process::id());
    let jcode_dir = crate::storage::jcode_dir().unwrap();
    let path = jcode_dir.join(format!("client-input-{}", session_id));
    std::fs::write(&path, "2\nhello").unwrap();

    let restored =
        App::restore_input_for_reload(&session_id).expect("legacy reload state should restore");
    assert_eq!(restored.input, "hello");
    assert_eq!(restored.cursor, 2);
    assert!(restored.queued_messages.is_empty());
}

#[test]
fn test_new_for_remote_requeues_restored_pending_soft_interrupts() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let session_id = "test-remote-restore";

        app.interleave_message = Some("local interleave".to_string());
        app.pending_soft_interrupts = vec!["sent one".to_string(), "sent two".to_string()];
        app.pending_soft_interrupt_requests =
            vec![(101, "sent one".to_string()), (102, "sent two".to_string())];
        app.queued_messages.push("queued later".to_string());
        app.save_input_for_reload(session_id);

        let restored = App::new_for_remote(Some(session_id.to_string()));
        assert!(restored.interleave_message.is_none());
        assert_eq!(
            restored.queued_messages(),
            &["local interleave", "sent one", "sent two", "queued later"]
        );
    });
}

#[test]
fn test_new_for_remote_restored_interleave_triggers_dispatch_state() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let session_id = "test-remote-interleave-dispatch";

        app.interleave_message = Some("interrupt after reload".to_string());
        app.save_input_for_reload(session_id);

        let mut restored = App::new_for_remote(Some(session_id.to_string()));
        assert!(restored.interleave_message.is_none());
        assert_eq!(restored.queued_messages(), &["interrupt after reload"]);
        assert!(!restored.pending_queued_dispatch);
        assert!(!restored.is_processing);
        assert!(matches!(restored.status, ProcessingStatus::Idle));

        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let mut remote = crate::tui::backend::RemoteConnection::dummy();
        rt.block_on(super::remote::process_remote_followups(
            &mut restored,
            &mut remote,
        ));
        assert_eq!(restored.queued_messages(), &["interrupt after reload"]);
        assert!(!restored.is_processing);

        remote.mark_history_loaded();
        rt.block_on(super::remote::process_remote_followups(
            &mut restored,
            &mut remote,
        ));

        assert!(restored.queued_messages().is_empty());
        assert!(restored.is_processing);
        assert!(matches!(restored.status, ProcessingStatus::Sending));
        assert!(restored.display_messages().iter().any(|message| {
            message.role == "user" && message.content == "interrupt after reload"
        }));
    });
}
