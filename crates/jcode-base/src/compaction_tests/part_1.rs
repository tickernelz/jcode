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
struct NativeCompactionCapturingProvider {
    messages: Arc<std::sync::Mutex<Vec<Message>>>,
    existing_summary: Arc<std::sync::Mutex<Option<String>>>,
    returned_summary: String,
}

#[derive(Clone)]
struct ScheduledProvider {
    active: Arc<std::sync::atomic::AtomicUsize>,
    maximum: Arc<std::sync::atomic::AtomicUsize>,
}

fn compaction_test_identity(
    provider: &str,
    model: &str,
) -> jcode_provider_core::ExactRuntimeIdentity {
    jcode_provider_core::ExactRuntimeIdentity {
        provider_key: provider.to_string(),
        route: jcode_provider_core::RouteSelection {
            model: model.to_string(),
            runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
            api_method: "test-compaction".to_string(),
            provider_label: provider.to_string(),
            detail: String::new(),
        },
        account_label: Some("test-compactor".to_string()),
        account_id: Some(format!("stable-{provider}")),
        account_generation: Some(1),
        reasoning_effort: None,
    }
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
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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
impl Provider for NativeCompactionCapturingProvider {
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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
        "native-capture"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn native_compact(
        &self,
        messages: &[Message],
        existing_summary_text: Option<&str>,
        _existing_openai_encrypted_content: Option<&str>,
    ) -> Result<crate::provider::NativeCompactionResult> {
        *self.messages.lock().expect("messages lock") = messages.to_vec();
        *self.existing_summary.lock().expect("summary lock") =
            existing_summary_text.map(ToString::to_string);
        Ok(crate::provider::NativeCompactionResult {
            summary_text: Some(self.returned_summary.clone()),
            openai_encrypted_content: None,
        })
    }
}

#[async_trait::async_trait]
impl Provider for StaticSummaryProvider {
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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

    fn set_model(&self, _model: &str) -> Result<()> {
        Ok(())
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        Ok(self.summary.clone())
    }
}

#[async_trait::async_trait]
impl Provider for StallingProvider {
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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

    fn set_model(&self, _model: &str) -> Result<()> {
        Ok(())
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::future::pending::<Result<String>>().await
    }
}

#[async_trait::async_trait]
impl Provider for RouteCapturingProvider {
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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
        let facts = prompt
            .split("planted fact: ")
            .skip(1)
            .filter_map(|tail| tail.split_once('.').map(|(fact, _)| fact.trim()))
            .map(|fact| format!("planted fact: {fact}."))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join("\n");
        Ok(structured_test_summary(&facts))
    }
}

#[async_trait::async_trait]
impl Provider for FactPreservingProvider {
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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
        let mut facts = std::collections::HashSet::<String>::new();
        let evidence = prompt
            .split_once("\nQUERIES:\n")
            .map_or(prompt, |(context, _)| context);
        for line in evidence.lines().map(str::trim) {
            if let Some((_, fact)) = line.split_once("planted fact: ") {
                let fact = fact.split_once('.').map_or(fact, |(fact, _)| fact);
                facts.insert(format!("planted fact: {fact}"));
            }
        }
        let mut facts = facts.into_iter().collect::<Vec<_>>();
        facts.sort();
        Ok(structured_test_summary(&facts.join("\n")))
    }
}

#[async_trait::async_trait]
impl Provider for ScheduledProvider {
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(compaction_test_identity(self.name(), &self.model()))
    }
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
    session.exact_runtime_identity = Some(compaction_test_identity("session", "session-model"));
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
fn lcm_rejects_opaque_session_identity_before_scheduling() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _guard = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_opaque_identity", 30);
    session.exact_runtime_identity = None;
    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let mut manager = CompactionManager::new().with_budget(200);

    let error = manager
        .force_lcm_compact_with(&session, provider)
        .expect_err("opaque session identity must fail closed");
    assert!(error.contains("session credential identity is opaque"));
    assert!(manager.pending_task.is_none());
    Ok(())
}

