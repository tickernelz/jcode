fn rolling_test_manager() -> CompactionManager {
    let mut manager = CompactionManager::new();
    manager.engine = crate::config::CompactionEngine::Rolling;
    manager
}

#[tokio::test]
async fn lcm_synthetic_thirty_trace_scorecard_preserves_planted_facts() -> Result<()> {
    let (facts_a, messages_a) = lcm_cert_trace(7, 1);
    let (facts_b, messages_b) = lcm_cert_trace(7, 1);
    assert_eq!(facts_a, facts_b);
    assert_eq!(
        messages_a.iter().map(content_text).collect::<Vec<_>>(),
        messages_b.iter().map(content_text).collect::<Vec<_>>()
    );
    assert!(
        (0..LCM_CERT_TRACE_COUNT)
            .flat_map(|trace| (0..LCM_CERT_CYCLES).map(move |cycle| lcm_cert_trace(trace, cycle)))
            .all(|(_, messages)| messages.len() > 10)
    );
    let provider: Arc<dyn Provider> = Arc::new(FactPreservingProvider);
    let mut results = serde_json::Map::new();

    for strategy in ["rolling", "native_lcm"] {
        let mut recovered = 0;
        let mut source_chars = 0;
        let mut output_chars = 0;
        let mut fabricated_completion_claims = 0;
        let mut latencies_us = Vec::with_capacity(LCM_CERT_TRACE_COUNT * LCM_CERT_CYCLES);

        for trace in 0..LCM_CERT_TRACE_COUNT {
            let mut prior_summary: Option<String> = None;
            for cycle in 0..LCM_CERT_CYCLES {
                let (facts, mut messages) = lcm_cert_trace(trace, cycle);
                if let Some(summary) = prior_summary.take() {
                    messages.insert(0, make_text_message(Role::Assistant, &summary));
                }
                source_chars += messages.iter().map(message_char_count).sum::<usize>();
                let started = Instant::now();
                let summary = if strategy == "native_lcm" {
                    generate_lcm_compaction_artifact(
                        Arc::clone(&provider),
                        messages,
                        None,
                        None,
                        LcmJobPriority::Background,
                    )
                    .await?
                    .summary_text
                } else {
                    generate_compaction_artifact(Arc::clone(&provider), messages, None)
                        .await?
                        .summary_text
                };
                latencies_us.push(started.elapsed().as_micros());
                output_chars += summary.len();
                fabricated_completion_claims += summary.matches("completed successfully").count();
                if cycle + 1 == LCM_CERT_CYCLES {
                    recovered += lcm_cert_active_recall(&summary, &facts);
                }
                prior_summary = Some(summary);
            }
        }

        let planted = LCM_CERT_TRACE_COUNT * LCM_CERT_FACTS_PER_TRACE;
        let recall = recovered as f64 / planted as f64;
        let wilson_95 = lcm_cert_wilson_95(recovered, planted);
        let ratio = output_chars as f64 / source_chars as f64;
        let p50 = lcm_cert_percentile(&mut latencies_us.clone(), 50);
        let p95 = lcm_cert_percentile(&mut latencies_us, 95);
        let passed = recall >= LCM_CERT_MIN_RECALL
            && wilson_95.0 >= LCM_CERT_MIN_WILSON_LOWER
            && ratio <= LCM_CERT_MAX_OUTPUT_SOURCE_RATIO
            && p95 <= LCM_CERT_MAX_P95_US
            && fabricated_completion_claims == 0
            && LCM_CERT_CYCLES >= 2;
        results.insert(
            strategy.to_string(),
            serde_json::json!({
                "active_fact_recall": recall,
                "active_fact_recall_wilson_95": {"lower": wilson_95.0, "upper": wilson_95.1},
                "source_output_character_ratio": ratio,
                "latency_p50_us": p50,
                "latency_p95_us": p95,
                "fabricated_completion_claims": fabricated_completion_claims,
                "multi_cycle_compactions": LCM_CERT_TRACE_COUNT * LCM_CERT_CYCLES,
                "passed": passed,
            }),
        );
    }

    let overall_passed = results.values().all(|result| result["passed"] == true);
    let scorecard = serde_json::json!({
        "schema_version": 1,
        "corpus": {
            "name": "neutral_coding_trace_v3",
            "trace_count": LCM_CERT_TRACE_COUNT,
            "cycles_per_trace": LCM_CERT_CYCLES,
            "facts_per_trace": LCM_CERT_FACTS_PER_TRACE,
            "messages_per_cycle": 12,
            "scrubbed": true,
            "generator": "trace 0..30, cycle 0..2, five fixed kind pairs plus fixed recall pair; canaries only in cycle 0; exact contract in lcm_cert_trace rustdoc",
            "oracle": "deterministic-oracle-v1",
        },
        "thresholds": {
            "min_active_fact_recall": LCM_CERT_MIN_RECALL,
            "min_active_fact_recall_wilson_95_lower": LCM_CERT_MIN_WILSON_LOWER,
            "max_source_output_character_ratio": LCM_CERT_MAX_OUTPUT_SOURCE_RATIO,
            "max_latency_p95_us": LCM_CERT_MAX_P95_US,
            "max_fabricated_completion_claims": 0,
            "min_cycles_per_trace": 2,
        },
        "results": results,
        "passed": overall_passed,
    });
    if let Some(path) = std::env::var_os("JCODE_LCM_CERT_OUTPUT") {
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&scorecard)?)?;
    }

    assert_eq!(scorecard["corpus"]["trace_count"], 30);
    assert_eq!(scorecard["corpus"]["cycles_per_trace"], 2);
    assert!(overall_passed, "{scorecard:#}");
    Ok(())
}

