#[tokio::test]
async fn new_agent_turn_invalidates_rewind_undo_snapshot() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let provider: Arc<dyn Provider> = Arc::new(DelayedProvider {
        open_delay: Duration::ZERO,
        first_event_delay: Duration::ZERO,
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    for index in 0..3 {
        agent.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("old branch {index}"),
                cache_control: None,
            }],
        );
    }

    agent.rewind_to_message(1).expect("rewind should succeed");
    assert!(agent.rewind_undo_snapshot.is_some());
    agent
        .run_once("new branch prompt")
        .await
        .expect("new branch turn should complete");

    assert!(agent.rewind_undo_snapshot.is_none());
    assert!(agent.undo_rewind().is_err());
    assert!(
        agent
            .session
            .messages
            .iter()
            .any(|message| content_text(&message.content).contains("new branch prompt"))
    );
    assert!(
        agent
            .session
            .messages
            .iter()
            .any(|message| content_text(&message.content).contains("old branch 2"))
    );
    assert!(
        agent
            .session
            .messages_for_provider_uncached()
            .iter()
            .all(|message| !message_text(message).contains("old branch 2"))
    );

    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn rolling_restore_rebuilds_raw_history_and_clears_owned_lcm_projection() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    assert_eq!(
        registry.compaction().read().await.engine(),
        crate::config::CompactionEngine::Rolling,
        "source default and rollback test must remain rolling"
    );

    let mut session = Session::create(None, None);
    let exact_runtime_identity = provider
        .exact_runtime_identity()
        .expect("native auto fixture exact identity");
    let runtime_identity_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&exact_runtime_identity).unwrap())
    );
    session.exact_runtime_identity = Some(exact_runtime_identity);
    session.add_message(
        Role::User,
        vec![
            ContentBlock::Text {
                text: "canonical raw alpha".to_string(),
                cache_control: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "exact-original-image-bytes".to_string(),
            },
        ],
    );
    let assistant_message_id = session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::Text {
                text: "canonical raw beta".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "exact-original-tool-id".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "true"}),
                thought_signature: Some("exact-original-tool-signature".to_string()),
            },
        ],
    );
    let canonical_raw = serde_json::to_vec(&session.messages).unwrap();
    assert_eq!(session.suppress_oversized_images_for_provider(0), 1);
    session.suppress_tool_use_blocks_for_provider(&assistant_message_id);
    assert_eq!(serde_json::to_vec(&session.messages).unwrap(), canonical_raw);
    let source_message_ids = session
        .messages
        .iter()
        .map(|message| message.id.clone())
        .collect::<Vec<_>>();
    use sha2::{Digest, Sha256};
    let source_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&session.messages).unwrap())
    );
    let summary_text = "stale native lcm projection".to_string();
    let mut node = crate::session::StoredContextNode {
        id: String::new(),
        schema_version: 2,
        level: 0,
        source_session_id: session.id.clone(),
        source_message_ids: source_message_ids.clone(),
        source_sha256: source_sha256.clone(),
        source_runtime_identity_sha256: runtime_identity_sha256.clone(),
        child_node_ids: Vec::new(),
        summary_text: summary_text.clone(),
        summary_sha256: Some(format!("{:x}", Sha256::digest(summary_text.as_bytes()))),
        estimated_tokens: 8,
        summarizer_model: "lcm-model".to_string(),
        summarizer_provider: "lcm-provider".to_string(),
        summarizer_route: "lcm-route".to_string(),
        summarizer_runtime_identity_sha256: runtime_identity_sha256,
        prompt_schema_version: 1,
        created_at: chrono::Utc::now(),
    };
    node.id = crate::session::test_stored_context_node_id(&node);
    let node_id = node.id.clone();
    let compaction = crate::session::StoredCompactionState {
        summary_text: "[LCM context node 1 level 0]\nstale native lcm projection".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 2,
        original_turn_count: 2,
        compacted_count: 2,
    };
    session
        .commit_context_graph_transaction_with_compaction(
            crate::session::ContextGraphTransaction {
                schema_version: 1,
                op_id: "owned-lcm-op".to_string(),
                base_generation: 0,
                generation: 1,
                append_context_nodes: vec![node],
                frontier: crate::session::StoredContextFrontier {
                    schema_version: 1,
                    generation: 1,
                    active_node_ids: vec![node_id],
                    covered_message_count: 2,
                    covered_through_message_id: session
                        .messages
                        .last()
                        .map(|message| message.id.clone()),
                    source_prefix_sha256: source_sha256.clone(),
                    next_node_sequence: 2,
                },
                input_proof: crate::session::ContextGraphInputProof {
                    schema_version: 1,
                    source_session_id: session.id.clone(),
                    source_message_ids,
                    source_sha256,
                },
            },
            Some(compaction),
        )
        .expect("commit valid LCM projection");
    assert!(session.has_owned_native_lcm_projection());

    let mut agent = Agent::new_with_session(provider, registry, session, None);
    let provider_messages = agent.provider_messages();
    let provider_text = provider_messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(provider_text.contains("canonical raw alpha"));
    assert!(provider_text.contains("canonical raw beta"));
    assert!(!provider_text.contains("stale native lcm projection"));
    assert!(agent.session.compaction.is_none());
    assert!(!agent.session.has_owned_native_lcm_projection());
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        canonical_raw,
        "rolling rollback must retain exact canonical image and tool bytes"
    );
    assert!(matches!(
        agent.session.messages[0].content.as_slice(),
        [ContentBlock::Text { .. }, ContentBlock::Image { data, .. }]
            if data == "exact-original-image-bytes"
    ));
    assert!(matches!(
        agent.session.messages[1].content.as_slice(),
        [ContentBlock::Text { .. }, ContentBlock::ToolUse { id, thought_signature, .. }]
            if id == "exact-original-tool-id"
                && thought_signature.as_deref() == Some("exact-original-tool-signature")
    ));
    let serialized = serde_json::to_value(&agent.session).expect("serialize rolled-back session");
    assert_eq!(
        serialized["context_nodes"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(
        serialized["context_frontier"]["active_node_ids"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "rollback deactivates the frontier without deleting committed nodes"
    );

    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn payload_too_large_recovery_persists_projection_without_mutating_raw_history() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let big = "a".repeat(8 * 1024 * 1024);
    for index in 0..3 {
        agent.add_message(
            Role::User,
            vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: format!("{index}{big}"),
            }],
        );
    }
    let session_id = agent.session.id.clone();
    let canonical_before = serde_json::to_vec(&agent.session.messages).unwrap();

    let event = agent
        .try_auto_compact_after_context_limit(
            "Anthropic API error (413 Payload Too Large): request_too_large",
        )
        .expect("image payload should recover through provider projection");

    assert_eq!(event.trigger, "auto_recovery_payload");
    assert_eq!(
        serde_json::to_vec(&agent.session.messages).unwrap(),
        canonical_before
    );
    assert_eq!(
        agent
            .session
            .messages_for_provider()
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| matches!(block, ContentBlock::Image { .. }))
            .count(),
        1
    );
    drop(agent);

    let mut reloaded = Session::load(&session_id).expect("reload persisted recovery projection");
    assert_eq!(serde_json::to_vec(&reloaded.messages).unwrap(), canonical_before);
    assert_eq!(
        reloaded.suppress_oversized_images_for_provider(
            crate::compaction::PAYLOAD_IMAGE_CHAR_BUDGET,
        ),
        0
    );
    assert_eq!(
        reloaded
            .messages_for_provider()
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| matches!(block, ContentBlock::Image { .. }))
            .count(),
        1
    );
    crate::env::remove_var("JCODE_HOME");
}

