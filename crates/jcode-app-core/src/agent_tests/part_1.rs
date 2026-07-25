use super::*;
use crate::agent::environment::EnvSnapshotDetail;
use crate::message::{Message, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use crate::tool::ToolOutput;
use async_trait::async_trait;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_stream::wrappers::ReceiverStream;

struct DelayedProvider {
    open_delay: Duration,
    first_event_delay: Duration,
}

struct NativeAutoCompactionProvider;

struct NativeCompactionStreamProvider;

struct SwitchingProvider {
    model: std::sync::Mutex<String>,
    reasoning_effort: std::sync::Mutex<Option<String>>,
}

struct IdentityProbeProvider {
    fail_effort_restore: bool,
    invalidations: std::sync::atomic::AtomicUsize,
}

struct AccountTransitionProvider {
    identity: Arc<std::sync::Mutex<jcode_provider_core::ExactRuntimeIdentity>>,
    switched: Arc<std::sync::atomic::AtomicBool>,
    seen_resume_ids: Arc<std::sync::Mutex<Vec<Option<String>>>>,
    refresh_identity: Arc<std::sync::Mutex<Option<jcode_provider_core::ExactRuntimeIdentity>>>,
    credential_refreshes: Arc<std::sync::atomic::AtomicUsize>,
}

struct StreamingGenerationProvider {
    identity: Arc<std::sync::Mutex<jcode_provider_core::ExactRuntimeIdentity>>,
    requests: Arc<std::sync::atomic::AtomicUsize>,
}

struct IdentityCheckingTool {
    expected_generation: u64,
    stale_executions: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl crate::tool::Tool for IdentityCheckingTool {
    fn name(&self) -> &str {
        "identity_check"
    }
    fn description(&self) -> &str {
        "checks the durable identity at execution"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        ctx: crate::tool::ToolContext,
    ) -> Result<ToolOutput> {
        let generation = crate::session::Session::load(&ctx.session_id)?
            .exact_runtime_identity
            .and_then(|identity| identity.account_generation);
        if generation != Some(self.expected_generation) {
            self.stale_executions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(ToolOutput::new("identity current"))
    }
}

fn account_transition_identity(
    label: &str,
    account_id: &str,
) -> jcode_provider_core::ExactRuntimeIdentity {
    jcode_provider_core::ExactRuntimeIdentity {
        provider_key: "openai".to_string(),
        route: jcode_provider_core::RouteSelection {
            model: "account-transition-model".to_string(),
            runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
            api_method: "openai-responses".to_string(),
            provider_label: "openai".to_string(),
            detail: String::new(),
        },
        account_label: Some(label.to_string()),
        account_id: Some(account_id.to_string()),
        account_generation: Some(1),
        reasoning_effort: Some("low".to_string()),
    }
}

fn agent_test_identity(provider: &str, model: &str) -> jcode_provider_core::ExactRuntimeIdentity {
    jcode_provider_core::ExactRuntimeIdentity {
        provider_key: provider.to_string(),
        route: jcode_provider_core::RouteSelection {
            model: model.to_string(),
            runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
            api_method: "test-api".to_string(),
            provider_label: provider.to_string(),
            detail: String::new(),
        },
        account_label: Some("test-account".to_string()),
        account_id: Some(format!("stable-{provider}")),
        account_generation: Some(1),
        reasoning_effort: None,
    }
}

fn content_text(content: &[ContentBlock]) -> &str {
    match content.first() {
        Some(ContentBlock::Text { text, .. }) => text,
        _ => "",
    }
}

fn message_text(message: &Message) -> &str {
    content_text(&message.content)
}

#[async_trait]
impl Provider for DelayedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        tokio::time::sleep(self.open_delay).await;

        let first_event_delay = self.first_event_delay;
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            tokio::time::sleep(first_event_delay).await;
            let _ = tx
                .send(Ok(StreamEvent::TextDelta("hello".to_string())))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "delayed"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            open_delay: self.open_delay,
            first_event_delay: self.first_event_delay,
        })
    }
}