#[test]
fn test_new_manager() {
    let manager = rolling_test_manager();
    assert_eq!(manager.compacted_count, 0);
    assert!(manager.active_summary.is_none());
    assert!(!manager.is_compacting());
}

#[test]
fn test_notify_message_added() {
    let mut manager = rolling_test_manager();
    manager.notify_message_added();
    manager.notify_message_added();
    assert_eq!(manager.total_turns, 2);
}

#[test]
fn test_restored_messages_do_not_trigger_compaction_immediately() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("restored {}", i)));
    }
    manager.seed_restored_messages(messages.len());
    manager.update_observed_input_tokens(900);

    assert!(
        !manager.should_compact_with(&messages),
        "restored history should not compact until a new message is added"
    );
}

#[test]
fn test_new_message_after_restore_reenables_compaction() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("restored {}", i)));
    }
    manager.seed_restored_messages(messages.len());
    manager.update_observed_input_tokens(900);
    assert!(!manager.should_compact_with(&messages));

    messages.push(make_text_message(Role::User, "new turn after restore"));
    manager.notify_message_added();

    assert!(
        manager.should_compact_with(&messages),
        "compaction should resume once a genuinely new message is added"
    );
}

#[test]
fn test_token_estimate() {
    let manager = rolling_test_manager();
    // 100 chars = ~25 tokens (plus 18k overhead for full budget)
    let messages = vec![make_text_message(Role::User, &"x".repeat(100))];
    let estimate = manager.token_estimate_with(&messages);
    // With DEFAULT_TOKEN_BUDGET and 18k overhead: 25 + 18000 = 18025
    assert!((18_000..19_000).contains(&estimate));
}

#[test]
fn test_should_compact() {
    let mut manager = rolling_test_manager().with_budget(100); // Very small budget

    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("Message {} with some content", i),
        ));
        manager.notify_message_added();
    }

    assert!(manager.should_compact_with(&messages));
}

#[test]
fn test_context_usage_prefers_observed_tokens() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let messages = vec![make_text_message(Role::User, "short message")];
    manager.notify_message_added();
    manager.update_observed_input_tokens(900);

    assert!(manager.context_usage_with(&messages) >= 0.90);
    assert!(manager.effective_token_count_with(&messages) >= 900);
}