#[tokio::test]
async fn restore_legacy_session_clears_unowned_resume_and_graphless_compaction() {
    let _guard = crate::storage::lock_test_env();
    let temp_home = tempfile::TempDir::new().expect("temp home");
    crate::env::set_var("JCODE_HOME", temp_home.path());
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(Arc::clone(&provider)).await;
    let mut agent = Agent::new(provider, registry);

    let mut legacy = crate::session::Session::create(None, None);
    legacy.provider_key = Some("openai".to_string());
    legacy.model = Some("native-auto-fixture".to_string());
    legacy.provider_session_id = Some("legacy-old-account-resume".to_string());
    legacy.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "canonical legacy raw message".to_string(),
            cache_control: None,
        }],
    );
    legacy.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "unowned legacy rolling summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    });
    legacy.save().expect("save legacy session");

    agent
        .restore_session(&legacy.id)
        .expect("legacy raw history should remain restorable");

    assert_eq!(agent.session.messages.len(), 1);
    assert!(agent.session.exact_runtime_identity.is_some());
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.session.provider_session_identity.is_none());
    assert!(agent.session.compaction.is_none());
    assert!(agent.session.context_nodes.is_empty());
    assert!(agent.session.context_frontier.is_none());
    assert!(
        agent
            .registry
            .compaction()
            .read()
            .await
            .persisted_state()
            .is_none()
    );
    let loaded = crate::session::Session::load(&legacy.id).expect("reload cleaned legacy session");
    assert!(loaded.exact_runtime_identity.is_some());
    assert!(loaded.provider_session_id.is_none());
    assert!(loaded.compaction.is_none());
    assert_eq!(loaded.messages.len(), 1);

    crate::env::remove_var("JCODE_HOME");
}

