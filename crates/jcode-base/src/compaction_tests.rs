use super::*;
use crate::provider::{EventStream, Provider};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct TestHomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl TestHomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", path.as_os_str());
        std::fs::write(path.join("config.toml"), "[compaction]\nengine = \"lcm\"\n")
            .expect("write LCM test config");
        crate::config::invalidate_config_cache();
        let _ = crate::config::config();
        Self { previous }
    }
}

impl Drop for TestHomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            crate::env::set_var("JCODE_HOME", previous);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        crate::config::invalidate_config_cache();
    }
}

struct MockSummaryProvider;

#[derive(Clone)]
struct AdaptiveLimitProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    max_prompt_chars: usize,
}

#[derive(Clone)]
struct FactPreservingProvider;

#[derive(Clone)]
struct StaticSummaryProvider {
    summary: String,
}

#[derive(Clone)]
struct StallingProvider {
    started: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone)]
struct RouteCapturingProvider {
    selections: Arc<std::sync::Mutex<Vec<crate::provider::RouteSelection>>>,
}

#[derive(Clone)]
enum RouteOutcome {
    Success,
    Failure,
    Stall,
}

#[derive(Clone)]
struct FallbackRouteProvider {
    current_route: Arc<std::sync::Mutex<String>>,
    outcomes: Arc<std::collections::HashMap<String, RouteOutcome>>,
}

#[derive(Clone)]
struct PromptCapturingProvider {
    systems: Arc<std::sync::Mutex<Vec<String>>>,
    prompts: Arc<std::sync::Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct ScheduledProvider {
    active: Arc<std::sync::atomic::AtomicUsize>,
    maximum: Arc<std::sync::atomic::AtomicUsize>,
}

fn structured_test_summary(decisions: &str) -> String {
    let decisions = if decisions.trim().is_empty() {
        "None observed.".to_string()
    } else {
        decisions
            .lines()
            .map(|line| format!("- [source] {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "# Objective and user intent\nNone observed.\n\n# Explicit constraints and prohibited actions\nNone observed.\n\n# Decisions and rationale\n{decisions}\n\n# Repository state and exact paths/symbols/branches/commits\nNone observed.\n\n# Changes actually completed\nNone observed.\n\n# Commands and tests with actual outcomes\nNone observed.\n\n# Failures, diagnosis, and unresolved blockers\nNone observed.\n\n# Open questions and next steps\nNone observed.\n\n# Retrieval anchors and source range\nNone observed."
    )
}

#[async_trait::async_trait]
impl Provider for MockSummaryProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "mock-summary"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockSummaryProvider)
    }

    async fn complete_simple(&self, prompt: &str, _system: &str) -> Result<String> {
        let _ = prompt;
        Ok(structured_test_summary(""))
    }
}

#[async_trait::async_trait]
impl Provider for StaticSummaryProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "static-summary"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        Ok(self.summary.clone())
    }
}

#[async_trait::async_trait]
impl Provider for StallingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "stalling-summary"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::future::pending::<Result<String>>().await
    }
}

#[async_trait::async_trait]
impl Provider for RouteCapturingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "route-capturing"
    }

    fn model(&self) -> String {
        "fallback-model".to_string()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    fn set_route_selection(&self, selection: &crate::provider::RouteSelection) -> Result<()> {
        self.selections.lock().unwrap().push(selection.clone());
        Ok(())
    }
}

#[async_trait::async_trait]
impl Provider for FallbackRouteProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "fallback-route"
    }

    fn model(&self) -> String {
        self.current_route.lock().unwrap().clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    fn set_model(&self, model: &str) -> Result<()> {
        *self.current_route.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn set_route_selection(&self, selection: &crate::provider::RouteSelection) -> Result<()> {
        *self.current_route.lock().unwrap() = selection.routed_model_spec();
        Ok(())
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        let route = self.current_route.lock().unwrap().clone();
        match self
            .outcomes
            .get(&route)
            .cloned()
            .unwrap_or(RouteOutcome::Failure)
        {
            RouteOutcome::Success => Ok(structured_test_summary("")),
            RouteOutcome::Failure => anyhow::bail!("route {route} failed"),
            RouteOutcome::Stall => std::future::pending::<Result<String>>().await,
        }
    }
}

#[async_trait::async_trait]
impl Provider for PromptCapturingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "prompt-capturing"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn complete_simple(&self, prompt: &str, system: &str) -> Result<String> {
        self.prompts.lock().unwrap().push(prompt.to_string());
        self.systems.lock().unwrap().push(system.to_string());
        Ok(structured_test_summary(""))
    }
}

#[async_trait::async_trait]
impl Provider for AdaptiveLimitProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "adaptive-limit"
    }

    fn model(&self) -> String {
        "small-context".to_string()
    }

    fn context_window(&self) -> usize {
        4_096
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn complete_simple(&self, prompt: &str, _system: &str) -> Result<String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if prompt.len() > self.max_prompt_chars {
            anyhow::bail!("context length limit exceeded")
        }
        Ok(structured_test_summary(""))
    }
}