#[test]
fn test_should_compact_uses_observed_tokens() {
    let mut manager = rolling_test_manager().with_budget(1_000);

    let mut messages = Vec::new();
    for _ in 0..12 {
        messages.push(make_text_message(Role::User, "x"));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(850);

    assert!(manager.should_compact_with(&messages));
}

#[test]
fn test_messages_for_api_no_summary() {
    let mut manager = rolling_test_manager();
    let messages = vec![
        make_text_message(Role::User, "Hello"),
        make_text_message(Role::Assistant, "Hi!"),
    ];
    manager.notify_message_added();
    manager.notify_message_added();

    let msgs = manager.messages_for_api_with(&messages);
    assert_eq!(msgs.len(), 2);
}

#[tokio::test]
async fn test_force_compact_applies_summary() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("Turn {} {}", i, "x".repeat(120)),
        ));
        manager.notify_message_added();
    }

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager
        .force_compact_with(&messages, provider)
        .expect("manual compaction should start");

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        manager.check_and_apply_compaction();
        if manager.stats().has_summary {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        manager.stats().has_summary,
        "summary should be applied after compaction task completes"
    );

    // After compaction, compacted_count should be > 0
    assert!(manager.compacted_count > 0);

    let msgs = manager.messages_for_api_with(&messages);
    assert!(msgs.len() < 30);
    let first = msgs.first().expect("summary message missing");
    assert_eq!(first.role, Role::User);
    match &first.content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
        }
        _ => panic!("expected text summary block"),
    }
}

// ── ensure_context_fits tests ──────────────────────────────

#[tokio::test]
async fn test_guard_below_80_does_nothing() {
    let mut manager = rolling_test_manager().with_budget(10_000);
    let mut messages = Vec::new();
    for i in 0..15 {
        messages.push(make_text_message(Role::User, &format!("msg {}", i)));
        manager.notify_message_added();
    }
    // Char estimate is tiny, observed tokens well below 80%
    manager.update_observed_input_tokens(5_000);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert_eq!(
        action,
        CompactionAction::None,
        "should do nothing below 80%"
    );
    assert!(
        !manager.is_compacting(),
        "should NOT start background compaction below 80%"
    );
    assert_eq!(manager.compacted_count, 0);
}

#[tokio::test]
async fn test_guard_between_80_and_95_starts_background_only() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("msg {}", i)));
        manager.notify_message_added();
    }
    // 85% usage — above 80% threshold but below 95% critical
    manager.update_observed_input_tokens(850);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert_eq!(
        action,
        CompactionAction::BackgroundStarted {
            trigger: "reactive".to_string()
        },
        "should start background compaction at 85%"
    );
    assert!(
        manager.is_compacting(),
        "SHOULD start background compaction at 85%"
    );
    assert_eq!(
        manager.compacted_count, 0,
        "compacted_count should stay 0 (no hard compact)"
    );
}