// ── InterruptSignal tests ────────────────────────────────────────────────

#[tokio::test]
async fn interrupt_signal_fire_before_notified_does_not_hang() {
    // Regression test: fire() called BEFORE notified().await must not hang.
    // The old code called notify_waiters() which drops the notification if
    // nobody is waiting yet. The flag is still set so the fast path catches it,
    // but only if the future is created before the flag check.
    let sig = InterruptSignal::new();
    sig.fire(); // fire before anyone is waiting
    tokio::time::timeout(std::time::Duration::from_millis(100), sig.notified())
        .await
        .expect("notified() hung when signal was already set before call");
}

#[tokio::test]
async fn interrupt_signal_fire_concurrent_with_notified() {
    // Regression test for the race window: fire() is called concurrently while
    // notified() is being set up. The fix (create future before flag check) ensures
    // the notify_waiters() in fire() wakes the registered future.
    let sig = Arc::new(InterruptSignal::new());
    let sig2 = Arc::clone(&sig);

    // Spawn a task that fires after a tiny delay, giving the main task time to
    // enter notified() but before it reaches notified().await.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        sig2.fire();
    });

    tokio::time::timeout(std::time::Duration::from_millis(500), sig.notified())
        .await
        .expect("notified() hung during concurrent fire()");
}