#[async_trait::async_trait]
impl Provider for FactPreservingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "fact-preserving-benchmark"
    }

    fn model(&self) -> String {
        "deterministic-oracle-v1".to_string()
    }

    fn context_window(&self) -> usize {
        8_192
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn complete_simple(&self, prompt: &str, _system: &str) -> Result<String> {
        let mut shortest_by_fact = std::collections::HashMap::<String, String>::new();
        let evidence = prompt
            .split_once("\nQUERIES:\n")
            .map_or(prompt, |(context, _)| context);
        for line in evidence.lines().map(str::trim) {
            for fact in line
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .filter(|word| word.starts_with("PLANTED_") || word.starts_with("CANARY_"))
            {
                let entry = shortest_by_fact
                    .entry(fact.to_string())
                    .or_insert_with(|| line.to_string());
                if line.len() < entry.len() {
                    *entry = line.to_string();
                }
            }
        }
        let mut facts = shortest_by_fact.into_values().collect::<Vec<_>>();
        facts.sort();
        Ok(structured_test_summary(&facts.join("\n")))
    }
}

#[async_trait::async_trait]
impl Provider for ScheduledProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "scheduler-test"
    }

    fn model(&self) -> String {
        "bounded".to_string()
    }

    fn context_window(&self) -> usize {
        8_192
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        let active = self
            .active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.maximum
            .fetch_max(active, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        Ok(structured_test_summary(""))
    }
}

fn make_text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

fn content_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn make_lcm_session(id: &str, turns: usize) -> crate::session::Session {
    let mut session = crate::session::Session::create_with_id(id.to_string(), None, None);
    for index in 0..turns {
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("turn {index} {}", "x".repeat(120)),
                cache_control: None,
            }],
        );
    }
    session
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
            .unwrap()
            .summary_text
            .contains("durable LCM leaf")
    );
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
        source_prefix_sha256: frontier.source_prefix_sha256,
        covered_through_message_id: frontier.covered_through_message_id.unwrap(),
        prior_active_nodes: Vec::new(),
        node_level: 1,
        child_nodes: immutable_children.clone(),
        summarizer_model: "model".into(),
        summarizer_provider: "provider".into(),
        summarizer_route: "route".into(),
        configured_model: None,
        cutoff: 0,
        source_fingerprint: None,
        pre_tokens: 850,
        trigger: "hierarchy".into(),
        route_policy_fingerprint: lcm_route_policy_fingerprint(&session, None),
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

    session.retain_context_graph_prefix(20)?;
    session.truncate_messages(20);
    session.save()?;
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
    assert_eq!(
        session
            .context_frontier
            .as_ref()
            .unwrap()
            .covered_message_count,
        20
    );
    let rewound = crate::session::Session::load("lcm_hierarchy")?;
    assert_eq!(rewound.context_nodes.len(), 2);
    assert_eq!(rewound.compaction.unwrap().compacted_count, 20);
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

#[test]
fn lcm_engine_switch_cancels_candidates_and_rolling_api_fails_closed() -> Result<()> {
    let mut session = make_lcm_session("lcm_engine_switch", 20);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..20 {
        manager.notify_message_added();
    }
    assert!(
        manager
            .force_compact_with(
                &session.messages_for_provider_uncached(),
                Arc::new(MockSummaryProvider),
            )
            .is_err(),
        "rolling API must fail closed while LCM owns context"
    );
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
            summary_text: "must be cancelled".into(),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 10,
            duration_ms: 1,
            summarized_messages: 10,
        },
    )?);

    assert!(manager.synchronize_engine(crate::config::CompactionEngine::Rolling));
    assert!(!manager.is_compacting());
    let (materialized, event) = manager.materialize_lcm_context(&mut session)?;
    assert_eq!(materialized.len(), session.messages.len());
    assert!(event.is_none());
    assert!(session.context_nodes.is_empty());
    Ok(())
}

#[test]
fn lcm_engine_switch_rebuilds_from_raw_after_encrypted_native_state() {
    let persisted = crate::session::StoredCompactionState {
        summary_text: String::new(),
        openai_encrypted_content: Some("opaque-native-state".to_string()),
        covers_up_to_turn: 12,
        original_turn_count: 20,
        compacted_count: 12,
    };
    let mut manager = CompactionManager::new();
    manager.engine = crate::config::CompactionEngine::Rolling;
    manager.restore_persisted_state(&persisted, 20);
    assert_eq!(manager.compacted_count, 12);
    assert!(manager.active_summary.is_some());

    assert!(manager.synchronize_engine(crate::config::CompactionEngine::Lcm));
    assert_eq!(manager.compacted_count, 0);
    assert!(manager.active_summary.is_none());
    assert!(manager.active_chars.is_dirty());

    let mut restarted_lcm = CompactionManager::new();
    restarted_lcm.engine = crate::config::CompactionEngine::Lcm;
    restarted_lcm.restore_persisted_state(&persisted, 20);
    assert_eq!(restarted_lcm.compacted_count, 0);
    assert!(restarted_lcm.active_summary.is_none());
    assert!(restarted_lcm.active_chars.is_dirty());
    assert!(!restarted_lcm.synchronize_engine(crate::config::CompactionEngine::Lcm));
}

#[test]
fn lcm_legacy_fallback_rejects_iterative_owned_projection() -> Result<()> {
    let mut session = make_lcm_session("lcm_iterative_legacy_rejection", 20);
    session.compaction = Some(crate::session::StoredCompactionState {
        summary_text: "already generated by LCM".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 10,
        original_turn_count: 10,
        compacted_count: 10,
    });
    let mut manager = CompactionManager::new();
    manager.engine = crate::config::CompactionEngine::Lcm;
    manager.compacted_count = 10;
    assert_eq!(
        manager.install_critical_legacy_summary(&mut session, 15)?,
        None
    );
    assert!(session.context_nodes.is_empty());
    Ok(())
}

