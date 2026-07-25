use super::*;
use anyhow::{Result, anyhow};

fn test_runtime_identity() -> jcode_provider_core::ExactRuntimeIdentity {
    jcode_provider_core::ExactRuntimeIdentity {
        provider_key: "test-provider".to_string(),
        route: jcode_provider_core::RouteSelection {
            model: "test-model".to_string(),
            runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
            api_method: "test-api".to_string(),
            provider_label: "Test Provider".to_string(),
            detail: String::new(),
        },
        account_label: Some("test-account".to_string()),
        account_id: Some("stable-test-account".to_string()),
        account_generation: Some(1),
        reasoning_effort: None,
    }
}

fn context_transaction(session_id: &str, op_id: &str) -> ContextGraphTransaction {
    let node_id = format!("node-{op_id}");
    ContextGraphTransaction {
        schema_version: 1,
        op_id: op_id.to_string(),
        base_generation: 0,
        generation: 1,
        append_context_nodes: vec![StoredContextNode {
            id: node_id.clone(),
            schema_version: 2,
            level: 0,
            source_session_id: session_id.to_string(),
            source_message_ids: vec!["message-1".to_string()],
            source_sha256: "a".repeat(64),
            source_runtime_identity_sha256: exact_runtime_identity_sha256(&test_runtime_identity())
                .unwrap(),
            child_node_ids: Vec::new(),
            summary_text: format!("summary-{op_id}"),
            summary_sha256: None,
            estimated_tokens: 4,
            summarizer_model: "model".to_string(),
            summarizer_provider: "provider".to_string(),
            summarizer_route: "route".to_string(),
            summarizer_runtime_identity_sha256: exact_runtime_identity_sha256(
                &test_runtime_identity(),
            )
            .unwrap(),
            prompt_schema_version: 1,
            created_at: chrono::Utc::now(),
        }],
        frontier: StoredContextFrontier {
            schema_version: 1,
            generation: 1,
            active_node_ids: vec![node_id],
            covered_message_count: 1,
            covered_through_message_id: Some("message-1".to_string()),
            source_prefix_sha256: "b".repeat(64),
            next_node_sequence: 2,
        },
        input_proof: ContextGraphInputProof {
            schema_version: 1,
            source_session_id: session_id.to_string(),
            source_message_ids: vec!["message-1".to_string()],
            source_sha256: "a".repeat(64),
        },
    }
}

fn seed_context_source(session: &mut Session) {
    session.exact_runtime_identity = Some(test_runtime_identity());
    session.messages.push(StoredMessage {
        id: "message-1".to_string(),
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "canonical source".to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    });
}

fn canonical_context_transaction(session: &Session, op_id: &str) -> ContextGraphTransaction {
    use sha2::{Digest, Sha256};
    let mut transaction = context_transaction(&session.id, op_id);
    let source_start = session
        .context_frontier
        .as_ref()
        .map_or(0, |frontier| frontier.covered_message_count);
    let source = &session.messages[source_start..];
    let source_ids: Vec<String> = source.iter().map(|message| message.id.clone()).collect();
    let source_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(source).unwrap()));
    let prefix_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&session.messages).unwrap())
    );
    transaction.append_context_nodes[0].source_message_ids = source_ids.clone();
    transaction.append_context_nodes[0].source_sha256 = source_sha256.clone();
    transaction.append_context_nodes[0].summary_sha256 = Some(format!(
        "{:x}",
        Sha256::digest(transaction.append_context_nodes[0].summary_text.as_bytes())
    ));
    transaction.append_context_nodes[0].id =
        stored_context_node_id(&transaction.append_context_nodes[0]);
    transaction.frontier.covered_message_count = session.messages.len();
    transaction.frontier.covered_through_message_id =
        session.messages.last().map(|message| message.id.clone());
    transaction.frontier.source_prefix_sha256 = prefix_sha256;
    let new_node_id = transaction.append_context_nodes[0].id.clone();
    transaction.frontier.active_node_ids = session
        .context_frontier
        .as_ref()
        .map_or_else(Vec::new, |frontier| frontier.active_node_ids.clone());
    transaction.frontier.active_node_ids.push(new_node_id);
    transaction.input_proof.source_message_ids = source_ids;
    transaction.input_proof.source_sha256 = source_sha256;
    transaction
}