#[tokio::test]
async fn interrupt_signal_is_set_false_initially() {
    let sig = InterruptSignal::new();
    assert!(!sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_is_set_true_after_fire() {
    let sig = InterruptSignal::new();
    sig.fire();
    assert!(sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_reset_clears_flag() {
    let sig = InterruptSignal::new();
    sig.fire();
    assert!(sig.is_set());
    sig.reset();
    assert!(!sig.is_set());
}

#[tokio::test]
async fn interrupt_signal_notified_completes_after_fire() {
    let sig = Arc::new(InterruptSignal::new());
    let sig2 = Arc::clone(&sig);

    let handle = tokio::spawn(async move {
        sig2.notified().await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    sig.fire();

    tokio::time::timeout(std::time::Duration::from_millis(200), handle)
        .await
        .expect("notified() task timed out after fire()")
        .expect("task panicked");
}

#[tokio::test]
async fn new_agent_registers_active_pid_and_clear_swaps_it() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let first_session_id = agent.session_id().to_string();
    assert!(
        crate::session::active_session_ids().contains(&first_session_id),
        "fresh agent session should be tracked as active"
    );

    agent.clear().expect("clear session");

    let second_session_id = agent.session_id().to_string();
    let active = crate::session::active_session_ids();
    assert_ne!(first_session_id, second_session_id);
    assert!(
        active.contains(&second_session_id),
        "replacement session should be tracked as active"
    );
    assert!(
        !active.contains(&first_session_id),
        "cleared session should no longer be tracked as active"
    );
}

#[tokio::test]
async fn loaded_reasoning_effort_restore_failure_blocks_turn_before_mutation() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(IdentityProbeProvider {
        fail_effort_restore: true,
        invalidations: std::sync::atomic::AtomicUsize::new(0),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut session = Session::create(None, None);
    session.model = Some("identity-probe-model".to_string());
    session.reasoning_effort = Some("high".to_string());
    let mut agent = Agent::new_with_session(provider, registry, session, None);
    let initial_messages = agent.session.messages.len();
    let error = agent
        .run_once("must not be appended")
        .await
        .expect_err("unsafe loaded reasoning identity must block the turn");

    assert!(error.to_string().contains("Provider identity is not safe"));
    assert!(
        error
            .to_string()
            .contains("injected effort restore failure")
    );
    assert_eq!(agent.session.messages.len(), initial_messages);
}

#[tokio::test]
async fn account_transition_invalidation_reaches_all_live_agent_providers() {
    let first = Arc::new(IdentityProbeProvider {
        fail_effort_restore: false,
        invalidations: std::sync::atomic::AtomicUsize::new(0),
    });
    let second = Arc::new(IdentityProbeProvider {
        fail_effort_restore: false,
        invalidations: std::sync::atomic::AtomicUsize::new(0),
    });
    let first_trait: Arc<dyn Provider> = first.clone();
    let second_trait: Arc<dyn Provider> = second.clone();
    let first_registry = Registry::new(first_trait.clone()).await;
    let second_registry = Registry::new(second_trait.clone()).await;
    let _first_agent = Agent::new(first_trait, first_registry);
    let _second_agent = Agent::new(second_trait, second_registry);

    invalidate_all_live_agent_credentials().await;

    assert_eq!(
        first
            .invalidations
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        second
            .invalidations
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn lifecycle_save_failures_do_not_publish_clear_or_close() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let session_id = agent.session_id().to_string();
    let active_before = crate::session::active_session_ids();

    let _failure = crate::storage::inject_write_failure(None);
    assert!(agent.clear().is_err());
    assert_eq!(agent.session_id(), session_id);
    assert_eq!(crate::session::active_session_ids(), active_before);
    drop(_failure);

    let _failure = crate::storage::inject_nth_write_failure(None, 3);
    assert!(agent.clear().is_err());
    assert_eq!(agent.session_id(), session_id);
    assert_eq!(crate::session::active_session_ids(), active_before);
    assert_eq!(
        crate::session::Session::load(&session_id)
            .expect("load rolled-back session")
            .status,
        crate::session::SessionStatus::Active
    );
    drop(_failure);

    let path = crate::session::session_path(&session_id).expect("session path");
    let _failure = crate::storage::inject_write_failure(Some(path));
    assert!(agent.try_mark_closed().is_err());
    assert_eq!(agent.session.status, crate::session::SessionStatus::Active);
    assert!(crate::session::active_session_ids().contains(&session_id));
    assert_eq!(
        crate::session::Session::load(&session_id)
            .expect("load still-active session")
            .status,
        crate::session::SessionStatus::Active
    );
}

#[tokio::test]
async fn clear_double_failure_retains_handoff_marker_for_startup_reconciliation() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let source_id = agent.session_id().to_string();
    let marker_path = crate::session::session_path(&source_id)
        .expect("source path")
        .with_extension("handoff");

    // Atomic writes are: inert target snapshot + backup, handoff marker,
    // closed source snapshot + backup, active target snapshot, then source
    // rollback. Fail target activation and the immediately following rollback.
    let failure = crate::storage::inject_nth_write_failures(None, 6, 2);
    let error = agent.clear().expect_err("double failure must fail clear");
    assert!(error.contains("old-session rollback"));
    assert!(marker_path.exists(), "recovery marker was retired early");
    assert!(agent.provider_identity_error.is_some());
    drop(failure);

    crate::session::recover_crashed_sessions().expect("reconcile retained handoff");
    assert!(!marker_path.exists());
    assert!(matches!(
        crate::session::Session::load(&source_id)
            .expect("load reconciled source")
            .status,
        crate::session::SessionStatus::Crashed { .. }
    ));
}

#[tokio::test]
async fn model_switch_atomically_resets_provider_session_and_rolls_back_on_save_failure() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(SwitchingProvider {
        model: std::sync::Mutex::new("old-model".to_string()),
        reasoning_effort: std::sync::Mutex::new(None),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.provider_session_id = Some("runtime-resume".to_string());
    agent.session.provider_session_id = Some("durable-resume".to_string());
    agent.session.provider_key = None;
    agent.session.route_api_method = None;
    agent.session.model = Some("old-model".to_string());
    agent.session.save().expect("save initial identity");

    let mut concurrent =
        crate::session::Session::load(agent.session_id()).expect("load concurrent");
    concurrent.title = Some("concurrent revision".to_string());
    concurrent.save().expect("advance durable revision");
    assert!(agent.set_model("new-model").is_err());
    assert_eq!(agent.provider_model(), "old-model");
    assert_eq!(
        agent.session.provider_session_id.as_deref(),
        Some("durable-resume")
    );
    assert_eq!(agent.provider_session_id.as_deref(), Some("runtime-resume"));
    agent.session.persistence_revision = concurrent.persistence_revision;

    agent.set_model("new-model").expect("durable model switch");
    assert_eq!(agent.provider_model(), "new-model");
    assert!(agent.session.provider_session_id.is_none());
    assert!(agent.provider_session_id.is_none());
    let loaded = crate::session::Session::load(agent.session_id()).expect("load switched session");
    assert_eq!(loaded.model.as_deref(), Some("new-model"));
    assert!(loaded.provider_session_id.is_none());
}

#[tokio::test]
async fn reasoning_effort_rolls_back_absent_override_on_stale_cas() {
    let _guard = crate::storage::lock_test_env();
    let provider = Arc::new(SwitchingProvider {
        model: std::sync::Mutex::new("effort-model".to_string()),
        reasoning_effort: std::sync::Mutex::new(None),
    });
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_trait.clone()).await;
    let mut agent = Agent::new(provider_trait, registry);
    agent.session.reasoning_effort = None;
    agent.session.save().expect("save initial effort identity");

    let mut concurrent =
        crate::session::Session::load(agent.session_id()).expect("load concurrent session");
    concurrent.title = Some("advance effort CAS".to_string());
    concurrent.save().expect("advance durable revision");

    assert!(agent.set_reasoning_effort("high").is_err());
    assert_eq!(provider.reasoning_effort(), None);
    assert_eq!(agent.session.reasoning_effort, None);
    assert_eq!(
        crate::session::Session::load(agent.session_id())
            .expect("load durable effort")
            .reasoning_effort,
        None
    );
}

#[tokio::test]
async fn restore_session_clears_previous_reasoning_override_when_target_is_none() {
    let _guard = crate::storage::lock_test_env();
    let provider = Arc::new(SwitchingProvider {
        model: std::sync::Mutex::new("effort-model".to_string()),
        reasoning_effort: std::sync::Mutex::new(Some("high".to_string())),
    });
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let registry = Registry::new(provider_trait.clone()).await;
    let mut agent = Agent::new(provider_trait, registry);
    agent.session.model = Some("effort-model".to_string());
    agent.session.reasoning_effort = Some("high".to_string());
    agent.session.save().expect("save source effort");

    let mut target = crate::session::Session::create(None, None);
    target.model = Some("effort-model".to_string());
    target.reasoning_effort = None;
    target.status = crate::session::SessionStatus::Closed;
    target.save().expect("save target effort");

    agent
        .restore_session(&target.id)
        .expect("restore target with default effort");
    assert_eq!(provider.reasoning_effort(), None);
    assert_eq!(agent.session.reasoning_effort, None);
    assert_eq!(
        crate::session::Session::load(&target.id)
            .expect("load restored target")
            .reasoning_effort,
        None
    );
}

#[tokio::test]
async fn rewind_and_undo_save_failures_keep_the_live_transcript() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    for i in 0..3 {
        agent.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("turn {i}"),
                cache_control: None,
            }],
        );
    }
    agent.session.save().expect("save transcript");
    let _failure = crate::storage::inject_nth_write_failures(None, 1, 2);
    assert!(agent.rewind_to_message(1).is_err());
    assert_eq!(agent.session.visible_conversation_message_count(), 3);
    drop(_failure);

    agent.rewind_to_message(1).expect("rewind should succeed");
    assert_eq!(agent.session.visible_conversation_message_count(), 1);
    let _failure = crate::storage::inject_nth_write_failures(None, 1, 2);
    assert!(agent.undo_rewind().is_err());
    assert_eq!(agent.session.visible_conversation_message_count(), 1);
}

#[tokio::test]
async fn restore_close_failure_keeps_the_current_session_published() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let previous_session_id = agent.session_id().to_string();
    let mut target = crate::session::Session::create(None, None);
    target.save().expect("save restore target");

    let previous_path =
        crate::session::session_path(&previous_session_id).expect("previous session path");
    let _failure = crate::storage::inject_write_failure(Some(previous_path));
    assert!(agent.restore_session(&target.id).is_err());
    assert_eq!(agent.session_id(), previous_session_id);
    assert!(crate::session::active_session_ids().contains(&previous_session_id));
    assert_eq!(
        crate::session::Session::load(&previous_session_id)
            .expect("load previous session")
            .status,
        crate::session::SessionStatus::Active
    );
}

#[tokio::test]
async fn restore_target_activation_failure_rolls_back_previous_session() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.session.save().expect("save previous session");
    let previous_session_id = agent.session_id().to_string();
    let mut target = crate::session::Session::create(None, None);
    target.mark_closed();
    target.save().expect("save closed restore target");
    let target_path = crate::session::session_path(&target.id).expect("target path");
    let _failure = crate::storage::inject_write_failure(Some(target_path));

    assert!(agent.restore_session(&target.id).is_err());

    assert_eq!(agent.session_id(), previous_session_id);
    assert_eq!(agent.session.status, crate::session::SessionStatus::Active);
    assert_eq!(
        crate::session::Session::load(&previous_session_id)
            .expect("load rolled-back previous session")
            .status,
        crate::session::SessionStatus::Active
    );
    assert_eq!(
        crate::session::Session::load(&target.id)
            .expect("load unactivated target")
            .status,
        crate::session::SessionStatus::Closed
    );
}