#[async_trait]
impl Provider for AccountTransitionProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.seen_resume_ids
            .lock()
            .unwrap()
            .push(resume_session_id.map(str::to_string));
        if !self
            .switched
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            *self.identity.lock().unwrap() = account_transition_identity("account-b", "stable-b");
        }
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            let _ = tx.send(Ok(StreamEvent::TextDelta("ok".to_string()))).await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> String {
        "account-transition-model".to_string()
    }

    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(self.identity.lock().unwrap().clone())
    }

    async fn ensure_credentials_current(&self) -> Result<()> {
        self.credential_refreshes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(identity) = self.refresh_identity.lock().unwrap().take() {
            *self.identity.lock().unwrap() = identity;
        }
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            identity: Arc::clone(&self.identity),
            switched: Arc::clone(&self.switched),
            seen_resume_ids: Arc::clone(&self.seen_resume_ids),
            refresh_identity: Arc::clone(&self.refresh_identity),
            credential_refreshes: Arc::clone(&self.credential_refreshes),
        })
    }
}

#[async_trait]
impl Provider for StreamingGenerationProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let request = self
            .requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let identity = Arc::clone(&self.identity);
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            if request == 0 {
                identity.lock().unwrap().account_generation = Some(2);
                let _ = tx
                    .send(Ok(StreamEvent::ToolUseStart {
                        id: "generation-tool".to_string(),
                        name: "identity_check".to_string(),
                    }))
                    .await;
                let _ = tx
                    .send(Ok(StreamEvent::ToolInputDelta("{}".to_string())))
                    .await;
                let _ = tx.send(Ok(StreamEvent::ToolUseEnd)).await;
            }
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }
    fn model(&self) -> String {
        "account-transition-model".to_string()
    }
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(self.identity.lock().unwrap().clone())
    }
    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            identity: Arc::clone(&self.identity),
            requests: Arc::clone(&self.requests),
        })
    }
}

#[async_trait]
impl Provider for IdentityProbeProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        anyhow::bail!("identity probe must not execute a model turn")
    }

    fn name(&self) -> &str {
        "identity-probe"
    }

    fn model(&self) -> String {
        "identity-probe-model".to_string()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        if model == "identity-probe-model" || model.ends_with(":identity-probe-model") {
            Ok(())
        } else {
            anyhow::bail!("unsupported identity probe model {model}")
        }
    }

    fn set_reasoning_effort(&self, _effort: &str) -> Result<()> {
        if self.fail_effort_restore {
            anyhow::bail!("injected effort restore failure")
        }
        Ok(())
    }

    async fn invalidate_credentials(&self) {
        self.invalidations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            fail_effort_restore: self.fail_effort_restore,
            invalidations: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl Provider for NativeAutoCompactionProvider {
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(agent_test_identity(self.name(), &self.model()))
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (_tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(1);
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> String {
        "native-auto-fixture".to_string()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        if model == "native-auto-fixture" || model.ends_with(":native-auto-fixture") {
            Ok(())
        } else {
            anyhow::bail!("unsupported native-auto fixture model {model}")
        }
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn uses_jcode_compaction(&self) -> bool {
        false
    }

    fn context_window(&self) -> usize {
        1_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        Ok("manual summary from native-auto provider".to_string())
    }
}

#[async_trait]
impl Provider for NativeCompactionStreamProvider {
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        Some(agent_test_identity(self.name(), &self.model()))
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::Compaction {
                    trigger: "openai_native".to_string(),
                    pre_tokens: Some(80_000),
                    openai_encrypted_content: Some("enc_native_test".to_string()),
                }))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn uses_jcode_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }
}

#[async_trait]
impl Provider for SwitchingProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (_tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(1);
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "switching"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        *self.model.lock().expect("model lock") = model.to_string();
        Ok(())
    }

    fn reasoning_effort(&self) -> Option<String> {
        self.reasoning_effort.lock().expect("effort lock").clone()
    }

    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        *self.reasoning_effort.lock().expect("effort lock") =
            (!effort.trim().is_empty()).then(|| effort.trim().to_string());
        Ok(())
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: std::sync::Mutex::new(self.model()),
            reasoning_effort: std::sync::Mutex::new(self.reasoning_effort()),
        })
    }
}