#[test]
fn lcm_route_policy_preserves_exact_identity_and_rejects_live_route_change() -> Result<()> {
    let mut session = make_lcm_session("lcm_route_policy", 20);
    session.model = Some("nvidia/example".into());
    session.provider_key = Some("openai-compatible:nvidia-nim".into());
    session.route_api_method = Some("openai-compatible:nvidia-nim".into());
    assert_eq!(
        lcm_route_spec(&session, "fallback", None),
        "nvidia-nim:nvidia/example"
    );
    assert_eq!(
        lcm_route_spec(&session, "fallback", Some("claude-api:claude-fable-5")),
        "claude-api:claude-fable-5"
    );

    let mut manager = CompactionManager::new();
    manager.engine = crate::config::CompactionEngine::Lcm;
    let source = manager.capture_lcm_source(
        &session,
        10,
        "nvidia/example".into(),
        "openai-compatible".into(),
        "nvidia-nim:nvidia/example".into(),
        None,
        850,
        "reactive".into(),
    )?;
    let mut policy = crate::config::CompactionConfig {
        engine: crate::config::CompactionEngine::Lcm,
        ..Default::default()
    };
    assert!(source.matches_runtime_policy(&session, &policy));

    session.route_api_method = Some("openai-api-key".into());
    session.provider_key = Some("openai".into());
    assert!(!source.matches_runtime_policy(&session, &policy));
    policy.model = Some("claude-api:claude-fable-5".into());
    assert!(!source.matches_runtime_policy(&session, &policy));

    session.model = Some("active-model".into());
    session.provider_key = Some("openai".into());
    session.route_api_method = Some("openai-api-key".into());
    let active_fallback = manager.capture_lcm_source(
        &session,
        10,
        "active-model".into(),
        "openai".into(),
        "openai-api:active-model".into(),
        Some("selected-model".into()),
        850,
        "critical_active_route".into(),
    )?;
    policy.model = Some("selected-model".into());
    assert!(active_fallback.matches_runtime_policy(&session, &policy));
    policy.model = Some("new-selected-model".into());
    assert!(!active_fallback.matches_runtime_policy(&session, &policy));
    Ok(())
}

#[test]
fn lcm_inherited_route_uses_typed_runtime_identity_matrix() -> Result<()> {
    let mut session = make_lcm_session("lcm_typed_route", 20);
    session.model = Some("nvidia/example".into());
    session.provider_key = Some("openai-compatible:nvidia-nim".into());
    session.route_api_method = Some("openai-compatible:nvidia-nim".into());
    let profile = lcm_inherited_route_selection(&session, "fallback").unwrap();
    assert_eq!(
        profile.runtime_key,
        crate::provider::RuntimeKey::OpenAiCompatible {
            profile_id: Some("nvidia-nim".to_string())
        }
    );

    session.model = Some("openai/gpt-5@Fireworks".into());
    session.provider_key = Some("openrouter".into());
    session.route_api_method = Some("openrouter".into());
    let openrouter = lcm_inherited_route_selection(&session, "fallback").unwrap();
    assert_eq!(
        openrouter.runtime_key,
        crate::provider::RuntimeKey::OpenRouter
    );
    assert_eq!(openrouter.model, "openai/gpt-5");
    assert_eq!(openrouter.provider_label, "Fireworks");
    assert_eq!(
        crate::provider::persisted_session_model_for_route(&openrouter, "openai/gpt-5"),
        "openai/gpt-5@Fireworks"
    );
    assert_eq!(
        crate::provider::persisted_session_model_for_route(&profile, "nvidia/example"),
        "nvidia/example"
    );

    session.model = Some("gpt-5.4".into());
    session.provider_key = Some("openai-api-key".into());
    session.route_api_method = Some("openai-api-key".into());
    let openai = lcm_inherited_route_selection(&session, "fallback").unwrap();
    assert_eq!(
        openai.runtime_key,
        crate::provider::RuntimeKey::OpenAIApiKey
    );

    let selections = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = RouteCapturingProvider {
        selections: Arc::clone(&selections),
    };
    set_lcm_inherited_route(&provider, &openai)?;
    assert_eq!(selections.lock().unwrap().as_slice(), &[openai]);
    Ok(())
}

#[test]
fn lcm_route_fingerprint_changes_after_auth_generation_change() {
    let session = make_lcm_session("lcm_auth_fingerprint", 20);
    let before = lcm_route_policy_fingerprint_with_auth_generation(&session, None, 41);
    let after = lcm_route_policy_fingerprint_with_auth_generation(&session, None, 42);
    assert_ne!(before, after);

    let account_one = lcm_route_policy_fingerprint_with_runtime_identity(
        &session,
        None,
        42,
        Some("claude-1"),
        Some("openai-1"),
    );
    let account_two = lcm_route_policy_fingerprint_with_runtime_identity(
        &session,
        None,
        42,
        Some("claude-1"),
        Some("openai-2"),
    );
    assert_ne!(account_one, account_two);
}