#[tokio::test]
async fn restore_double_failure_retains_handoff_marker_for_startup_reconciliation() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.session.save().expect("save restore source");
    let source_id = agent.session_id().to_string();
    let marker_path = crate::session::session_path(&source_id)
        .expect("source path")
        .with_extension("handoff");
    let mut target = crate::session::Session::create(None, None);
    target.mark_closed();
    target.save().expect("save closed restore target");

    // Atomic writes are: handoff marker, closed source snapshot + backup,
    // active target snapshot, then source rollback. Fail activation and the
    // immediately following rollback, preserving the marker.
    let failure = crate::storage::inject_nth_write_failures(None, 4, 2);
    let error = agent
        .restore_session(&target.id)
        .expect_err("double failure must fail restore");
    assert!(error.to_string().contains("previous-session rollback"));
    assert!(marker_path.exists(), "recovery marker was retired early");
    assert!(agent.provider_identity_error.is_some());
    drop(failure);

    crate::session::recover_crashed_sessions().expect("reconcile retained restore handoff");
    assert!(!marker_path.exists());
    assert!(matches!(
        crate::session::Session::load(&source_id)
            .expect("load reconciled restore source")
            .status,
        crate::session::SessionStatus::Crashed { .. }
    ));
}

#[tokio::test]
async fn gmail_is_exposed_by_default_and_can_be_explicitly_disabled() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_tools = std::env::var_os("JCODE_TOOLS");
    let prev_disabled_tools = std::env::var_os("JCODE_DISABLED_TOOLS");
    let prev_tool_profile = std::env::var_os("JCODE_TOOL_PROFILE");
    let prev_disable_base_tools = std::env::var_os("JCODE_DISABLE_BASE_TOOLS");
    let temp_home = tempfile::TempDir::new().expect("temp home");

    crate::env::set_var("JCODE_HOME", temp_home.path());
    crate::env::remove_var("JCODE_TOOLS");
    crate::env::remove_var("JCODE_DISABLED_TOOLS");
    crate::env::remove_var("JCODE_TOOL_PROFILE");
    crate::env::remove_var("JCODE_DISABLE_BASE_TOOLS");
    crate::config::Config::invalidate_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let definitions = agent.tool_definitions().await;
    let tool_names = agent.tool_names().await;
    let tool_name = "gmail";

    assert!(
        definitions
            .iter()
            .any(|definition| definition.name == tool_name),
        "{tool_name} must be sent in model-visible tool definitions by default"
    );
    assert!(
        tool_names.iter().any(|name| name == tool_name),
        "{tool_name} must be listed as model-visible by default"
    );
    agent
        .validate_tool_allowed(tool_name)
        .expect("gmail must be executable by default");

    crate::env::set_var("JCODE_DISABLED_TOOLS", tool_name);
    crate::config::Config::invalidate_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let definitions = agent.tool_definitions().await;
    let tool_names = agent.tool_names().await;

    assert!(
        !definitions
            .iter()
            .any(|definition| definition.name == tool_name),
        "explicitly disabled {tool_name} must not be sent in model-visible tool definitions"
    );
    assert!(
        !tool_names.iter().any(|name| name == tool_name),
        "explicitly disabled {tool_name} must not be listed as model-visible"
    );
    let err = agent
        .validate_tool_allowed(tool_name)
        .expect_err("explicitly disabled gmail must not be executable");
    assert!(err.to_string().contains("disabled"));

    if let Some(previous) = prev_home {
        crate::env::set_var("JCODE_HOME", previous);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    if let Some(previous) = prev_tools {
        crate::env::set_var("JCODE_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_TOOLS");
    }
    if let Some(previous) = prev_disabled_tools {
        crate::env::set_var("JCODE_DISABLED_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_DISABLED_TOOLS");
    }
    if let Some(previous) = prev_tool_profile {
        crate::env::set_var("JCODE_TOOL_PROFILE", previous);
    } else {
        crate::env::remove_var("JCODE_TOOL_PROFILE");
    }
    if let Some(previous) = prev_disable_base_tools {
        crate::env::set_var("JCODE_DISABLE_BASE_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_DISABLE_BASE_TOOLS");
    }
    crate::config::Config::invalidate_cache();
}