/// Regression: a hard compact that runs while a background (reactive)
/// compaction is in flight must abort the background task and discard its
/// stale `pending_cutoff`. Otherwise, when the background task completes,
/// `check_and_apply_compaction_with` adds the stale cutoff on top of the
/// already-advanced `compacted_count`, double-compacting and wiping out all
/// live messages (observed as "kept 0 recent messages").
#[tokio::test]
async fn test_hard_compact_aborts_inflight_background_compaction() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "z".repeat(60)),
        ));
        manager.notify_message_added();
    }

    // Start a background reactive compaction (85% usage, below critical).
    manager.update_observed_input_tokens(850);
    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager.maybe_start_compaction_with(&messages, provider);
    assert!(
        manager.is_compacting(),
        "background compaction should be in flight"
    );
    let inflight_cutoff = manager.pending_cutoff;
    assert!(inflight_cutoff > 0, "background task should have a cutoff");

    // Now pressure spikes to critical and we hard-compact synchronously while
    // the background task is still pending.
    let dropped = manager
        .hard_compact_with(&messages)
        .expect("hard compact should succeed");
    assert!(dropped > 0);

    // The in-flight background compaction must have been aborted/discarded.
    assert!(
        !manager.is_compacting(),
        "hard compact must abort the in-flight background compaction"
    );
    assert_eq!(
        manager.pending_cutoff, 0,
        "stale pending_cutoff must be reset"
    );

    let compacted_after_hard = manager.compacted_count;

    // Simulate the (now-aborted) background task completion path. With the fix
    // there is no pending task, so this is a no-op and must NOT advance
    // compacted_count again.
    manager.check_and_apply_compaction_with(&messages);
    assert_eq!(
        manager.compacted_count, compacted_after_hard,
        "completing after abort must not double-advance compacted_count"
    );

    // Live messages must survive: active_messages_count stays positive.
    assert!(
        manager.active_messages_count() > 0,
        "must keep recent messages live, not wipe everything to 0"
    );
    let active = manager.active_messages(&messages);
    assert!(
        !active.is_empty(),
        "active message slice must not be empty after hard compact"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_threshold_pending_compaction_wait_does_not_block_tokio_executor() {
    let mut manager = rolling_test_manager().with_budget(100);
    let mut messages = Vec::new();
    for index in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("critical turn {index} {}", "x".repeat(100)),
        ));
        manager.notify_message_added();
    }
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    manager.pending_task = Some(tokio::spawn(async move {
        let _ = started_tx.send(());
        futures::future::pending::<Result<CompactionResult>>().await
    }));
    manager.pending_trigger = Some("reactive".to_string());
    manager.pending_cutoff = 10;
    started_rx.await.expect("pending compaction started");

    let start = std::time::Instant::now();
    let action = manager.ensure_context_fits(&messages, Arc::new(MockSummaryProvider));

    assert!(
        start.elapsed() < std::time::Duration::from_millis(100),
        "hard-threshold fallback slept on the runtime thread for {:?}",
        start.elapsed()
    );
    assert!(matches!(action, CompactionAction::HardCompacted(_)));
    assert!(!manager.is_compacting());
}

#[tokio::test]
async fn dropping_compaction_manager_aborts_pending_lcm_task() {
    let mut manager = rolling_test_manager();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = started_tx.send(());
        futures::future::pending::<Result<CompactionResult>>().await
    });
    let abort_handle = task.abort_handle();
    manager.pending_task = Some(task);
    manager.pending_trigger = Some("native_lcm".to_string());
    started_rx.await.expect("pending LCM task started");

    drop(manager);

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !abort_handle.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("manager drop must abort pending LCM task");
}

/// Defense-in-depth: if `compacted_count` advances while a background
/// compaction is in flight (so its `pending_cutoff` becomes stale), applying
/// the completed result must NOT over-advance `compacted_count` and wipe the
/// live tail. The stale result should be discarded instead.
#[tokio::test]
async fn test_stale_background_result_discarded_when_context_shrinks() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "q".repeat(60)),
        ));
        manager.notify_message_added();
    }

    manager.update_observed_input_tokens(850);
    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager.maybe_start_compaction_with(&messages, provider);
    assert!(manager.is_compacting());
    let pending = manager.pending_cutoff;
    assert!(pending > 0);

    // Simulate an interleaving mutation that advances compacted_count out from
    // under the in-flight task (e.g. a hard compact via a different path),
    // leaving only a small active tail.
    manager.compacted_count = messages.len() - 3;

    // Drain the background task to completion, then apply.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        manager.check_and_apply_compaction_with(&messages);
        if !manager.is_compacting() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!manager.is_compacting(), "task should have been drained");

    // The stale result must have been discarded: compacted_count stays where
    // the interleaving mutation left it, and the live tail survives.
    assert_eq!(
        manager.compacted_count,
        messages.len() - 3,
        "stale pending_cutoff must not advance compacted_count further"
    );
    assert!(
        manager.active_messages(&messages).len() >= 3,
        "live tail must survive a discarded stale compaction"
    );
    assert_eq!(manager.pending_cutoff, 0, "pending_cutoff must be reset");
}