#[tokio::test]
async fn lcm_attempt_telemetry_has_one_terminal_outcome_and_redacts_secrets() -> Result<()> {
    let attempt_id = crate::id::new_id("telemetry_attempt");
    let secret = "sk-test-telemetry-secret-1234567890";
    let attempt = LcmAttemptContext {
        attempt_id: attempt_id.clone(),
        session_id: "telemetry-session".to_string(),
        trigger: "reactive".to_string(),
        stage: "leaf".to_string(),
        configured_route: Some(format!("openai-api:model?api_key={secret}")),
        effective_route: format!("profile:model?token={secret}"),
        source_messages: 1,
        pre_tokens: 42,
        terminal_emitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let provider: Arc<dyn Provider> = Arc::new(StaticSummaryProvider {
        summary: format!("# Objective\nleaked {secret}"),
    });
    let messages = vec![Message::user("telemetry source")];

    let result = generate_lcm_compaction_artifact_with_attempt(
        provider,
        messages,
        None,
        None,
        LcmJobPriority::Background,
        attempt,
    )
    .await;
    let Err(error) = result else {
        panic!("secret-shaped generated summary must fail validation")
    };
    assert!(error.to_string().contains("secret-shaped"));

    let events = take_lcm_attempt_test_events(&attempt_id);
    let field = |event: &Vec<(String, String)>, key: &str| {
        event
            .iter()
            .find_map(|(candidate, value)| (candidate == key).then_some(value.clone()))
            .unwrap_or_default()
    };
    assert_eq!(
        events
            .iter()
            .map(|event| field(event, "outcome"))
            .collect::<Vec<_>>(),
        ["queued", "running", "failed"]
    );
    let terminals = events
        .iter()
        .filter(|event| {
            matches!(
                field(event, "outcome").as_str(),
                "published" | "failed" | "cancelled" | "superseded"
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1);
    assert_eq!(field(terminals[0], "reason_code"), "secret_output_rejected");
    assert!(field(&events[1], "queue_ms").parse::<u64>().is_ok());
    assert!(field(terminals[0], "execution_ms").parse::<u64>().is_ok());
    for event in &events {
        let encoded = serde_json::to_string(event)?;
        assert!(!encoded.contains(secret));
        for key in [
            "attempt_id",
            "session_id",
            "stage",
            "trigger",
            "configured_route",
            "effective_route",
            "source_messages",
            "pre_tokens",
            "queue_ms",
            "execution_ms",
            "persistence_ms",
            "adaptive_retries",
            "reason_code",
            "outcome",
        ] {
            assert!(event.iter().any(|(candidate, _)| candidate == key));
        }
    }
    Ok(())
}

#[tokio::test]
async fn lcm_attempt_telemetry_records_manager_cancellation_once() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _guard = TestHomeGuard::set(home.path());
    let session = make_lcm_session("lcm_cancel_telemetry", 30);
    let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(StallingProvider {
        started: Arc::clone(&started),
    });
    let mut manager = CompactionManager::new().with_budget(200);

    manager
        .force_lcm_compact_with(&session, provider)
        .map_err(anyhow::Error::msg)?;
    let attempt_id = manager
        .pending_lcm_source
        .as_ref()
        .expect("pending canonical source")
        .attempt_context("leaf")
        .attempt_id;
    tokio::time::timeout(Duration::from_secs(1), async {
        while started.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    manager.reset();

    let events = take_lcm_attempt_test_events(&attempt_id);
    let outcomes = events
        .iter()
        .filter_map(|event| {
            event
                .iter()
                .find_map(|(key, value)| (key == "outcome").then_some(value.as_str()))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| {
                ["published", "failed", "cancelled", "superseded"].contains(*outcome)
            })
            .count(),
        1
    );
    assert_eq!(outcomes.last(), Some(&"cancelled"));
    let terminal = events.last().expect("terminal cancellation event");
    assert!(
        terminal
            .iter()
            .any(|(key, value)| { key == "reason_code" && value == "manager_reset_or_drop" })
    );
    Ok(())
}

#[tokio::test]
async fn lcm_attempt_telemetry_suppresses_reset_after_task_terminal_failure() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _guard = TestHomeGuard::set(home.path());
    let session = make_lcm_session("lcm_failed_then_reset_telemetry", 30);
    let provider: Arc<dyn Provider> = Arc::new(StaticSummaryProvider {
        summary: "# Objective\nsk-test-terminal-race-secret-123456789".to_string(),
    });
    let mut manager = CompactionManager::new().with_budget(200);
    manager
        .force_lcm_compact_with(&session, provider)
        .map_err(anyhow::Error::msg)?;
    let attempt_id = manager
        .pending_lcm_source
        .as_ref()
        .expect("pending source")
        .attempt_context("leaf")
        .attempt_id;
    tokio::time::timeout(Duration::from_secs(1), async {
        while !manager
            .pending_task
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    manager.reset();
    let events = take_lcm_attempt_test_events(&attempt_id);
    let outcomes = events
        .iter()
        .filter_map(|event| {
            event
                .iter()
                .find_map(|(key, value)| (key == "outcome").then_some(value.as_str()))
        })
        .collect::<Vec<_>>();
    assert_eq!(outcomes, ["queued", "running", "failed"]);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| {
                ["published", "failed", "cancelled", "superseded"].contains(*outcome)
            })
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn lcm_attempt_telemetry_success_has_full_lifecycle_and_one_terminal() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "[compaction]\nengine = \"lcm\"\n",
    )?;
    let _guard = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_success_telemetry", 20);
    session.model = Some("session-model".to_string());
    session.provider_key = Some("session".to_string());
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..20 {
        manager.notify_message_added();
    }
    let route = lcm_route_spec(&session, "session-model", None);
    let source = manager.capture_lcm_source(
        &session,
        10,
        "session-model".into(),
        "session".into(),
        route,
        None,
        850,
        "reactive".into(),
    )?;
    let attempt = source.attempt_context("leaf");
    let attempt_id = attempt.attempt_id.clone();
    let result = generate_lcm_compaction_artifact_with_attempt(
        Arc::new(MockSummaryProvider),
        session.messages_for_provider_uncached()[..10].to_vec(),
        None,
        None,
        LcmJobPriority::Background,
        attempt,
    )
    .await?;
    manager.prepared_lcm_context = Some(CompactionManager::prepare_lcm_context(source, result)?);
    let (_, event) = manager.materialize_lcm_context(&mut session)?;
    assert!(event.is_some());

    let events = take_lcm_attempt_test_events(&attempt_id);
    let field = |event: &Vec<(String, String)>, key: &str| {
        event
            .iter()
            .find_map(|(candidate, value)| (candidate == key).then_some(value.clone()))
            .unwrap_or_default()
    };
    assert_eq!(
        events
            .iter()
            .map(|event| field(event, "outcome"))
            .collect::<Vec<_>>(),
        ["queued", "running", "generated", "published"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                field(event, "outcome").as_str(),
                "published" | "failed" | "cancelled" | "superseded"
            ))
            .count(),
        1
    );
    assert_eq!(
        field(events.last().unwrap(), "reason_code"),
        "durable_commit_succeeded"
    );
    assert!(field(&events[1], "queue_ms").parse::<u64>().is_ok());
    assert!(field(&events[2], "execution_ms").parse::<u64>().is_ok());
    assert!(
        field(events.last().unwrap(), "persistence_ms")
            .parse::<u64>()
            .is_ok()
    );
    Ok(())
}

#[tokio::test]
async fn lcm_attempt_telemetry_supersession_is_single_and_stable() -> Result<()> {
    let _lock = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "[compaction]\nengine = \"lcm\"\n",
    )?;
    let _guard = TestHomeGuard::set(home.path());
    let mut session = make_lcm_session("lcm_superseded_telemetry", 20);
    let mut manager = CompactionManager::new().with_budget(1_000);
    manager.engine = crate::config::CompactionEngine::Lcm;
    for _ in 0..20 {
        manager.notify_message_added();
    }
    let source = manager.capture_lcm_source(
        &session,
        10,
        "session-model".into(),
        "session".into(),
        lcm_route_spec(&session, "session-model", None),
        None,
        850,
        "reactive".into(),
    )?;
    let attempt_id = source.attempt_context("leaf").attempt_id;
    manager.pending_lcm_source = Some(source);
    manager.pending_task = Some(tokio::spawn(async {
        Ok(CompactionResult {
            summary_text: structured_test_summary(""),
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: 10,
            duration_ms: 1,
            summarized_messages: 10,
        })
    }));
    manager.pending_cutoff = 10;
    tokio::task::yield_now().await;
    session.messages[0].content = vec![ContentBlock::Text {
        text: "canonical source changed".to_string(),
        cache_control: None,
    }];
    manager.poll_lcm_candidate(&session);

    let events = take_lcm_attempt_test_events(&attempt_id);
    assert_eq!(events.len(), 1);
    assert!(
        events[0]
            .iter()
            .any(|(key, value)| key == "outcome" && value == "superseded")
    );
    assert!(
        events[0]
            .iter()
            .any(|(key, value)| key == "reason_code" && value == "canonical_source_changed")
    );
    assert!(manager.pending_task.is_none());
    assert!(manager.pending_lcm_source.is_none());
    Ok(())
}

#[test]
fn lcm_attempt_telemetry_reason_and_outcome_vocabularies_are_allowlisted() {
    let outcomes = [
        LcmAttemptOutcome::Queued,
        LcmAttemptOutcome::Running,
        LcmAttemptOutcome::Retrying,
        LcmAttemptOutcome::Generated,
        LcmAttemptOutcome::PersistenceRetry,
        LcmAttemptOutcome::Published,
        LcmAttemptOutcome::Failed,
        LcmAttemptOutcome::Cancelled,
        LcmAttemptOutcome::Superseded,
    ];
    let reasons = [
        LcmAttemptReason::ManagerResetOrDrop,
        LcmAttemptReason::PreparedCandidateDiscarded,
        LcmAttemptReason::RoutePolicyChanged,
        LcmAttemptReason::CanonicalSourceChanged,
        LcmAttemptReason::CandidateValidationFailed,
        LcmAttemptReason::TaskJoinFailed,
        LcmAttemptReason::RoutePolicyChangedBeforeCommit,
        LcmAttemptReason::DurableCommitFailed,
        LcmAttemptReason::DurableCommitSucceeded,
        LcmAttemptReason::SchedulerWait,
        LcmAttemptReason::SchedulerQueueTimeout,
        LcmAttemptReason::SchedulerClosed,
        LcmAttemptReason::SchedulerAdmitted,
        LcmAttemptReason::ProviderTimeout,
        LcmAttemptReason::ContextLimit,
        LcmAttemptReason::ContextLimitAdaptiveRetry,
        LcmAttemptReason::SecretOutputRejected,
        LcmAttemptReason::OutputValidationFailed,
        LcmAttemptReason::ProviderError,
        LcmAttemptReason::CandidateReady,
    ];
    let unique_outcomes = outcomes
        .iter()
        .map(|value| value.as_str())
        .collect::<std::collections::HashSet<_>>();
    let unique_reasons = reasons
        .iter()
        .map(|value| value.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique_outcomes.len(), outcomes.len());
    assert_eq!(unique_reasons.len(), reasons.len());
    assert_eq!(
        unique_outcomes
            .intersection(
                &["published", "failed", "cancelled", "superseded"]
                    .into_iter()
                    .collect()
            )
            .count(),
        4
    );
}

#[test]
fn lcm_attempt_telemetry_hierarchy_stages_share_one_terminal_lifecycle() {
    let attempt_id = crate::id::new_id("hierarchy_telemetry_attempt");
    let attempt = LcmAttemptContext {
        attempt_id: attempt_id.clone(),
        session_id: "hierarchy-telemetry-session".to_string(),
        trigger: "reactive".to_string(),
        stage: "leaf".to_string(),
        configured_route: None,
        effective_route: "test-route".to_string(),
        source_messages: 8,
        pre_tokens: 400,
        terminal_emitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let carry = attempt.child("carry-0");
    assert_eq!(carry.attempt_id, attempt.attempt_id);
    assert!(Arc::ptr_eq(
        &carry.terminal_emitted,
        &attempt.terminal_emitted
    ));
    emit_lcm_attempt(
        &carry,
        LcmAttemptOutcome::Generated,
        LcmAttemptReason::CandidateReady,
        Some(0),
        Some(1),
        None,
        0,
    );
    emit_lcm_attempt(
        &attempt,
        LcmAttemptOutcome::Published,
        LcmAttemptReason::DurableCommitSucceeded,
        Some(0),
        Some(1),
        Some(1),
        0,
    );
    emit_lcm_attempt(
        &carry,
        LcmAttemptOutcome::Failed,
        LcmAttemptReason::ProviderError,
        Some(0),
        Some(2),
        None,
        0,
    );

    let events = take_lcm_attempt_test_events(&attempt_id);
    let terminal_count = events
        .iter()
        .filter(|event| {
            event.iter().any(|(key, value)| {
                key == "outcome"
                    && ["published", "failed", "cancelled", "superseded"].contains(&value.as_str())
            })
        })
        .count();
    assert_eq!(terminal_count, 1);
    assert!(events.iter().any(|event| {
        event
            .iter()
            .any(|(key, value)| key == "stage" && value == "carry-0")
    }));
}
