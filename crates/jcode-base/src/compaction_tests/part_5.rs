#[test]
fn test_persisted_state_round_trip_preserves_compacted_view() {
    let mut manager = rolling_test_manager().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "x".repeat(40)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);
    manager
        .hard_compact_with(&messages)
        .expect("should compact before persisting");

    let persisted = manager
        .persisted_state()
        .expect("compaction state should be exportable");
    let expected = manager.messages_for_api_with(&messages);

    let mut restored = rolling_test_manager().with_budget(500);
    restored.restore_persisted_state(&persisted, messages.len());
    let restored_msgs = restored.messages_for_api_with(&messages);

    assert_eq!(restored.compacted_count, persisted.compacted_count);
    assert_eq!(restored_msgs.len(), expected.len());
    match &restored_msgs[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected restored summary block"),
    }
}

// ── context_usage accuracy ──────────────────────────────────────

#[test]
fn test_context_usage_with_both_estimate_and_observed() {
    let mut manager = rolling_test_manager().with_budget(200_000);
    // Build messages totalling ~50k chars = ~12.5k token estimate
    let mut messages = Vec::new();
    for i in 0..50 {
        messages.push(make_text_message(
            Role::User,
            &format!("{} {}", i, "a".repeat(1000)),
        ));
        manager.notify_message_added();
    }

    // Without observed tokens, usage should be based on char estimate
    let usage_no_observed = manager.context_usage_with(&messages);
    assert!(
        usage_no_observed < 0.2,
        "char estimate should be low: {}",
        usage_no_observed
    );

    // With observed tokens at 160k, should use observed (higher) value
    manager.update_observed_input_tokens(160_000);
    let usage_with_observed = manager.context_usage_with(&messages);
    assert!(
        usage_with_observed >= 0.79,
        "should use observed tokens: {}",
        usage_with_observed
    );
}

#[test]
fn test_context_usage_after_compaction_resets_observed() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("msg {} pad {}", i, "x".repeat(50)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(960);

    // Hard compact should reset observed_input_tokens
    manager
        .hard_compact_with(&messages)
        .expect("should compact");
    assert!(
        manager.observed_input_tokens.is_none(),
        "observed_input_tokens should be cleared after hard compact"
    );

    // After compaction, usage should be based on char estimate of remaining messages only
    let post_usage = manager.context_usage_with(&messages);
    // The remaining messages are small, so usage should be well below the critical threshold
    assert!(
        post_usage < CRITICAL_THRESHOLD,
        "post-compaction usage should be below critical: {}",
        post_usage
    );
}

#[test]
fn test_recover_within_budget_drops_messages_without_truncation() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("msg {} pad {}", i, "x".repeat(40)),
        ));
        manager.notify_message_added();
    }
    // Push well over budget so recovery triggers.
    manager.update_observed_input_tokens(2_000);

    let recovery = manager.recover_within_budget(&mut messages);
    assert!(
        recovery.dropped.unwrap_or(0) > 0,
        "should drop old messages"
    );
    // Dropping turns alone should fit the small remaining tail, so no
    // truncation escalation is needed.
    assert_eq!(
        recovery.truncated, 0,
        "should not truncate when dropping turns fits the budget"
    );
    assert!(recovery.did_anything());
    assert!(
        manager.context_usage_with(&messages) <= 1.0,
        "context should be back under budget after recovery"
    );
}

#[test]
fn test_recover_within_budget_truncates_when_tail_still_too_large() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    // Build tool-use/tool-result pairs whose results are each individually
    // larger than the whole budget. After hard compaction drops down to the
    // minimum kept tail, the surviving tool result is still far over budget, so
    // recovery must escalate to truncation (which only acts on tool results).
    for i in 0..10 {
        let id = format!("tool_{i}");
        messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: "bash".to_string(),
                input: serde_json::json!({ "command": "cat big.log" }),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        });
        manager.notify_message_added();
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id,
                content: format!("huge {} {}", i, "y".repeat(20_000)),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        });
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(50_000);

    let recovery = manager.recover_within_budget(&mut messages);
    assert!(recovery.did_anything());
    assert!(
        recovery.truncated > 0,
        "should escalate to truncation when the remaining tail is still too large"
    );
}

#[test]
fn test_recover_within_budget_summary_line_variants() {
    let dropped_only = EmergencyRecovery {
        pre_usage: 1.6,
        dropped: Some(7),
        truncated: 0,
    };
    let line = dropped_only.summary_line(dropped_only.pre_usage);
    assert!(line.contains("dropped 7 old messages"));
    assert!(line.contains("160%"));
    assert!(!line.contains("truncated"));

    let dropped_and_truncated = EmergencyRecovery {
        pre_usage: 2.0,
        dropped: Some(3),
        truncated: 2,
    };
    let line = dropped_and_truncated.summary_line(dropped_and_truncated.pre_usage);
    assert!(line.contains("dropped 3 old messages"));
    assert!(line.contains("truncated 2 tool result(s)"));

    let truncate_only = EmergencyRecovery {
        pre_usage: 1.2,
        dropped: None,
        truncated: 5,
    };
    let line = truncate_only.summary_line(truncate_only.pre_usage);
    assert!(line.contains("shortened 5 large tool result(s)"));
    assert!(!line.contains("dropped"));
}