#[test]
fn transcript_mutations_preserve_or_invalidate_context_graph_proofs() -> Result<()> {
    let mut session = Session::create_with_id("context_graph_mutation".to_string(), None, None);
    seed_context_source(&mut session);
    session.messages[0].content.push(ContentBlock::ToolUse {
        id: "covered-tool".to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({"command": "true"}),
        thought_signature: None,
    });
    session.apply_context_graph_transaction(canonical_context_transaction(&session, "first"))?;
    session.compaction = Some(StoredCompactionState {
        summary_text: "derived projection".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    });

    let tail_id = session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tail-tool".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "true"}),
            thought_signature: None,
        }],
    );
    let tail_original = serde_json::to_vec(&session.messages).unwrap();
    session.suppress_tool_use_blocks_for_provider(&tail_id);
    assert_eq!(
        serde_json::to_vec(&session.messages).unwrap(),
        tail_original
    );
    assert!(session.context_frontier.is_some());
    assert!(session.compaction.is_some());
    assert!(matches!(
        session
            .messages_for_provider()
            .last()
            .unwrap()
            .content
            .as_slice(),
        []
    ));

    let covered_original = serde_json::to_vec(&session.messages).unwrap();
    session.suppress_tool_use_blocks_for_provider("message-1");
    assert_eq!(
        serde_json::to_vec(&session.messages).unwrap(),
        covered_original
    );
    assert_eq!(session.context_nodes.len(), 1);
    assert!(session.context_frontier.is_some());
    assert!(session.compaction.is_some());

    let mut truncated = Session::create_with_id("context_graph_truncate".to_string(), None, None);
    seed_context_source(&mut truncated);
    truncated
        .apply_context_graph_transaction(canonical_context_transaction(&truncated, "second"))?;
    truncated.truncate_messages(0);
    assert_eq!(truncated.context_nodes.len(), 1);
    assert!(
        truncated
            .context_frontier
            .as_ref()
            .unwrap()
            .active_node_ids
            .is_empty()
    );
    Ok(())
}

#[test]
fn context_graph_snapshot_and_journal_store_nodes_once() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_once";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.apply_context_graph_transaction(canonical_context_transaction(&session, "first"))?;
    session.save()?;

    let snapshot = std::fs::read_to_string(session_path(id)?)?;
    let node_id = &session.context_nodes[0].id;
    assert!(node_id.starts_with("lcm-"));
    assert_eq!(snapshot.matches("summary-first").count(), 1);
    assert_eq!(snapshot.matches(node_id).count(), 2); // node id plus frontier reference

    session.messages.push(StoredMessage {
        id: "message-2".to_string(),
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "second canonical source".to_string(),
            cache_control: None,
        }],
        display_role: None,
        timestamp: None,
        tool_duration_ms: None,
        token_usage: None,
    });
    let mut second = canonical_context_transaction(&session, "second");
    second.base_generation = 1;
    second.generation = 2;
    second.frontier.generation = 2;
    session.apply_context_graph_transaction(second)?;
    session.save()?;
    let journal = std::fs::read_to_string(session_journal_path(id)?)?;
    assert_eq!(journal.matches("summary-second").count(), 1);
    assert!(!journal.contains("summary-first"));

    let loaded = Session::load(id)?;
    assert_eq!(loaded.context_nodes.len(), 2);
    assert_eq!(loaded.context_frontier.unwrap().generation, 2);
    Ok(())
}

#[test]
fn production_graph_commit_is_journal_native_and_replayable() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_production_journal";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.save()?;
    let transaction = canonical_context_transaction(&session, "production-journal");
    let node_id = transaction.append_context_nodes[0].id.clone();
    let projection = StoredCompactionState {
        summary_text: "[LCM context node 1 level 0]\nsummary-production-journal".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };

    assert!(
        session.commit_context_graph_transaction_with_compaction(transaction, Some(projection))?
    );

    let snapshot = std::fs::read_to_string(session_path(id)?)?;
    assert!(!snapshot.contains("summary-production-journal"));
    let journal = std::fs::read_to_string(session_journal_path(id)?)?;
    assert_eq!(journal.matches("summary-production-journal").count(), 2);
    assert_eq!(journal.matches(&node_id).count(), 2);
    let loaded = Session::load(id)?;
    assert_eq!(loaded.context_nodes.len(), 1);
    assert_eq!(
        loaded
            .context_frontier
            .as_ref()
            .map(|frontier| frontier.generation),
        Some(1)
    );
    assert!(loaded.has_owned_native_lcm_projection());
    Ok(())
}