fn seed_transient_session_state(agent: &mut Agent) {
    agent.push_alert("pending alert".to_string());
    agent.queue_soft_interrupt(
        "queued interrupt".to_string(),
        true,
        SoftInterruptSource::User,
    );
    agent.background_tool_signal.fire();
    agent.request_graceful_shutdown();
    agent.tool_call_ids.insert("tool_call_old".to_string());
    agent.tool_result_ids.insert("tool_result_old".to_string());
    agent.tool_output_scan_index = 7;
    agent.last_upstream_provider = Some("upstream_old".to_string());
    agent.last_connection_type = Some("websocket".to_string());
    agent.current_turn_system_reminder = Some("reminder".to_string());
    agent.last_usage = TokenUsage {
        input_tokens: 11,
        output_tokens: 17,
        cache_read_input_tokens: Some(3),
        cache_creation_input_tokens: Some(5),
    };
    agent.locked_tools = Some(vec![ToolDefinition {
        name: "test_tool".to_string(),
        description: "test tool".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
    }]);
}

#[tokio::test]
async fn clear_resets_runtime_interrupt_and_queue_state() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    seed_transient_session_state(&mut agent);
    assert_eq!(agent.soft_interrupt_count(), 1);
    assert!(agent.background_tool_signal().is_set());
    assert!(agent.graceful_shutdown_signal().is_set());

    agent.clear().expect("clear session");

    assert_eq!(agent.soft_interrupt_count(), 0);
    assert!(!agent.background_tool_signal().is_set());
    assert!(!agent.graceful_shutdown_signal().is_set());
    assert_eq!(agent.pending_alert_count(), 0);
    assert!(agent.tool_call_ids.is_empty());
    assert!(agent.tool_result_ids.is_empty());
    assert_eq!(agent.tool_output_scan_index, 0);
    assert!(agent.last_upstream_provider.is_none());
    assert!(agent.last_connection_type.is_none());
    assert!(agent.current_turn_system_reminder.is_none());
    assert_eq!(agent.last_usage.input_tokens, 0);
    assert_eq!(agent.last_usage.output_tokens, 0);
    assert!(agent.locked_tools.is_none());
}