#[test]
fn tool_output_to_content_blocks_preserves_labeled_images() {
    let output = ToolOutput::new("Image ready").with_labeled_image(
        "image/png",
        "ZmFrZQ==",
        "screenshots/example.png",
    );

    let blocks = tool_output_to_content_blocks("call_1".to_string(), output);
    assert_eq!(blocks.len(), 3);

    match &blocks[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            assert_eq!(tool_use_id, "call_1");
            assert_eq!(content, "Image ready");
            assert_eq!(*is_error, None);
        }
        other => panic!("expected tool result, got {other:?}"),
    }

    match &blocks[1] {
        ContentBlock::Image { media_type, data } => {
            assert_eq!(media_type, "image/png");
            assert_eq!(data, "ZmFrZQ==");
        }
        other => panic!("expected image block, got {other:?}"),
    }

    match &blocks[2] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("screenshots/example.png"));
            assert!(text.contains("preceding tool result"));
        }
        other => panic!("expected trailing label text, got {other:?}"),
    }
}

#[tokio::test]
async fn run_turn_streaming_mpsc_emits_keepalive_while_provider_is_quiet() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(DelayedProvider {
        open_delay: Duration::from_secs(2),
        first_event_delay: Duration::from_secs(2),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "test".to_string(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(tx).await });

    let mut saw_keepalive = false;
    let keepalive_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < keepalive_deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::Pong { id })) => {
                assert_eq!(id, STREAM_KEEPALIVE_PONG_ID);
                saw_keepalive = true;
                break;
            }
            Ok(Some(ServerEvent::TextDelta { text })) => {
                panic!("expected keepalive before text delta, got: {text}");
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before keepalive"),
            Err(_) => {
                assert!(
                    !task.is_finished(),
                    "streaming task finished before keepalive arrived"
                );
            }
        }
    }
    assert!(saw_keepalive, "expected keepalive before provider response");

    let mut saw_text = false;
    let text_deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < text_deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::TextDelta { text })) => {
                assert_eq!(text, "hello");
                saw_text = true;
                break;
            }
            Ok(Some(ServerEvent::Pong { id })) => {
                assert_eq!(id, STREAM_KEEPALIVE_PONG_ID);
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before text delta"),
            Err(_) => {
                assert!(
                    !task.is_finished(),
                    "streaming task finished before text delta arrived"
                );
            }
        }
    }

    assert!(saw_text, "expected delayed provider text after keepalive");
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn run_turn_streaming_mpsc_emits_native_compaction_for_client_cache_reset() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(NativeCompactionStreamProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "compact this".to_string(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent.run_turn_streaming_mpsc(tx).await.unwrap();

    let mut saw_native_compaction = false;
    while let Ok(event) = rx.try_recv() {
        if let ServerEvent::Compaction {
            trigger,
            messages_compacted,
            ..
        } = event
        {
            assert_eq!(trigger, "openai_native");
            assert!(
                messages_compacted.is_some_and(|count| count > 0),
                "native compaction should report a non-empty compacted prefix"
            );
            saw_native_compaction = true;
        }
    }
    assert!(
        saw_native_compaction,
        "native provider compaction must reach clients so they clear KV baselines"
    );
}

/// Provider that transparently switches its model mid-stream, mimicking the
/// Anthropic retired-model fallback (`claude-fable-5` -> `claude-opus-4-8`).
struct MidStreamModelSwitchProvider {
    model: std::sync::Mutex<String>,
    switch_to: String,
}

#[async_trait]
impl Provider for MidStreamModelSwitchProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        // Emulate the provider switching its own model state during the request.
        *self.model.lock().unwrap() = self.switch_to.clone();
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(StreamEvent::TextDelta("hello".to_string())))
                .await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            model: std::sync::Mutex::new(self.model.lock().unwrap().clone()),
            switch_to: self.switch_to.clone(),
        })
    }
}

#[tokio::test]
async fn run_turn_streaming_mpsc_emits_model_changed_on_midstream_switch() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(MidStreamModelSwitchProvider {
        model: std::sync::Mutex::new("claude-fable-5".to_string()),
        switch_to: "claude-opus-4-8".to_string(),
    });
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "test".to_string(),
            cache_control: None,
        }],
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move { agent.run_turn_streaming_mpsc(tx).await });

    let mut switched_model = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            Ok(Some(ServerEvent::ModelChanged { model, error, .. })) => {
                assert!(error.is_none(), "unexpected model-change error: {error:?}");
                switched_model = Some(model);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                if task.is_finished() {
                    break;
                }
            }
        }
    }

    task.await.unwrap().unwrap();
    assert_eq!(
        switched_model.as_deref(),
        Some("claude-opus-4-8"),
        "expected a ModelChanged event resyncing to the served model"
    );
}