#[test]
fn opaque_identity_cannot_publish_context_graph() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut session = Session::create_with_id("opaque_graph".to_string(), None, None);
    seed_context_source(&mut session);
    session.exact_runtime_identity = None;
    let transaction = canonical_context_transaction(&session, "opaque");

    let error = session
        .commit_context_graph_transaction(transaction)
        .expect_err("opaque identity must fail closed");
    assert!(
        error
            .to_string()
            .contains("exact account identity is opaque")
    );
    assert!(session.context_nodes.is_empty());
    Ok(())
}

#[test]
fn production_graph_commit_append_failure_does_not_publish_or_checkpoint() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_production_append_failure";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.save()?;
    let transaction = canonical_context_transaction(&session, "append-failure");
    let projection = StoredCompactionState {
        summary_text: "[LCM context node 1 level 0]\nsummary-append-failure".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };
    let _failure = crate::storage::inject_write_failure(Some(session_journal_path(id)?));

    assert!(
        session
            .commit_context_graph_transaction_with_compaction(transaction, Some(projection))
            .is_err()
    );
    assert!(session.context_nodes.is_empty());
    assert!(session.context_frontier.is_none());
    assert!(session.compaction.is_none());
    let snapshot = std::fs::read_to_string(session_path(id)?)?;
    assert!(!snapshot.contains("summary-append-failure"));
    let loaded = Session::load(id)?;
    assert!(loaded.context_nodes.is_empty());
    assert!(loaded.context_frontier.is_none());
    assert!(loaded.compaction.is_none());
    Ok(())
}

#[test]
fn production_graph_commit_reconciles_post_append_durability_error() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = crate::id::new_id("context_graph_production_post_append_failure");
    let mut session = Session::create_with_id(id.clone(), None, None);
    seed_context_source(&mut session);
    session.save()?;
    let transaction = canonical_context_transaction(&session, "post-append-failure");
    let projection = StoredCompactionState {
        summary_text: "[LCM context node 1 level 0]\nsummary-post-append-failure".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };
    let _failure = crate::storage::inject_post_append_failure(session_journal_path(&id)?);

    assert!(
        session.commit_context_graph_transaction_with_compaction(transaction, Some(projection))?
    );
    assert_eq!(session.context_nodes.len(), 1);
    assert!(session.compaction.is_some());
    drop(_failure);

    let loaded = Session::load(&id)?;
    assert_eq!(loaded.context_nodes, session.context_nodes);
    assert_eq!(loaded.context_frontier, session.context_frontier);
    assert_eq!(loaded.compaction, session.compaction);
    Ok(())
}

#[test]
fn invalid_replayed_graph_transaction_rejects_its_projection_locally_and_remotely() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_invalid_paired_projection";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.save()?;
    let transaction = canonical_context_transaction(&session, "invalid-paired");
    let projection = StoredCompactionState {
        summary_text: "[LCM context node 1 level 0]\nsummary-invalid-paired".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };
    assert!(
        session.commit_context_graph_transaction_with_compaction(transaction, Some(projection))?
    );

    let journal_path = session_journal_path(id)?;
    let mut entry: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&journal_path)?.trim_end())?;
    entry["context_transaction"]["generation"] = serde_json::json!(2);
    std::fs::write(
        &journal_path,
        format!("{}\n", serde_json::to_string(&entry)?),
    )?;

    let local = Session::load(id)?;
    assert_eq!(local.messages.len(), 1);
    assert!(local.context_nodes.is_empty());
    assert!(local.context_frontier.is_none());
    assert!(local.compaction.is_none());

    let remote = Session::load_for_remote_startup(id)?;
    assert_eq!(remote.messages.len(), 1);
    assert!(remote.context_nodes.is_empty());
    assert!(remote.context_frontier.is_none());
    assert!(remote.compaction.is_none());
    Ok(())
}

#[test]
fn tampered_projection_text_is_deactivated_without_deleting_committed_nodes() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_tampered_projection";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.save()?;
    let transaction = canonical_context_transaction(&session, "tampered-projection");
    let projection = StoredCompactionState {
        summary_text: "[LCM context node 1 level 0]\nsummary-tampered-projection".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };
    assert!(
        session.commit_context_graph_transaction_with_compaction(transaction, Some(projection))?
    );

    let journal_path = session_journal_path(id)?;
    let mut entry: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&journal_path)?.trim_end())?;
    entry["meta"]["compaction"]["summary_text"] = serde_json::json!("fabricated projection text");
    std::fs::write(
        &journal_path,
        format!("{}\n", serde_json::to_string(&entry)?),
    )?;

    let loaded = Session::load(id)?;
    assert!(loaded.compaction.is_none());
    assert_eq!(loaded.context_nodes.len(), 1);
    assert_eq!(
        loaded.context_nodes[0].summary_text,
        "summary-tampered-projection"
    );
    assert!(
        loaded
            .context_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.active_node_ids.is_empty())
    );
    Ok(())
}

