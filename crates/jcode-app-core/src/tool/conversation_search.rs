#![cfg_attr(test, allow(clippy::await_holding_lock))]

//! Conversation search tool - RAG for compacted conversation history

use super::{Tool, ToolContext, ToolOutput};
use crate::compaction::CompactionManager;
use crate::message::{Message, Role};
use crate::session::Session;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::RwLock;

const MAX_SEARCH_MATCHES: usize = 10;
const MAX_TURN_RANGE: usize = 50;
const MAX_OUTPUT_CHARS: usize = 24_000;

#[derive(Debug, Deserialize)]
struct SearchInput {
    /// Search query (keyword search)
    #[serde(default)]
    query: Option<String>,

    /// Current session or an explicitly authorized ancestor session.
    #[serde(default)]
    source_session: Option<String>,

    /// Get specific turns by range
    #[serde(default)]
    turns: Option<TurnRange>,

    /// Retrieve an inclusive durable message-ID range.
    #[serde(default)]
    message_range: Option<MessageRange>,

    /// Character offset used for exact, redacted tool-payload continuation.
    #[serde(default)]
    payload_offset: Option<usize>,

    /// Character count used for exact, redacted tool-payload continuation.
    #[serde(default)]
    payload_limit: Option<usize>,

    /// Character offset into the complete rendered range response.
    #[serde(default)]
    response_offset: Option<usize>,

    /// Character count for deterministic rendered range pagination.
    #[serde(default)]
    response_limit: Option<usize>,

    /// Get stats about conversation
    #[serde(default)]
    stats: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TurnRange {
    start: usize,
    end: usize,
}

#[derive(Debug, Deserialize)]
struct MessageRange {
    start: String,
    end: String,
}

pub struct ConversationSearchTool {
    compaction: Arc<RwLock<CompactionManager>>,
}

impl ConversationSearchTool {
    pub fn new(compaction: Arc<RwLock<CompactionManager>>) -> Self {
        Self { compaction }
    }
}

#[async_trait]
impl Tool for ConversationSearchTool {
    fn name(&self) -> &str {
        "conversation_search"
    }