#[tokio::test]
async fn restore_session_resets_runtime_interrupt_and_queue_state() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let previous_session_id = agent.session_id().to_string();

    let mut restored_session = crate::session::Session::create(None, None);
    restored_session.save().expect("save restored session");

    seed_transient_session_state(&mut agent);
    assert_eq!(agent.soft_interrupt_count(), 1);
    assert!(agent.background_tool_signal().is_set());
    assert!(agent.graceful_shutdown_signal().is_set());

    let status = agent
        .restore_session(&restored_session.id)
        .expect("restore session should succeed");

    assert_eq!(status, crate::session::SessionStatus::Active);
    assert_eq!(agent.session_id(), restored_session.id);
    assert_eq!(
        crate::session::Session::load(&previous_session_id)
            .expect("load replaced session")
            .status,
        crate::session::SessionStatus::Closed
    );
    assert_eq!(agent.soft_interrupt_count(), 0);
    assert!(!agent.background_tool_signal().is_set());
    assert!(!agent.graceful_shutdown_signal().is_set());
    assert_eq!(agent.pending_alert_count(), 0);
    assert!(agent.tool_call_ids.is_empty());
    assert!(agent.tool_result_ids.is_empty());
    assert_eq!(agent.tool_output_scan_index, 0);
    assert!(agent.last_upstream_provider.is_none());
    assert!(agent.last_connection_type.is_none());
    assert!(agent.current_turn_system_reminder.is_none());
    assert_eq!(agent.last_usage.input_tokens, 0);
    assert_eq!(agent.last_usage.output_tokens, 0);
    assert!(agent.locked_tools.is_none());
}

#[tokio::test]
async fn restore_session_rehydrates_injected_memory_ids() {
    let _guard = crate::storage::lock_test_env();
    crate::memory::clear_all_pending_memory();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let mut restored_session = crate::session::Session::create(None, None);
    restored_session.record_memory_injection(
        "🧠 auto-recalled 1 memory".to_string(),
        "persisted memory".to_string(),
        1,
        5,
        vec!["memory-persisted".to_string()],
    );
    restored_session.save().expect("save restored session");

    crate::memory::mark_memories_injected(&restored_session.id, &["memory-stale".to_string()]);

    agent
        .restore_session(&restored_session.id)
        .expect("restore session should succeed");

    assert!(crate::memory::is_memory_injected(
        &restored_session.id,
        "memory-persisted"
    ));
    assert!(
        !crate::memory::is_memory_injected(&restored_session.id, "memory-stale"),
        "restore should replace stale in-memory dedup state with persisted session data"
    );

    crate::memory::clear_all_pending_memory();
}