#[test]
fn tampered_inactive_context_node_is_discarded_on_reload() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_tampered_inactive_node";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.commit_context_graph_transaction(canonical_context_transaction(
        &session,
        "tampered-inactive-node",
    ))?;
    session.deactivate_context_graph_state();
    assert!(session.context_frontier.as_ref().is_some_and(|frontier| {
        frontier.active_node_ids.is_empty() && !session.context_nodes.is_empty()
    }));

    // Write an otherwise valid inactive graph whose summarizer provenance no
    // longer matches its immutable ID. With no active root, source validation
    // cannot be relied on to notice this mutation.
    session.context_nodes[0].summarizer_model = "tampered-model".to_string();
    std::fs::write(session_path(id)?, serde_json::to_vec(&session)?)?;
    let journal_path = session_journal_path(id)?;
    if journal_path.exists() {
        std::fs::remove_file(journal_path)?;
    }

    let loaded = Session::load(id)?;
    assert!(loaded.context_nodes.is_empty());
    assert!(loaded.context_frontier.is_none());
    Ok(())
}

#[test]
fn identity_transition_preserves_inactive_forensic_nodes_across_reload() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_identity_transition_forensics";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.commit_context_graph_transaction(canonical_context_transaction(
        &session,
        "identity-transition-forensics",
    ))?;
    let forensic_nodes = session.context_nodes.clone();
    let canonical_history = serde_json::to_vec(&session.messages)?;
    let mut next_identity = test_runtime_identity();
    next_identity.account_id = Some("next-stable-account".to_string());
    next_identity.account_generation = Some(2);

    session.exact_runtime_identity = Some(next_identity);
    session.reset_context_graph_for_identity_transition();
    assert_eq!(session.context_nodes, forensic_nodes);
    assert!(session.context_frontier.as_ref().is_some_and(|frontier| {
        frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
    }));
    assert!(session.compaction.is_none());
    assert!(!session.has_owned_native_lcm_projection());
    assert_eq!(serde_json::to_vec(&session.messages)?, canonical_history);
    session.save()?;

    let loaded = Session::load(id)?;
    assert_eq!(loaded.context_nodes, forensic_nodes);
    assert!(loaded.context_frontier.as_ref().is_some_and(|frontier| {
        frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
    }));
    assert!(loaded.compaction.is_none());
    assert!(!loaded.has_owned_native_lcm_projection());
    assert_eq!(serde_json::to_vec(&loaded.messages)?, canonical_history);
    Ok(())
}

#[test]
fn context_graph_replay_is_op_id_idempotent() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_idempotent";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.save()?;
    session.apply_context_graph_transaction(canonical_context_transaction(&session, "once"))?;
    session.save()?;

    let path = session_journal_path(id)?;
    let line = std::fs::read_to_string(&path)?;
    std::fs::write(&path, format!("{line}{line}"))?;
    let loaded = Session::load(id)?;
    assert_eq!(loaded.context_nodes.len(), 1);
    assert_eq!(loaded.context_frontier.unwrap().generation, 1);
    Ok(())
}

#[test]
fn post_replay_reconciliation_discards_inconsistent_projection_locally_and_remotely() -> Result<()>
{
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_projection_replay_reconciliation";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    let transaction = canonical_context_transaction(&session, "projection-replay");
    let projection = StoredCompactionState {
        summary_text: "[LCM context node 1 level 0]\nsummary-projection-replay".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };
    assert!(
        session.commit_context_graph_transaction_with_compaction(transaction, Some(projection))?
    );
    assert!(session.has_owned_native_lcm_projection());

    let projection = session.compaction.as_mut().expect("committed projection");
    projection.covers_up_to_turn = 0;
    projection.compacted_count = 0;
    session.save()?;
    let journal = std::fs::read_to_string(session_journal_path(id)?)?;
    assert!(
        journal.contains(r#""compacted_count":0"#),
        "fixture must exercise post-snapshot journal replay"
    );

    let local = Session::load(id)?;
    assert!(local.compaction.is_none());
    assert_eq!(local.context_nodes.len(), 1);
    assert!(
        local
            .context_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.active_node_ids.is_empty())
    );

    let remote = Session::load_for_remote_startup(id)?;
    assert!(remote.compaction.is_none());
    assert_eq!(remote.context_nodes.len(), 1);
    assert!(
        remote
            .context_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.active_node_ids.is_empty())
    );
    Ok(())
}