#[tokio::test]
async fn test_stale_background_result_discarded_when_source_changes_at_same_length() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {i} content {}", "q".repeat(60)),
        ));
        manager.notify_message_added();
    }

    manager.update_observed_input_tokens(850);
    manager.maybe_start_compaction_with(&messages, Arc::new(MockSummaryProvider));
    assert!(manager.is_compacting());

    messages[0] = make_text_message(Role::User, "divergent history with the same message count");
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && manager.is_compacting() {
        manager.check_and_apply_compaction_with(&messages);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(!manager.is_compacting());
    assert_eq!(manager.compacted_count, 0);
    assert!(manager.active_summary.is_none());
    assert_eq!(manager.pending_source_fingerprint, None);
}

#[tokio::test]
async fn test_guard_at_95_triggers_hard_compact() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("message {} with padding {}", i, "x".repeat(50)),
        ));
        manager.notify_message_added();
    }
    // 96% usage — above critical threshold
    manager.update_observed_input_tokens(960);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert!(
        matches!(action, CompactionAction::HardCompacted(_)),
        "SHOULD hard-compact at 96%"
    );
    assert!(
        manager.compacted_count > 0,
        "compacted_count should increase after hard compact"
    );
    assert!(
        manager.active_summary.is_some(),
        "should have an emergency summary"
    );
}

#[tokio::test]
async fn test_guard_at_100_percent_drops_messages() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "y".repeat(80)),
        ));
        manager.notify_message_added();
    }
    // Over 100% — simulates the exact bug scenario
    manager.update_observed_input_tokens(1_050);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert!(
        matches!(action, CompactionAction::HardCompacted(_)),
        "MUST hard-compact when over 100%"
    );

    let api_messages = manager.messages_for_api_with(&messages);
    assert!(
        api_messages.len() < messages.len(),
        "API messages should be fewer after hard compact"
    );
    // First message should be the emergency summary
    match &api_messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected text summary block"),
    }
}

// ── hard_compact_with edge cases ────────────────────────────────

#[test]
fn test_hard_compact_too_few_messages() {
    let mut manager = rolling_test_manager().with_budget(100);
    let messages = vec![
        make_text_message(Role::User, "hello"),
        make_text_message(Role::Assistant, "hi"),
    ];
    manager.notify_message_added();
    manager.notify_message_added();

    let result = manager.hard_compact_with(&messages);
    assert!(
        result.is_err(),
        "should fail with only 2 messages (MIN_TURNS_TO_KEEP)"
    );
}

#[test]
fn test_hard_compact_preserves_recent_turns() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..25 {
        messages.push(make_text_message(Role::User, &format!("turn {}", i)));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(950);

    let dropped = manager
        .hard_compact_with(&messages)
        .expect("should compact");
    assert!(dropped > 0, "should drop some messages");
    assert!(dropped < 25, "should not drop ALL messages");

    let api_messages = manager.messages_for_api_with(&messages);
    // Should have summary + recent turns
    assert!(
        api_messages.len() >= 2,
        "should keep at least MIN_TURNS_TO_KEEP + summary"
    );
    assert!(
        api_messages.len() <= 15,
        "should have dropped a significant number"
    );
}

// ── safe_compaction_cutoff: tool call/result pair integrity ─────────

