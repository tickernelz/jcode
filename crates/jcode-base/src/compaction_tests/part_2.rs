#[test]
fn lcm_process_death_during_inflight_attempt_recovers_canonical_session() -> Result<()> {
    const CHILD_ENV: &str = "JCODE_TEST_LCM_PROCESS_DEATH_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let home =
            std::path::PathBuf::from(std::env::var_os("JCODE_HOME").expect("child JCODE_HOME"));
        std::fs::create_dir_all(&home)?;
        std::fs::write(home.join("config.toml"), "[compaction]\nengine = \"lcm\"\n")?;
        crate::config::invalidate_config_cache();
        let mut session = make_lcm_session("lcm_real_process_death", 30);
        session.save()?;
        std::fs::write(home.join("crash-session-id"), &session.id)?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        runtime.block_on(async move {
            let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let provider: Arc<dyn Provider> = Arc::new(StallingProvider {
                started: Arc::clone(&started),
            });
            let mut manager = CompactionManager::new().with_budget(1_000);
            manager.engine = crate::config::CompactionEngine::Lcm;
            for _ in 0..session.messages.len() {
                manager.notify_message_added();
            }
            manager
                .start_lcm_job(
                    &session,
                    provider,
                    &session.messages_for_provider_uncached(),
                    20,
                    "reactive".to_string(),
                )
                .expect("start native LCM crash-campaign job");
            tokio::time::timeout(Duration::from_secs(5), async {
                while started.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("provider attempt should enter its in-flight state");
            std::fs::write(home.join("provider-in-flight"), b"ready")
                .expect("write in-flight marker");
            std::process::abort();
        });
        unreachable!("process abort must terminate the child");
    }

    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let test_name =
        "compaction::tests::lcm_process_death_during_inflight_attempt_recovers_canonical_session";
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("JCODE_HOME", home.path())
        .status()?;
    assert!(!status.success(), "child must terminate by process abort");
    assert!(
        home.path().join("provider-in-flight").exists(),
        "child died before proving an in-flight provider attempt"
    );

    let session_id = std::fs::read_to_string(home.path().join("crash-session-id"))?;
    let _home = TestHomeGuard::set(home.path());
    let recovered = crate::session::Session::load(session_id.trim())?;
    assert_eq!(recovered.messages.len(), 30);
    assert!(recovered.context_nodes.is_empty());
    assert!(recovered.context_frontier.is_none());
    assert!(recovered.compaction.is_none());
    Ok(())
}

#[test]
fn lcm_prepared_leaf_commits_graph_and_projection_before_materialization() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_prepared_leaf", 20);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..20 {
        manager.notify_message_added();
    }
    let source = manager.capture_lcm_source(
        &session,
        10,
        "compactor-model".to_string(),
        "compactor-provider".to_string(),
        "openai-api:compactor-model".to_string(),
        None,
        850,
        "reactive".to_string(),
    )?;
    manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
        source,
        CompactionResult {
            summary_text: "durable LCM leaf".to_string(),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 10,
            duration_ms: 7,
            summarized_messages: 10,
        },
    )?);

    let (messages, event) = manager.materialize_lcm_context(&mut session)?;
    assert!(event.is_some());
    assert_eq!(messages.len(), 11);
    assert_eq!(session.context_nodes.len(), 1);
    assert_eq!(session.context_frontier.as_ref().unwrap().generation, 1);
    assert_eq!(session.compaction.as_ref().unwrap().compacted_count, 10);
    assert!(
        session
            .compaction
            .as_ref()
            .unwrap()
            .openai_encrypted_content
            .is_none()
    );

    let loaded = crate::session::Session::load("lcm_prepared_leaf")?;
    assert_eq!(loaded.context_nodes.len(), 1);
    assert!(
        loaded
            .compaction
            .as_ref()
            .unwrap()
            .summary_text
            .contains("durable LCM leaf")
    );
    let snapshot_path = crate::session::session_path("lcm_prepared_leaf")?;
    let mut snapshot = serde_json::to_value(&loaded)?;
    snapshot["context_nodes"][0]["source_runtime_identity_sha256"] =
        serde_json::Value::String("d".repeat(64));
    std::fs::write(&snapshot_path, serde_json::to_vec_pretty(&snapshot)?)?;
    let journal_path = crate::session::session_journal_path("lcm_prepared_leaf")?;
    if journal_path.exists() {
        std::fs::remove_file(journal_path)?;
    }
    let rejected = crate::session::Session::load("lcm_prepared_leaf")?;
    assert!(rejected.context_nodes.is_empty());
    assert!(rejected.context_frontier.is_none());
    assert!(rejected.compaction.is_none());
    Ok(())
}