#[test]
fn context_graph_rejects_conflicting_op_id_reuse() -> Result<()> {
    let id = "context_graph_conflicting_op";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    let original = canonical_context_transaction(&session, "same-op");
    assert!(session.apply_context_graph_transaction(original.clone())?);
    assert!(!session.apply_context_graph_transaction(original)?);

    let mut conflicting = canonical_context_transaction(&session, "same-op");
    conflicting.append_context_nodes[0].summary_text = "different".into();
    let err = session
        .apply_context_graph_transaction(conflicting)
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("reused with a different transaction")
    );
    assert_eq!(session.context_nodes.len(), 1);
    assert_eq!(session.context_nodes[0].summary_text, "summary-same-op");
    Ok(())
}

#[test]
fn context_graph_commit_is_idempotent_after_reload() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_retry_after_reload";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    let transaction = canonical_context_transaction(&session, "durable-retry");
    assert!(session.commit_context_graph_transaction(transaction.clone())?);

    let mut loaded = Session::load(id)?;
    assert!(!loaded.commit_context_graph_transaction(transaction)?);
    assert_eq!(loaded.context_nodes.len(), 1);
    assert_eq!(loaded.context_frontier.unwrap().generation, 1);
    Ok(())
}

#[test]
fn context_graph_commit_failure_does_not_publish_candidate() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_failed_commit";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    let transaction = canonical_context_transaction(&session, "must-not-publish");

    // Make the expected sessions directory a regular file so the durable
    // snapshot write fails before publication.
    std::fs::write(home.path().join("sessions"), b"not a directory")?;
    assert!(
        session
            .commit_context_graph_transaction(transaction)
            .is_err()
    );
    assert!(session.context_nodes.is_empty());
    assert!(session.context_frontier.is_none());
    assert!(session.last_context_op_id.is_none());
    assert!(session.last_context_op_sha256.is_none());
    Ok(())
}

#[test]
fn context_graph_inheritance_preserves_origin_identity_and_remains_reloadable() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut parent = Session::create_with_id("context_graph_parent".to_string(), None, None);
    seed_context_source(&mut parent);
    parent.commit_context_graph_transaction(canonical_context_transaction(&parent, "parent"))?;
    let parent_node_id = parent.context_nodes[0].id.clone();

    let mut child = Session::create_with_id(
        "context_graph_child".to_string(),
        Some(parent.id.clone()),
        None,
    );
    child.exact_runtime_identity = parent.exact_runtime_identity.clone();
    child.replace_messages(parent.messages.clone());
    child.inherit_context_graph_from(&parent)?;
    child.save()?;

    assert_eq!(child.context_nodes[0].id, parent_node_id);
    assert_eq!(child.context_nodes[0].source_session_id, parent.id);
    assert_eq!(
        child
            .context_frontier
            .as_ref()
            .unwrap()
            .source_prefix_sha256,
        parent
            .context_frontier
            .as_ref()
            .unwrap()
            .source_prefix_sha256
    );
    let loaded = Session::load(&child.id)?;
    assert_eq!(loaded.context_nodes, child.context_nodes);
    assert!(loaded.context_frontier.is_some());
    Ok(())
}

#[test]
fn context_graph_inheritance_rejects_exact_account_mismatch() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut parent = Session::create_with_id("context_identity_parent".to_string(), None, None);
    seed_context_source(&mut parent);
    parent.commit_context_graph_transaction(canonical_context_transaction(&parent, "parent"))?;

    let mut child = Session::create_with_id(
        "context_identity_child".to_string(),
        Some(parent.id.clone()),
        None,
    );
    child.replace_messages(parent.messages.clone());
    let mut mismatched = parent.exact_runtime_identity.clone().unwrap();
    mismatched.account_id = Some("different-account".to_string());
    child.exact_runtime_identity = Some(mismatched);

    let error = child
        .inherit_context_graph_from(&parent)
        .expect_err("cross-account graph inheritance must fail closed");
    assert!(
        error
            .to_string()
            .contains("exact runtime/account identity match")
    );
    assert!(child.context_nodes.is_empty());
    assert!(child.context_frontier.is_none());
    Ok(())
}