#[tokio::test]
async fn lcm_adapts_to_real_compactor_ceiling_without_dropping_source_coverage() -> Result<()> {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(AdaptiveLimitProvider {
        calls: Arc::clone(&calls),
        max_prompt_chars: 2_500,
    });
    let messages = (0..40)
        .map(|index| make_text_message(Role::User, &format!("source-{index} {}", "x".repeat(300))))
        .collect::<Vec<_>>();

    let result = generate_lcm_compaction_artifact(
        provider,
        messages.clone(),
        None,
        None,
        LcmJobPriority::Background,
    )
    .await?;

    assert_eq!(result.summarized_messages, messages.len());
    assert!(!result.summary_text.is_empty());
    assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 3);
    assert!(
        LCM_SAFE_PROMPT_CHARS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get("adaptive-limit:small-context")
            .copied()
            .is_some_and(|chars| chars <= 2_048)
    );
    Ok(())
}

#[test]
fn lcm_context_limit_classifier_accepts_common_provider_phrasing() {
    for message in [
        "context length exceeded",
        "too many tokens in request",
        "prompt is too long",
        "maximum token limit reached",
        "input is too long",
    ] {
        assert!(
            is_lcm_context_limit_error(&anyhow::anyhow!(message)),
            "missed {message}"
        );
    }
    assert!(!is_lcm_context_limit_error(&anyhow::anyhow!(
        "authentication failed"
    )));
}

#[test]
fn lcm_output_requires_exact_source_grounding_for_every_claim() {
    let source = "Do not claim all tests pass.\nObserved command failed.";
    assert!(!lcm_output_is_grounded(
        "- [source] The whole change set is now green and landed.",
        source
    ));
    assert!(!lcm_output_is_grounded(
        "- [source] All tests pass.",
        source
    ));
    assert!(!lcm_output_is_grounded(
        "# Everything is complete and credentials follow",
        source
    ));
    assert!(lcm_output_is_grounded(
        "- [source] Do not claim all tests pass.",
        source
    ));
    assert!(lcm_output_is_grounded("None observed.", source));
}

#[test]
fn lcm_grounding_repair_keeps_exact_excerpts_and_drops_unsupported_claims() {
    let source = "Do not claim all tests pass.\nObserved command failed.";
    let candidate = structured_test_summary(
        "Observed command failed.\nThe whole change set is now green and landed.",
    );

    let repaired =
        repair_lcm_output_grounding(&candidate, source, usize::MAX).expect("structured repair");

    assert!(repaired.contains("- [source] Observed command failed."));
    assert!(!repaired.contains("green and landed"));
    assert!(lcm_output_has_required_sections(&repaired));
    assert!(lcm_output_is_grounded(&repaired, source));
}

#[test]
fn lcm_grounding_repair_accepts_exact_contiguous_partial_line() {
    let source = "Observed command: cargo test -p jcode-base. Result: 36 passed.";
    let candidate = structured_test_summary("Result: 36 passed.");
    let repaired = repair_lcm_output_grounding(&candidate, source, 900)
        .expect("exact contiguous excerpt should survive repair");

    assert!(
        repaired
            .contains("- [source] Observed command: cargo test -p jcode-base. Result: 36 passed.")
    );
    assert!(lcm_output_is_grounded(&repaired, source));
}

#[test]
fn lcm_deterministic_reduction_preserves_chunk_order() {
    let first = structured_test_summary("Decision alpha was observed.");
    let second = structured_test_summary("Decision beta corrected alpha.");

    let reduced = reduce_validated_lcm_summaries(&[first, second], usize::MAX)
        .expect("deterministic reduction");

    let alpha = reduced.find("Decision alpha").expect("alpha retained");
    let beta = reduced.find("Decision beta").expect("beta retained");
    assert!(alpha < beta);
    assert!(lcm_output_has_required_sections(&reduced));
    assert!(lcm_output_is_grounded(
        &reduced,
        "Decision alpha was observed.\nDecision beta corrected alpha."
    ));
}

#[test]
fn lcm_small_source_summary_is_exact_and_bounded() {
    let messages = vec![make_text_message(
        Role::Assistant,
        "Observed correction: keep rolling as default.",
    )];
    let summary =
        summarize_small_lcm_source(&messages, None, 4_096).expect("small deterministic summary");

    assert!(summary.contains("- [source] Observed correction: keep rolling as default."));
    assert!(summary.len() <= LCM_INLINE_SOURCE_CHARS);
    assert!(lcm_output_has_required_sections(&summary));
    assert!(lcm_output_is_grounded(
        &summary,
        &lcm_safe_source_text(&messages, None)
    ));
}

#[tokio::test]
async fn lcm_critical_prompt_uses_latency_safe_cap() -> Result<()> {
    let systems = Arc::new(std::sync::Mutex::new(Vec::new()));
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(PromptCapturingProvider {
        systems,
        prompts: Arc::clone(&prompts),
    });

    generate_lcm_compaction_artifact(
        provider,
        vec![make_text_message(
            Role::User,
            &"observed line\n".repeat(4_000),
        )],
        None,
        Some("latency-cap-route".to_string()),
        LcmJobPriority::Critical,
    )
    .await?;

    let prompts = prompts.lock().expect("prompt lock");
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].len() <= LCM_CRITICAL_PROMPT_CHARS);
    Ok(())
}

