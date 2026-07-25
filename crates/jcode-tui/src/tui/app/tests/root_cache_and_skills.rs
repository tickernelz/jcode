#[test]
fn kv_cache_signature_prefix_match_allows_appended_messages() {
    let baseline_messages = vec![
        crate::message::Message::user("first prompt"),
        crate::message::Message::assistant_text("first answer"),
    ];
    let mut current_messages = baseline_messages.clone();
    current_messages.push(crate::message::Message::user("follow up"));

    let baseline = App::kv_cache_request_signature(&baseline_messages, &[], "system", "memory a");
    let current = App::kv_cache_request_signature(&current_messages, &[], "system", "memory b");

    assert!(App::kv_cache_signatures_prefix_match(&current, &baseline));
    assert_eq!(
        App::kv_cache_common_prefix_messages(&current, &baseline),
        baseline_messages.len()
    );
    assert_ne!(baseline.ephemeral_hash, current.ephemeral_hash);
}

#[test]
fn kv_cache_signature_prefix_match_detects_prefix_mutation() {
    let baseline_messages = vec![
        crate::message::Message::user("first prompt"),
        crate::message::Message::assistant_text("first answer"),
    ];
    let current_messages = vec![
        crate::message::Message::user("changed first prompt"),
        crate::message::Message::assistant_text("first answer"),
        crate::message::Message::user("follow up"),
    ];

    let baseline = App::kv_cache_request_signature(&baseline_messages, &[], "system", "");
    let current = App::kv_cache_request_signature(&current_messages, &[], "system", "");

    assert!(!App::kv_cache_signatures_prefix_match(&current, &baseline));
    assert_eq!(App::kv_cache_common_prefix_messages(&current, &baseline), 0);
}

#[test]
fn kv_cache_signature_ignores_non_transmitted_message_metadata() {
    use crate::message::{ContentBlock, Message, Role};

    // A boundary assistant message that has already been sent upstream. The
    // provider only ever receives its `Text` block; the struct-level timestamp
    // and tool_duration_ms, plus any history-only ReasoningTrace block, are
    // never part of the prompt token stream.
    let baseline_messages = vec![
        Message::user("first prompt"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "boundary answer".to_string(),
                cache_control: None,
            }],
            timestamp: Some(chrono::Utc::now()),
            tool_duration_ms: None,
        },
    ];

    // Next request: the same boundary message but with volatile/harness-only
    // fields backfilled in memory, then two appended messages. This mirrors the
    // real production sequence where PROVIDER_CANONICAL_INPUT stayed append-only
    // yet the harness previously reported harness:_prefix_changed.
    let mut current_messages = vec![
        Message::user("first prompt"),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "boundary answer".to_string(),
                    // Ephemeral cache breakpoint that hops to the newest message.
                    cache_control: Some(crate::message::CacheControl::ephemeral(None)),
                },
                // History-only reasoning trace, never replayed to a provider.
                ContentBlock::ReasoningTrace {
                    text: "internal scratch thinking".to_string(),
                },
            ],
            // Backfilled after the tool ran / a newer turn was committed.
            timestamp: Some(chrono::Utc::now() + chrono::Duration::seconds(5)),
            tool_duration_ms: Some(1234),
        },
    ];
    current_messages.push(Message::user("follow up"));
    current_messages.push(Message::assistant_text("second answer"));

    let baseline = App::kv_cache_request_signature(&baseline_messages, &[], "system", "");
    let current = App::kv_cache_request_signature(&current_messages, &[], "system", "");

    assert!(
        App::kv_cache_signatures_prefix_match(&current, &baseline),
        "non-transmitted metadata changes on the boundary message must not break the cache prefix"
    );
    assert_eq!(
        App::kv_cache_common_prefix_messages(&current, &baseline),
        baseline_messages.len(),
        "the whole prior request should still count as a common prefix"
    );
}

#[test]
fn cold_cache_warning_is_persisted_when_starting_next_request() {
    let mut app = create_test_app();
    crate::provider::anthropic::set_cache_ttl_1h(true);
    app.display_messages.push(DisplayMessage::user("first"));
    let session_id = app.kv_cache_session_id();
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id,
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 911_873,
        completed_at: Instant::now() - Duration::from_secs(3723),
        provider: "anthropic".to_string(),
        model: "claude-opus-4-6".to_string(),
        upstream_provider: None,
        signature: None,
    });

    app.display_messages.push(DisplayMessage::user("second"));
    app.begin_kv_cache_request(&[Message::user("second")], &[], "system", "");

    let warning = app
        .display_messages()
        .iter()
        .find(|message| {
            message.role == "system" && message.content.contains("Prompt cache went cold")
        })
        .expect("cold cache warning should be persisted in the transcript");
    assert!(warning.content.contains("911K"));
    assert!(warning.content.contains("went cold 2m ago"), "{warning:?}");
    assert!(
        warning.content.lines().count() == 1,
        "cold-cache warning must stay one line: {warning:?}"
    );
}