#[test]
fn imported_context_root_is_self_contained_durable_and_parent_proven() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut parent = Session::create_with_id("import_parent".to_string(), None, None);
    seed_context_source(&mut parent);
    parent.save()?;
    let mut child =
        Session::create_with_id("import_child".to_string(), Some(parent.id.clone()), None);
    child.exact_runtime_identity = parent.exact_runtime_identity.clone();
    let state = StoredCompactionState {
        summary_text: "portable imported parent context".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 0,
    };
    let mut summarizer_identity = parent.exact_runtime_identity.clone().unwrap();
    summarizer_identity.provider_key = "transfer-compactor".to_string();
    summarizer_identity.route.model = "transfer-summary-model".to_string();
    let mut mismatched_child = child.clone();
    mismatched_child
        .exact_runtime_identity
        .as_mut()
        .unwrap()
        .account_generation = Some(2);
    let mismatch_error = mismatched_child
        .install_imported_context_root(&parent, state.clone(), &summarizer_identity)
        .expect_err("imported roots must reject any exact identity mismatch");
    assert!(
        mismatch_error
            .to_string()
            .contains("exact child and parent")
    );
    assert!(mismatched_child.context_nodes.is_empty());
    child.install_imported_context_root(&parent, state.clone(), &summarizer_identity)?;
    let exported = child.redacted_for_export();
    assert!(exported.context_nodes.is_empty());
    assert!(exported.context_frontier.is_none());
    child.save()?;

    assert!(child.messages.is_empty());
    let child_projection = child.compaction.as_ref().expect("imported projection");
    assert_eq!(child_projection.compacted_count, 0);
    assert_eq!(child_projection.covers_up_to_turn, 0);
    assert!(
        child_projection
            .summary_text
            .contains("portable imported parent context")
    );
    assert!(child_projection.summary_text.contains("Retrieval anchor"));
    assert_eq!(child.context_nodes[0].source_session_id, parent.id);
    let expected_identity_sha256 =
        exact_runtime_identity_sha256(parent.exact_runtime_identity.as_ref().unwrap())?;
    assert_eq!(
        child.context_nodes[0].source_runtime_identity_sha256,
        expected_identity_sha256
    );
    assert_eq!(
        child.context_nodes[0].summarizer_runtime_identity_sha256,
        exact_runtime_identity_sha256(&summarizer_identity)?
    );
    assert_eq!(
        child.context_nodes[0].summarizer_provider,
        summarizer_identity.provider_key
    );
    assert_eq!(
        child.context_nodes[0].summarizer_model,
        summarizer_identity.route.model
    );
    assert_eq!(
        child.context_nodes[0].summarizer_route,
        serde_json::to_string(&summarizer_identity.route)?
    );
    assert_eq!(
        child
            .context_frontier
            .as_ref()
            .unwrap()
            .covered_message_count,
        0
    );
    let loaded = Session::load(&child.id)?;
    assert_eq!(loaded.context_nodes, child.context_nodes);
    assert_eq!(loaded.context_frontier, child.context_frontier);
    assert_eq!(loaded.compaction, child.compaction);
    assert_eq!(loaded.parent_id, Some(parent.id));

    let mut split = Session::create_with_id(
        "import_split_child".to_string(),
        Some(child.id.clone()),
        None,
    );
    split.exact_runtime_identity = child.exact_runtime_identity.clone();
    split.replace_messages(child.messages.clone());
    split.compaction = child.compaction.clone();
    split.inherit_context_graph_from(&child)?;
    split.save()?;
    let reloaded_split = Session::load(&split.id)?;
    assert_eq!(reloaded_split.context_nodes, child.context_nodes);
    assert_eq!(reloaded_split.context_frontier, child.context_frontier);

    let path = session_path(&child.id)?;
    let mut snapshot: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    snapshot["context_nodes"][0]["summarizer_provider"] =
        serde_json::Value::String("tampered-transfer-provider".to_string());
    std::fs::write(&path, serde_json::to_vec_pretty(&snapshot)?)?;
    let repaired = Session::load(&child.id)?;
    assert!(repaired.context_nodes.is_empty());
    assert!(repaired.context_frontier.is_none());
    assert!(repaired.compaction.is_none());
    Ok(())
}