#[test]
fn lcm_stale_source_is_not_published() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_stale_source", 20);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..20 {
        manager.notify_message_added();
    }
    let source = manager.capture_lcm_source(
        &session,
        10,
        "model".to_string(),
        "provider".to_string(),
        "route".to_string(),
        None,
        850,
        "reactive".to_string(),
    )?;
    manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
        source,
        CompactionResult {
            summary_text: "must stay hidden".to_string(),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 10,
            duration_ms: 1,
            summarized_messages: 10,
        },
    )?);
    session.messages[0].content = vec![ContentBlock::Text {
        text: "same position, divergent content".to_string(),
        cache_control: None,
    }];

    assert!(manager.materialize_lcm_context(&mut session).is_err());
    assert!(session.context_nodes.is_empty());
    assert!(session.context_frontier.is_none());
    assert!(manager.active_summary.is_none());
    Ok(())
}

#[test]
fn lcm_completed_source_validation_uses_captured_raw_range() -> Result<()> {
    let mut session = make_lcm_session("lcm_captured_raw_range", 20);
    session.context_frontier = Some(crate::session::StoredContextFrontier {
        schema_version: 1,
        generation: 1,
        active_node_ids: Vec::new(),
        covered_message_count: 1,
        covered_through_message_id: Some(session.messages[0].id.clone()),
        source_prefix_sha256: stored_message_prefix_sha256(&session.messages[..1])?,
        next_node_sequence: 2,
    });
    let manager = CompactionManager::new();
    let source = manager.capture_lcm_source(
        &session,
        10,
        "model".into(),
        "provider".into(),
        "route".into(),
        None,
        850,
        "critical_selected_route".into(),
    )?;

    assert_eq!(source.source_message_ids.len(), 9);
    assert!(lcm_source_matches_canonical_session(&source, &session));

    let original_prefix = session.messages[0].content.clone();
    session.messages[0].content = vec![ContentBlock::Text {
        text: "changed covered prefix".into(),
        cache_control: None,
    }];
    assert!(!lcm_source_matches_canonical_session(&source, &session));
    session.messages[0].content = original_prefix;

    session.messages[1].content = vec![ContentBlock::Text {
        text: "changed captured source".into(),
        cache_control: None,
    }];
    assert!(!lcm_source_matches_canonical_session(&source, &session));
    Ok(())
}

#[test]
fn lcm_resume_restores_only_explicit_native_graph_projection() {
    let session = make_lcm_session("lcm_native_resume", 20);
    let state = crate::session::StoredCompactionState {
        summary_text: "native graph projection".into(),
        openai_encrypted_content: None,
        covers_up_to_turn: 10,
        original_turn_count: 10,
        compacted_count: 10,
    };
    let mut manager = CompactionManager::new();
    manager.engine = crate::config::CompactionEngine::Lcm;

    manager.restore_persisted_stored_state_with(&state, &session.messages);
    assert_eq!(manager.compacted_count, 0);
    assert!(manager.active_summary.is_none());

    manager.restore_native_lcm_stored_state_with(&state, &session.messages);
    assert_eq!(manager.compacted_count, 10);
    assert_eq!(
        manager
            .active_summary
            .as_ref()
            .map(|summary| summary.text.as_str()),
        Some("native graph projection")
    );
}