#[test]
fn lcm_prompt_uses_only_the_structured_schema() {
    let opaque_secret = "correct horse battery staple";
    let messages = vec![Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolUse {
                id: "opaque-call".to_string(),
                name: "credential_helper".to_string(),
                input: serde_json::json!({"password": opaque_secret}),
                thought_signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: "opaque-call".to_string(),
                content: opaque_secret.to_string(),
                is_error: Some(false),
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    }];
    let prompt = build_lcm_compaction_prompt(&messages, None, 8_000);
    assert!(prompt.contains("section schema"));
    assert!(prompt.contains("status=success"));
    assert!(prompt.contains("use conversation_search"));
    assert!(!prompt.contains(opaque_secret));
    assert!(!prompt.contains("Your response should be structured as follows"));
    assert!(
        LCM_REQUIRED_SECTIONS
            .iter()
            .all(|section| LCM_SUMMARY_SYSTEM_PROMPT.contains(section))
    );
}

#[test]
fn lcm_prompt_omits_opaque_secrets_from_plain_text() {
    let labeled_secret = "correct horse battery staple";
    let unlabeled_secret = "q7Vn4Zp9Lx2Kc8Mw5Rt1Hs6Bd3Yf";
    let messages = vec![make_text_message(
        Role::User,
        &format!(
            "Session credential: {labeled_secret}\nOpaque value {unlabeled_secret}\nKeep this safe line"
        ),
    )];

    let prompt = build_lcm_compaction_prompt(&messages, None, 8_000);

    assert!(!prompt.contains(labeled_secret));
    assert!(!prompt.contains(unlabeled_secret));
    assert!(prompt.contains("Keep this safe line"));
}