#[test]
fn test_safe_cutoff_preserves_tool_pairs() {
    // Messages: [user, assistant(tool_use), user(tool_result), assistant, user]
    // If cutoff tries to split between tool_use and tool_result, it should back up
    let messages = vec![
        make_text_message(Role::User, "do something"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool_1".to_string(),
                content: "file1.txt\nfile2.txt".to_string(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "I see the files"),
        make_text_message(Role::User, "thanks"),
    ];

    // Try to cut between tool_use (index 1) and tool_result (index 2)
    let cutoff = safe_compaction_cutoff(&messages, 2);
    // Should move back to include the tool_use at index 1
    assert!(
        cutoff <= 1,
        "cutoff should back up to include tool_use (got {})",
        cutoff
    );
}

#[test]
fn test_safe_cutoff_no_tool_pairs() {
    let messages = vec![
        make_text_message(Role::User, "hello"),
        make_text_message(Role::Assistant, "hi"),
        make_text_message(Role::User, "how are you"),
        make_text_message(Role::Assistant, "fine"),
    ];

    let cutoff = safe_compaction_cutoff(&messages, 2);
    assert_eq!(cutoff, 2, "no tool pairs, cutoff should stay unchanged");
}

#[test]
fn test_safe_cutoff_handles_chained_tool_dependencies_without_rescan() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_a".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file": "a.txt"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::User, "intermediate"),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "tool_a".to_string(),
                    content: "a contents".to_string(),
                    is_error: Some(false),
                },
                ContentBlock::ToolUse {
                    id: "tool_b".to_string(),
                    name: "grep".to_string(),
                    input: serde_json::json!({"pattern": "foo"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool_b".to_string(),
                content: "foo".to_string(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "done"),
    ];

    let cutoff = safe_compaction_cutoff(&messages, 3);
    assert_eq!(
        cutoff, 0,
        "cutoff should walk back through nested tool dependencies until the kept suffix is self-contained"
    );
}

// ── emergency_truncate_with ─────────────────────────────────────

#[test]
fn test_emergency_truncate_large_tool_results() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let big_result = "x".repeat(10_000); // Way over EMERGENCY_TOOL_RESULT_MAX_CHARS (4000)
    let mut messages = vec![
        make_text_message(Role::User, "run something"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "cat bigfile"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool_1".to_string(),
                content: big_result.clone(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "that's a big file"),
    ];
    for _ in &messages {
        manager.notify_message_added();
    }

    let truncated = manager.emergency_truncate_with(&mut messages);
    assert_eq!(truncated, 1, "should truncate exactly 1 tool result");

    // Check the truncated content
    if let ContentBlock::ToolResult { content, .. } = &messages[2].content[0] {
        assert!(
            content.len() < big_result.len(),
            "content should be shorter"
        );
        assert!(
            content.contains("truncated for context recovery"),
            "should have truncation marker"
        );
    } else {
        panic!("expected tool result");
    }
}

#[test]
fn test_emergency_truncate_skips_small_results() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "small output".to_string(),
            is_error: Some(false),
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];
    manager.notify_message_added();

    let truncated = manager.emergency_truncate_with(&mut messages);
    assert_eq!(truncated, 0, "should not truncate small results");
}

// ── Double compaction ───────────────────────────────────────────

#[test]
fn test_hard_compact_twice() {
    let mut manager = rolling_test_manager().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "z".repeat(40)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(480);

    // First hard compact
    let dropped1 = manager
        .hard_compact_with(&messages)
        .expect("first compact should work");
    assert!(dropped1 > 0);
    let count_after_first = manager.compacted_count;

    // Simulate more messages arriving after first compact
    for i in 30..45 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "z".repeat(40)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);

    // Second hard compact
    let dropped2 = manager
        .hard_compact_with(&messages)
        .expect("second compact should work");
    assert!(dropped2 > 0);
    assert!(
        manager.compacted_count > count_after_first,
        "compacted_count should increase"
    );

    // Summary should mention both compactions
    let api_messages = manager.messages_for_api_with(&messages);
    assert!(api_messages.len() < messages.len());
    match &api_messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected summary"),
    }
}