#[test]
fn lcm_critical_cutoff_preserves_ten_unless_the_tail_itself_overflows() {
    let normal = (0..12)
        .map(|index| make_text_message(Role::User, &format!("message {index}")))
        .collect::<Vec<_>>();
    assert_eq!(critical_lcm_cutoff(&normal, 1_000), 2);

    let oversized = vec![
        make_text_message(Role::User, &"large observed source ".repeat(3_000)),
        make_text_message(Role::Assistant, "short response"),
        make_text_message(Role::User, "short follow-up"),
    ];
    assert_eq!(critical_lcm_cutoff(&oversized, 1_000), 1);

    let canary_shape = vec![
        make_text_message(Role::User, &"injected context ".repeat(40)),
        make_text_message(Role::User, &"pressure source ".repeat(3_500)),
        make_text_message(Role::Assistant, "short response"),
        make_text_message(Role::User, "short follow-up"),
    ];
    assert_eq!(critical_lcm_cutoff(&canary_shape, 12_000), 2);
}

#[test]
fn lcm_durable_write_failure_keeps_candidate_and_live_state_unpublished() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_write_failure", 20);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..20 {
        manager.notify_message_added();
    }
    let source = manager.capture_lcm_source(
        &session,
        10,
        "model".into(),
        "provider".into(),
        "route".into(),
        None,
        850,
        "reactive".into(),
    )?;
    let attempt_id = source.attempt_context("leaf").attempt_id;
    manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
        source,
        CompactionResult {
            summary_text: "never publish before durable write".into(),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 10,
            duration_ms: 1,
            summarized_messages: 10,
        },
    )?);
    std::fs::write(home.path().join("sessions"), b"not a directory")?;

    assert!(manager.materialize_lcm_context(&mut session).is_err());
    assert!(manager.is_compacting());
    assert!(session.context_nodes.is_empty());
    assert!(session.context_frontier.is_none());
    assert!(session.compaction.is_none());
    assert!(manager.active_summary.is_none());
    let retry_events = take_lcm_attempt_test_events(&attempt_id);
    assert_eq!(
        retry_events
            .iter()
            .filter_map(|event| event
                .iter()
                .find_map(|(key, value)| (key == "outcome").then_some(value.as_str())))
            .collect::<Vec<_>>(),
        ["persistence_retry"]
    );

    std::fs::remove_file(home.path().join("sessions"))?;
    std::fs::create_dir(home.path().join("sessions"))?;
    let (_, event) = manager.materialize_lcm_context(&mut session)?;
    assert!(event.is_some());
    let terminal_events = take_lcm_attempt_test_events(&attempt_id);
    assert_eq!(terminal_events.len(), 1);
    let published = &terminal_events[0];
    assert!(
        published
            .iter()
            .any(|(key, value)| key == "outcome" && value == "published")
    );
    assert!(
        published
            .iter()
            .any(|(key, value)| { key == "reason_code" && value == "durable_commit_succeeded" })
    );
    assert!(
        published
            .iter()
            .any(|(key, value)| { key == "persistence_ms" && value.parse::<u64>().is_ok() })
    );
    Ok(())
}

#[test]
fn lcm_appends_leaf_without_rewriting_existing_provider_prefix() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_cache_stable_leaves", 20);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..20 {
        manager.notify_message_added();
    }

    let first_source = manager.capture_lcm_source(
        &session,
        10,
        "model".into(),
        "provider".into(),
        "route".into(),
        None,
        850,
        "reactive".into(),
    )?;
    manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
        first_source,
        CompactionResult {
            summary_text: "first stable leaf".into(),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 10,
            duration_ms: 1,
            summarized_messages: 10,
        },
    )?);
    let (first_messages, _) = manager.materialize_lcm_context(&mut session)?;
    let stable_prefix = first_messages[0].clone();

    for index in 20..30 {
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("turn {index} {}", "y".repeat(120)),
                cache_control: None,
            }],
        );
        manager.notify_message_added();
    }
    let second_source = manager.capture_lcm_source(
        &session,
        10,
        "model".into(),
        "provider".into(),
        "route".into(),
        None,
        850,
        "reactive".into(),
    )?;
    manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
        second_source,
        CompactionResult {
            summary_text: "second stable leaf".into(),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 10,
            duration_ms: 1,
            summarized_messages: 10,
        },
    )?);
    let (second_messages, _) = manager.materialize_lcm_context(&mut session)?;

    assert_eq!(
        crate::message::stable_message_hash(&second_messages[0]),
        crate::message::stable_message_hash(&stable_prefix)
    );
    assert_eq!(session.context_nodes.len(), 2);
    assert_eq!(
        session
            .context_frontier
            .as_ref()
            .unwrap()
            .active_node_ids
            .len(),
        2
    );
    assert_eq!(second_messages.len(), 12); // two stable leaves + fresh tail 10
    Ok(())
}

