use chrono::{DateTime, Utc};
use jcode_message_types::{ContentBlock, Message, Role, ToolCall};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Identifies a session to resume, across the agent backends jcode can import
/// from. This is pure data (only ids/paths) with no UI dependency; it lives in
/// `jcode-session-types` so the foundation/import layer can match on it without
/// depending on any `jcode-tui-*` crate. The session-picker UI re-exports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeTarget {
    JcodeSession {
        session_id: String,
    },
    ClaudeCodeSession {
        session_id: String,
        session_path: String,
    },
    CodexSession {
        session_id: String,
        session_path: String,
    },
    PiSession {
        session_path: String,
    },
    OpenCodeSession {
        session_id: String,
        session_path: String,
    },
    CursorSession {
        session_id: String,
        session_path: String,
    },
}

impl ResumeTarget {
    pub fn stable_id(&self) -> &str {
        match self {
            Self::JcodeSession { session_id } => session_id,
            Self::ClaudeCodeSession { session_id, .. } => session_id,
            Self::CodexSession { session_id, .. } => session_id,
            Self::PiSession { session_path } => session_path,
            Self::OpenCodeSession { session_id, .. } => session_id,
            Self::CursorSession { session_id, .. } => session_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedMessage {
    pub role: String,
    pub content: String,
    pub tool_calls: Vec<String>,
    pub tool_data: Option<ToolCall>,
    /// Index of the stored session message this rendered message came from.
    /// `None` for synthetic UI-only messages (e.g. the compacted-history
    /// notice). Used to map user-facing rewind targets back to the stored
    /// transcript (issue #432).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_index: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedCompactedHistoryInfo {
    /// Number of compacted historical messages that can render visibly in the UI.
    /// Hidden internal reminders are excluded from this count.
    pub total_messages: usize,
    /// Number of renderable compacted historical messages included in this payload.
    pub visible_messages: usize,
    /// Number of older renderable compacted historical messages still hidden.
    pub remaining_messages: usize,
    /// Number of user prompts (turns) that are hidden before the first rendered
    /// message. Used to keep prompt numbering correct when older history is
    /// truncated (e.g. the first visible prompt is really the 5th, not the 1st).
    #[serde(default)]
    pub hidden_user_prompts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderedImageSource {
    UserInput,
    ToolResult { tool_name: String },
    Other { role: String },
}

/// Where an image belongs in the transcript flow. Used by UIs to render the
/// image inline at the message that produced it instead of appending it at the
/// bottom of the transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderedImageAnchor {
    /// The image came from the tool result for this tool call id.
    ToolCall { id: String },
    /// The image was attached to the nth (0-based) user prompt in the rendered
    /// transcript.
    UserPrompt { ordinal: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenderedImage {
    pub media_type: String,
    pub data: String,
    pub label: Option<String>,
    pub source: RenderedImageSource,
    /// Transcript anchor identifying the message this image belongs to, so the
    /// UI can render it inline at that spot. `None` when the producer cannot
    /// anchor it (e.g. older servers); unanchored images fall back to the
    /// bottom of the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<RenderedImageAnchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum SessionStatus {
    #[default]
    Active,
    Closed,
    Crashed {
        message: Option<String>,
    },
    Reloaded,
    Compacted,
    RateLimited,
    Error {
        message: String,
    },
}

impl SessionStatus {
    pub fn display(&self) -> &'static str {
        match self {
            SessionStatus::Active => "active",
            SessionStatus::Closed => "closed",
            SessionStatus::Crashed { .. } => "crashed",
            SessionStatus::Reloaded => "reloaded",
            SessionStatus::Compacted => "compacted",
            SessionStatus::RateLimited => "rate limited",
            SessionStatus::Error { .. } => "error",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            SessionStatus::Active => "▶",
            SessionStatus::Closed => "✓",
            SessionStatus::Crashed { .. } => "💥",
            SessionStatus::Reloaded => "🔄",
            SessionStatus::Compacted => "📦",
            SessionStatus::RateLimited => "⏳",
            SessionStatus::Error { .. } => "❌",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            SessionStatus::Crashed { message } => message.as_deref(),
            SessionStatus::Error { message } => Some(message.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionImproveMode {
    #[serde(rename = "improve_run", alias = "run")]
    ImproveRun,
    #[serde(rename = "improve_plan", alias = "plan")]
    ImprovePlan,
    #[serde(rename = "refactor_run")]
    RefactorRun,
    #[serde(rename = "refactor_plan")]
    RefactorPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitState {
    pub root: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub dirty: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvSnapshot {
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub reason: String,
    pub session_id: String,
    pub working_dir: Option<String>,
    pub provider: String,
    pub model: String,
    pub jcode_version: String,
    pub jcode_git_hash: Option<String>,
    pub jcode_git_dirty: Option<bool>,
    pub os: String,
    pub arch: String,
    pub pid: u32,
    pub is_selfdev: bool,
    pub is_debug: bool,
    pub is_canary: bool,
    pub testing_build: Option<String>,
    pub working_git: Option<GitState>,
}

/// A memory injection event, stored for replay visualization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMemoryInjection {
    /// Human-readable summary (e.g., "🧠 auto-recalled 3 memories")
    pub summary: String,
    /// The recalled memory content that was injected
    pub content: String,
    /// Number of memories recalled
    pub count: u32,
    /// Stable memory IDs included in this injection, used to avoid re-injecting
    /// the same memories after session resume/reload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_ids: Vec<String>,
    /// Age of memories in milliseconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
    /// Message index this injection occurred before (for replay timing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_message: Option<usize>,
    /// Timestamp when injection occurred
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_role: Option<StoredDisplayRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<StoredTokenUsage>,
}

/// Immutable persisted summary node. Hashes are lowercase SHA-256 hex strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextNode {
    pub id: String,
    pub schema_version: u32,
    pub level: u32,
    pub source_session_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_message_ids: Vec<String>,
    pub source_sha256: String,
    /// SHA-256 of the full non-secret ExactRuntimeIdentity that owned the
    /// canonical source transcript when this immutable node was created.
    #[serde(default)]
    pub source_runtime_identity_sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_node_ids: Vec<String>,
    pub summary_text: String,
    /// Integrity proof for the portable summary text. `None` is accepted only
    /// for graph nodes written by binaries predating this additive field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_sha256: Option<String>,
    pub estimated_tokens: u64,
    pub summarizer_model: String,
    pub summarizer_provider: String,
    pub summarizer_route: String,
    /// SHA-256 of the full non-secret ExactRuntimeIdentity of the provider that
    /// generated this summary. This may differ from the source identity when an
    /// explicit compactor route is configured.
    #[serde(default)]
    pub summarizer_runtime_identity_sha256: String,
    pub prompt_schema_version: u32,
    pub created_at: DateTime<Utc>,
}

/// Small mutable pointer set into the immutable context node graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredContextFrontier {
    pub schema_version: u32,
    pub generation: u64,
    pub active_node_ids: Vec<String>,
    pub covered_message_count: usize,
    pub covered_through_message_id: Option<String>,
    pub source_prefix_sha256: String,
    pub next_node_sequence: u64,
}

/// Canonical input captured before a context graph transaction is prepared.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextGraphInputProof {
    pub schema_version: u32,
    pub source_session_id: String,
    pub source_message_ids: Vec<String>,
    pub source_sha256: String,
}

/// One append-only, generation-checked graph publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextGraphTransaction {
    pub schema_version: u32,
    pub op_id: String,
    pub base_generation: u64,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub append_context_nodes: Vec<StoredContextNode>,
    pub frontier: StoredContextFrontier,
    pub input_proof: ContextGraphInputProof,
}

/// Validate node identity, parent availability, cycles, and frontier references.
pub fn validate_context_graph(
    nodes: &[StoredContextNode],
    frontier: Option<&StoredContextFrontier>,
) -> Result<(), String> {
    use std::collections::{HashMap, HashSet};

    fn is_lower_sha256(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    let mut by_id = HashMap::new();
    for node in nodes {
        if node.schema_version != 2
            || node.prompt_schema_version == 0
            || node.id.is_empty()
            || node.source_session_id.is_empty()
            || node.source_message_ids.is_empty()
            || !is_lower_sha256(&node.source_sha256)
            || !is_lower_sha256(&node.source_runtime_identity_sha256)
            || !node.summary_sha256.as_deref().is_some_and(is_lower_sha256)
            || node.summary_text.trim().is_empty()
            || node.estimated_tokens == 0
            || node.summarizer_model.is_empty()
            || node.summarizer_provider.is_empty()
            || node.summarizer_route.is_empty()
            || !is_lower_sha256(&node.summarizer_runtime_identity_sha256)
        {
            return Err(format!("context node {} has invalid metadata", node.id));
        }
        if by_id.insert(node.id.as_str(), node).is_some() {
            return Err(format!("duplicate context node id {}", node.id));
        }
    }
    for node in nodes {
        let mut unique_children = HashSet::new();
        for child in &node.child_node_ids {
            if !unique_children.insert(child.as_str()) {
                return Err(format!(
                    "context node {} references child {child} more than once",
                    node.id
                ));
            }
            if !by_id.contains_key(child.as_str()) {
                return Err(format!(
                    "context node {} has missing child {child}",
                    node.id
                ));
            }
        }
    }

    fn visit<'a>(
        id: &'a str,
        nodes: &HashMap<&'a str, &'a StoredContextNode>,
        visiting: &mut HashSet<&'a str>,
        visited: &mut HashSet<&'a str>,
    ) -> Result<(), String> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(format!("context graph cycle at {id}"));
        }
        for child in &nodes[id].child_node_ids {
            visit(child, nodes, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for id in by_id.keys().copied() {
        visit(id, &by_id, &mut visiting, &mut visited)?;
    }
    for node in nodes {
        for child in &node.child_node_ids {
            let child_node = by_id[child.as_str()];
            if child_node.level >= node.level {
                return Err(format!(
                    "context node {} level {} must be above child {child} level {}",
                    node.id, node.level, child_node.level
                ));
            }
        }
    }
    if let Some(frontier) = frontier {
        if frontier.schema_version != 1
            || frontier.generation == 0
            || (frontier.active_node_ids.is_empty()
                && (frontier.covered_message_count != 0
                    || frontier.covered_through_message_id.is_some()))
            || (frontier.covered_message_count == 0
                && frontier.covered_through_message_id.is_some())
            || (frontier.covered_message_count > 0 && frontier.covered_through_message_id.is_none())
            || !is_lower_sha256(&frontier.source_prefix_sha256)
            || frontier.next_node_sequence == 0
        {
            return Err("context frontier has invalid metadata".to_string());
        }
        let mut unique_active = HashSet::new();
        for id in &frontier.active_node_ids {
            if !unique_active.insert(id.as_str()) {
                return Err(format!(
                    "context frontier references node {id} more than once"
                ));
            }
            if !by_id.contains_key(id.as_str()) {
                return Err(format!("context frontier references missing node {id}"));
            }
        }
        fn reject_active_descendant(
            root: &str,
            current: &str,
            nodes: &HashMap<&str, &StoredContextNode>,
            active: &HashSet<&str>,
        ) -> Result<(), String> {
            for child in &nodes[current].child_node_ids {
                if active.contains(child.as_str()) {
                    return Err(format!(
                        "context frontier contains ancestor {root} and descendant {child}"
                    ));
                }
                reject_active_descendant(root, child, nodes, active)?;
            }
            Ok(())
        }
        for id in &frontier.active_node_ids {
            reject_active_descendant(id, id, &by_id, &unique_active)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredDisplayRole {
    System,
    BackgroundTask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredCompactionState {
    pub summary_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_encrypted_content: Option<String>,
    pub covers_up_to_turn: usize,
    pub original_turn_count: usize,
    pub compacted_count: usize,
}

impl StoredMessage {
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role.clone(),
            content: self.content.clone(),
            timestamp: self.timestamp,
            tool_duration_ms: self.tool_duration_ms,
        }
    }

    /// Get a text preview of the message content
    pub fn content_preview(&self) -> String {
        for block in &self.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    // Return first non-empty text block
                    let text = text.trim();
                    if !text.is_empty() {
                        return text.replace('\n', " ");
                    }
                }
                ContentBlock::ToolUse { name, .. } => {
                    return format!("[tool: {}]", name);
                }
                ContentBlock::ToolResult { content, .. } => {
                    let preview = content.trim().replace('\n', " ");
                    if !preview.is_empty() {
                        return format!("[result: {}]", preview);
                    }
                }
                _ => {}
            }
        }
        "(empty)".to_string()
    }
}

mod session_search;

pub use session_search::*;