#[test]
fn cold_cache_warning_fires_on_idle_tick_before_next_message() {
    // The whole point of the warning is to appear *before* the user submits
    // the next message, so they can decide to /cache-extend or compact. The
    // idle tick must therefore push it as soon as the TTL expires, not wait
    // for the next request to start.
    let mut app = create_test_app();
    crate::provider::anthropic::set_cache_ttl_1h(true);
    app.display_messages.push(DisplayMessage::user("first"));
    let session_id = app.kv_cache_session_id();
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id,
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 42_000,
        completed_at: Instant::now() - Duration::from_secs(3700),
        provider: "anthropic".to_string(),
        model: "claude-opus-4-6".to_string(),
        upstream_provider: None,
        signature: None,
    });

    assert!(
        app.maybe_push_idle_cold_cache_warning(),
        "idle tick should push the cold-cache warning once the TTL expires"
    );
    let warning = app
        .display_messages()
        .iter()
        .find(|message| {
            message.role == "system" && message.content.contains("Prompt cache went cold")
        })
        .expect("idle cold cache warning should be persisted in the transcript");
    assert!(
        warning.content.contains("next turn may resend"),
        "{warning:?}"
    );
    assert!(
        !warning.content.contains("ago"),
        "idle warning should not report a stale 'went cold N ago' detail: {warning:?}"
    );

    // Subsequent ticks for the same cold period must not spam the transcript.
    assert!(!app.maybe_push_idle_cold_cache_warning());
    let count = app
        .display_messages()
        .iter()
        .filter(|message| message.content.contains("Prompt cache went cold"))
        .count();
    assert_eq!(count, 1);

    // And the request-start fallback must not duplicate the idle warning.
    app.display_messages.push(DisplayMessage::user("second"));
    app.begin_kv_cache_request(&[Message::user("second")], &[], "system", "");
    let count = app
        .display_messages()
        .iter()
        .filter(|message| message.content.contains("Prompt cache went cold"))
        .count();
    assert_eq!(
        count, 1,
        "request start should not repeat the warning for the same cold period"
    );
}

#[test]
fn idle_cold_cache_warning_waits_for_ttl_and_rearms_after_new_cache_write() {
    let mut app = create_test_app();
    crate::provider::anthropic::set_cache_ttl_1h(true);
    app.display_messages.push(DisplayMessage::user("first"));
    let session_id = app.kv_cache_session_id();
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id: session_id.clone(),
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 42_000,
        completed_at: Instant::now() - Duration::from_secs(60),
        provider: "anthropic".to_string(),
        model: "claude-opus-4-6".to_string(),
        upstream_provider: None,
        signature: None,
    });

    assert!(
        !app.maybe_push_idle_cold_cache_warning(),
        "a warm cache must not warn"
    );

    // TTL expires: warn once.
    app.kv_cache
        .kv_cache_baseline
        .as_mut()
        .unwrap()
        .completed_at = Instant::now() - Duration::from_secs(3700);
    assert!(app.maybe_push_idle_cold_cache_warning());
    assert!(!app.maybe_push_idle_cold_cache_warning());

    // A new completed call refreshes the baseline (new cache write), which
    // re-arms the warning for the next cold period.
    app.kv_cache
        .kv_cache_baseline
        .as_mut()
        .unwrap()
        .completed_at = Instant::now() - Duration::from_secs(3800);
    assert!(
        app.maybe_push_idle_cold_cache_warning(),
        "a fresh cache write should re-arm the cold warning"
    );
}