#[test]
fn lcm_condenses_four_leaf_suffix_into_durable_parent() -> Result<()> {
    use sha2::{Digest, Sha256};

    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_hierarchy", 40);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..40 {
        manager.notify_message_added();
    }
    for leaf in 1..=4 {
        let source = manager.capture_lcm_source(
            &session,
            10,
            "model".into(),
            "provider".into(),
            "route".into(),
            None,
            850,
            "reactive".into(),
        )?;
        manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
            source,
            CompactionResult {
                summary_text: format!("leaf {leaf}"),
                atomic_parent_summaries: Vec::new(),
                openai_encrypted_content: None,
                covers_up_to_turn: 10,
                duration_ms: 1,
                summarized_messages: 10,
            },
        )?);
        manager.materialize_lcm_context(&mut session)?;
    }

    let immutable_children = session.context_nodes.clone();
    let frontier = session.context_frontier.clone().unwrap();
    let mut seen = std::collections::HashSet::new();
    let raw_source_ids = immutable_children
        .iter()
        .flat_map(|node| node.source_message_ids.iter().cloned())
        .filter(|id| seen.insert(id.clone()))
        .collect::<Vec<_>>();
    let child_ids = immutable_children
        .iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    let child_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&immutable_children)?)
    );
    let source = super::PendingLcmSource {
        session_id: session.id.clone(),
        base_generation: frontier.generation,
        generation: frontier.generation + 1,
        next_node_sequence: frontier.next_node_sequence,
        covered_message_count: frontier.covered_message_count,
        source_message_ids: raw_source_ids,
        input_proof_ids: child_ids,
        source_sha256: child_sha256,
        source_runtime_identity_sha256: immutable_children[0]
            .source_runtime_identity_sha256
            .clone(),
        source_prefix_sha256: frontier.source_prefix_sha256,
        covered_through_message_id: frontier.covered_through_message_id.unwrap(),
        prior_active_nodes: Vec::new(),
        node_level: 1,
        child_nodes: immutable_children.clone(),
        summarizer_model: "model".into(),
        summarizer_provider: "provider".into(),
        summarizer_route: "route".into(),
        summarizer_runtime_identity_sha256: immutable_children[0]
            .summarizer_runtime_identity_sha256
            .clone(),
        configured_model: None,
        cutoff: 0,
        source_fingerprint: None,
        pre_tokens: 850,
        trigger: "hierarchy".into(),
        route_policy_fingerprint: lcm_route_policy_fingerprint(&session, None),
        attempt_terminal_emitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
        source,
        CompactionResult {
            summary_text: "durable parent".into(),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 0,
            duration_ms: 1,
            summarized_messages: 4,
        },
    )?);
    let (messages, _) = manager.materialize_lcm_context(&mut session)?;

    assert_eq!(&session.context_nodes[..4], immutable_children.as_slice());
    assert_eq!(session.context_nodes.len(), 5);
    let active_ids = session
        .context_frontier
        .as_ref()
        .unwrap()
        .active_node_ids
        .clone();
    assert_eq!(active_ids.len(), 1);
    let parent = session
        .context_nodes
        .iter()
        .find(|node| node.id == active_ids[0])
        .unwrap();
    assert_eq!(parent.level, 1);
    assert_eq!(parent.child_node_ids.len(), 4);
    assert_eq!(messages.len(), 1);

    let loaded = crate::session::Session::load("lcm_hierarchy")?;
    assert_eq!(loaded.context_nodes.len(), 5);
    assert_eq!(loaded.context_frontier.unwrap().active_node_ids, active_ids);

    let canonical_message_count = session.messages.len();
    session.rewind_active_branch_through(19)?;
    session.save()?;
    assert_eq!(session.messages.len(), canonical_message_count);
    assert_eq!(session.context_nodes.len(), 5);
    assert_eq!(
        session
            .context_frontier
            .as_ref()
            .unwrap()
            .active_node_ids
            .len(),
        2
    );
    assert_eq!(
        session
            .context_frontier
            .as_ref()
            .unwrap()
            .covered_message_count,
        20
    );
    let mut rewound = crate::session::Session::load("lcm_hierarchy")?;
    assert_eq!(rewound.context_nodes.len(), 5);
    assert_eq!(rewound.compaction.as_ref().unwrap().compacted_count, 20);

    let next_node_sequence = rewound
        .context_frontier
        .as_ref()
        .unwrap()
        .next_node_sequence;
    rewound.retain_context_graph_prefix(0)?;
    rewound.archived_message_ids = rewound
        .messages
        .iter()
        .map(|message| message.id.clone())
        .collect();
    rewound
        .test_validate_context_graph_state()
        .expect("fully inactive graph must remain valid before persistence");
    rewound.save()?;
    rewound
        .test_validate_context_graph_state()
        .expect("fully inactive graph must remain valid after persistence");
    let serialized: crate::session::Session = serde_json::from_slice(&std::fs::read(
        crate::session::session_path("lcm_hierarchy")?,
    )?)?;
    serialized
        .test_validate_context_graph_state()
        .expect("serialized inactive graph must remain valid before journal replay");
    assert_eq!(rewound.messages.len(), canonical_message_count);
    assert_eq!(rewound.context_nodes.len(), 5);
    for node in &rewound.context_nodes {
        assert_eq!(
            node.id,
            crate::session::test_stored_context_node_id(node),
            "inactive context node must retain its immutable identity"
        );
    }
    let empty_frontier = rewound.context_frontier.as_ref().unwrap();
    assert!(empty_frontier.active_node_ids.is_empty());
    assert_eq!(empty_frontier.covered_message_count, 0);
    assert_eq!(empty_frontier.next_node_sequence, next_node_sequence);

    let mut empty_reloaded = crate::session::Session::load("lcm_hierarchy")?;
    assert_eq!(empty_reloaded.context_nodes.len(), 5);
    assert!(
        empty_reloaded
            .context_frontier
            .as_ref()
            .unwrap()
            .active_node_ids
            .is_empty()
    );

    for index in 0..10 {
        empty_reloaded.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("fresh branch {index}"),
                cache_control: None,
            }],
        );
    }
    let mut rebuilt_manager = CompactionManager::new().with_budget(1_000);
    rebuilt_manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..10 {
        rebuilt_manager.notify_message_added();
    }
    let rebuilt_source = rebuilt_manager.capture_lcm_source(
        &empty_reloaded,
        10,
        "compactor-model".to_string(),
        "compactor-provider".to_string(),
        "openai-api:compactor-model".to_string(),
        None,
        850,
        "reactive".to_string(),
    )?;
    rebuilt_manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
        rebuilt_source,
        CompactionResult {
            summary_text: "fresh branch summary".into(),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 10,
            duration_ms: 1,
            summarized_messages: 10,
        },
    )?);
    rebuilt_manager.materialize_lcm_context(&mut empty_reloaded)?;
    assert_eq!(empty_reloaded.context_nodes.len(), 6);
    assert_eq!(
        empty_reloaded
            .context_frontier
            .as_ref()
            .unwrap()
            .active_node_ids
            .len(),
        1
    );
    Ok(())
}