#[tokio::test]
async fn build_memory_prompt_nonblocking_defers_pending_memory_during_tool_loop() {
    let _guard = crate::storage::lock_test_env();
    crate::memory::clear_all_pending_memory();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let agent = Agent::new(provider, registry);
    let session_id = agent.session.id.clone();

    crate::memory::set_pending_memory_with_ids(
        &session_id,
        "remember this later".to_string(),
        1,
        vec!["memory-deferred".to_string()],
    );

    let tool_loop_messages = vec![
        Message::user("hello"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({}),
                thought_signature: None,
            }],
            timestamp: Some(chrono::Utc::now()),
            tool_duration_ms: None,
        },
        Message::tool_result("call_1", "ok", false),
    ];

    let pending = agent.build_memory_prompt_nonblocking(&tool_loop_messages, None);
    assert!(pending.is_none(), "memory should not inject mid tool loop");
    assert!(crate::memory::has_pending_memory(&session_id));

    let next_turn_messages = vec![Message::user("follow up")];
    let pending = agent.build_memory_prompt_nonblocking(&next_turn_messages, None);
    assert!(
        pending.is_some(),
        "memory should inject on the next real user turn"
    );
    assert!(!crate::memory::has_pending_memory(&session_id));

    crate::memory::clear_all_pending_memory();
}

#[tokio::test]
async fn memory_injection_message_defaults_to_ephemeral_history() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_PERSIST_MEMORY_INJECTIONS");
    crate::env::set_var("JCODE_PERSIST_MEMORY_INJECTIONS", "false");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let before = agent.session.messages.len();
    let memory = crate::memory::PendingMemory {
        prompt: "# Memory\n\n## Facts\n1. Use ephemeral mode".to_string(),
        display_prompt: None,
        computed_at: Instant::now(),
        count: 1,
        memory_ids: vec!["mem-ephemeral".to_string()],
    };

    let (message, persisted) = agent.prepare_memory_injection_message(&memory);

    assert!(!persisted);
    assert_eq!(agent.session.messages.len(), before);
    assert!(matches!(message.role, Role::User));
    assert!(message_text(&message).contains("Use ephemeral mode"));

    match previous {
        Some(value) => crate::env::set_var("JCODE_PERSIST_MEMORY_INJECTIONS", value),
        None => crate::env::remove_var("JCODE_PERSIST_MEMORY_INJECTIONS"),
    }
    crate::config::invalidate_config_cache();
}

#[tokio::test]
async fn memory_injection_message_can_persist_to_history() {
    let _guard = crate::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_PERSIST_MEMORY_INJECTIONS");
    crate::env::set_var("JCODE_PERSIST_MEMORY_INJECTIONS", "true");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let before = agent.session.messages.len();
    let memory = crate::memory::PendingMemory {
        prompt: "# Memory\n\n## Facts\n1. Persist for cache".to_string(),
        display_prompt: None,
        computed_at: Instant::now(),
        count: 1,
        memory_ids: vec!["mem-persisted".to_string()],
    };

    let (message, persisted) = agent.prepare_memory_injection_message(&memory);

    assert!(persisted);
    assert_eq!(agent.session.messages.len(), before + 1);
    assert_eq!(
        content_text(&agent.session.messages.last().unwrap().content),
        message_text(&message)
    );
    assert!(
        content_text(&agent.session.messages.last().unwrap().content).contains("Persist for cache")
    );

    match previous {
        Some(value) => crate::env::set_var("JCODE_PERSIST_MEMORY_INJECTIONS", value),
        None => crate::env::remove_var("JCODE_PERSIST_MEMORY_INJECTIONS"),
    }
    crate::config::invalidate_config_cache();
}

#[tokio::test]
async fn mark_closed_persists_soft_interrupts_for_restore_after_reload() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider.clone(), registry.clone());
    let session_id = agent.session_id().to_string();
    agent.session.save().expect("save active session");
    agent.queue_soft_interrupt(
        "resume me after reload".to_string(),
        true,
        SoftInterruptSource::System,
    );

    agent.mark_closed();

    let mut restored = Agent::new(provider, registry);
    restored
        .restore_session(&session_id)
        .expect("restore session with persisted interrupts");

    assert_eq!(restored.soft_interrupt_count(), 1);
    assert!(restored.has_urgent_interrupt());
    assert!(
        crate::soft_interrupt_store::load(&session_id)
            .expect("store should be readable after restore")
            .is_empty()
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}