#[test]
fn harness_caused_kv_cache_miss_pushes_in_chat_alarm() {
    let _invalidation_guard = crate::storage::lock_test_env();
    // A warm session whose system prompt hash silently changes between turns is
    // the exact failure mode of the skill-ordering bug: the conversation only
    // grew, yet the cached prefix is invalidated. We must surface that loudly.
    let mut app = create_test_app();
    crate::provider::anthropic::set_cache_ttl_1h(true);
    // No documented invalidation may explain this miss, or the alarm downgrades
    // to the informational attribution notice.
    crate::cache_invalidation::clear_for_tests();

    let messages = vec![
        Message::user("first prompt"),
        Message::assistant_text("first answer"),
        Message::user("second prompt"),
    ];

    // Baseline captured last turn with a *different* system static hash.
    let baseline_signature = App::kv_cache_request_signature(&messages, &[], "system PROMPT A", "");
    let session_id = app.kv_cache_session_id();
    // Match the live provider/model exactly so the miss is classified as a
    // harness system change rather than a provider/model switch.
    let provider = app.kv_cache_provider_name();
    let model = app.kv_cache_provider_model();
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id,
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 50_000,
        completed_at: Instant::now(),
        provider,
        model,
        upstream_provider: None,
        signature: Some(baseline_signature),
    });

    // This turn: same provider/model, conversation grew, but the system prompt
    // changed (hash differs). Register the pending request, then complete the
    // stream with a near-zero cache read to model the bust.
    app.begin_kv_cache_request(&messages, &[], "system PROMPT B", "");
    app.streaming.streaming_input_tokens = 50_000;
    app.streaming.streaming_cache_read_tokens = Some(0);
    app.streaming.streaming_cache_creation_tokens = Some(50_000);
    app.kv_cache.current_api_usage_recorded = false;
    app.record_completed_stream_cache_usage();

    let alarm = app
        .display_messages()
        .iter()
        .find(|message| message.role == "system" && message.content.contains("KV cache miss"))
        .expect("harness-caused cache miss should push an in-chat alarm");
    assert!(
        alarm.content.contains("harness: system changed"),
        "{alarm:?}"
    );
    assert!(alarm.content.contains("50K"), "{alarm:?}");
}

#[test]
fn documented_invalidation_downgrades_kv_cache_alarm_to_attribution() {
    let _invalidation_guard = crate::storage::lock_test_env();
    // Config/skill reloads legitimately change the system prompt mid-session.
    // Those sites document the invalidation; a harness-attributed miss that
    // follows must be surfaced as an informational "refresh" with the cause,
    // not as the unexplained harness-bust alarm.
    let mut app = create_test_app();
    crate::provider::anthropic::set_cache_ttl_1h(true);
    crate::cache_invalidation::clear_for_tests();

    let messages = vec![
        Message::user("first prompt"),
        Message::assistant_text("first answer"),
        Message::user("second prompt"),
    ];
    let baseline_signature = App::kv_cache_request_signature(&messages, &[], "system PROMPT A", "");
    let session_id = app.kv_cache_session_id();
    let provider = app.kv_cache_provider_name();
    let model = app.kv_cache_provider_model();
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id,
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 50_000,
        completed_at: Instant::now(),
        provider,
        model,
        upstream_provider: None,
        signature: Some(baseline_signature),
    });

    // The documented cause lands between the baseline and the busted request.
    crate::cache_invalidation::record("config reload", "modified_changed=true");

    app.begin_kv_cache_request(&messages, &[], "system PROMPT B", "");
    app.streaming.streaming_input_tokens = 50_000;
    app.streaming.streaming_cache_read_tokens = Some(0);
    app.streaming.streaming_cache_creation_tokens = Some(50_000);
    app.kv_cache.current_api_usage_recorded = false;
    app.record_completed_stream_cache_usage();

    let notice = app
        .display_messages()
        .iter()
        .find(|message| message.role == "system" && message.content.contains("KV cache refresh"))
        .expect("documented invalidation should push an attribution notice");
    assert!(notice.content.contains("config reload"), "{notice:?}");
    assert!(
        !app.display_messages()
            .iter()
            .any(|message| message.role == "system" && message.content.contains("KV cache miss")),
        "documented invalidation must not also raise the harness alarm"
    );

    crate::cache_invalidation::clear_for_tests();
}

#[test]
fn kv_cache_baseline_stores_effective_prompt_tokens() {
    // For split-accounting providers (Anthropic), the reported `input` is only
    // the uncached remainder of the request. The baseline drives the cold-cache
    // warning's "~N input tokens will be resent" figure, so it must store the
    // whole effective prompt (input + cache read + cache creation), not the
    // bare input.
    let mut app = create_test_app();
    crate::provider::anthropic::set_cache_ttl_1h(true);

    let messages = vec![Message::user("first prompt")];
    app.begin_kv_cache_request(&messages, &[], "system", "");
    app.streaming.streaming_input_tokens = 700;
    app.streaming.streaming_cache_read_tokens = Some(90_000);
    app.streaming.streaming_cache_creation_tokens = Some(5_000);
    app.kv_cache.current_api_usage_recorded = false;
    app.record_completed_stream_cache_usage();

    let baseline = app
        .kv_cache
        .kv_cache_baseline
        .as_ref()
        .expect("completed stream should record a baseline");
    assert_eq!(
        baseline.input_tokens, 95_700,
        "baseline must capture input + read + creation, not bare input"
    );
}