#[tokio::test]
async fn lcm_rejects_ungrounded_model_written_secret() {
    let secret = "Authorization: Bearer sk-super-secret-value";
    let provider: Arc<dyn Provider> = Arc::new(StaticSummaryProvider {
        summary: structured_test_summary(secret),
    });
    let result = generate_lcm_compaction_artifact(
        provider,
        vec![make_text_message(Role::User, "Observed work only")],
        None,
        None,
        LcmJobPriority::Background,
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn lcm_rejects_grounded_opaque_secret() {
    for secret_source in [
        "q7Vn4Zp9Lx2Kc8Mw5Rt1Hs6Bd3Yf",
        "Session credential: correct horse battery staple",
    ] {
        let provider: Arc<dyn Provider> = Arc::new(StaticSummaryProvider {
            summary: structured_test_summary(secret_source),
        });
        let result = generate_lcm_compaction_artifact(
            provider,
            vec![make_text_message(Role::User, secret_source)],
            None,
            None,
            LcmJobPriority::Background,
        )
        .await;

        assert!(result.is_err(), "secret source must fail closed");
    }
}

#[tokio::test]
async fn lcm_rejects_completion_claim_absent_from_canonical_source() {
    let provider: Arc<dyn Provider> = Arc::new(StaticSummaryProvider {
        summary: structured_test_summary("All tests pass."),
    });
    let result = generate_lcm_compaction_artifact(
        provider,
        vec![make_text_message(Role::User, "Plan tests for tomorrow")],
        None,
        None,
        LcmJobPriority::Background,
    )
    .await;
    let Err(error) = result else {
        panic!("unsupported completion claim must fail closed")
    };
    assert!(error.to_string().contains("unsupported"));
}

#[tokio::test]
async fn transfer_compaction_uses_explicit_engine_snapshot() -> Result<()> {
    let systems = Arc::new(std::sync::Mutex::new(Vec::new()));
    let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(PromptCapturingProvider {
        systems: Arc::clone(&systems),
        prompts: Arc::clone(&prompts),
    });
    let secret = "tool input {\"api_key\":\"credential-that-must-not-leak\"}";
    let messages = vec![make_text_message(
        Role::User,
        &format!("{} {secret}", "source ".repeat(200)),
    )];
    build_transfer_compaction_state(
        Arc::clone(&provider),
        messages.clone(),
        None,
        crate::config::CompactionEngine::Rolling,
    )
    .await?;
    build_transfer_compaction_state(
        provider,
        messages,
        None,
        crate::config::CompactionEngine::Lcm,
    )
    .await?;

    let captured = systems.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        captured[0],
        "You are a helpful assistant that summarizes conversations."
    );
    assert_eq!(captured[1], LCM_SUMMARY_SYSTEM_PROMPT);
    let captured_prompts = prompts.lock().unwrap();
    assert_eq!(captured_prompts.len(), 2);
    for prompt in captured_prompts.iter() {
        assert!(!prompt.contains("credential-that-must-not-leak"));
    }
    assert!(captured_prompts[0].contains("[REDACTED_SECRET]"));
    assert!(captured_prompts[1].contains(LCM_OMITTED_SOURCE));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn lcm_scheduler_caps_process_wide_compactor_concurrency() -> Result<()> {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for index in 0..12 {
        let provider: Arc<dyn Provider> = Arc::new(ScheduledProvider {
            active: Arc::clone(&active),
            maximum: Arc::clone(&maximum),
        });
        tasks.push(tokio::spawn(async move {
            generate_lcm_compaction_artifact(
                provider,
                vec![make_text_message(
                    Role::User,
                    &format!("scheduled source {index} {}", "x".repeat(1_000)),
                )],
                None,
                None,
                LcmJobPriority::Background,
            )
            .await
        }));
    }
    for task in tasks {
        task.await??;
    }
    let observed = maximum.load(std::sync::atomic::Ordering::SeqCst);
    assert!(observed > 0);
    assert!(observed <= 3, "observed {observed} background compactors");
    assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn lcm_scheduler_reserves_capacity_for_critical_work() -> Result<()> {
    let shared = Arc::new(tokio::sync::Semaphore::new(4));
    let background = Arc::new(tokio::sync::Semaphore::new(3));
    let mut admitted_background = Vec::new();
    for _ in 0..4 {
        if admitted_background.len() == 3 {
            break;
        }
        admitted_background.push(
            acquire_lcm_scheduler_from(
                Arc::clone(&shared),
                Arc::clone(&background),
                LcmJobPriority::Background,
                std::time::Duration::from_millis(50),
            )
            .await?,
        );
    }
    assert_eq!(admitted_background.len(), 3);
    let queued_background = acquire_lcm_scheduler_from(
        Arc::clone(&shared),
        Arc::clone(&background),
        LcmJobPriority::Background,
        std::time::Duration::from_millis(20),
    )
    .await;
    match queued_background {
        Err(error) => assert!(error.to_string().contains("queue timed out")),
        Ok(_) => panic!("fourth background job bypassed reserved critical capacity"),
    }

    let critical = acquire_lcm_scheduler_from(
        Arc::clone(&shared),
        Arc::clone(&background),
        LcmJobPriority::Critical,
        std::time::Duration::from_millis(20),
    )
    .await?;
    assert_eq!(shared.available_permits(), 0);
    drop(critical);
    drop(admitted_background);
    assert_eq!(shared.available_permits(), 4);
    assert_eq!(background.available_permits(), 3);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn lcm_scheduler_fairness_scorecard_has_bounded_p95_and_no_starvation() -> Result<()> {
    const JOBS_PER_CLASS: usize = 24;
    let shared = Arc::new(tokio::sync::Semaphore::new(4));
    let background = Arc::new(tokio::sync::Semaphore::new(3));
    let mut background_holders = Vec::new();
    for _ in 0..3 {
        background_holders.push(
            acquire_lcm_scheduler_from(
                Arc::clone(&shared),
                Arc::clone(&background),
                LcmJobPriority::Background,
                Duration::from_secs(1),
            )
            .await?,
        );
    }

    let mut jobs = Vec::with_capacity(JOBS_PER_CLASS * 2);
    for priority in [LcmJobPriority::Background, LcmJobPriority::Critical] {
        for sequence in 0..JOBS_PER_CLASS {
            let shared = Arc::clone(&shared);
            let background = Arc::clone(&background);
            jobs.push(tokio::spawn(async move {
                let queued_at = Instant::now();
                let permit = acquire_lcm_scheduler_from(
                    shared,
                    background,
                    priority,
                    Duration::from_secs(2),
                )
                .await?;
                let waited_us = queued_at.elapsed().as_micros();
                tokio::time::sleep(Duration::from_millis(2)).await;
                drop(permit);
                Ok::<_, anyhow::Error>((priority, sequence, waited_us))
            }));
        }
    }

    // Critical work must make progress through the reserved fourth slot even
    // while all three background permits are occupied.
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(background_holders);

    let mut background_wait_us = Vec::with_capacity(JOBS_PER_CLASS);
    let mut critical_wait_us = Vec::with_capacity(JOBS_PER_CLASS);
    for job in jobs {
        let (priority, _sequence, waited_us) = job.await??;
        match priority {
            LcmJobPriority::Background => background_wait_us.push(waited_us),
            LcmJobPriority::Critical => critical_wait_us.push(waited_us),
        }
    }
    let percentile_us = |samples: &mut Vec<u128>, percentile: usize| {
        samples.sort_unstable();
        samples[(samples.len() * percentile).div_ceil(100) - 1]
    };
    let background_p95_us = percentile_us(&mut background_wait_us, 95);
    let critical_p95_us = percentile_us(&mut critical_wait_us, 95);
    let passed = background_wait_us.len() == JOBS_PER_CLASS
        && critical_wait_us.len() == JOBS_PER_CLASS
        && background_p95_us <= 500_000
        && critical_p95_us <= 500_000
        && shared.available_permits() == 4
        && background.available_permits() == 3;
    let scorecard = serde_json::json!({
        "schema_version": 1,
        "jobs_per_class": JOBS_PER_CLASS,
        "completed": {
            "background": background_wait_us.len(),
            "critical": critical_wait_us.len(),
        },
        "queue_p95_us": {
            "background": background_p95_us,
            "critical": critical_p95_us,
        },
        "thresholds": {
            "queue_p95_us_max": 500_000,
            "starved_jobs_max": 0,
            "shared_permits_restored": 4,
            "background_permits_restored": 3,
        },
        "starved_jobs": 0,
        "passed": passed,
    });
    if let Some(path) = std::env::var_os("JCODE_LCM_SCHEDULER_CERT_OUTPUT") {
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&scorecard)?)?;
    }
    assert!(passed, "scheduler scorecard failed: {scorecard}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn lcm_scheduler_recovers_capacity_after_stalls_and_cancellation() -> Result<()> {
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut stalled = Vec::new();
    for _ in 0..4 {
        let provider: Arc<dyn Provider> = Arc::new(StallingProvider {
            started: Arc::clone(&started),
        });
        stalled.push(tokio::spawn(generate_lcm_compaction_artifact(
            provider,
            vec![make_text_message(Role::User, &"x".repeat(1_000))],
            None,
            None,
            LcmJobPriority::Critical,
        )));
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while started.load(std::sync::atomic::Ordering::SeqCst) < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    for task in &stalled {
        task.abort();
    }
    for task in stalled {
        match task.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("aborted compactor task unexpectedly completed"),
        }
    }

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        generate_lcm_compaction_artifact(
            provider,
            vec![make_text_message(Role::User, &"y".repeat(1_000))],
            None,
            None,
            LcmJobPriority::Critical,
        ),
    )
    .await??;

    let provider: Arc<dyn Provider> = Arc::new(StallingProvider {
        started: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let stalled_result = generate_lcm_compaction_artifact(
        provider,
        vec![make_text_message(Role::User, &"z".repeat(1_000))],
        None,
        None,
        LcmJobPriority::Critical,
    )
    .await;
    let Err(error) = stalled_result else {
        panic!("stalled provider must time out")
    };
    assert!(error.to_string().contains("timed out"));
    Ok(())
}

#[test]
fn lcm_chunking_does_not_split_parallel_tool_transactions() {
    let calls = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolUse {
                id: "call-a".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"path": "a.rs"}),
                thought_signature: None,
            },
            ContentBlock::ToolUse {
                id: "call-b".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"path": "b.rs"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    };
    let result_a = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "call-a".to_string(),
            content: "a".repeat(200),
            is_error: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    };
    let result_b = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "call-b".to_string(),
            content: "b".repeat(200),
            is_error: Some(true),
        }],
        timestamp: None,
        tool_duration_ms: None,
    };
    let chunks = lcm_message_chunks(
        vec![
            make_text_message(Role::User, "before"),
            calls,
            result_a,
            result_b,
            make_text_message(Role::Assistant, "consumed both results"),
        ],
        100,
    );

    let transaction_chunk = chunks
        .iter()
        .find(|chunk| {
            chunk.iter().any(|message| {
                message.content.iter().any(
                    |block| matches!(block, ContentBlock::ToolUse { id, .. } if id == "call-a"),
                )
            })
        })
        .expect("parallel tool transaction chunk");
    let rendered =
        jcode_compaction_core::build_compaction_conversation_text(transaction_chunk, None);
    assert!(rendered.contains("id=call-a"));
    assert!(rendered.contains("id=call-b"));
    assert!(rendered.contains("tool_use_id=call-a"));
    assert!(rendered.contains("tool_use_id=call-b"));
    assert!(rendered.contains("tool_use_id=call-a status=unknown"));
    assert!(rendered.contains("tool_use_id=call-b status=error"));
    assert!(rendered.contains("consumed both results"));
}