    fn description(&self) -> &str {
        "Search conversation history."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "query": {
                    "type": "string",
                    "description": "Search query."
                },
                "source_session": {
                    "type": "string",
                    "description": "Current session or a recorded ancestor session ID."
                },
                "turns": {
                    "type": "object",
                    "properties": {
                        "start": {"type": "integer", "description": "Start turn."},
                        "end": {"type": "integer", "description": "End turn."}
                    },
                    "required": ["start", "end"],
                    "description": "Turn range."
                },
                "message_range": {
                    "type": "object",
                    "properties": {
                        "start": {"type": "string", "description": "First durable message ID (inclusive)."},
                        "end": {"type": "string", "description": "Last durable message ID (inclusive)."}
                    },
                    "required": ["start", "end"],
                    "description": "Durable message-ID range, matching LCM retrieval anchors."
                },
                "payload_offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional character offset for redacted tool input/result continuation."
                },
                "payload_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 8000,
                    "description": "Optional character count for redacted tool input/result continuation."
                },
                "response_offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Character offset into the complete rendered turns/message_range response."
                },
                "response_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 23000,
                    "description": "Character count for deterministic ordinary-content range continuation."
                },
                "stats": {
                    "type": "boolean",
                    "description": "Return stats."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: SearchInput = serde_json::from_value(input)?;
        let (stats, engine) = {
            let manager = self.compaction.read().await;
            (manager.stats(), manager.engine())
        };
        let source_session_id = params
            .source_session
            .as_deref()
            .unwrap_or(&ctx.session_id)
            .to_string();
        if source_session_id != ctx.session_id
            && !is_recorded_ancestor(&ctx.session_id, &source_session_id)
        {
            anyhow::bail!(
                "conversation_search source session is not in the current session's recorded parent chain"
            );
        }
        let loaded_session = Session::load(&source_session_id).ok();
        if loaded_session.is_none() {
            crate::logging::warn(&format!(
                "[tool:conversation_search] failed to load session history for session {}",
                source_session_id
            ));
        }

        let mut output = String::new();

        // Handle stats request
        if params.stats == Some(true) {
            let frontier = loaded_session
                .as_ref()
                .and_then(|session| session.context_frontier.as_ref());
            let max_level = loaded_session
                .as_ref()
                .and_then(|session| session.context_nodes.iter().map(|node| node.level).max())
                .unwrap_or(0);
            let is_current_session = source_session_id == ctx.session_id;
            let covered_messages = frontier.map_or(0, |frontier| frontier.covered_message_count);
            let snapshot_turns = loaded_session
                .as_ref()
                .map_or(0, |session| session.messages.len());
            let snapshot_active = snapshot_turns.saturating_sub(covered_messages);
            let snapshot_tokens = loaded_session.as_ref().map_or(0, |session| {
                session
                    .messages
                    .iter()
                    .map(|message| crate::compaction::content_char_count(&message.content))
                    .sum::<usize>()
                    .div_ceil(crate::compaction::CHARS_PER_TOKEN)
            });
            let total_turns = if is_current_session {
                stats.total_turns
            } else {
                snapshot_turns
            };
            let active_messages = if is_current_session {
                stats.active_messages
            } else {
                snapshot_active
            };
            let has_summary = if is_current_session {
                stats.has_summary
            } else {
                frontier.is_some()
                    || loaded_session
                        .as_ref()
                        .is_some_and(|session| session.compaction.is_some())
            };
            let estimated_tokens = if is_current_session {
                stats.token_estimate
            } else {
                snapshot_tokens
            };
            let context_usage = if is_current_session {
                format!("{:.1}%", stats.context_usage * 100.0)
            } else {
                "n/a for ancestor snapshot".to_string()
            };
            output.push_str(&format!(
                "## Conversation Stats\n\n\
                 - Source session: {}\n\
                 - Engine: {}\n\
                 - Total turns: {}\n\
                 - Active messages in context: {}\n\
                 - Has summary: {}\n\
                 - Compaction in progress: {}\n\
                 - Estimated tokens: {}\n\
                 - Context usage: {}\n\
                 - Frontier nodes: {}\n\
                 - Maximum node level: {}\n\
                 - Covered canonical messages: {}\n\
                 - Fresh raw tail: {}\n",
                source_session_id,
                engine.as_str(),
                total_turns,
                active_messages,
                has_summary,
                is_current_session && stats.is_compacting,
                estimated_tokens,
                context_usage,
                frontier.map_or(0, |frontier| frontier.active_node_ids.len()),
                max_level,
                covered_messages,
                loaded_session.as_ref().map_or(0, |session| {
                    session.messages.len().saturating_sub(
                        frontier.map_or(0, |frontier| frontier.covered_message_count),
                    )
                })
            ));
        }

        // Handle keyword search
        if let Some(query) = params.query {
            let results = loaded_session
                .as_ref()
                .map(|session| search_messages(&session.messages, &query))
                .unwrap_or_default();

            if results.is_empty() {
                output.push_str(&format!(
                    "## Search Results\n\nNo results found for '{}'\n",
                    query
                ));
            } else {
                output.push_str(&format!(
                    "## Search Results for '{}'\n\nFound {} matches:\n\n",
                    query,
                    results.len()
                ));

                for result in results.iter().take(MAX_SEARCH_MATCHES) {
                    let role = match result.role {
                        Role::User => "User",
                        Role::Assistant => "Assistant",
                    };
                    output.push_str(&format!(
                        "**{} · Turn {} · Message {} ({}):**\n{}\n\n",
                        source_session_id, result.turn, result.message_id, role, result.snippet
                    ));
                }

                if results.len() > MAX_SEARCH_MATCHES {
                    crate::logging::warn(&format!(
                        "[tool:conversation_search] truncating displayed search results for session {} query={} total_results={}",
                        ctx.session_id,
                        query,
                        results.len()
                    ));
                    output.push_str(&format!(
                        "... additional matches omitted after the {}-match cap\n",
                        MAX_SEARCH_MATCHES
                    ));
                }
            }
        }

        if params.turns.is_some() && params.message_range.is_some() {
            anyhow::bail!("provide either turns or message_range, not both");
        }
        let exact_message_range = params.message_range.is_some();
        let has_range = params.turns.is_some() || exact_message_range;

        // Handle numeric or durable message-ID range request.
        let resolved_range = if let Some(range) = params.turns {
            Some((
                range.start,
                range.end,
                format!("Turns {}-{}", range.start, range.end),
            ))
        } else if let Some(range) = params.message_range {
            let session = loaded_session.as_ref().ok_or_else(|| {
                anyhow::anyhow!("conversation source session could not be loaded")
            })?;
            let start = session
                .messages
                .iter()
                .position(|message| message.id == range.start)
                .ok_or_else(|| anyhow::anyhow!("message_range start ID was not found"))?;
            let end = session
                .messages
                .iter()
                .position(|message| message.id == range.end)
                .ok_or_else(|| anyhow::anyhow!("message_range end ID was not found"))?;
            if end < start {
                anyhow::bail!("message_range end precedes start");
            }
            Some((
                start,
                end.saturating_add(1),
                format!("Messages {}..{}", range.start, range.end),
            ))
        } else {
            None
        };
        if let Some((range_start, range_end, range_label)) = resolved_range {
            let available_end = loaded_session
                .as_ref()
                .map_or(range_end, |session| range_end.min(session.messages.len()));
            let capped_end = available_end.min(range_start.saturating_add(MAX_TURN_RANGE));
            let turns = loaded_session.as_ref().map(|session| {
                session
                    .messages
                    .iter()
                    .skip(range_start)
                    .take(capped_end.saturating_sub(range_start))
                    .collect::<Vec<_>>()
            });

            if turns.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
                output.push_str(&format!(
                    "## {} · source {}\n\nNo turns found in that range.\n",
                    range_label, source_session_id
                ));
            } else if let Some(turns) = turns {
                output.push_str(&format!(
                    "## {} · source {}\n\n",
                    range_label, source_session_id
                ));

                for (idx, msg) in turns.iter().enumerate() {
                    let turn_num = range_start + idx;
                    let role = match msg.role {
                        Role::User => "User",
                        Role::Assistant => "Assistant",
                    };

                    output.push_str(&format!(
                        "**Turn {} · Message {} ({}):**\n",
                        turn_num, msg.id, role
                    ));

                    for block in &msg.content {
                        match block {
                            crate::message::ContentBlock::Text { text, .. } => {
                                let text = crate::message::redact_secrets(text);
                                if exact_message_range {
                                    output.push_str(&text);
                                    output.push('\n');
                                } else if text.len() > 1000 {
                                    output.push_str(crate::util::truncate_str(&text, 1000));
                                    output.push_str("... (truncated)\n");
                                } else {
                                    output.push_str(&text);
                                    output.push('\n');
                                }
                            }
                            crate::message::ContentBlock::ToolUse {
                                id, name, input, ..
                            } => {
                                let input = if exact_message_range
                                    && params.payload_offset.is_none()
                                    && params.payload_limit.is_none()
                                {
                                    crate::message::redact_secrets(&input.to_string())
                                } else {
                                    render_redacted_tool_payload(
                                        &input.to_string(),
                                        1_000,
                                        params.payload_offset,
                                        params.payload_limit,
                                    )
                                };
                                output.push_str(&format!(
                                    "[Tool call: id={id} name={name} input={input}]\n"
                                ));
                            }
                            crate::message::ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => {
                                let status = match is_error {
                                    Some(true) => "error",
                                    Some(false) => "success",
                                    None => "unknown",
                                };
                                let preview = if exact_message_range
                                    && params.payload_offset.is_none()
                                    && params.payload_limit.is_none()
                                {
                                    crate::message::redact_secrets(content)
                                } else {
                                    render_redacted_tool_payload(
                                        content,
                                        400,
                                        params.payload_offset,
                                        params.payload_limit,
                                    )
                                };
                                output.push_str(&format!(
                                    "[Tool result: tool_use_id={tool_use_id} status={status} content={preview}]\n"
                                ));
                            }
                            crate::message::ContentBlock::Reasoning { .. }
                            | crate::message::ContentBlock::ReasoningTrace { .. }
                            | crate::message::ContentBlock::AnthropicThinking { .. }
                            | crate::message::ContentBlock::OpenAIReasoning { .. } => {}
                            crate::message::ContentBlock::Image { .. } => {
                                output.push_str("[Image]\n");
                            }
                            crate::message::ContentBlock::OpenAICompaction { .. } => {
                                output.push_str("[OpenAI native compaction]\n");
                            }
                        }
                    }
                    output.push('\n');
                }
                if capped_end < available_end
                    && let Some(session) = loaded_session.as_ref()
                {
                    if exact_message_range {
                        output.push_str(&format!(
                            "[message range capped at {MAX_TURN_RANGE} messages; continue with message_range.start={} message_range.end={} and response_offset=0]\n",
                            session.messages[capped_end].id,
                            session.messages[available_end - 1].id,
                        ));
                    } else {
                        output.push_str(&format!(
                            "[turn range capped at {MAX_TURN_RANGE} messages; continue with turns.start={capped_end} turns.end={available_end} and response_offset=0]\n"
                        ));
                    }
                }
            }
        }

        if output.is_empty() {
            output = "Please provide a 'query' to search, 'turns' range to retrieve, \
                      or 'stats': true to see conversation statistics."
                .to_string();
        }

        if has_range
            && (params.response_offset.is_some()
                || params.response_limit.is_some()
                || output.len() > MAX_OUTPUT_CHARS)
        {
            output = paginate_range_output(
                &output,
                params.response_offset.unwrap_or(0),
                params.response_limit,
            );
        } else if output.len() > MAX_OUTPUT_CHARS {
            output = format!(
                "{}\n\n... output truncated at {} characters",
                crate::util::truncate_str(&output, MAX_OUTPUT_CHARS),
                MAX_OUTPUT_CHARS
            );
        }

        Ok(ToolOutput::new(output).with_title("conversation_search"))
    }
}