#[test]
fn legitimate_model_switch_miss_does_not_push_in_chat_alarm() {
    // Switching models legitimately invalidates the cache; that is user-driven,
    // not a harness bug, so it must NOT raise the alarm.
    let mut app = create_test_app();
    crate::provider::anthropic::set_cache_ttl_1h(true);

    let messages = vec![
        Message::user("first prompt"),
        Message::assistant_text("first answer"),
        Message::user("second prompt"),
    ];
    let baseline_signature = App::kv_cache_request_signature(&messages, &[], "system", "");
    let session_id = app.kv_cache_session_id();
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id,
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 50_000,
        completed_at: Instant::now(),
        provider: "anthropic".to_string(),
        // Different model than the current request -> ModelSwitch.
        model: "claude-opus-4-5".to_string(),
        upstream_provider: None,
        signature: Some(baseline_signature),
    });

    app.begin_kv_cache_request(&messages, &[], "system", "");
    app.streaming.streaming_input_tokens = 50_000;
    app.streaming.streaming_cache_read_tokens = Some(0);
    app.streaming.streaming_cache_creation_tokens = Some(50_000);
    app.kv_cache.current_api_usage_recorded = false;
    app.record_completed_stream_cache_usage();

    assert!(
        !app.display_messages()
            .iter()
            .any(|message| message.role == "system" && message.content.contains("KV cache miss")),
        "model-switch miss must not raise the harness alarm"
    );
}

#[test]
fn kv_cache_baseline_from_other_session_is_ignored() {
    // A single App can stream multiple sessions over its lifetime. A baseline
    // captured for a large session must not be diffed against a fresh, smaller
    // session, or the new history looks like a broken prefix and emits a
    // spurious `harness:_prefix_changed` miss. See the false positives in
    // ~/.jcode/logs KV_CACHE_USAGE telemetry (common_prefix=0, current
    // message_count << baseline_message_count, yet read_pct=100/miss=none).
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_session_id = Some("session_big".to_string());

    let big_history: Vec<Message> = (0..40)
        .map(|i| Message::user(format!("big session message {i}").as_str()))
        .collect();
    let big_signature = App::kv_cache_request_signature(&big_history, &[], "system", "");
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id: Some("session_big".to_string()),
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 200_000,
        completed_at: Instant::now(),
        provider: "anthropic".to_string(),
        model: "claude-opus-4-6".to_string(),
        upstream_provider: None,
        signature: Some(big_signature),
    });

    // Switch to a brand-new, much smaller session and start its first request.
    app.remote_session_id = Some("session_small".to_string());
    let small_signature = App::kv_cache_request_signature(
        &[Message::user("hello from small session")],
        &[],
        "system",
        "",
    );
    app.begin_remote_kv_cache_request(small_signature);

    let request = app
        .kv_cache
        .pending_kv_cache_request
        .as_ref()
        .expect("request should be pending");
    assert!(
        request.baseline.is_none(),
        "foreign-session baseline must be treated as absent: {:?}",
        request.baseline
    );
    assert_eq!(
        request.baseline_messages_prefix_matches, None,
        "no cross-session prefix comparison should happen"
    );
}

#[test]
fn kv_cache_baseline_same_session_still_compares() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_session_id = Some("session_same".to_string());

    let history = vec![
        Message::user("first prompt"),
        Message::assistant_text("first answer"),
    ];
    let baseline_signature = App::kv_cache_request_signature(&history, &[], "system", "");
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id: Some("session_same".to_string()),
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 1_000,
        completed_at: Instant::now(),
        provider: "anthropic".to_string(),
        model: "claude-opus-4-6".to_string(),
        upstream_provider: None,
        signature: Some(baseline_signature),
    });

    // Append-only growth in the same session keeps the prefix intact.
    let mut grown = history.clone();
    grown.push(Message::user("follow up"));
    let grown_signature = App::kv_cache_request_signature(&grown, &[], "system", "");
    app.begin_remote_kv_cache_request(grown_signature);

    let request = app
        .kv_cache
        .pending_kv_cache_request
        .as_ref()
        .expect("request should be pending");
    assert!(
        request.baseline.is_some(),
        "same-session baseline should be retained"
    );
    assert_eq!(
        request.baseline_messages_prefix_matches,
        Some(true),
        "append-only same-session growth keeps the cached prefix"
    );
}