#[test]
fn lcm_fourth_leaf_and_parent_publish_atomically() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_atomic_hierarchy", 40);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;

    for leaf in 1..=4 {
        let source = manager.capture_lcm_source(
            &session,
            10,
            "model".into(),
            "provider".into(),
            "route".into(),
            None,
            850,
            "reactive".into(),
        )?;
        manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
            source,
            CompactionResult {
                summary_text: format!("leaf {leaf}"),
                atomic_parent_summaries: if leaf == 4 {
                    vec!["atomic parent".to_string()]
                } else {
                    Vec::new()
                },
                openai_encrypted_content: None,
                covers_up_to_turn: 10,
                duration_ms: 1,
                summarized_messages: 10,
            },
        )?);
        manager.materialize_lcm_context(&mut session)?;
    }

    let frontier = session.context_frontier.as_ref().unwrap();
    assert_eq!(frontier.generation, 4);
    assert_eq!(frontier.next_node_sequence, 6);
    assert_eq!(frontier.active_node_ids.len(), 1);
    assert_eq!(session.context_nodes.len(), 5);
    let parent = session
        .context_nodes
        .iter()
        .find(|node| node.id == frontier.active_node_ids[0])
        .unwrap();
    assert_eq!(parent.level, 1);
    assert_eq!(parent.child_node_ids.len(), 4);
    assert!(parent.summary_sha256.is_some());

    let loaded = crate::session::Session::load("lcm_atomic_hierarchy")?;
    assert_eq!(loaded.context_nodes, session.context_nodes);
    assert_eq!(loaded.context_frontier, session.context_frontier);
    Ok(())
}