fn saved_imported_context_parent_child(
    home: &tempfile::TempDir,
    suffix: &str,
) -> Result<(Session, Session)> {
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut parent = Session::create_with_id(format!("import_parent_{suffix}"), None, None);
    seed_context_source(&mut parent);
    parent.save()?;
    let mut child = Session::create_with_id(
        format!("import_child_{suffix}"),
        Some(parent.id.clone()),
        None,
    );
    child.exact_runtime_identity = parent.exact_runtime_identity.clone();
    let state = StoredCompactionState {
        summary_text: "portable imported parent context".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 0,
    };
    let summarizer_identity = parent.exact_runtime_identity.clone().unwrap();
    child.install_imported_context_root(&parent, state, &summarizer_identity)?;
    child.save()?;
    // Force the imported graph into the canonical snapshot, rather than leaving
    // it only in the journal, so provenance deactivation must dirty a baseline
    // that already contains the active imported root.
    child.title = Some(format!("checkpointed imported root {suffix}"));
    child.save()?;
    Ok((parent, child))
}

#[test]
fn imported_context_root_reload_deactivates_when_ancestor_tampered() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let (parent, child) = saved_imported_context_parent_child(&home, "tamper")?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let parent_path = session_path(&parent.id)?;
    let mut snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&parent_path)?)?;
    snapshot["messages"][0]["id"] = serde_json::Value::String("tampered-message-id".to_string());
    std::fs::write(&parent_path, serde_json::to_vec_pretty(&snapshot)?)?;

    let mut loaded = Session::load(&child.id)?;
    assert_eq!(loaded.context_nodes, child.context_nodes);
    assert!(
        loaded
            .context_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.active_node_ids.is_empty())
    );
    assert!(loaded.compaction.is_none());
    loaded.save()?;
    let saved_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(session_path(&child.id)?)?)?;
    assert!(
        saved_snapshot["context_frontier"]["active_node_ids"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(saved_snapshot["compaction"].is_null());
    let reloaded = Session::load(&child.id)?;
    assert_eq!(reloaded.context_nodes, child.context_nodes);
    assert!(
        reloaded
            .context_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.active_node_ids.is_empty())
    );
    Ok(())
}

#[test]
fn imported_context_root_reload_deactivates_when_ancestor_unavailable() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let (parent, child) = saved_imported_context_parent_child(&home, "missing")?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let parent_path = session_path(&parent.id)?;
    for path in [
        parent_path.clone(),
        parent_path.with_extension("bak"),
        session_journal_path(&parent.id)?,
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    let mut loaded = Session::load_for_remote_startup(&child.id)?;
    assert_eq!(loaded.context_nodes, child.context_nodes);
    assert!(
        loaded
            .context_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.active_node_ids.is_empty())
    );
    assert!(loaded.compaction.is_none());
    loaded.save()?;
    let saved_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(session_path(&child.id)?)?)?;
    assert!(
        saved_snapshot["context_frontier"]["active_node_ids"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(saved_snapshot["compaction"].is_null());
    let reloaded = Session::load(&child.id)?;
    assert_eq!(reloaded.context_nodes, child.context_nodes);
    assert!(
        reloaded
            .context_frontier
            .as_ref()
            .is_some_and(|frontier| frontier.active_node_ids.is_empty())
    );
    Ok(())
}

#[test]
fn imported_context_root_reload_keeps_valid_parent_transfer_active() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let (_parent, child) = saved_imported_context_parent_child(&home, "valid")?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());

    let loaded = Session::load(&child.id)?;
    assert_eq!(loaded.context_nodes, child.context_nodes);
    assert_eq!(loaded.context_frontier, child.context_frontier);
    assert_eq!(loaded.compaction, child.compaction);
    assert!(!loaded.context_frontier.unwrap().active_node_ids.is_empty());
    Ok(())
}

#[test]
fn context_graph_rejects_generation_mismatch() {
    let id = "context_graph_generation";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    let mut transaction = canonical_context_transaction(&session, "bad-generation");
    transaction.base_generation = 1;
    assert!(
        session
            .apply_context_graph_transaction(transaction)
            .unwrap_err()
            .to_string()
            .contains("generation mismatch")
    );
    assert!(session.context_nodes.is_empty());
    assert!(session.context_frontier.is_none());
}