#[test]
fn compaction_invalidates_kv_cache_baseline_and_stale_completion_cannot_restore_it() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_session_id = Some("session_compacted".to_string());

    let old_history: Vec<Message> = (0..176)
        .map(|i| Message::user(format!("old message {i}").as_str()))
        .collect();
    let old_signature = App::kv_cache_request_signature(&old_history, &[], "system", "memory");
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id: Some("session_compacted".to_string()),
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 80_169,
        completed_at: Instant::now(),
        provider: app.kv_cache_provider_name(),
        model: app.kv_cache_provider_model(),
        upstream_provider: None,
        signature: Some(old_signature.clone()),
    });

    // Model the request that was already in flight when background compaction
    // finished. Its completion must not re-establish a pre-compaction baseline.
    app.begin_remote_kv_cache_request(old_signature);
    let old_generation = app.kv_cache.cache_generation;
    app.handle_compaction_event(crate::compaction::CompactionEvent {
        trigger: "semantic".to_string(),
        pre_tokens: Some(80_169),
        post_tokens: Some(27_641),
        tokens_saved: Some(52_528),
        duration_ms: Some(500),
        messages_dropped: None,
        messages_compacted: Some(173),
        summary_chars: Some(10_000),
        active_messages: Some(3),
        ..Default::default()
    });
    assert_ne!(app.kv_cache.cache_generation, old_generation);
    assert!(app.kv_cache.kv_cache_baseline.is_none());

    app.streaming.streaming_input_tokens = 80_169;
    app.streaming.streaming_cache_read_tokens = Some(80_000);
    app.streaming.streaming_cache_creation_tokens = Some(0);
    app.kv_cache.current_api_usage_recorded = false;
    app.record_completed_stream_cache_usage();
    assert!(
        app.kv_cache_baseline_for_current_session().is_none(),
        "a pre-compaction request completion must remain stale"
    );

    // The first compacted request is a new cache generation, not an unexplained
    // mutation of the old 176-message prefix.
    let compacted = vec![
        Message::user("compaction summary"),
        Message::assistant_text("recent answer"),
        Message::user("next tool callback"),
    ];
    let compacted_signature = App::kv_cache_request_signature(&compacted, &[], "system", "");
    app.begin_remote_kv_cache_request(compacted_signature);
    let request = app
        .kv_cache
        .pending_kv_cache_request
        .as_ref()
        .expect("compacted request should be pending");
    assert!(request.baseline.is_none());
    assert_eq!(request.baseline_messages_prefix_matches, None);

    app.streaming.streaming_input_tokens = 27_641;
    app.streaming.streaming_cache_read_tokens = Some(20_224);
    app.streaming.streaming_cache_creation_tokens = Some(0);
    app.kv_cache.current_api_usage_recorded = false;
    app.record_completed_stream_cache_usage();

    assert!(app.kv_cache.kv_cache_miss_samples.is_empty());
    assert!(
        !app.display_messages()
            .iter()
            .any(|message| message.content.contains("KV cache miss")),
        "compaction must not surface as a harness-caused cache miss"
    );
}

#[test]
fn native_compaction_application_invalidates_kv_cache_baseline_before_continuation() {
    let mut app = create_test_app();
    let old_history: Vec<Message> = (0..264)
        .map(|i| Message::user(format!("old message {i}").as_str()))
        .collect();
    let old_signature = App::kv_cache_request_signature(&old_history, &[], "system", "");
    app.kv_cache.kv_cache_baseline = Some(KvCacheBaseline {
        session_id: app.kv_cache_session_id(),
        cache_generation: app.kv_cache.cache_generation,
        input_tokens: 262_419,
        completed_at: Instant::now(),
        provider: app.kv_cache_provider_name(),
        model: app.kv_cache_provider_model(),
        upstream_provider: None,
        signature: Some(old_signature),
    });
    let old_generation = app.kv_cache.cache_generation;

    app.apply_openai_native_compaction("enc_native_test".to_string(), old_history.len())
        .expect("native compaction should persist");

    assert_ne!(app.kv_cache.cache_generation, old_generation);
    assert!(app.kv_cache.kv_cache_baseline.is_none());

    let compacted = vec![
        Message::user("native summary"),
        Message::assistant_text("recent answer"),
        Message::user("next callback"),
    ];
    app.begin_kv_cache_request(&compacted, &[], "system", "");
    let request = app
        .kv_cache
        .pending_kv_cache_request
        .as_ref()
        .expect("compacted request should be pending");
    assert!(request.baseline.is_none());
    assert_eq!(request.baseline_messages_prefix_matches, None);
}