#[test]
fn lcm_chunking_preserves_missing_and_orphan_tool_result_evidence() {
    let missing_call = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "missing-call".to_string(),
            name: "write".to_string(),
            input: serde_json::json!({"path": "src/missing.rs"}),
            thought_signature: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    };
    let repair_context = make_text_message(
        Role::Assistant,
        "The tool result is missing; do not claim the write completed.",
    );
    let orphan_result = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "orphan-call".to_string(),
            content: "recovered result tail".to_string(),
            is_error: Some(true),
        }],
        timestamp: None,
        tool_duration_ms: None,
    };
    let chunks = lcm_message_chunks(vec![missing_call, repair_context, orphan_result], 1);
    assert!(chunks.iter().any(|chunk| {
        let rendered = build_compaction_conversation_text(chunk, None);
        rendered.contains("missing-call") && rendered.contains("do not claim")
    }));
    let rendered = chunks
        .iter()
        .map(|chunk| build_compaction_conversation_text(chunk, None))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("orphan-call"));
    assert!(rendered.contains("status=error"));
    assert!(rendered.contains("recovered result tail"));
}

#[tokio::test]
async fn lcm_source_chunks_run_with_bounded_concurrency() -> Result<()> {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(ScheduledProvider {
        active: Arc::clone(&active),
        maximum: Arc::clone(&maximum),
    });
    let messages = (0..6)
        .map(|index| {
            make_text_message(
                Role::User,
                &format!("chunk-{index}: {}", "observed source ".repeat(80)),
            )
        })
        .collect();

    let summary = summarize_lcm_source_with_budget(&provider, messages, None, 2_000, 2_000).await?;

    assert!(lcm_output_has_required_sections(&summary));
    assert_eq!(
        maximum.load(std::sync::atomic::Ordering::SeqCst),
        LCM_CHUNK_CONCURRENCY
    );
    assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 0);
    Ok(())
}

const LCM_CERT_TRACE_COUNT: usize = 30;
const LCM_CERT_FACTS_PER_TRACE: usize = 5;
const LCM_CERT_CYCLES: usize = 2;
const LCM_CERT_MIN_RECALL: f64 = 0.95;
const LCM_CERT_MIN_WILSON_LOWER: f64 = 0.95;
const LCM_CERT_MAX_OUTPUT_SOURCE_RATIO: f64 = 0.35;
const LCM_CERT_MAX_P95_US: u128 = 1_000_000;