#[test]
fn context_graph_commit_rejects_stale_concurrent_writer() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut original = Session::create_with_id("context_graph_cas".to_string(), None, None);
    seed_context_source(&mut original);
    original.save()?;

    let mut first = Session::load(&original.id)?;
    let mut stale = Session::load(&original.id)?;
    let mut locally_advanced = Session::load(&original.id)?;
    let first_transaction = canonical_context_transaction(&first, "first-writer");
    let stale_transaction = canonical_context_transaction(&stale, "stale-writer");
    let local_transaction = canonical_context_transaction(&locally_advanced, "local-writer");
    assert!(locally_advanced.apply_context_graph_transaction(local_transaction)?);
    assert!(first.commit_context_graph_transaction(first_transaction)?);
    let error = stale
        .commit_context_graph_transaction(stale_transaction)
        .expect_err("stale graph writer must not overwrite the committed generation");
    assert!(error.to_string().contains("stale session writer rejected"));
    let error = locally_advanced
        .save()
        .expect_err("locally advanced stale writer must not overwrite durable graph state");
    assert!(error.to_string().contains("stale session writer rejected"));

    let reloaded = Session::load(&original.id)?;
    assert_eq!(
        reloaded
            .context_frontier
            .as_ref()
            .map(|frontier| frontier.generation),
        Some(1)
    );
    assert_eq!(reloaded.context_nodes.len(), 1);
    assert!(
        reloaded.context_nodes[0]
            .summary_text
            .contains("first-writer")
    );
    Ok(())
}

#[test]
fn full_checkpoint_rejects_same_sequence_stale_writer() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut original = Session::create_with_id("full_checkpoint_cas".to_string(), None, None);
    original.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".into(),
            cache_control: None,
        }],
    );
    original.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".into(),
            cache_control: None,
        }],
    );
    original.save()?;

    let mut first = Session::load(&original.id)?;
    let mut stale = Session::load(&original.id)?;
    let shared_sequence = first.journal_sequence;
    let shared_revision = first.persistence_revision;
    first.truncate_messages(1);
    first.save()?;
    assert_eq!(first.journal_sequence, shared_sequence);
    assert_eq!(stale.journal_sequence, shared_sequence);
    assert!(first.persistence_revision > shared_revision);
    assert_eq!(stale.persistence_revision, shared_revision);

    stale.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "stale append".into(),
            cache_control: None,
        }],
    );
    let error = stale
        .save()
        .expect_err("stale clone must not overwrite a newer full checkpoint");
    assert!(error.to_string().contains("stale session writer rejected"));

    let reloaded = Session::load(&original.id)?;
    assert_eq!(reloaded.messages.len(), 1);
    assert!(matches!(
        reloaded.messages[0].content.as_slice(),
        [ContentBlock::Text { text, .. }] if text == "first"
    ));
    Ok(())
}

#[test]
fn durable_journal_append_survives_deferred_checkpoint_failure() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "journal_append_checkpoint_failure";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    session.save()?;
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "large ordinary journal payload ".repeat(180_000),
            cache_control: None,
        }],
    );
    let snapshot_path = session_path(id)?;
    let _failure = crate::storage::inject_write_failure(Some(snapshot_path));

    session.save()?;

    assert_eq!(session.journal_sequence, 1);
    assert_eq!(Session::load(id)?.messages.len(), 1);
    Ok(())
}

#[test]
fn torn_context_transaction_replay_keeps_old_frontier() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_torn";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    seed_context_source(&mut session);
    session.save()?;
    session.apply_context_graph_transaction(canonical_context_transaction(&session, "torn"))?;
    session.save()?;

    let path = session_journal_path(id)?;
    let journal = std::fs::read(&path)?;
    std::fs::write(&path, &journal[..journal.len() / 2])?;
    let loaded = Session::load(id)?;
    assert!(loaded.context_nodes.is_empty());
    assert!(loaded.context_frontier.is_none());
    Ok(())
}

#[test]
fn legacy_snapshot_loads_with_empty_context_graph() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_legacy";
    let session = Session::create_with_id(id.to_string(), None, None);
    let mut value = serde_json::to_value(&session)?;
    let object = value
        .as_object_mut()
        .expect("session serializes as an object");
    object.remove("context_nodes");
    object.remove("context_frontier");
    std::fs::create_dir_all(session_path(id)?.parent().unwrap())?;
    std::fs::write(session_path(id)?, serde_json::to_vec(&value)?)?;

    let loaded = Session::load(id)?;
    assert!(loaded.context_nodes.is_empty());
    assert!(loaded.context_frontier.is_none());
    Ok(())
}