#[test]
fn remote_token_usage_records_cache_stats_before_done_and_dedupes_snapshots() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.is_remote = true;
    app.remote_provider_name = Some("OpenAI".to_string());
    app.remote_provider_model = Some("gpt-5.5".to_string());
    app.display_messages
        .push(DisplayMessage::user("live prompt"));

    app.handle_server_event(
        crate::protocol::ServerEvent::KvCacheRequest {
            system_static_hash: 1,
            tools_hash: 2,
            messages_hash: 3,
            message_hashes: vec![11, 22],
            message_count: 2,
            tool_count: 33,
            system_static_chars: 11155,
            tools_json_chars: 35228,
            messages_json_chars: 198612,
            ephemeral_hash: None,
            ephemeral_chars: 2,
            ephemeral_message_count: 0,
        },
        &mut remote,
    );
    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 63_762,
            output: 153,
            cache_read_input: Some(0),
            cache_creation_input: None,
        },
        &mut remote,
    );

    assert_eq!(
        app.token_accounting.total_cache_reported_input_tokens,
        63_762
    );
    assert_eq!(app.token_accounting.total_cache_read_tokens, 0);
    assert_eq!(
        app.token_accounting.last_cache_reported_input_tokens,
        Some(63_762)
    );
    assert_eq!(app.token_accounting.total_input_tokens, 63_762);
    assert!(app.last_api_completed.is_some());
    assert!(app.kv_cache.pending_kv_cache_request.is_none());

    app.handle_server_event(
        crate::protocol::ServerEvent::TokenUsage {
            input: 63_762,
            output: 153,
            cache_read_input: Some(0),
            cache_creation_input: None,
        },
        &mut remote,
    );

    assert_eq!(
        app.token_accounting.total_cache_reported_input_tokens,
        63_762
    );
    assert_eq!(app.token_accounting.total_input_tokens, 63_762);

    assert!(super::state_ui::handle_info_command(
        &mut app,
        "/cache stats"
    ));
    let stats = app.display_messages().last().unwrap().content.clone();
    assert!(
        stats.contains("- total_cache_reported_input_tokens: 63.8k (63,762)"),
        "{stats}"
    );
    assert!(
        stats.contains("- baseline.signature.messages_json_chars: 198.6k (198,612)"),
        "{stats}"
    );
    assert!(
        stats.contains("- current_api_usage_recorded: true"),
        "{stats}"
    );
}

#[test]
fn remote_account_switch_publishes_only_after_correlated_terminal_event() {
    let mut app = create_test_app();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    app.is_remote = true;
    app.pending_remote_account_switch = Some(PendingRemoteAccountSwitch {
        id: 77,
        provider_id: "openai".to_string(),
        label: "work".to_string(),
    });

    app.handle_server_event(crate::protocol::ServerEvent::Done { id: 76 }, &mut remote);
    assert_eq!(
        app.pending_remote_account_switch
            .as_ref()
            .map(|pending| pending.id),
        Some(77)
    );
    assert!(
        app.display_messages
            .iter()
            .all(|message| !message.content.contains("Switched `openai` account"))
    );

    app.handle_server_event(crate::protocol::ServerEvent::Done { id: 77 }, &mut remote);
    assert!(app.pending_remote_account_switch.is_none());
    assert!(app.display_messages.iter().any(|message| {
        message
            .content
            .contains("Switched `openai` account to `work`.")
    }));

    app.pending_remote_account_switch = Some(PendingRemoteAccountSwitch {
        id: 88,
        provider_id: "anthropic".to_string(),
        label: "personal".to_string(),
    });
    app.handle_server_event(
        crate::protocol::ServerEvent::Error {
            id: 87,
            message: "unrelated".to_string(),
            retry_after_secs: None,
        },
        &mut remote,
    );
    assert_eq!(
        app.pending_remote_account_switch
            .as_ref()
            .map(|pending| pending.id),
        Some(88)
    );

    app.handle_server_event(
        crate::protocol::ServerEvent::Error {
            id: 88,
            message: "denied".to_string(),
            retry_after_secs: None,
        },
        &mut remote,
    );
    assert!(app.pending_remote_account_switch.is_none());
    assert!(
        app.display_messages
            .iter()
            .any(|message| message.content.contains("Account switch failed: denied"))
    );
}

#[test]
fn cache_stats_uses_remote_history_token_usage_totals() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_total_tokens = Some((1_250_000, 200_000));
    app.remote_token_usage_totals = Some(crate::protocol::TokenUsageTotals {
        messages_with_token_usage: 3,
        input_tokens: 1_250_000,
        output_tokens: 200_000,
        cache_reported_input_tokens: 1_000_000,
        cache_read_input_tokens: 600_000,
        cache_creation_input_tokens: 50_000,
    });

    assert!(super::state_ui::handle_info_command(
        &mut app,
        "/cache stats"
    ));
    let stats = app.display_messages().last().unwrap().content.clone();
    assert!(
        stats.contains("- total_tokens_source: remote_history"),
        "{stats}"
    );
    assert!(
        stats.contains("- total_input_tokens: 1.25m (1,250,000)"),
        "{stats}"
    );
    assert!(
        stats.contains("- cache_totals_source: remote_history"),
        "{stats}"
    );
    assert!(
        stats.contains("- total_cache_reported_input_tokens: 1m (1,000,000)"),
        "{stats}"
    );
    assert!(
        stats.contains("- persisted_token_usage_source: remote_history"),
        "{stats}"
    );
    assert!(stats.contains("- messages_with_token_usage: 3"), "{stats}");
}