/// Neutral generator contract for external Hermes: iterate trace `0..30`, then
/// cycle `0..2`, then kinds `DECISION,CONSTRAINT,CORRECTION,PATH,ERROR`. Cycle 0
/// plants `CANARY_PHASE56_<KIND>_<TRACE:02> = VALUE_PHASE56_<KIND>_<TRACE:02>`;
/// cycle 1 deliberately does not repeat it, so recall must survive the prior
/// compaction. Use the exact format strings below, UTF-8, LF, and ascending
/// indexes. This is byte-reproducible and scrubbed: no secrets or operator data.
fn lcm_cert_trace(trace: usize, cycle: usize) -> (Vec<String>, Vec<Message>) {
    let kinds = ["DECISION", "CONSTRAINT", "CORRECTION", "PATH", "ERROR"];
    let facts = kinds
        .iter()
        .map(|kind| format!("CANARY_PHASE56_{kind}_{trace:02} = VALUE_PHASE56_{kind}_{trace:02}"))
        .collect::<Vec<_>>();
    let filler = (0..40)
        .map(|index| format!("phase56_trace_{trace:02}_coding_filler_{index:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut messages: Vec<Message> = facts
        .iter()
        .enumerate()
        .flat_map(|(turn, fact)| {
            let planted = if cycle == 0 {
                format!("retain {fact}. ")
            } else {
                String::new()
            };
            [
                make_text_message(
                    Role::User,
                    &format!(
                        "trace {trace:02} cycle {cycle} turn {turn}: {planted}inspect src/module_{trace:02}.rs"
                    ),
                ),
                make_text_message(
                    Role::Assistant,
                    &format!(
                        "Observed cycle {cycle} turn {turn}; the coding task remains pending. Evidence: {} {filler}",
                        "source and test output remain canonical. ".repeat(4),
                    ),
                ),
            ]
        })
        .collect();
    messages.push(make_text_message(
        Role::User,
        "Recall every planted token from this trace; do not claim the task is complete.",
    ));
    messages.push(make_text_message(
        Role::Assistant,
        "Recall is pending compaction; the coding task remains pending.",
    ));
    (facts, messages)
}

fn lcm_cert_active_recall(summary: &str, facts: &[String]) -> usize {
    facts
        .iter()
        .filter(|fact| summary.contains(fact.as_str()))
        .count()
}

fn lcm_cert_percentile(samples: &mut [u128], percentile: usize) -> u128 {
    samples.sort_unstable();
    samples[(samples.len() * percentile).div_ceil(100) - 1]
}

fn lcm_cert_wilson_95(successes: usize, total: usize) -> (f64, f64) {
    let z = 1.959_963_984_540_054_f64;
    let n = total as f64;
    let proportion = successes as f64 / n;
    let denominator = 1.0 + z * z / n;
    let center = (proportion + z * z / (2.0 * n)) / denominator;
    let margin =
        z * ((proportion * (1.0 - proportion) / n + z * z / (4.0 * n * n)).sqrt()) / denominator;
    (center - margin, center + margin)
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
            "name": "neutral_coding_trace_v2",
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
    let manager = CompactionManager::new();
    assert_eq!(manager.compacted_count, 0);
    assert!(manager.active_summary.is_none());
    assert!(!manager.is_compacting());
}

#[test]
fn test_notify_message_added() {
    let mut manager = CompactionManager::new();
    manager.notify_message_added();
    manager.notify_message_added();
    assert_eq!(manager.total_turns, 2);
}

#[test]
fn test_restored_messages_do_not_trigger_compaction_immediately() {
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let manager = CompactionManager::new();
    // 100 chars = ~25 tokens (plus 18k overhead for full budget)
    let messages = vec![make_text_message(Role::User, &"x".repeat(100))];
    let estimate = manager.token_estimate_with(&messages);
    // With DEFAULT_TOKEN_BUDGET and 18k overhead: 25 + 18000 = 18025
    assert!((18_000..19_000).contains(&estimate));
}

#[test]
fn test_should_compact() {
    let mut manager = CompactionManager::new().with_budget(100); // Very small budget

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
    let mut manager = CompactionManager::new().with_budget(1_000);
    let messages = vec![make_text_message(Role::User, "short message")];
    manager.notify_message_added();
    manager.update_observed_input_tokens(900);

    assert!(manager.context_usage_with(&messages) >= 0.90);
    assert!(manager.effective_token_count_with(&messages) >= 900);
}

#[test]
fn test_should_compact_uses_observed_tokens() {
    let mut manager = CompactionManager::new().with_budget(1_000);

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
    let mut manager = CompactionManager::new();
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(10_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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

/// Defense-in-depth: if `compacted_count` advances while a background
/// compaction is in flight (so its `pending_cutoff` becomes stale), applying
/// the completed result must NOT over-advance `compacted_count` and wipe the
/// live tail. The stale result should be discarded instead.
#[tokio::test]
async fn test_stale_background_result_discarded_when_context_shrinks() {
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(100);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(500);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(500);
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

#[test]
fn test_persisted_state_round_trip_preserves_compacted_view() {
    let mut manager = CompactionManager::new().with_budget(500);
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

    let mut restored = CompactionManager::new().with_budget(500);
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
    let mut manager = CompactionManager::new().with_budget(200_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
    let mut manager = CompactionManager::new().with_budget(1_000);
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
