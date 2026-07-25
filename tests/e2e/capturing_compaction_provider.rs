use anyhow::Result;
use async_trait::async_trait;
use futures::stream;
use jcode::message::{ContentBlock, Message, StreamEvent, ToolDefinition};
use jcode::provider::{EventStream, Provider};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct CapturingCompactionProvider {
    captured_messages: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl CapturingCompactionProvider {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn captured_messages(&self) -> Arc<Mutex<Vec<Vec<Message>>>> {
        Arc::clone(&self.captured_messages)
    }
}

#[async_trait]
impl Provider for CapturingCompactionProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.captured_messages
            .lock()
            .unwrap()
            .push(messages.to_vec());

        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta("compaction-ok".to_string())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            }),
        ])))
    }

    fn name(&self) -> &str {
        "capturing-compaction"
    }

    fn model(&self) -> String {
        "capturing-compaction-model".to_string()
    }

    fn exact_runtime_identity(&self) -> Option<jcode::provider::ExactRuntimeIdentity> {
        Some(crate::mock_provider::test_runtime_identity(
            self.name(),
            &self.model(),
        ))
    }

    fn supports_compaction(&self) -> bool {
        true
    }

    fn context_window(&self) -> usize {
        1_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

pub(crate) fn flatten_text_blocks(message: &Message) -> String {
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