#[test]
fn version_command_shows_remote_server_identity_and_update_status() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_server_short_name = Some("blazing".to_string());
    app.remote_server_icon = Some("🔥".to_string());
    app.remote_server_version = Some("v0.14.2-dev (old)".to_string());
    app.remote_server_has_update = Some(true);

    assert!(super::state_ui::handle_info_command(&mut app, "/version"));
    let content = app.display_messages().last().unwrap().content.clone();
    assert!(content.contains("jcode client:"), "{content}");
    assert!(content.contains("mode: remote/shared-server"), "{content}");
    assert!(content.contains("server: 🔥 blazing"), "{content}");
    assert!(
        content.contains("server version: v0.14.2-dev (old)"),
        "{content}"
    );
    assert!(content.contains("reload recommended"), "{content}");
}

#[test]
fn skills_command_lists_loaded_and_endorsed_skills() {
    let mut app = create_test_app();

    assert!(super::state_ui::handle_info_command(&mut app, "/skills"));
    let content = app.display_messages().last().unwrap().content.clone();

    assert!(content.contains("Loaded skills"), "{content}");
    assert!(
        content.contains("Endorsed skills (recommended by jcode)"),
        "{content}"
    );
    // Every endorsed skill should appear with an install status marker.
    for endorsed in crate::skill::endorsed_skills() {
        assert!(
            content.contains(&format!("/{}", endorsed.name)),
            "expected endorsed skill /{} in:\n{content}",
            endorsed.name
        );
    }
    assert!(
        content.contains("[installed]") || content.contains("[not installed]"),
        "{content}"
    );
    // NVIDIA CUDA-X skills are grouped under their own category with install hints.
    assert!(content.contains("NVIDIA CUDA-X"), "{content}");
    assert!(
        content.contains("/cuopt-numerical-optimization-api-python"),
        "{content}"
    );
    assert!(
        content.contains("install: npx skills add nvidia/skills"),
        "{content}"
    );
    assert!(
        content.contains("https://github.com/NVIDIA/skills"),
        "{content}"
    );
    assert_eq!(
        app.display_messages().last().unwrap().title.as_deref(),
        Some("Skills")
    );
}

#[test]
fn skills_command_marks_active_skill_in_remote_mode() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_skills = vec!["optimization".to_string(), "firefox-browser".to_string()];
    app.active_skill = Some("optimization".to_string());

    assert!(super::state_ui::handle_info_command(&mut app, "/skills"));
    let content = app.display_messages().last().unwrap().content.clone();

    assert!(content.contains("- /optimization (active)"), "{content}");
    assert!(content.contains("- /firefox-browser\n"), "{content}");
    // Endorsed list should mark remote-installed skills as installed.
    assert!(
        content.contains("/firefox-browser [installed]"),
        "{content}"
    );
}

/// Regression for issue #431 (and #457): skills added on disk after startup
/// must show up in `/skills` and the skills snapshot without a session
/// restart. With the session-scoped project overlay, project-local skills are
/// visible immediately, without even running `/skills` first.
#[test]
fn skills_command_refreshes_registry_from_disk_before_listing() {
    let mut app = create_test_app();

    // Point the session at a fresh project dir and add a project-local skill
    // after the app (and its skill snapshot) was created.
    let temp = tempfile::tempdir().expect("tempdir");
    let skill_dir = temp.path().join(".jcode").join("skills").join("late-skill");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: late-skill\ndescription: Added after startup\n---\n# Late skill\n",
    )
    .expect("write SKILL.md");
    app.session.working_dir = Some(temp.path().to_string_lossy().to_string());

    // Project-local skills are a session overlay composed at read time, so
    // the new skill is available immediately (issue #457).
    assert!(
        app.current_skills_snapshot().get("late-skill").is_some(),
        "project-local skill must be visible immediately without reload"
    );

    assert!(super::state_ui::handle_info_command(&mut app, "/skills"));
    let content = app.display_messages().last().unwrap().content.clone();

    assert!(
        content.contains("- /late-skill"),
        "expected late-added skill in /skills output:\n{content}"
    );
    assert!(
        app.current_skills_snapshot().get("late-skill").is_some(),
        "registry snapshot must be synced so /late-skill invocations resolve"
    );
}