#[tokio::test]
async fn messages_for_provider_replays_persisted_native_compaction_in_auto_mode() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
    );

    agent
        .apply_openai_native_compaction("enc_auto".to_string(), 1)
        .expect("persist native compaction");

    let (messages, event) = agent.messages_for_provider();
    assert!(event.is_none());
    assert!(!messages.is_empty());
    match &messages[0].content[0] {
        ContentBlock::OpenAICompaction { encrypted_content } => {
            assert_eq!(encrypted_content, "enc_auto");
        }
        other => panic!("expected OpenAI compaction block, got {other:?}"),
    }
    assert!(
        messages
            .iter()
            .any(|message| message.role == Role::Assistant)
    );
}

#[tokio::test]
async fn oversized_openai_native_compaction_is_persisted_as_text_fallback() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
    );
    agent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
    );

    let oversized =
        "x".repeat(crate::provider::openai_request::OPENAI_ENCRYPTED_CONTENT_SAFE_MAX_CHARS + 1);
    agent
        .apply_openai_native_compaction(oversized, 1)
        .expect("persist fallback compaction");

    let state = agent
        .session
        .compaction
        .as_ref()
        .expect("compaction should be persisted");
    assert!(state.openai_encrypted_content.is_none());
    assert!(
        state
            .summary_text
            .contains("OpenAI native compaction state was discarded")
    );

    let (messages, event) = agent.messages_for_provider();
    assert!(event.is_none());
    assert!(!messages.is_empty());
    assert!(messages.iter().all(|message| {
        message
            .content
            .iter()
            .all(|block| !matches!(block, ContentBlock::OpenAICompaction { .. }))
    }));
    match &messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("OpenAI native compaction state was discarded"));
        }
        other => panic!("expected text fallback summary, got {other:?}"),
    }
    assert!(
        messages
            .iter()
            .any(|message| message.role == Role::Assistant)
    );
}

#[tokio::test]
async fn messages_for_provider_applies_manual_compaction_in_native_auto_mode() {
    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    for i in 0..30 {
        agent.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: format!("turn {i} {}", "x".repeat(120)),
                cache_control: None,
            }],
        );
    }

    agent.provider_session_id = Some("stale-provider-session".to_string());
    agent.session.provider_session_id = Some("stale-provider-session".to_string());

    let provider_messages = agent.provider_messages();
    let (message, success) = agent.request_manual_compaction();
    assert!(success, "manual compaction should start: {message}");

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut event = None;
    let mut compacted_messages = Vec::new();
    while Instant::now() < deadline {
        let (messages, maybe_event) = agent.messages_for_provider();
        if maybe_event.is_some() {
            event = maybe_event;
            compacted_messages = messages;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let event = event.expect("manual compaction event should be applied");
    assert_eq!(event.trigger, "manual");
    assert!(agent.session.compaction.is_some());
    assert!(agent.provider_session_id.is_none());
    assert!(agent.session.provider_session_id.is_none());
    assert!(compacted_messages.len() < provider_messages.len());
    match &compacted_messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("manual summary from native-auto provider"));
        }
        other => panic!("expected text summary block, got {other:?}"),
    }
}

#[tokio::test]
async fn rewind_undo_restores_exact_compaction_state() {
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
    let compaction = crate::session::StoredCompactionState {
        summary_text: "exact pre-rewind summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };
    agent.session.compaction = Some(compaction.clone());
    agent.seed_compaction_from_session();

    agent.rewind_to_message(1).expect("rewind should succeed");
    assert!(agent.session.compaction.is_none());
    assert!(
        agent
            .registry
            .compaction()
            .read()
            .await
            .persisted_state()
            .is_none()
    );

    agent.undo_rewind().expect("undo should succeed");
    assert_eq!(agent.session.compaction, Some(compaction.clone()));
    assert_eq!(
        agent.registry.compaction().read().await.persisted_state(),
        Some(compaction)
    );
}