/// Search result from conversation history
struct SearchResult {
    turn: usize,
    message_id: String,
    role: Role,
    snippet: String,
}

fn is_recorded_ancestor(session_id: &str, requested_ancestor: &str) -> bool {
    let mut current = session_id.to_string();
    for _ in 0..64 {
        let Ok(session) = Session::load(&current) else {
            return false;
        };
        let Some(parent_id) = session.parent_id else {
            return false;
        };
        if parent_id == requested_ancestor {
            return true;
        }
        current = parent_id;
    }
    false
}

fn search_messages(messages: &[crate::session::StoredMessage], query: &str) -> Vec<SearchResult> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    for (idx, msg) in messages.iter().enumerate() {
        let message = msg.to_message();
        let text = message_to_text(&message);
        if text.to_lowercase().contains(&query_lower) {
            let snippet = extract_snippet(&text, &query_lower);
            results.push(SearchResult {
                turn: idx,
                message_id: msg.id.clone(),
                role: msg.role.clone(),
                snippet,
            });
            if results.len() > MAX_SEARCH_MATCHES {
                break;
            }
        }
    }

    results
}

fn message_to_text(msg: &Message) -> String {
    msg.content
        .iter()
        .filter_map(|block| match block {
            crate::message::ContentBlock::Text { text, .. } => Some(text.clone()),
            crate::message::ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            crate::message::ContentBlock::ToolUse {
                id, name, input, ..
            } => Some(format!("tool_call_id={id} name={name} input={input}")),
            crate::message::ContentBlock::OpenAICompaction { .. } => {
                Some("[OpenAI native compaction]".to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_snippet(text: &str, query: &str) -> String {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.find(query) {
        let mut start = if text.starts_with("tool_call_id=") {
            0
        } else {
            pos.saturating_sub(50).min(text.len())
        };
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = pos
            .saturating_add(query.len())
            .saturating_add(50)
            .min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut snippet = text[start..end].to_string();
        if start > 0 {
            snippet = format!("...{}", snippet);
        }
        if end < text.len() {
            snippet = format!("{}...", snippet);
        }
        crate::message::redact_secrets(&snippet)
    } else {
        crate::message::redact_secrets(&text.chars().take(100).collect::<String>())
    }
}

fn bounded_redacted_tool_payload(text: &str, max_chars: usize) -> String {
    let redacted = crate::message::redact_secrets(text);
    let char_count = redacted.chars().count();
    if char_count <= max_chars {
        return redacted;
    }
    let head_chars = max_chars / 2;
    let tail_chars = max_chars.saturating_sub(head_chars);
    let head = redacted.chars().take(head_chars).collect::<String>();
    let tail = redacted
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{head}... [middle omitted; retrieve canonical source for full payload] ...{tail}")
}

fn render_redacted_tool_payload(
    text: &str,
    preview_chars: usize,
    offset: Option<usize>,
    limit: Option<usize>,
) -> String {
    if offset.is_none() && limit.is_none() {
        return bounded_redacted_tool_payload(text, preview_chars);
    }
    let redacted = crate::message::redact_secrets(text);
    let total = redacted.chars().count();
    let start = offset.unwrap_or(0).min(total);
    let requested = limit.unwrap_or(4_000).clamp(1, 8_000);
    let slice = redacted
        .chars()
        .skip(start)
        .take(requested)
        .collect::<String>();
    let end = start.saturating_add(slice.chars().count());
    if end < total {
        format!(
            "{slice} [redacted canonical payload chars {start}..{end} of {total}; continue with payload_offset={end}]"
        )
    } else {
        format!("{slice} [redacted canonical payload chars {start}..{end} of {total}; complete]")
    }
}

fn paginate_range_output(text: &str, offset: usize, limit: Option<usize>) -> String {
    let total = text.chars().count();
    let start = offset.min(total);
    let requested = limit.unwrap_or(23_000).clamp(1, 23_000);
    let page = text.chars().skip(start).take(requested).collect::<String>();
    let end = start.saturating_add(page.chars().count());
    let continuation = if end < total {
        format!("continue with response_offset={end}")
    } else {
        "complete".to_string()
    };
    format!(
        "## Exact range response page\n\n- Characters: {start}..{end} of {total}\n- Continuation: {continuation}\n\n{page}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::CompactionManager;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn create_test_tool() -> ConversationSearchTool {
        let manager = Arc::new(RwLock::new(CompactionManager::new()));
        ConversationSearchTool::new(manager)
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::storage::lock_test_env()
    }

    fn setup_session(messages: Vec<Message>) -> (ToolContext, std::path::PathBuf, Option<String>) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("jcode-test-{}", nonce));
        let _ = std::fs::create_dir_all(base.join("sessions"));

        let previous_home = std::env::var("JCODE_HOME").ok();
        crate::env::set_var("JCODE_HOME", &base);

        let session_id = format!("test-session-{}", nonce);
        let mut session = Session::create_with_id(session_id.clone(), None, None);
        for msg in messages {
            session.add_message(msg.role.clone(), msg.content.clone());
        }
        session.save().unwrap();

        let ctx = ToolContext {
            session_id,
            message_id: "test-message".to_string(),
            tool_call_id: "test-tool-call".to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        };

        (ctx, base, previous_home)
    }

    fn restore_env(base: std::path::PathBuf, previous_home: Option<String>) {
        if let Some(prev) = previous_home {
            crate::env::set_var("JCODE_HOME", prev);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn test_tool_name() {
        let tool = create_test_tool();
        assert_eq!(tool.name(), "conversation_search");
    }

    #[test]
    fn snippets_are_unicode_safe_and_secret_redacted() {
        let text = format!(
            "{} needle OPENAI_API_KEY=sk-test-super-secret-value",
            "🦀".repeat(30)
        );
        let snippet = extract_snippet(&text, "needle");
        assert!(snippet.contains("needle"));
        assert!(!snippet.contains("sk-test-super-secret-value"));
        assert!(snippet.contains("[REDACTED_SECRET]"));
    }

    #[tokio::test]
    async fn test_stats() {
        let _guard = env_lock();
        let tool = create_test_tool();
        let (ctx, base, previous_home) = setup_session(Vec::new());
        let input = json!({"stats": true});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("Conversation Stats"));
        assert!(result.output.contains("Total turns"));
        restore_env(base, previous_home);
    }

    #[tokio::test]
    async fn ancestor_search_requires_recorded_parent_lineage_and_returns_message_ids() {
        let _guard = env_lock();
        let base = tempfile::tempdir().unwrap();
        let previous_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", base.path());
        let mut parent = Session::create_with_id("search-parent".to_string(), None, None);
        parent.add_message(
            Role::User,
            vec![crate::message::ContentBlock::Text {
                text: "PLANTED_ANCESTOR_DECISION".to_string(),
                cache_control: None,
            }],
        );
        let parent_message_id = parent.messages[0].id.clone();
        parent.save().unwrap();
        let mut child =
            Session::create_with_id("search-child".to_string(), Some(parent.id.clone()), None);
        child.save().unwrap();
        let mut unrelated = Session::create_with_id("search-unrelated".to_string(), None, None);
        unrelated.save().unwrap();
        let ctx = ToolContext {
            session_id: child.id.clone(),
            message_id: "message".to_string(),
            tool_call_id: "tool".to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        };
        let tool = create_test_tool();

        let result = tool
            .execute(
                json!({"query": "PLANTED_ANCESTOR", "source_session": parent.id}),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert!(result.output.contains("search-parent"));
        assert!(result.output.contains(&parent_message_id));
        assert!(
            tool.execute(
                json!({"query": "anything", "source_session": unrelated.id}),
                ctx,
            )
            .await
            .is_err()
        );

        if let Some(previous) = previous_home {
            crate::env::set_var("JCODE_HOME", previous);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    #[tokio::test]
    async fn tool_transactions_are_exactly_searchable_and_range_retrievable() {
        let _guard = env_lock();
        let tool = create_test_tool();
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![crate::message::ContentBlock::ToolUse {
                    id: "call-exact-17".to_string(),
                    name: "read".to_string(),
                    input: json!({
                        "file_path": "src/lib.rs",
                        "OPENAI_API_KEY": "sk-secret-that-must-not-return"
                    }),
                    thought_signature: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
            Message {
                role: Role::User,
                content: vec![crate::message::ContentBlock::ToolResult {
                    tool_use_id: "call-exact-17".to_string(),
                    content: format!(
                        "{}line 17: exact durable middle output{}",
                        "a".repeat(600),
                        "z".repeat(600)
                    ),
                    is_error: Some(false),
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
        ];
        let (ctx, base, previous_home) = setup_session(messages);
        let stored = Session::load(&ctx.session_id).unwrap();
        let first_message_id = stored.messages[0].id.clone();
        let result_message_id = stored.messages[1].id.clone();

        let search = tool
            .execute(json!({"query": "src/lib.rs"}), ctx.clone())
            .await
            .unwrap();
        assert!(search.output.contains("call-exact-17"));
        assert!(search.output.contains("src/lib.rs"));
        assert!(!search.output.contains("sk-secret-that-must-not-return"));

        let range = tool
            .execute(json!({"turns": {"start": 0, "end": 2}}), ctx.clone())
            .await
            .unwrap();
        assert!(range.output.contains("id=call-exact-17"));
        assert!(range.output.contains("tool_use_id=call-exact-17"));
        assert!(range.output.contains("src/lib.rs"));
        assert!(range.output.contains("status=success"));
        assert!(!range.output.contains("exact durable middle output"));
        assert!(!range.output.contains("sk-secret-that-must-not-return"));

        let anchored = tool
            .execute(
                json!({
                    "message_range": {"start": first_message_id, "end": result_message_id},
                    "payload_offset": 590,
                    "payload_limit": 100
                }),
                ctx,
            )
            .await
            .unwrap();
        assert!(anchored.output.contains("exact durable middle output"));
        assert!(anchored.output.contains("continue with payload_offset="));
        assert!(anchored.output.contains("status=success"));
        restore_env(base, previous_home);
    }

    #[tokio::test]
    async fn exact_message_ranges_continue_ordinary_content_and_message_caps() {
        let _guard = env_lock();
        let tool = create_test_tool();
        let long_text = format!("ordinary-start-{}-ordinary-tail", "x".repeat(30_000));
        let mut messages = vec![Message::user(&long_text)];
        messages.extend((1..=50).map(|index| Message::user(&format!("message-{index}"))));
        let (ctx, base, previous_home) = setup_session(messages);
        let stored = Session::load(&ctx.session_id).unwrap();
        let first = stored.messages[0].id.clone();
        let last = stored.messages[50].id.clone();

        let first_page = tool
            .execute(
                json!({
                    "message_range": {"start": first, "end": last},
                    "response_limit": 12000
                }),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert!(first_page.output.contains("ordinary-start-"));
        assert!(
            first_page
                .output
                .contains("Continuation: continue with response_offset=12000")
        );

        let final_page = tool
            .execute(
                json!({
                    "message_range": {
                        "start": stored.messages[0].id,
                        "end": stored.messages[50].id
                    },
                    "response_offset": 24000,
                    "response_limit": 12000
                }),
                ctx.clone(),
            )
            .await
            .unwrap();
        assert!(final_page.output.contains("ordinary-tail"));
        assert!(
            final_page
                .output
                .contains("message range capped at 50 messages")
        );
        assert!(final_page.output.contains(&stored.messages[50].id));
        assert!(!final_page.output.contains("output truncated at"));

        let oversized_numeric = tool
            .execute(json!({"turns": {"start": 0, "end": 999999}}), ctx)
            .await
            .unwrap();
        assert!(
            oversized_numeric
                .output
                .contains("turns.start=50 turns.end=51")
        );
        restore_env(base, previous_home);
    }

    #[tokio::test]
    async fn test_empty_search() {
        let _guard = env_lock();
        let tool = create_test_tool();
        let (ctx, base, previous_home) = setup_session(Vec::new());
        let input = json!({"query": "nonexistent"});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("No results found"));
        restore_env(base, previous_home);
    }

    #[tokio::test]
    async fn test_empty_turns() {
        let _guard = env_lock();
        let tool = create_test_tool();
        let (ctx, base, previous_home) = setup_session(Vec::new());
        let input = json!({"turns": {"start": 0, "end": 5}});

        let result = tool.execute(input, ctx).await.unwrap();
        assert!(result.output.contains("No turns found"));
        restore_env(base, previous_home);
    }
}