#[test]
fn lcm_recursive_hierarchy_carry_publishes_in_one_generation() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_recursive_atomic_hierarchy", 160);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;

    for leaf in 1..=16 {
        let source = manager.capture_lcm_source(
            &session,
            10,
            "model".into(),
            "provider".into(),
            "route".into(),
            None,
            850,
            "reactive".into(),
        )?;
        let atomic_parent_summaries = if leaf == 16 {
            vec![
                "level one parent 4".to_string(),
                "level two root".to_string(),
            ]
        } else if leaf % 4 == 0 {
            vec![format!("level one parent {}", leaf / 4)]
        } else {
            Vec::new()
        };
        manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(
            source,
            CompactionResult {
                summary_text: format!("leaf {leaf}"),
                atomic_parent_summaries,
                openai_encrypted_content: None,
                covers_up_to_turn: 10,
                duration_ms: 1,
                summarized_messages: 10,
            },
        )?);
        manager.materialize_lcm_context(&mut session)?;
    }

    let frontier = session.context_frontier.as_ref().unwrap();
    assert_eq!(frontier.generation, 16);
    assert_eq!(frontier.next_node_sequence, 22);
    assert_eq!(frontier.active_node_ids.len(), 1);
    assert_eq!(session.context_nodes.len(), 21);
    let root = session
        .context_nodes
        .iter()
        .find(|node| node.id == frontier.active_node_ids[0])
        .unwrap();
    assert_eq!(root.level, 2);
    assert_eq!(root.child_node_ids.len(), 4);
    assert!(root.child_node_ids.iter().all(|id| {
        session
            .context_nodes
            .iter()
            .any(|node| node.id == *id && node.level == 1)
    }));

    let mut loaded = crate::session::Session::load("lcm_recursive_atomic_hierarchy")?;
    assert_eq!(loaded.context_nodes, session.context_nodes);
    assert_eq!(loaded.context_frontier, session.context_frontier);
    loaded.retain_context_graph_prefix(150)?;
    loaded.messages.truncate(150);
    let rewound_frontier = loaded.context_frontier.as_ref().unwrap();
    assert_eq!(rewound_frontier.covered_message_count, 150);
    let rewound_levels = rewound_frontier
        .active_node_ids
        .iter()
        .map(|id| {
            loaded
                .context_nodes
                .iter()
                .find(|node| node.id == *id)
                .unwrap()
                .level
        })
        .collect::<Vec<_>>();
    assert_eq!(rewound_levels, vec![1, 1, 1, 0, 0, 0]);
    loaded.save()?;
    let rewound = crate::session::Session::load("lcm_recursive_atomic_hierarchy")?;
    assert_eq!(rewound.context_frontier, loaded.context_frontier);
    assert_eq!(rewound.context_nodes, loaded.context_nodes);
    Ok(())
}