#[test]
fn skill_invocation_with_prompt_activates_and_submits_in_one_turn() {
    let mut app = create_test_app();
    let temp = tempfile::tempdir().expect("tempdir");
    let skill_dir = temp.path().join(".jcode/skills/prompt-skill");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: prompt-skill\ndescription: Prompt regression skill\n---\nUse it.\n",
    )
    .expect("write skill");
    app.session.working_dir = Some(temp.path().to_string_lossy().to_string());
    app.input = "/prompt-skill \"then type prompt here and all that\"".to_string();
    app.cursor_pos = app.input.len();

    app.submit_input();

    assert_eq!(app.active_skill.as_deref(), Some("prompt-skill"));
    assert!(app.is_processing, "the trailing prompt should start a turn");
    let submitted = app
        .session
        .messages
        .last()
        .expect("submitted session message");
    assert!(matches!(
        submitted.content.as_slice(),
        [ContentBlock::Text { text, .. }] if text == "then type prompt here and all that"
    ));
}

#[test]
fn skill_invocation_with_prompt_attaches_pending_image_to_user_message() {
    let mut app = create_test_app();
    let temp = tempfile::tempdir().expect("tempdir");
    let skill_dir = temp.path().join(".jcode/skills/image-skill");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: image-skill\ndescription: Image attachment regression skill\n---\nUse it.\n",
    )
    .expect("write skill");
    app.session.working_dir = Some(temp.path().to_string_lossy().to_string());
    app.pending_images = vec![("image/png".to_string(), "ZmFrZSBwbmcgYnl0ZXM=".to_string())];
    app.input = "/image-skill describe this screenshot".to_string();
    app.cursor_pos = app.input.len();

    app.submit_input();

    assert_eq!(app.active_skill.as_deref(), Some("image-skill"));
    assert!(app.is_processing, "the trailing prompt should start a turn");
    assert!(
        app.pending_images.is_empty(),
        "pending images must be consumed by the submitted turn"
    );
    let submitted = app
        .session
        .messages
        .last()
        .expect("submitted session message");
    assert_eq!(submitted.role, Role::User);
    assert!(matches!(
        submitted.content.as_slice(),
        [
            ContentBlock::Image { media_type, data },
            ContentBlock::Text { text, .. },
        ] if media_type == "image/png"
            && data == "ZmFrZSBwbmcgYnl0ZXM="
            && text == "describe this screenshot"
    ));
}

#[test]
fn unknown_skill_invocation_surfaces_error_and_sends_nothing() {
    let mut app = create_test_app();
    let temp = tempfile::tempdir().expect("tempdir");
    app.session.working_dir = Some(temp.path().to_string_lossy().to_string());
    app.input = "/definitely-not-a-real-skill".to_string();
    app.cursor_pos = app.input.len();
    let session_messages_before = app.session.messages.len();

    app.submit_input();

    assert!(!app.is_processing, "unknown skill must not start a turn");
    assert_eq!(
        app.session.messages.len(),
        session_messages_before,
        "no message should be sent to the model"
    );
    assert!(app.active_skill.is_none());
    let last = app.display_messages().last().expect("error message");
    assert_eq!(last.role, "error");
    assert_eq!(last.content, "Unknown skill: /definitely-not-a-real-skill");
}

#[test]
fn endorsed_but_not_installed_skill_invocation_surfaces_install_hint() {
    let mut app = create_test_app();
    // Empty working dir so no endorsed skill is actually installed there.
    let temp = tempfile::tempdir().expect("tempdir");
    app.session.working_dir = Some(temp.path().to_string_lossy().to_string());

    let endorsed = crate::skill::endorsed_skills()
        .iter()
        .find(|endorsed| {
            endorsed.install.is_some() && app.current_skills_snapshot().get(endorsed.name).is_none()
        })
        .expect("an endorsed skill with an install hint that is not installed");

    app.input = format!("/{}", endorsed.name);
    app.cursor_pos = app.input.len();
    let session_messages_before = app.session.messages.len();

    app.submit_input();

    assert!(!app.is_processing, "missing skill must not start a turn");
    assert_eq!(
        app.session.messages.len(),
        session_messages_before,
        "no message should be sent to the model"
    );
    assert!(app.active_skill.is_none());
    let last = app.display_messages().last().expect("error message");
    assert_eq!(last.role, "error");
    assert!(
        last.content.contains(&format!(
            "Skill /{} is endorsed but not installed",
            endorsed.name
        )),
        "{}",
        last.content
    );
    assert!(
        last.content
            .contains(&format!("`{}`", endorsed.install.unwrap())),
        "install hint missing from: {}",
        last.content
    );
    assert!(
        !last.content.contains("Unknown skill"),
        "endorsed skill must not be reported as a typo: {}",
        last.content
    );
}