#[test]
fn test_hard_compact_clamps_pathological_compacted_count() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "x".repeat(200)),
        ));
        manager.notify_message_added();
    }

    // Reproduce the #175 bad state: bookkeeping says more messages were
    // compacted than exist in the current message vector. Before the fix,
    // active_messages() returned the full transcript in this state, so each
    // hard compaction appended another emergency marker and increased
    // compacted_count even further past messages.len().
    manager.compacted_count = 100;
    manager.active_summary = Some(Summary {
        text: "# Existing summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 100,
        original_turn_count: 100,
    });
    manager.active_chars.invalidate();

    for _ in 0..3 {
        let _ = manager.hard_compact_with(&messages);
    }

    assert_eq!(
        manager.compacted_count,
        messages.len(),
        "hard compaction must clamp compacted_count to the available messages"
    );
    let summary_markers = manager
        .active_summary
        .as_ref()
        .map(|summary| summary.text.matches("[Emergency compaction]").count())
        .unwrap_or(0);
    assert_eq!(
        summary_markers, 0,
        "pathological state should not append repeated emergency markers"
    );

    let api_messages = manager.messages_for_api_with(&messages);
    assert_eq!(
        api_messages.len(),
        1,
        "all current messages should remain covered by the existing summary until new turns arrive"
    );
}

#[test]
fn test_hard_compact_reduces_api_payload_and_reports_saved_tokens() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..40 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "payload ".repeat(80)),
        ));
        manager.notify_message_added();
    }

    let pre_api_messages = manager.messages_for_api_with(&messages);
    let pre_chars: usize = pre_api_messages.iter().map(message_char_count).sum();
    let pre_tokens = manager.effective_token_count_with(&messages);

    manager
        .hard_compact_with(&messages)
        .expect("hard compaction should recover oversized context");

    let post_api_messages = manager.messages_for_api_with(&messages);
    let post_chars: usize = post_api_messages.iter().map(message_char_count).sum();
    let post_tokens = manager.effective_token_count_with(&messages);
    let event = manager
        .take_compaction_event()
        .expect("hard compaction should publish an event");

    assert!(
        post_api_messages.len() < pre_api_messages.len(),
        "hard compaction should send fewer messages"
    );
    assert!(
        post_chars < pre_chars,
        "hard compaction should reduce outgoing payload chars: pre={pre_chars}, post={post_chars}"
    );
    assert!(
        post_tokens <= pre_tokens,
        "hard compaction must not increase effective tokens: pre={pre_tokens}, post={post_tokens}"
    );
    assert!(
        event.tokens_saved.unwrap_or(0) > 0,
        "event should attribute positive token savings: {event:?}"
    );
}

#[test]
fn test_invalid_compacted_count_does_not_resurrect_full_transcript_after_new_turn() {
    let mut manager = rolling_test_manager().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("old turn {} {}", i, "x".repeat(120)),
        ));
        manager.notify_message_added();
    }

    manager.compacted_count = 500;
    manager.active_summary = Some(Summary {
        text: "# Existing summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 500,
        original_turn_count: 500,
    });
    manager.active_chars.invalidate();

    let before_new_turn = manager.messages_for_api_with(&messages);
    assert_eq!(before_new_turn.len(), 1);
    assert_eq!(manager.compacted_count(), messages.len());

    messages.push(make_text_message(Role::User, "new turn after restore"));
    manager.notify_message_added();

    let after_new_turn = manager.messages_for_api_with(&messages);
    assert_eq!(
        after_new_turn.len(),
        2,
        "request should contain summary plus only the new active turn"
    );
    match &after_new_turn[1].content[0] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "new turn after restore"),
        _ => panic!("expected new active text turn"),
    }
}

// ── messages_for_api_with after compaction ──────────────────────

#[test]
fn test_messages_for_api_with_summary_prepended() {
    let mut manager = rolling_test_manager().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("turn {}", i)));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);

    manager
        .hard_compact_with(&messages)
        .expect("should compact");

    let api_msgs = manager.messages_for_api_with(&messages);
    // First message should be the summary
    assert_eq!(api_msgs[0].role, Role::User);
    match &api_msgs[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.starts_with("## Previous Conversation Summary"));
        }
        _ => panic!("expected text"),
    }
    // Remaining should be recent turns from original messages
    assert!(api_msgs.len() < messages.len());
}