#[test]
fn critical_lcm_recovery_is_synchronous_portable_and_durable() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_critical", 20);
    let opaque_secret = "vault/q7Vn4Zp9Lx2Kc8Mw5Rt1Hs6Bd3Yf.rs";
    let labeled_secret = "correct horse battery staple";
    let ordinary_path = "src/ordinary_module.rs";
    let ContentBlock::Text { text, .. } = &mut session.messages[0].content[0] else {
        panic!("LCM fixture must start with text")
    };
    *text = format!("{opaque_secret}\nSession credential: {labeled_secret}\n{ordinary_path}");
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..20 {
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(960);

    let action = manager.ensure_lcm_context_fits(&mut session, Arc::new(MockSummaryProvider));
    let CompactionAction::HardCompacted(dropped) = action else {
        panic!("critical LCM must synchronously recover")
    };
    assert!(dropped > 0);
    assert!(!manager.is_compacting());
    let event = manager
        .take_compaction_event()
        .expect("critical LCM compaction event");
    assert_eq!(event.engine.as_deref(), Some("lcm"));
    assert_eq!(event.ownership.as_deref(), Some("lcm"));
    assert_eq!(
        event.fallback_reason.as_deref(),
        Some(
            "selected_route_failed>active_route_failed_or_unavailable>legacy_summary_rejected_or_unavailable>local_emergency_succeeded"
        )
    );
    assert_eq!(session.context_nodes.len(), 1);
    assert_eq!(session.context_nodes[0].summarizer_route, "local:emergency");
    assert!(
        !session.context_nodes[0]
            .summary_text
            .contains(opaque_secret)
    );
    assert!(
        !session.context_nodes[0]
            .summary_text
            .contains(labeled_secret)
    );
    assert!(
        session.context_nodes[0]
            .summary_text
            .contains(ordinary_path)
    );
    assert!(
        session
            .compaction
            .as_ref()
            .unwrap()
            .openai_encrypted_content
            .is_none()
    );
    let mut loaded = crate::session::Session::load("lcm_critical")?;
    assert_eq!(
        loaded
            .context_frontier
            .as_ref()
            .unwrap()
            .covered_message_count,
        dropped
    );
    assert!(!loaded.context_nodes[0].summary_text.contains(opaque_secret));
    assert!(
        !loaded.context_nodes[0]
            .summary_text
            .contains(labeled_secret)
    );
    assert!(loaded.context_nodes[0].summary_text.contains(ordinary_path));
    assert!(
        !loaded
            .compaction
            .as_ref()
            .unwrap()
            .summary_text
            .contains(opaque_secret)
    );
    assert!(
        !loaded
            .compaction
            .as_ref()
            .unwrap()
            .summary_text
            .contains(labeled_secret)
    );
    loaded.context_nodes[0]
        .summary_text
        .push_str(&format!("\nFiles referenced: {opaque_secret}"));
    loaded
        .compaction
        .as_mut()
        .unwrap()
        .summary_text
        .push_str(&format!("\nFiles referenced: {opaque_secret}"));
    let mut restored_manager = CompactionManager::new().with_budget(1_000);
    restored_manager.engine = crate::config::CompactionEngine::Lcm;
    restored_manager.restore_native_lcm_stored_state_with(
        loaded.compaction.as_ref().unwrap(),
        &loaded.messages,
    );
    let (reloaded_provider_messages, _) = restored_manager.materialize_lcm_context(&mut loaded)?;
    let reloaded_context = content_text(&reloaded_provider_messages[0]);
    assert!(!reloaded_context.contains(opaque_secret));
    assert!(!reloaded_context.contains(labeled_secret));
    assert!(reloaded_context.contains(ordinary_path));
    let (provider_messages, _) = manager.materialize_lcm_context(&mut session)?;
    assert!(provider_messages.len() <= RECENT_TURNS_TO_KEEP + 1);
    assert!(content_text(&provider_messages[0]).contains("Emergency compaction"));
    assert!(content_text(&provider_messages[0]).contains("## Previous Conversation Summary"));
    assert!(!content_text(&provider_messages[0]).contains(opaque_secret));
    assert!(!content_text(&provider_messages[0]).contains(labeled_secret));
    assert!(content_text(&provider_messages[0]).contains(ordinary_path));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn critical_lcm_recovery_chain_covers_route_failure_timeout_and_legacy() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = TestHomeGuard::set(home.path());
    std::fs::write(
        home.path().join("config.toml"),
        "[compaction]\nengine = \"lcm\"\nmodel = \"selected-model\"\n",
    )?;
    crate::config::invalidate_config_cache();

    let run = |id: &str,
               selected: RouteOutcome,
               active: RouteOutcome,
               legacy: Option<crate::session::StoredCompactionState>| {
        let mut session = make_lcm_session(id, 20);
        session.model = Some("active-model".to_string());
        session.compaction = legacy;
        let mut manager = CompactionManager::new().with_budget(1_000);
        manager.engine = crate::config::CompactionEngine::Lcm;
        for _ in 0..20 {
            manager.notify_message_added();
        }
        manager.update_observed_input_tokens(960);
        let provider: Arc<dyn Provider> = Arc::new(FallbackRouteProvider {
            current_route: Arc::new(std::sync::Mutex::new("active-model".to_string())),
            outcomes: Arc::new(std::collections::HashMap::from([
                ("selected-model".to_string(), selected),
                ("active-model".to_string(), active),
            ])),
        });
        let action = manager.ensure_lcm_context_fits(&mut session, provider);
        let CompactionAction::HardCompacted(_) = action else {
            panic!("critical fallback case {id} did not recover")
        };
        let event = manager
            .take_compaction_event()
            .unwrap_or_else(|| panic!("critical fallback case {id} emitted no event"));
        (session, event)
    };

    let (_, selected) = run(
        "critical_selected_success",
        RouteOutcome::Success,
        RouteOutcome::Failure,
        None,
    );
    assert_eq!(selected.effective_route.as_deref(), Some("selected-model"));
    assert_eq!(selected.fallback_reason, None);

    let (_, active) = run(
        "critical_active_success",
        RouteOutcome::Failure,
        RouteOutcome::Success,
        None,
    );
    assert_eq!(active.effective_route.as_deref(), Some("active-model"));
    assert_eq!(
        active.fallback_reason.as_deref(),
        Some("selected_route_failed>active_route_succeeded")
    );

    let (_, timed_out) = run(
        "critical_selected_timeout",
        RouteOutcome::Stall,
        RouteOutcome::Success,
        None,
    );
    assert_eq!(timed_out.effective_route.as_deref(), Some("active-model"));
    assert_eq!(
        timed_out.fallback_reason.as_deref(),
        Some("selected_route_failed>active_route_succeeded")
    );

    let legacy_state = crate::session::StoredCompactionState {
        summary_text: "Verified legacy textual prefix".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 10,
        original_turn_count: 10,
        compacted_count: 10,
    };
    let (legacy_session, legacy) = run(
        "critical_legacy_success",
        RouteOutcome::Failure,
        RouteOutcome::Failure,
        Some(legacy_state),
    );
    assert_eq!(legacy.effective_route.as_deref(), Some("legacy:rolling"));
    assert!(
        legacy_session.context_nodes[0]
            .summary_text
            .contains("Verified legacy textual prefix")
    );
    assert_eq!(
        legacy.fallback_reason.as_deref(),
        Some("selected_route_failed>active_route_failed_or_unavailable>legacy_summary_succeeded")
    );

    let encrypted_legacy = crate::session::StoredCompactionState {
        summary_text: "must not be trusted".to_string(),
        openai_encrypted_content: Some("opaque-provider-state".to_string()),
        covers_up_to_turn: 10,
        original_turn_count: 10,
        compacted_count: 10,
    };
    let (emergency_session, emergency) = run(
        "critical_encrypted_legacy_rejected",
        RouteOutcome::Failure,
        RouteOutcome::Failure,
        Some(encrypted_legacy),
    );
    assert_eq!(
        emergency.effective_route.as_deref(),
        Some("local:emergency")
    );
    assert!(
        !emergency_session.context_nodes[0]
            .summary_text
            .contains("must not be trusted")
    );
    assert_eq!(
        emergency.fallback_reason.as_deref(),
        Some(
            "selected_route_failed>active_route_failed_or_unavailable>legacy_summary_rejected_or_unavailable>local_emergency_succeeded"
        )
    );
    Ok(())
}
