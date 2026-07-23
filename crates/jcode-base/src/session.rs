use crate::id::{extract_session_name, new_id, new_memorable_session_id_avoiding};
use crate::message::{ContentBlock, Message, Role};
pub use crate::storage::{
    SessionCounts, SessionPresence, active_session_ids, find_active_session_id_by_pid,
    mark_streaming, session_counts, session_presence, unmark_streaming, user_session_counts,
    user_session_presence,
};
use crate::storage::{active_pids_dir, register_active_pid, unregister_active_pid};

/// RAII guard that marks a session as actively streaming for its lifetime.
///
/// Wraps the on-disk streaming marker from `jcode-storage` (cleared on every
/// exit path so presence UIs never show a phantom streaming session) and
/// additionally holds a macOS power assertion so the system does not
/// idle-sleep in the middle of a streaming model response.
pub struct StreamingGuard {
    _marker: crate::storage::StreamingGuard,
    #[allow(dead_code)]
    sleep_assertion: crate::platform::PowerAssertion,
}

impl StreamingGuard {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            _marker: crate::storage::StreamingGuard::new(session_id),
            sleep_assertion: crate::platform::PowerAssertion::prevent_user_idle_system_sleep(
                "Jcode streaming model response",
            ),
        }
    }
}
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
mod crash;
mod journal;
mod maintenance;
mod memory_profile;
mod model;
mod persistence;
mod render;
mod storage_paths;
pub use crash::{
    CrashedSessionsInfo, detect_crashed_sessions, find_recent_crashed_sessions,
    find_session_by_name_or_id, recover_crashed_sessions, recover_crashed_sessions_by_ids,
};
pub use jcode_session_types::{
    ContextGraphInputProof, ContextGraphTransaction, EnvSnapshot, GitState, SessionImproveMode,
    SessionStatus, StoredCompactionState, StoredContextFrontier, StoredContextNode,
    StoredDisplayRole, StoredMemoryInjection, StoredMessage, StoredTokenUsage,
    validate_context_graph,
};
use journal::{PersistVectorMode, SessionJournalMeta, SessionPersistState};
pub use maintenance::prune_old_session_backups;
pub use memory_profile::SessionMemoryProfileSnapshot;
use memory_profile::{
    ContentBlockMemoryStats, SessionMemoryProfileCache, summarize_blocks, summarize_message_content,
};
use model::SESSION_CONTEXT_PREFIX;
pub use model::{StoredReplayEvent, StoredReplayEventKind};
pub use render::{
    RenderedCompactedHistoryInfo, RenderedImage, RenderedImageAnchor, RenderedImageSource,
    RenderedMessage, has_rendered_images, is_attached_image_label_text, render_images,
    render_messages, render_messages_and_images, render_messages_and_images_with_compacted_history,
    summarize_tool_calls,
};
pub use storage_paths::session_journal_path_from_snapshot;
#[cfg(test)]
pub(crate) use storage_paths::session_path_in_dir;
use storage_paths::{estimate_json_bytes, persist_vector_mode_label};
pub use storage_paths::{session_exists, session_journal_path, session_path};

fn stored_messages_to_messages(messages: &[StoredMessage]) -> Vec<Message> {
    messages.iter().map(StoredMessage::to_message).collect()
}

fn is_internal_system_reminder_message(message: &StoredMessage) -> bool {
    message
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.trim_start()),
            _ => None,
        })
        .is_some_and(|text| text.starts_with("<system-reminder>"))
}

fn is_visible_conversation_message(message: &StoredMessage) -> bool {
    message.display_role.is_none() && !is_internal_system_reminder_message(message)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<StoredMessage>,
    /// Highest journal sequence incorporated into this in-memory session.
    #[serde(default)]
    pub journal_sequence: u64,
    /// Highest journal sequence covered by this installed snapshot.
    #[serde(default)]
    pub journal_watermark: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_nodes: Vec<StoredContextNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_frontier: Option<StoredContextFrontier>,
    /// Persisted compacted-view state so reload/resume can continue using the
    /// active summary + recent tail instead of re-sending the full transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<StoredCompactionState>,
    /// Provider-specific session ID (e.g., Claude Code CLI session for resume)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// Stable provider/profile key for session-source filtering (e.g. "openai",
    /// "opencode", "opencode-go").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<String>,
    /// Model identifier for this session (e.g., "gpt-5.2-codex")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// API method/runtime route used to select this model (e.g. "openrouter",
    /// "openai-compatible:nvidia-nim", "openai-api").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_api_method: Option<String>,
    /// Provider reasoning/thinking effort for this session (e.g., OpenAI low|medium|high|xhigh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Optional fixed model to use for subagents launched from this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_model: Option<String>,
    /// Last requested `/improve` mode for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub improve_mode: Option<SessionImproveMode>,
    /// Whether automatic end-of-turn review is enabled for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoreview_enabled: Option<bool>,
    /// Whether automatic end-of-turn judging is enabled for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autojudge_enabled: Option<bool>,
    /// Whether this session is a canary session (testing new builds)
    #[serde(default)]
    pub is_canary: bool,
    /// Build hash this session is testing (if canary)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testing_build: Option<String>,
    /// Working directory (for self-dev detection)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Memorable short name (e.g., "fox", "oak")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    /// Session exit status - why it ended (if not active)
    #[serde(default)]
    pub status: SessionStatus,
    /// PID of the process that last owned this session (for crash detection)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pid: Option<u32>,
    /// Last time the session was marked active
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<DateTime<Utc>>,
    /// Whether this is a debug/test session (created via debug socket)
    #[serde(default)]
    pub is_debug: bool,
    /// Whether this session has been saved/bookmarked by the user
    #[serde(default)]
    pub saved: bool,
    /// Optional user-provided label for saved sessions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_label: Option<String>,
    /// Environment snapshots for post-mortem debugging
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_snapshots: Vec<EnvSnapshot>,
    /// Memory injection events (for replay visualization)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_injections: Vec<StoredMemoryInjection>,
    /// Non-conversation UI/state events persisted for higher-fidelity replay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replay_events: Vec<StoredReplayEvent>,
    #[serde(skip)]
    persist_state: SessionPersistState,
    /// Receipt for the most recent accepted context transaction. This keeps an
    /// exact retry idempotent across reload without retaining every transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_context_op_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_context_op_sha256: Option<String>,
    #[serde(skip)]
    provider_messages_cache: Vec<Message>,
    #[serde(skip)]
    provider_message_prefix_hashes_cache: Vec<u64>,
    #[serde(skip)]
    provider_messages_cache_len: usize,
    #[serde(skip)]
    provider_messages_cache_mode: PersistVectorMode,
    #[serde(skip)]
    memory_profile_cache: SessionMemoryProfileCache,
    #[serde(skip)]
    memory_profile_dirty: bool,
}

#[derive(Debug, Clone)]
pub struct ContextGraphState {
    context_nodes: Vec<StoredContextNode>,
    context_frontier: Option<StoredContextFrontier>,
    last_context_op_id: Option<String>,
    last_context_op_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionStartupStub {
    id: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    custom_title: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    compaction: Option<StoredCompactionState>,
    #[serde(default)]
    provider_session_id: Option<String>,
    #[serde(default)]
    provider_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    route_api_method: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    subagent_model: Option<String>,
    #[serde(default)]
    improve_mode: Option<SessionImproveMode>,
    #[serde(default)]
    autoreview_enabled: Option<bool>,
    #[serde(default)]
    autojudge_enabled: Option<bool>,
    #[serde(default)]
    is_canary: bool,
    #[serde(default)]
    testing_build: Option<String>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    short_name: Option<String>,
    #[serde(default)]
    status: SessionStatus,
    #[serde(default)]
    last_pid: Option<u32>,
    #[serde(default)]
    last_active_at: Option<DateTime<Utc>>,
    #[serde(default)]
    is_debug: bool,
    #[serde(default)]
    saved: bool,
    #[serde(default)]
    save_label: Option<String>,
}

const MAX_SESSION_JOURNAL_BYTES: u64 = 512 * 1024;

/// Max number of environment snapshots to retain per session
const MAX_ENV_SNAPSHOTS: usize = 8;

fn current_working_dir_string() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let trimmed = v.trim();
            !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

fn default_is_test_session() -> bool {
    env_flag_enabled("JCODE_TEST_SESSION")
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn context_transaction_sha256(transaction: &ContextGraphTransaction) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec(transaction)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn stored_messages_sha256(messages: &[StoredMessage]) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec(messages)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn imported_context_root_id(
    source_session_id: &str,
    source_sha256: &str,
    summary_text: &str,
) -> String {
    let mut digest = Sha256::new();
    // Imported roots are self-contained derived projections. Their identity is
    // bound to the canonical source and portable summary, not to the first
    // transfer child, so an exact-transcript split can inherit them unchanged.
    digest.update(b"jcode-lcm-import-v2");
    digest.update(source_session_id.as_bytes());
    digest.update(source_sha256.as_bytes());
    digest.update(summary_text.as_bytes());
    format!("lcm-{:x}", digest.finalize())
}

fn validate_context_node_source_proofs(
    current_session_id: &str,
    messages: &[StoredMessage],
    nodes: &[StoredContextNode],
) -> Result<(), String> {
    for node in nodes {
        if let Some(summary_sha256) = node.summary_sha256.as_deref() {
            let actual = format!("{:x}", Sha256::digest(node.summary_text.as_bytes()));
            if summary_sha256 != actual {
                return Err(format!(
                    "context node {} portable summary proof is invalid",
                    node.id
                ));
            }
        }
        if node.level == 0 {
            if node.source_message_ids.is_empty() {
                if node.source_session_id != current_session_id {
                    continue;
                }
                return Err(format!("context leaf {} has no canonical source", node.id));
            }
            let source = node
                .source_message_ids
                .iter()
                .map(|id| {
                    messages
                        .iter()
                        .find(|message| message.id == *id)
                        .ok_or_else(|| {
                            format!("context leaf {} references missing message {id}", node.id)
                        })
                })
                .collect::<Result<Vec<_>, _>>();
            if node.source_session_id != current_session_id && source.is_err() {
                // Imported roots deliberately prove a canonical ancestor
                // transcript that is not duplicated into the transfer child.
                // Their hash and lineage remain available for authorized
                // ancestor lookup, while the text projection is self-contained.
                let expected_id = imported_context_root_id(
                    &node.source_session_id,
                    &node.source_sha256,
                    &node.summary_text,
                );
                if node.id != expected_id {
                    return Err(format!(
                        "imported context root {} has an invalid portable proof",
                        node.id
                    ));
                }
                continue;
            }
            let source = source?;
            let sha256 = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&source)
                        .map_err(|error| format!("context leaf hashing failed: {error}"))?
                )
            );
            if sha256 != node.source_sha256 {
                return Err(format!("context leaf {} source proof is invalid", node.id));
            }
            continue;
        }

        let children = node
            .child_node_ids
            .iter()
            .map(|id| {
                nodes.iter().find(|child| child.id == *id).ok_or_else(|| {
                    format!("context parent {} references missing child {id}", node.id)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let sha256 = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&children)
                    .map_err(|error| format!("context parent hashing failed: {error}"))?
            )
        );
        if sha256 != node.source_sha256 {
            return Err(format!(
                "context parent {} source proof is invalid",
                node.id
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let expected_source_ids = children
            .iter()
            .flat_map(|child| child.source_message_ids.iter().cloned())
            .filter(|id| seen.insert(id.clone()))
            .collect::<Vec<_>>();
        if node.source_message_ids != expected_source_ids {
            return Err(format!(
                "context parent {} raw source coverage is invalid",
                node.id
            ));
        }
    }
    Ok(())
}

fn validate_context_frontier_coverage(
    messages: &[StoredMessage],
    nodes: &[StoredContextNode],
    frontier: &StoredContextFrontier,
) -> Result<(), String> {
    let positions = messages
        .iter()
        .enumerate()
        .map(|(index, message)| (message.id.as_str(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<std::collections::HashMap<_, _>>();
    let mut expected = 0;
    for id in &frontier.active_node_ids {
        let mut covered = by_id[id.as_str()]
            .source_message_ids
            .iter()
            .filter_map(|message_id| positions.get(message_id.as_str()).copied())
            .collect::<Vec<_>>();
        if covered.is_empty() {
            continue; // self-contained imported ancestor root
        }
        covered.sort_unstable();
        covered.dedup();
        if covered[0] != expected
            || covered
                .iter()
                .enumerate()
                .any(|(offset, position)| *position != expected + offset)
        {
            return Err(format!(
                "context frontier node {id} is out of order, overlapping, or non-contiguous"
            ));
        }
        expected += covered.len();
    }
    if expected != frontier.covered_message_count {
        return Err(format!(
            "context frontier covers {expected} canonical messages but declares {}",
            frontier.covered_message_count
        ));
    }
    Ok(())
}

pub fn derive_session_provider_key(provider_name: &str) -> Option<String> {
    let normalized_name = provider_name.trim().to_ascii_lowercase();
    if normalized_name == "jcode" {
        return Some("jcode".to_string());
    }

    if let Ok(runtime_provider) = std::env::var("JCODE_RUNTIME_PROVIDER") {
        let runtime_provider = runtime_provider.trim().to_ascii_lowercase();
        if !runtime_provider.is_empty() && runtime_provider != "openai-compatible" {
            return Some(runtime_provider);
        }
    }

    if let Ok(namespace) = std::env::var("JCODE_OPENROUTER_CACHE_NAMESPACE") {
        let namespace = namespace.trim().to_ascii_lowercase();
        if !namespace.is_empty() {
            return Some(namespace);
        }
    }

    if let Ok(active) = std::env::var("JCODE_ACTIVE_PROVIDER") {
        let active = active.trim().to_ascii_lowercase();
        if !active.is_empty() {
            return Some(active);
        }
    }

    let fallback = match normalized_name.as_str() {
        "anthropic" | "claude" | "claude cli" => "claude",
        "openai" => "openai",
        "github copilot" | "copilot" => "copilot",
        "openrouter" => "openrouter",
        "cursor" => "cursor",
        "gemini" => "gemini",
        "antigravity" => "antigravity",
        "" => return None,
        other => other,
    };

    Some(fallback.to_string())
}

impl Session {
    pub fn context_graph_state(&self) -> ContextGraphState {
        ContextGraphState {
            context_nodes: self.context_nodes.clone(),
            context_frontier: self.context_frontier.clone(),
            last_context_op_id: self.last_context_op_id.clone(),
            last_context_op_sha256: self.last_context_op_sha256.clone(),
        }
    }

    pub fn restore_context_graph_state(&mut self, state: ContextGraphState) {
        self.context_nodes = state.context_nodes;
        self.context_frontier = state.context_frontier;
        self.last_context_op_id = state.last_context_op_id;
        self.last_context_op_sha256 = state.last_context_op_sha256;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
    }

    pub fn clear_context_graph_state(&mut self) {
        self.context_nodes.clear();
        self.context_frontier = None;
        self.last_context_op_id = None;
        self.last_context_op_sha256 = None;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
    }

    /// Retain the maximal chronological graph prefix fully covered by the first
    /// `new_len` canonical messages. Parents that straddle the rewind boundary
    /// are expanded into their immutable children before the invalid suffix is
    /// dropped. Call this before truncating `messages`.
    pub fn retain_context_graph_prefix(&mut self, new_len: usize) -> anyhow::Result<()> {
        let Some(old_frontier) = self.context_frontier.clone() else {
            self.compaction = None;
            return Ok(());
        };
        if new_len >= old_frontier.covered_message_count {
            return Ok(());
        }
        let positions = self
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| (message.id.clone(), index))
            .collect::<std::collections::HashMap<_, _>>();
        let nodes_by_id = self
            .context_nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<std::collections::HashMap<_, _>>();

        fn retain_node(
            id: &str,
            new_len: usize,
            current_session_id: &str,
            positions: &std::collections::HashMap<String, usize>,
            nodes: &std::collections::HashMap<String, StoredContextNode>,
            selected: &mut Vec<String>,
        ) -> bool {
            let node = &nodes[id];
            let local_positions = node
                .source_message_ids
                .iter()
                .filter_map(|message_id| positions.get(message_id.as_str()).copied())
                .collect::<Vec<_>>();
            let imported =
                node.source_session_id != current_session_id && local_positions.is_empty();
            if imported
                || (!local_positions.is_empty()
                    && local_positions.iter().all(|position| *position < new_len))
            {
                selected.push(node.id.clone());
                return true;
            }
            if node.child_node_ids.is_empty() {
                return false;
            }
            for child_id in &node.child_node_ids {
                if !retain_node(
                    child_id,
                    new_len,
                    current_session_id,
                    positions,
                    nodes,
                    selected,
                ) {
                    return false;
                }
            }
            false
        }

        let mut selected = Vec::new();
        for id in &old_frontier.active_node_ids {
            if !retain_node(
                id,
                new_len,
                &self.id,
                &positions,
                &nodes_by_id,
                &mut selected,
            ) {
                break;
            }
        }
        if selected.is_empty() {
            self.compaction = None;
            self.clear_context_graph_state();
            return Ok(());
        }

        let mut reachable = std::collections::HashSet::new();
        fn mark_reachable(
            id: &str,
            nodes: &std::collections::HashMap<String, StoredContextNode>,
            reachable: &mut std::collections::HashSet<String>,
        ) {
            if !reachable.insert(id.to_string()) {
                return;
            }
            for child in &nodes[id].child_node_ids {
                mark_reachable(child, nodes, reachable);
            }
        }
        for id in &selected {
            mark_reachable(id, &nodes_by_id, &mut reachable);
        }
        self.context_nodes
            .retain(|node| reachable.contains(&node.id));
        let covered = selected
            .iter()
            .filter_map(|id| nodes_by_id.get(id.as_str()))
            .flat_map(|node| node.source_message_ids.iter())
            .filter_map(|message_id| positions.get(message_id.as_str()).copied())
            .max()
            .map_or(0, |position| position + 1)
            .min(new_len);
        let projection_text = selected
            .iter()
            .filter_map(|id| nodes_by_id.get(id.as_str()))
            .enumerate()
            .map(|(index, node)| format!("[LCM context node {}]\n{}", index + 1, node.summary_text))
            .collect::<Vec<_>>()
            .join("\n\n");
        self.context_frontier = Some(StoredContextFrontier {
            schema_version: 1,
            generation: old_frontier.generation.saturating_add(1),
            active_node_ids: selected,
            covered_message_count: covered,
            covered_through_message_id: covered
                .checked_sub(1)
                .map(|index| self.messages[index].id.clone()),
            source_prefix_sha256: if covered == 0 {
                old_frontier.source_prefix_sha256
            } else {
                stored_messages_sha256(&self.messages[..covered])?
            },
            next_node_sequence: old_frontier.next_node_sequence,
        });
        self.compaction = Some(StoredCompactionState {
            summary_text: projection_text,
            openai_encrypted_content: None,
            covers_up_to_turn: covered,
            original_turn_count: covered,
            compacted_count: covered,
        });
        self.last_context_op_id = None;
        self.last_context_op_sha256 = None;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
        Ok(())
    }

    /// Inherit rebuildable LCM graph state when a lifecycle operation copies the
    /// complete canonical transcript into a new child session. Immutable node
    /// identities retain their original source-session namespace; the child only
    /// receives nodes whose proofs validate against its copied transcript.
    pub fn inherit_context_graph_from(&mut self, parent: &Session) -> anyhow::Result<()> {
        let Some(parent_frontier) = parent.context_frontier.as_ref() else {
            self.clear_context_graph_state();
            return Ok(());
        };
        if self.messages.len() != parent.messages.len()
            || stored_messages_sha256(&self.messages)? != stored_messages_sha256(&parent.messages)?
        {
            anyhow::bail!("LCM graph inheritance requires an exact canonical transcript copy");
        }

        let nodes = parent.context_nodes.clone();
        let frontier = parent_frontier.clone();
        validate_context_graph(&nodes, Some(&frontier)).map_err(anyhow::Error::msg)?;
        validate_context_node_source_proofs(&self.id, &self.messages, &nodes)
            .map_err(anyhow::Error::msg)?;
        validate_context_frontier_coverage(&self.messages, &nodes, &frontier)
            .map_err(anyhow::Error::msg)?;
        self.context_nodes = nodes;
        self.context_frontier = Some(frontier);
        self.last_context_op_id = None;
        self.last_context_op_sha256 = None;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
        Ok(())
    }

    /// Install a self-contained transfer summary while retaining an auditable
    /// proof and lineage back to the canonical parent transcript. Transfer
    /// children intentionally do not duplicate parent raw messages.
    pub fn install_imported_context_root(
        &mut self,
        parent: &Session,
        compaction: StoredCompactionState,
    ) -> anyhow::Result<()> {
        if !self.messages.is_empty() {
            anyhow::bail!("LCM imported root requires an empty child transcript");
        }
        if self.parent_id.as_deref() != Some(parent.id.as_str()) {
            anyhow::bail!("LCM imported root source must be the child's recorded parent");
        }
        if compaction.summary_text.trim().is_empty()
            || compaction.openai_encrypted_content.is_some()
        {
            anyhow::bail!("LCM imported root requires a portable text summary");
        }
        let source_sha256 = stored_messages_sha256(&parent.messages)?;
        let imported_summary = format!(
            "{}\n\n## Retrieval anchor\nUse `conversation_search` against recorded ancestor session `{}` when exact canonical parent details are needed.",
            compaction.summary_text.trim(),
            parent.id
        );
        let node_id = imported_context_root_id(&parent.id, &source_sha256, &imported_summary);
        let node = StoredContextNode {
            id: node_id.clone(),
            schema_version: 1,
            level: 0,
            source_session_id: parent.id.clone(),
            source_message_ids: parent
                .messages
                .iter()
                .map(|message| message.id.clone())
                .collect(),
            source_sha256: source_sha256.clone(),
            child_node_ids: Vec::new(),
            summary_text: imported_summary.clone(),
            summary_sha256: Some(format!("{:x}", Sha256::digest(imported_summary.as_bytes()))),
            estimated_tokens: ((imported_summary.len() + 3) / 4).max(1) as u64,
            summarizer_model: parent
                .model
                .clone()
                .unwrap_or_else(|| "inherited".to_string()),
            summarizer_provider: parent
                .provider_key
                .clone()
                .unwrap_or_else(|| "inherited".to_string()),
            summarizer_route:
                crate::provider::MultiProvider::model_switch_request_for_session_route(
                    parent.model.as_deref().unwrap_or("inherited"),
                    parent.provider_key.as_deref(),
                    parent.route_api_method.as_deref(),
                ),
            prompt_schema_version: 1,
            created_at: Utc::now(),
        };
        let frontier = StoredContextFrontier {
            schema_version: 1,
            generation: 1,
            active_node_ids: vec![node_id],
            covered_message_count: 0,
            covered_through_message_id: None,
            source_prefix_sha256: source_sha256,
            next_node_sequence: 2,
        };
        validate_context_graph(std::slice::from_ref(&node), Some(&frontier))
            .map_err(anyhow::Error::msg)?;
        validate_context_node_source_proofs(&self.id, &self.messages, std::slice::from_ref(&node))
            .map_err(anyhow::Error::msg)?;
        validate_context_frontier_coverage(&self.messages, std::slice::from_ref(&node), &frontier)
            .map_err(anyhow::Error::msg)?;
        self.compaction = Some(compaction);
        self.context_nodes = vec![node];
        self.context_frontier = Some(frontier);
        self.last_context_op_id = None;
        self.last_context_op_sha256 = None;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
        Ok(())
    }

    fn session_from_startup_stub(stub: SessionStartupStub) -> Self {
        let mut session = Self::create_with_id(stub.id, stub.parent_id, stub.title);
        session.custom_title = stub.custom_title;
        session.created_at = stub.created_at;
        session.updated_at = stub.updated_at;
        session.compaction = stub.compaction;
        session.provider_session_id = stub.provider_session_id;
        session.provider_key = stub.provider_key;
        session.model = stub.model;
        session.route_api_method = stub.route_api_method;
        session.reasoning_effort = stub.reasoning_effort;
        session.subagent_model = stub.subagent_model;
        session.improve_mode = stub.improve_mode;
        session.autoreview_enabled = stub.autoreview_enabled;
        session.autojudge_enabled = stub.autojudge_enabled;
        session.is_canary = stub.is_canary;
        session.testing_build = stub.testing_build;
        session.working_dir = stub.working_dir;
        session.short_name = stub.short_name;
        session.status = stub.status;
        session.last_pid = stub.last_pid;
        session.last_active_at = stub.last_active_at;
        session.is_debug = stub.is_debug;
        session.saved = stub.saved;
        session.save_label = stub.save_label;
        session.messages.clear();
        session.env_snapshots.clear();
        session.memory_injections.clear();
        session.replay_events.clear();
        session.rebuild_memory_profile_cache();
        session.reset_persist_state(true);
        session
    }

    fn session_from_remote_startup_snapshot(snapshot: RemoteStartupSessionSnapshot) -> Self {
        let mut session = Self::create_with_id(snapshot.id, snapshot.parent_id, snapshot.title);
        session.custom_title = snapshot.custom_title;
        session.created_at = snapshot.created_at;
        session.updated_at = snapshot.updated_at;
        session.messages = snapshot.messages;
        session.journal_sequence = snapshot.journal_sequence;
        session.journal_watermark = snapshot.journal_watermark;
        session.context_nodes = snapshot.context_nodes;
        session.context_frontier = snapshot.context_frontier;
        session.compaction = snapshot.compaction;
        session.provider_session_id = snapshot.provider_session_id;
        session.provider_key = snapshot.provider_key;
        session.model = snapshot.model;
        session.route_api_method = snapshot.route_api_method;
        session.reasoning_effort = snapshot.reasoning_effort;
        session.subagent_model = snapshot.subagent_model;
        session.improve_mode = snapshot.improve_mode;
        session.autoreview_enabled = snapshot.autoreview_enabled;
        session.autojudge_enabled = snapshot.autojudge_enabled;
        session.is_canary = snapshot.is_canary;
        session.testing_build = snapshot.testing_build;
        session.working_dir = snapshot.working_dir;
        session.short_name = snapshot.short_name;
        session.status = snapshot.status;
        session.last_pid = snapshot.last_pid;
        session.last_active_at = snapshot.last_active_at;
        session.is_debug = snapshot.is_debug;
        session.saved = snapshot.saved;
        session.save_label = snapshot.save_label;
        session.replay_events.clear();
        session.env_snapshots.clear();
        session.memory_injections.clear();
        session.mark_memory_profile_dirty();
        session.reset_persist_state(true);
        session.reset_provider_messages_cache();
        session
    }

    pub fn debug_memory_profile(&self) -> serde_json::Value {
        let message_stats =
            summarize_message_content(self.messages.iter().map(|message| &message.content));

        let session_message_json_bytes: usize = self.messages.iter().map(estimate_json_bytes).sum();
        let provider_cache_stats = summarize_message_content(
            self.provider_messages_cache
                .iter()
                .map(|message| &message.content),
        );
        let provider_messages_cache_json_bytes: usize = self
            .provider_messages_cache
            .iter()
            .map(estimate_json_bytes)
            .sum();
        let env_snapshots_json_bytes: usize =
            self.env_snapshots.iter().map(estimate_json_bytes).sum();
        let memory_injections_json_bytes: usize =
            self.memory_injections.iter().map(estimate_json_bytes).sum();
        let replay_events_json_bytes: usize =
            self.replay_events.iter().map(estimate_json_bytes).sum();
        let compaction_json_bytes = self
            .compaction
            .as_ref()
            .map(estimate_json_bytes)
            .unwrap_or(0);
        let compaction_summary_bytes = self
            .compaction
            .as_ref()
            .map(|c| c.summary_text.len())
            .unwrap_or(0);
        let compaction_encrypted_bytes = self
            .compaction
            .as_ref()
            .and_then(|c| c.openai_encrypted_content.as_ref())
            .map(|text| text.len())
            .unwrap_or(0);

        serde_json::json!({
            "session_id": self.id,
            "messages": {
                "count": self.messages.len(),
                "json_bytes": session_message_json_bytes,
                "memory": message_stats.to_json(),
            },
            "compaction": {
                "present": self.compaction.is_some(),
                "covers_up_to_turn": self
                    .compaction
                    .as_ref()
                    .map(|c| c.covers_up_to_turn)
                    .unwrap_or(0),
                "original_turn_count": self
                    .compaction
                    .as_ref()
                    .map(|c| c.original_turn_count)
                    .unwrap_or(0),
                "compacted_count": self
                    .compaction
                    .as_ref()
                    .map(|c| c.compacted_count)
                    .unwrap_or(0),
                "json_bytes": compaction_json_bytes,
                "summary_text_bytes": compaction_summary_bytes,
                "encrypted_content_bytes": compaction_encrypted_bytes,
            },
            "env_snapshots": {
                "count": self.env_snapshots.len(),
                "json_bytes": env_snapshots_json_bytes,
            },
            "memory_injections": {
                "count": self.memory_injections.len(),
                "json_bytes": memory_injections_json_bytes,
            },
            "replay_events": {
                "count": self.replay_events.len(),
                "json_bytes": replay_events_json_bytes,
            },
            "provider_messages_cache": {
                "count": self.provider_messages_cache.len(),
                "source_len": self.provider_messages_cache_len,
                "mode": persist_vector_mode_label(self.provider_messages_cache_mode),
                "json_bytes": provider_messages_cache_json_bytes,
                "memory": provider_cache_stats.to_json(),
            },
            "totals": {
                "payload_text_bytes": message_stats.payload_text_bytes(),
                "json_bytes": session_message_json_bytes
                    + provider_messages_cache_json_bytes
                    + env_snapshots_json_bytes
                    + memory_injections_json_bytes
                    + replay_events_json_bytes
                    + compaction_json_bytes,
                "canonical_transcript_json_bytes": session_message_json_bytes,
                "provider_cache_json_bytes": provider_messages_cache_json_bytes,
                "canonical_tool_result_bytes": message_stats.tool_result_bytes,
                "provider_cache_tool_result_bytes": provider_cache_stats.tool_result_bytes,
                "canonical_large_blob_bytes": message_stats.large_block_bytes,
                "provider_cache_large_blob_bytes": provider_cache_stats.large_block_bytes,
            }
        })
    }

    fn journal_meta(&self) -> SessionJournalMeta {
        SessionJournalMeta {
            parent_id: self.parent_id.clone(),
            title: self.title.clone(),
            custom_title: self.custom_title.clone(),
            updated_at: self.updated_at,
            compaction: self.compaction.clone(),
            provider_session_id: self.provider_session_id.clone(),
            provider_key: self.provider_key.clone(),
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            subagent_model: self.subagent_model.clone(),
            improve_mode: self.improve_mode,
            autoreview_enabled: self.autoreview_enabled,
            autojudge_enabled: self.autojudge_enabled,
            is_canary: self.is_canary,
            testing_build: self.testing_build.clone(),
            working_dir: self.working_dir.clone(),
            short_name: self.short_name.clone(),
            status: self.status.clone(),
            last_pid: self.last_pid,
            last_active_at: self.last_active_at,
            is_debug: self.is_debug,
            saved: self.saved,
            save_label: self.save_label.clone(),
        }
    }

    fn reset_persist_state(&mut self, snapshot_exists: bool) {
        self.persist_state = SessionPersistState {
            snapshot_exists,
            messages_len: self.messages.len(),
            env_snapshots_len: self.env_snapshots.len(),
            memory_injections_len: self.memory_injections.len(),
            replay_events_len: self.replay_events.len(),
            context_nodes_len: self.context_nodes.len(),
            context_generation: self
                .context_frontier
                .as_ref()
                .map_or(0, |frontier| frontier.generation),
            context_op_id: self.last_context_op_id.clone(),
            messages_mode: PersistVectorMode::Clean,
            env_snapshots_mode: PersistVectorMode::Clean,
            memory_injections_mode: PersistVectorMode::Clean,
            replay_events_mode: PersistVectorMode::Clean,
            pending_context_transaction: None,
            last_meta: Some(self.journal_meta()),
        };
    }

    fn apply_context_transaction_inner(
        &mut self,
        transaction: &ContextGraphTransaction,
        persist: bool,
    ) -> anyhow::Result<bool> {
        let transaction_sha256 = context_transaction_sha256(transaction)?;
        if self.last_context_op_id.as_deref() == Some(transaction.op_id.as_str()) {
            if self.last_context_op_sha256.as_deref() == Some(transaction_sha256.as_str()) {
                return Ok(false);
            }
            anyhow::bail!(
                "context op id {} was reused with a different transaction",
                transaction.op_id
            );
        }
        let current_generation = self
            .context_frontier
            .as_ref()
            .map_or(0, |frontier| frontier.generation);
        if transaction.base_generation != current_generation
            || transaction.generation != current_generation + 1
            || transaction.frontier.generation != transaction.generation
        {
            anyhow::bail!(
                "context generation mismatch: current {}, base {}, transaction {}, frontier {}",
                current_generation,
                transaction.base_generation,
                transaction.generation,
                transaction.frontier.generation
            );
        }
        if transaction.schema_version != 1
            || transaction.input_proof.schema_version != 1
            || transaction.op_id.is_empty()
            || transaction.input_proof.source_session_id != self.id
            || !is_sha256_hex(&transaction.input_proof.source_sha256)
            || transaction.append_context_nodes.iter().any(|node| {
                node.source_session_id != self.id || !is_sha256_hex(&node.source_sha256)
            })
            || !is_sha256_hex(&transaction.frontier.source_prefix_sha256)
        {
            anyhow::bail!("invalid context transaction identity or SHA-256 proof");
        }
        if transaction
            .append_context_nodes
            .iter()
            .all(|node| node.level == 0)
        {
            let covered = transaction.frontier.covered_message_count;
            if covered == 0 || covered > self.messages.len() {
                anyhow::bail!("context transaction covers an invalid message prefix");
            }
            let source_start = self
                .context_frontier
                .as_ref()
                .map_or(0, |frontier| frontier.covered_message_count)
                .min(covered);
            let source = &self.messages[source_start..covered];
            let source_prefix = &self.messages[..covered];
            if source.is_empty() {
                anyhow::bail!("context transaction has an empty leaf source");
            }
            let source_ids: Vec<String> = source.iter().map(|message| message.id.clone()).collect();
            let source_sha256 = stored_messages_sha256(source)?;
            let source_prefix_sha256 = stored_messages_sha256(source_prefix)?;
            if transaction.input_proof.source_message_ids != source_ids
                || transaction.input_proof.source_sha256 != source_sha256
                || transaction.frontier.source_prefix_sha256 != source_prefix_sha256
                || transaction.frontier.covered_through_message_id.as_deref()
                    != source_prefix.last().map(|message| message.id.as_str())
                || transaction.append_context_nodes.iter().any(|node| {
                    node.source_message_ids != source_ids || node.source_sha256 != source_sha256
                })
            {
                anyhow::bail!("context transaction source proof does not match canonical history");
            }
        } else if transaction.append_context_nodes.len() >= 2
            && transaction.append_context_nodes[0].level == 0
            && transaction.append_context_nodes[1..]
                .iter()
                .all(|node| node.level > 0)
        {
            const FANOUT: usize = 4;
            let current_frontier = self.context_frontier.as_ref().ok_or_else(|| {
                anyhow::anyhow!("atomic hierarchy carry requires an existing frontier")
            })?;
            let leaf = &transaction.append_context_nodes[0];
            let covered = transaction.frontier.covered_message_count;
            let source_start = current_frontier.covered_message_count;
            if covered <= source_start || covered > self.messages.len() {
                anyhow::bail!("atomic leaf-parent transaction covers an invalid message delta");
            }
            let source = &self.messages[source_start..covered];
            let source_prefix = &self.messages[..covered];
            let source_ids = source
                .iter()
                .map(|message| message.id.clone())
                .collect::<Vec<_>>();
            let source_sha256 = stored_messages_sha256(source)?;
            if transaction.input_proof.source_message_ids != source_ids
                || transaction.input_proof.source_sha256 != source_sha256
                || leaf.source_message_ids != source_ids
                || leaf.source_sha256 != source_sha256
                || transaction.frontier.source_prefix_sha256
                    != stored_messages_sha256(source_prefix)?
                || transaction.frontier.covered_through_message_id.as_deref()
                    != source_prefix.last().map(|message| message.id.as_str())
            {
                anyhow::bail!("atomic leaf-parent canonical source proof is invalid");
            }
            let mut expected_frontier = current_frontier.active_node_ids.clone();
            let mut carried_id = leaf.id.clone();
            for (parent_offset, parent) in transaction.append_context_nodes[1..].iter().enumerate()
            {
                if expected_frontier.len() < FANOUT - 1 {
                    anyhow::bail!(
                        "atomic hierarchy carry lacks three active prior level-{} children",
                        parent.level.saturating_sub(1)
                    );
                }
                let retained = expected_frontier.len() - (FANOUT - 1);
                let mut expected_child_ids = expected_frontier[retained..].to_vec();
                expected_child_ids.push(carried_id.clone());
                if parent.child_node_ids != expected_child_ids {
                    anyhow::bail!(
                        "atomic hierarchy parent children are not the adjacent frontier suffix"
                    );
                }
                let available_new_nodes = &transaction.append_context_nodes[..=parent_offset];
                let children = expected_child_ids
                    .iter()
                    .map(|child_id| {
                        self.context_nodes
                            .iter()
                            .chain(available_new_nodes.iter())
                            .find(|node| node.id == *child_id)
                            .ok_or_else(|| {
                                anyhow::anyhow!("context child node {child_id} is missing")
                            })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                if children
                    .iter()
                    .any(|child| child.level != children[0].level)
                    || parent.level != children[0].level.saturating_add(1)
                {
                    anyhow::bail!("atomic parent must directly exceed four equal-level children");
                }
                let children_sha256 =
                    format!("{:x}", Sha256::digest(serde_json::to_vec(&children)?));
                let mut seen = std::collections::HashSet::new();
                let expected_source_ids = children
                    .iter()
                    .flat_map(|child| child.source_message_ids.iter().cloned())
                    .filter(|id| seen.insert(id.clone()))
                    .collect::<Vec<_>>();
                if parent.source_sha256 != children_sha256
                    || parent.source_message_ids != expected_source_ids
                {
                    anyhow::bail!("atomic parent source proof does not match its children");
                }
                expected_frontier.truncate(retained);
                carried_id = parent.id.clone();
            }
            expected_frontier.push(carried_id);
            if transaction.frontier.active_node_ids != expected_frontier {
                anyhow::bail!("atomic hierarchy root was not installed as the frontier suffix");
            }
        } else {
            let current_frontier = self.context_frontier.as_ref().ok_or_else(|| {
                anyhow::anyhow!("hierarchical transaction requires an existing frontier")
            })?;
            if transaction.frontier.covered_message_count != current_frontier.covered_message_count
                || transaction.frontier.covered_through_message_id
                    != current_frontier.covered_through_message_id
                || transaction.frontier.source_prefix_sha256
                    != current_frontier.source_prefix_sha256
            {
                anyhow::bail!("hierarchical transaction cannot change canonical source coverage");
            }
            if transaction.append_context_nodes.len() != 1 {
                anyhow::bail!(
                    "hierarchical context transactions must append exactly one parent node"
                );
            }
            let parent = &transaction.append_context_nodes[0];
            if parent.level == 0 || parent.child_node_ids.is_empty() {
                anyhow::bail!("hierarchical context node must reference child nodes");
            }
            let children = parent
                .child_node_ids
                .iter()
                .map(|child_id| {
                    self.context_nodes
                        .iter()
                        .find(|node| node.id == *child_id)
                        .ok_or_else(|| anyhow::anyhow!("context child node {child_id} is missing"))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if children.iter().any(|child| child.level >= parent.level) {
                anyhow::bail!("context parent level must exceed every child level");
            }
            if transaction.input_proof.source_message_ids != parent.child_node_ids {
                anyhow::bail!("hierarchical input proof does not match child node order");
            }
            let children_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&children)?));
            if transaction.input_proof.source_sha256 != children_sha256
                || parent.source_sha256 != children_sha256
            {
                anyhow::bail!("hierarchical input proof hash does not match child nodes");
            }
            let mut seen = std::collections::HashSet::new();
            let expected_source_ids = children
                .iter()
                .flat_map(|child| child.source_message_ids.iter().cloned())
                .filter(|id| seen.insert(id.clone()))
                .collect::<Vec<_>>();
            if parent.source_message_ids != expected_source_ids {
                anyhow::bail!("context parent raw source coverage does not match children");
            }
        }
        if persist && self.persist_state.pending_context_transaction.is_some() {
            anyhow::bail!("a context transaction is already pending persistence");
        }

        let mut candidate = self.context_nodes.clone();
        for node in &transaction.append_context_nodes {
            match candidate.iter().find(|existing| existing.id == node.id) {
                Some(existing) if existing == node => {}
                Some(_) => anyhow::bail!("conflicting duplicate context node id {}", node.id),
                None => candidate.push(node.clone()),
            }
        }
        validate_context_graph(&candidate, Some(&transaction.frontier))
            .map_err(anyhow::Error::msg)?;
        validate_context_node_source_proofs(&self.id, &self.messages, &candidate)
            .map_err(anyhow::Error::msg)?;
        validate_context_frontier_coverage(&self.messages, &candidate, &transaction.frontier)
            .map_err(anyhow::Error::msg)?;

        self.context_nodes = candidate;
        self.context_frontier = Some(transaction.frontier.clone());
        self.last_context_op_id = Some(transaction.op_id.clone());
        self.last_context_op_sha256 = Some(transaction_sha256);
        if persist {
            self.persist_state.pending_context_transaction = Some(transaction.clone());
        }
        Ok(true)
    }

    /// Stage one immutable graph delta for journal and recovery tests. Runtime
    /// callers must use `commit_context_graph_transaction`, which persists a
    /// cloned candidate before publishing it to this live session.
    #[cfg(test)]
    pub(crate) fn apply_context_graph_transaction(
        &mut self,
        transaction: ContextGraphTransaction,
    ) -> anyhow::Result<bool> {
        self.apply_context_transaction_inner(&transaction, true)
    }

    fn discard_invalid_context_graph(&mut self) {
        let validation = if let Some(frontier) = self.context_frontier.as_ref() {
            let covered = frontier.covered_message_count;
            if covered == 0 && frontier.covered_through_message_id.is_none() {
                validate_context_graph(&self.context_nodes, Some(frontier)).and_then(|()| {
                    validate_context_node_source_proofs(
                        &self.id,
                        &self.messages,
                        &self.context_nodes,
                    )
                    .and_then(|()| {
                        validate_context_frontier_coverage(
                            &self.messages,
                            &self.context_nodes,
                            frontier,
                        )
                    })
                })
            } else if covered > self.messages.len() {
                Err("context frontier covers an invalid message prefix".to_string())
            } else {
                let source = &self.messages[..covered];
                match stored_messages_sha256(source) {
                    Ok(sha256)
                        if sha256 == frontier.source_prefix_sha256
                            && frontier.covered_through_message_id.as_deref()
                                == source.last().map(|message| message.id.as_str()) =>
                    {
                        validate_context_graph(&self.context_nodes, Some(frontier)).and_then(|()| {
                            validate_context_node_source_proofs(
                                &self.id,
                                &self.messages,
                                &self.context_nodes,
                            )
                            .and_then(|()| {
                                validate_context_frontier_coverage(
                                    &self.messages,
                                    &self.context_nodes,
                                    frontier,
                                )
                            })
                        })
                    }
                    Ok(_) => Err(
                        "context frontier source proof does not match canonical history"
                            .to_string(),
                    ),
                    Err(error) => Err(format!("context frontier source hashing failed: {error}")),
                }
            }
        } else {
            validate_context_graph(&self.context_nodes, self.context_frontier.as_ref()).and_then(
                |()| {
                    validate_context_node_source_proofs(
                        &self.id,
                        &self.messages,
                        &self.context_nodes,
                    )
                },
            )
        };
        if let Err(err) = validation {
            crate::logging::warn(&format!(
                "Ignoring invalid persisted context graph for session {}: {}",
                self.id, err
            ));
            self.context_nodes.clear();
            self.context_frontier = None;
            self.last_context_op_id = None;
            self.last_context_op_sha256 = None;
        }
    }

    fn reset_provider_messages_cache(&mut self) {
        self.provider_messages_cache.clear();
        self.provider_message_prefix_hashes_cache.clear();
        self.provider_messages_cache_len = 0;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
        self.memory_profile_cache.provider_cache_count = 0;
        self.memory_profile_cache.provider_cache_json_bytes = 0;
        self.memory_profile_cache.provider_cache_stats = ContentBlockMemoryStats::default();
    }

    /// Drop the derived provider-facing transcript once the current request has
    /// copied the messages it needs. The canonical [`StoredMessage`] history is
    /// still retained, so this cache can be rebuilt on the next provider call.
    ///
    /// Long-running server sessions otherwise keep two fully owned transcript
    /// copies while waiting on the network. Tool results and reasoning payloads
    /// can make that duplicate tens of MiB per active session.
    pub fn release_provider_messages_cache(&mut self) {
        self.provider_messages_cache = Vec::new();
        self.provider_message_prefix_hashes_cache = Vec::new();
        self.provider_messages_cache_len = 0;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
        self.memory_profile_cache.provider_cache_count = 0;
        self.memory_profile_cache.provider_cache_json_bytes = 0;
        self.memory_profile_cache.provider_cache_stats = ContentBlockMemoryStats::default();
    }

    fn push_provider_message_cache_entry(&mut self, message: Message) {
        let message_hash = crate::message::stable_message_hash(&message);
        let prefix_hash = self
            .provider_message_prefix_hashes_cache
            .last()
            .copied()
            .map(|prev| crate::message::extend_stable_hash(prev, message_hash))
            .unwrap_or(message_hash);
        self.memory_profile_cache.provider_cache_count += 1;
        self.memory_profile_cache.provider_cache_json_bytes += estimate_json_bytes(&message);
        self.memory_profile_cache
            .provider_cache_stats
            .merge_from(&summarize_blocks(&message.content));
        self.provider_messages_cache.push(message);
        self.provider_message_prefix_hashes_cache.push(prefix_hash);
    }

    fn mark_memory_profile_dirty(&mut self) {
        self.memory_profile_dirty = true;
    }

    fn rebuild_memory_profile_cache(&mut self) {
        let message_stats =
            summarize_message_content(self.messages.iter().map(|message| &message.content));
        let provider_cache_stats = summarize_message_content(
            self.provider_messages_cache
                .iter()
                .map(|message| &message.content),
        );

        self.memory_profile_cache = SessionMemoryProfileCache {
            messages_count: self.messages.len(),
            messages_json_bytes: self.messages.iter().map(estimate_json_bytes).sum(),
            message_stats,
            env_snapshots_count: self.env_snapshots.len(),
            env_snapshots_json_bytes: self.env_snapshots.iter().map(estimate_json_bytes).sum(),
            memory_injections_count: self.memory_injections.len(),
            memory_injections_json_bytes: self
                .memory_injections
                .iter()
                .map(estimate_json_bytes)
                .sum(),
            replay_events_count: self.replay_events.len(),
            replay_events_json_bytes: self.replay_events.iter().map(estimate_json_bytes).sum(),
            provider_cache_count: self.provider_messages_cache.len(),
            provider_cache_json_bytes: self
                .provider_messages_cache
                .iter()
                .map(estimate_json_bytes)
                .sum(),
            provider_cache_stats,
        };
        self.memory_profile_dirty = false;
    }

    fn ensure_memory_profile_cache(&mut self) {
        if self.memory_profile_dirty {
            self.rebuild_memory_profile_cache();
        }
    }

    pub fn memory_profile_snapshot(&mut self) -> SessionMemoryProfileSnapshot {
        self.ensure_memory_profile_cache();
        let compaction_json_bytes = self
            .compaction
            .as_ref()
            .map(estimate_json_bytes)
            .unwrap_or(0);

        SessionMemoryProfileSnapshot {
            message_count: self.memory_profile_cache.messages_count,
            provider_cache_message_count: self.memory_profile_cache.provider_cache_count,
            env_snapshot_count: self.memory_profile_cache.env_snapshots_count,
            memory_injection_count: self.memory_profile_cache.memory_injections_count,
            replay_event_count: self.memory_profile_cache.replay_events_count,
            payload_text_bytes: self.memory_profile_cache.message_stats.payload_text_bytes(),
            total_json_bytes: self.memory_profile_cache.messages_json_bytes
                + self.memory_profile_cache.provider_cache_json_bytes
                + self.memory_profile_cache.env_snapshots_json_bytes
                + self.memory_profile_cache.memory_injections_json_bytes
                + self.memory_profile_cache.replay_events_json_bytes
                + compaction_json_bytes,
            provider_cache_json_bytes: self.memory_profile_cache.provider_cache_json_bytes,
            canonical_tool_result_bytes: self.memory_profile_cache.message_stats.tool_result_bytes,
            provider_cache_tool_result_bytes: self
                .memory_profile_cache
                .provider_cache_stats
                .tool_result_bytes,
            canonical_large_blob_bytes: self.memory_profile_cache.message_stats.large_block_bytes,
            provider_cache_large_blob_bytes: self
                .memory_profile_cache
                .provider_cache_stats
                .large_block_bytes,
        }
    }

    fn mark_messages_append_dirty(&mut self) {
        if self.persist_state.messages_mode != PersistVectorMode::Full {
            self.persist_state.messages_mode = PersistVectorMode::Append;
        }
        if self.provider_messages_cache_mode != PersistVectorMode::Full {
            self.provider_messages_cache_mode = PersistVectorMode::Append;
        }
    }

    fn mark_messages_full_dirty(&mut self) {
        self.persist_state.messages_mode = PersistVectorMode::Full;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
    }

    fn mark_env_snapshots_append_dirty(&mut self) {
        if self.persist_state.env_snapshots_mode != PersistVectorMode::Full {
            self.persist_state.env_snapshots_mode = PersistVectorMode::Append;
        }
    }

    fn mark_env_snapshots_full_dirty(&mut self) {
        self.persist_state.env_snapshots_mode = PersistVectorMode::Full;
    }

    fn mark_memory_injections_append_dirty(&mut self) {
        if self.persist_state.memory_injections_mode != PersistVectorMode::Full {
            self.persist_state.memory_injections_mode = PersistVectorMode::Append;
        }
    }

    fn mark_replay_events_append_dirty(&mut self) {
        if self.persist_state.replay_events_mode != PersistVectorMode::Full {
            self.persist_state.replay_events_mode = PersistVectorMode::Append;
        }
    }

    fn apply_journal_meta(&mut self, meta: SessionJournalMeta) {
        self.parent_id = meta.parent_id;
        self.title = meta.title;
        self.custom_title = meta.custom_title;
        self.updated_at = meta.updated_at;
        self.compaction = meta.compaction;
        self.provider_session_id = meta.provider_session_id;
        self.provider_key = meta.provider_key;
        self.model = meta.model;
        self.reasoning_effort = meta.reasoning_effort;
        self.subagent_model = meta.subagent_model;
        self.improve_mode = meta.improve_mode;
        self.autoreview_enabled = meta.autoreview_enabled;
        self.autojudge_enabled = meta.autojudge_enabled;
        self.is_canary = meta.is_canary;
        self.testing_build = meta.testing_build;
        self.working_dir = meta.working_dir;
        self.short_name = meta.short_name;
        self.status = meta.status;
        self.last_pid = meta.last_pid;
        self.last_active_at = meta.last_active_at;
        self.is_debug = meta.is_debug;
        self.saved = meta.saved;
        self.save_label = meta.save_label;
        self.mark_memory_profile_dirty();
    }

    pub fn create_with_id(
        session_id: String,
        parent_id: Option<String>,
        title: Option<String>,
    ) -> Self {
        let now = Utc::now();
        let is_debug = default_is_test_session();
        // Try to extract short name from ID if it's a memorable ID
        let short_name = extract_session_name(&session_id).map(|s| s.to_string());
        let mut session = Self {
            id: session_id,
            parent_id,
            title,
            custom_title: None,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            journal_sequence: 0,
            journal_watermark: 0,
            context_nodes: Vec::new(),
            context_frontier: None,
            compaction: None,
            provider_session_id: None,
            provider_key: None,
            model: None,
            route_api_method: None,
            reasoning_effort: None,
            subagent_model: None,
            improve_mode: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            is_canary: false,
            testing_build: None,
            working_dir: current_working_dir_string(),
            short_name,
            status: SessionStatus::Active,
            last_pid: Some(std::process::id()),
            last_active_at: Some(now),
            is_debug,
            saved: false,
            save_label: None,
            env_snapshots: Vec::new(),
            memory_injections: Vec::new(),
            replay_events: Vec::new(),
            persist_state: SessionPersistState::default(),
            last_context_op_id: None,
            last_context_op_sha256: None,
            provider_messages_cache: Vec::new(),
            provider_message_prefix_hashes_cache: Vec::new(),
            provider_messages_cache_len: 0,
            provider_messages_cache_mode: PersistVectorMode::Full,
            memory_profile_cache: SessionMemoryProfileCache::default(),
            memory_profile_dirty: false,
        };
        session.reset_persist_state(false);
        session
    }

    pub fn create(parent_id: Option<String>, title: Option<String>) -> Self {
        let now = Utc::now();
        // Keep memorable identities distinct across all currently active
        // sessions. This naturally covers swarm members and survives a server
        // reload because active PID markers retain their encoded short names.
        let used_names = active_session_ids()
            .into_iter()
            .filter_map(|session_id| extract_session_name(&session_id).map(str::to_string))
            .collect::<HashSet<_>>();
        let (id, short_name) = new_memorable_session_id_avoiding(&used_names);
        let is_debug = default_is_test_session();
        let mut session = Self {
            id,
            parent_id,
            title,
            custom_title: None,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            journal_sequence: 0,
            journal_watermark: 0,
            context_nodes: Vec::new(),
            context_frontier: None,
            compaction: None,
            provider_session_id: None,
            provider_key: None,
            model: None,
            route_api_method: None,
            reasoning_effort: None,
            subagent_model: None,
            improve_mode: None,
            autoreview_enabled: None,
            autojudge_enabled: None,
            is_canary: false,
            testing_build: None,
            working_dir: current_working_dir_string(),
            short_name: Some(short_name),
            status: SessionStatus::Active,
            last_pid: Some(std::process::id()),
            last_active_at: Some(now),
            is_debug,
            saved: false,
            save_label: None,
            env_snapshots: Vec::new(),
            memory_injections: Vec::new(),
            replay_events: Vec::new(),
            persist_state: SessionPersistState::default(),
            last_context_op_id: None,
            last_context_op_sha256: None,
            provider_messages_cache: Vec::new(),
            provider_message_prefix_hashes_cache: Vec::new(),
            provider_messages_cache_len: 0,
            provider_messages_cache_mode: PersistVectorMode::Full,
            memory_profile_cache: SessionMemoryProfileCache::default(),
            memory_profile_dirty: false,
        };
        session.reset_persist_state(false);
        session
    }

    /// Mark this session as a debug/test session
    pub fn set_debug(&mut self, is_debug: bool) {
        self.is_debug = is_debug;
        // Debug status can change after activation (e.g. debug-socket created
        // sessions); keep presence UIs in sync when we are the active owner.
        if self.status == SessionStatus::Active {
            self.sync_internal_presence_flag();
        }
    }

    /// Save/bookmark this session with an optional label
    pub fn mark_saved(&mut self, label: Option<String>) {
        self.saved = true;
        if label.is_some() {
            self.save_label = label;
        }
    }

    /// Remove the saved/bookmark status
    pub fn unmark_saved(&mut self) {
        self.saved = false;
        self.save_label = None;
    }

    /// Set or clear the user-provided display title.
    ///
    /// This intentionally does not change the immutable session id, memorable
    /// short name, generated title, provider session id, or saved/bookmark label.
    pub fn rename_title(&mut self, title: Option<String>) {
        self.custom_title = title.and_then(|title| {
            let title = title.trim();
            (!title.is_empty()).then(|| title.to_string())
        });
        self.updated_at = Utc::now();
    }

    /// Get the title users should see for this session: custom rename first,
    /// then the generated/imported title, if one exists.
    pub fn display_title(&self) -> Option<&str> {
        fn non_empty_trimmed(title: Option<&str>) -> Option<&str> {
            title.map(str::trim).filter(|title| !title.is_empty())
        }

        non_empty_trimmed(self.custom_title.as_deref())
            .or_else(|| non_empty_trimmed(self.title.as_deref()))
    }

    /// Get a visible label for title-oriented surfaces, falling back to the
    /// memorable session name when there is no generated or custom title.
    pub fn display_title_or_name(&self) -> &str {
        self.display_title().unwrap_or_else(|| self.display_name())
    }

    /// Record an environment snapshot for post-mortem debugging
    pub fn record_env_snapshot(&mut self, snapshot: EnvSnapshot) {
        self.memory_profile_cache.env_snapshots_count += 1;
        self.memory_profile_cache.env_snapshots_json_bytes += estimate_json_bytes(&snapshot);
        self.env_snapshots.push(snapshot);
        if self.env_snapshots.len() > MAX_ENV_SNAPSHOTS {
            let excess = self.env_snapshots.len() - MAX_ENV_SNAPSHOTS;
            self.env_snapshots.drain(0..excess);
            self.mark_memory_profile_dirty();
            self.mark_env_snapshots_full_dirty();
        } else {
            self.mark_env_snapshots_append_dirty();
        }
    }

    pub fn has_session_context_message(&self) -> bool {
        self.messages.iter().any(|message| {
            message.content.iter().any(|block| match block {
                ContentBlock::Text { text, .. } => text.starts_with(SESSION_CONTEXT_PREFIX),
                _ => false,
            })
        })
    }

    /// Persist an immutable session-context snapshot as the first provider-visible
    /// transcript item for new sessions. Existing non-empty sessions are left
    /// untouched so their historical context is never rewritten with newer state.
    pub fn ensure_initial_session_context_message(&mut self) -> bool {
        if !self.messages.is_empty() || self.has_session_context_message() {
            return false;
        }

        // Preserve an explicitly bound session directory. Shared-server clients
        // provide their cwd before this message is created, and replacing it with
        // the daemon process cwd would leak the directory that launched the server.
        if self.working_dir.is_none() {
            self.working_dir = current_working_dir_string();
        }

        let context =
            crate::prompt::build_session_context(self.working_dir.as_deref().map(Path::new));
        let wrapped = format!("<system-reminder>\n{}\n</system-reminder>", context.trim());
        self.add_message_with_display_role(
            Role::User,
            vec![ContentBlock::Text {
                text: wrapped,
                cache_control: None,
            }],
            Some(StoredDisplayRole::System),
        );
        true
    }

    /// Refresh the initial immutable session-context message if the session has
    /// not started a real conversation yet. This covers remote/client-server
    /// startup where the server creates an Agent before the subscribing client
    /// sends the terminal working directory that tools will use.
    pub fn refresh_initial_session_context_message(&mut self) -> bool {
        if self.messages.iter().any(is_visible_conversation_message) {
            return false;
        }

        let Some(message_index) = self.messages.iter().position(|message| {
            message.content.iter().any(|block| match block {
                ContentBlock::Text { text, .. } => text.starts_with(SESSION_CONTEXT_PREFIX),
                _ => false,
            })
        }) else {
            return false;
        };

        let context =
            crate::prompt::build_session_context(self.working_dir.as_deref().map(Path::new));
        let wrapped = format!("<system-reminder>\n{}\n</system-reminder>", context.trim());
        let message_id = self.messages[message_index].id.clone();
        for block in &mut self.messages[message_index].content {
            if let ContentBlock::Text { text, .. } = block
                && text.starts_with(SESSION_CONTEXT_PREFIX)
            {
                if *text == wrapped {
                    return false;
                }
                *text = wrapped;
                if self.context_nodes.iter().any(|node| {
                    node.source_message_ids
                        .iter()
                        .any(|source_id| source_id == &message_id)
                }) {
                    self.compaction = None;
                    self.clear_context_graph_state();
                }
                self.mark_memory_profile_dirty();
                self.mark_messages_full_dirty();
                return true;
            }
        }

        false
    }

    /// Get the display name for this session (short memorable name if available)
    pub fn display_name(&self) -> &str {
        self.short_name
            .as_deref()
            .or_else(|| extract_session_name(&self.id))
            .unwrap_or(&self.id)
    }

    /// Append a model-visible notice telling the agent this session is a fork
    /// of `parent_session_id`'s conversation.
    ///
    /// Forking happens when the user splits a window mid-conversation (often
    /// while the parent agent is still streaming) and points the new window at
    /// a clone of the transcript. Without this notice the forked agent assumes
    /// it owns the in-flight request, duplicating the parent's work. The
    /// notice is wrapped in `<system-reminder>` so it stays out of the visible
    /// transcript while still reaching the model on the next turn.
    pub fn append_fork_notice(&mut self, parent_session_id: &str, parent_display_name: &str) {
        let text = format!(
            "<system-reminder>\nThis session was forked (split) from session {parent} ({parent_id}) by the user. \
The full conversation above is inherited from that session, but the original agent in {parent} \
is still active and will continue handling whatever request or work was in progress there. \
Do NOT continue or duplicate that in-flight work here. Treat the next user message as a fresh \
request in this new forked session, using the inherited conversation only as context.\n</system-reminder>",
            parent = parent_display_name,
            parent_id = parent_session_id,
        );
        self.add_message_with_display_role(
            Role::User,
            vec![ContentBlock::Text {
                text,
                cache_control: None,
            }],
            Some(StoredDisplayRole::System),
        );
    }

    /// Mark this session as a canary tester
    pub fn set_canary(&mut self, build_hash: &str) {
        self.is_canary = true;
        self.testing_build = Some(build_hash.to_string());
    }

    /// Clear canary status
    pub fn clear_canary(&mut self) {
        self.is_canary = false;
        self.testing_build = None;
    }

    /// Set the session status
    pub fn set_status(&mut self, status: SessionStatus) {
        self.status = status;
    }

    /// Mark session as closed normally
    pub fn mark_closed(&mut self) {
        self.status = SessionStatus::Closed;
        unregister_active_pid(&self.id);
    }

    /// Mark session as crashed
    pub fn mark_crashed(&mut self, message: Option<String>) {
        self.status = SessionStatus::Crashed { message };
        unregister_active_pid(&self.id);
    }

    /// Mark session as having an error
    pub fn mark_error(&mut self, message: String) {
        self.status = SessionStatus::Error { message };
    }

    /// Mark session as active (e.g., when resuming)
    pub fn mark_active(&mut self) {
        self.status = SessionStatus::Active;
        let pid = std::process::id();
        self.last_pid = Some(pid);
        self.last_active_at = Some(Utc::now());
        register_active_pid(&self.id, pid);
        self.sync_internal_presence_flag();
    }

    /// Mark session as active for a specific PID
    pub fn mark_active_with_pid(&mut self, pid: u32) {
        self.status = SessionStatus::Active;
        self.last_pid = Some(pid);
        self.last_active_at = Some(Utc::now());
        register_active_pid(&self.id, pid);
        self.sync_internal_presence_flag();
    }

    /// Keep the on-disk internal-session flag in sync with this session's
    /// role. Debug/test sessions and spawned children (swarm workers,
    /// subagents) are internal: they stay tracked for lifecycle purposes but
    /// are hidden from user-facing presence UIs like the menu bar (issue
    /// #508).
    fn sync_internal_presence_flag(&self) {
        let internal = self.is_debug || self.parent_id.is_some();
        crate::storage::set_session_internal(&self.id, internal);
    }

    /// Detect if an active session likely crashed (process no longer running)
    /// Returns true if status was updated.
    pub fn detect_crash(&mut self) -> bool {
        if self.status != SessionStatus::Active {
            return false;
        }

        if let Some(pid) = self.last_pid {
            if !crash::is_pid_running(pid) {
                self.mark_crashed(Some(format!(
                    "Process {} exited unexpectedly (no shutdown signal captured)",
                    pid
                )));
                return true;
            }
        } else {
            // No PID info (older sessions): fall back to age heuristic
            let age = Utc::now().signed_duration_since(self.updated_at);
            if age.num_seconds() > 120 {
                self.mark_crashed(Some(
                    "Stale active session (possible abrupt termination)".to_string(),
                ));
                return true;
            }
        }

        false
    }

    /// Check if this session is working on the jcode repository
    pub fn is_self_dev(&self) -> bool {
        if let Some(ref dir) = self.working_dir {
            // Check if working dir contains jcode source
            let path = std::path::Path::new(dir);
            path.join("Cargo.toml").exists()
                && path.join("src/main.rs").exists()
                && std::fs::read_to_string(path.join("Cargo.toml"))
                    .map(|s| s.contains("name = \"jcode\""))
                    .unwrap_or(false)
        } else {
            false
        }
    }

    pub fn redacted_for_export(&self) -> Self {
        let mut redacted = self.clone();
        if let Some(title) = redacted.title.as_mut() {
            *title = crate::message::redact_secrets(title);
        }
        if let Some(title) = redacted.custom_title.as_mut() {
            *title = crate::message::redact_secrets(title);
        }
        if let Some(compaction) = redacted.compaction.as_mut() {
            compaction.summary_text = crate::message::redact_secrets(&compaction.summary_text);
        }
        for msg in &mut redacted.messages {
            for block in &mut msg.content {
                match block {
                    ContentBlock::Text { text, .. }
                    | ContentBlock::Reasoning { text }
                    | ContentBlock::ReasoningTrace { text } => {
                        *text = crate::message::redact_secrets(text);
                    }
                    ContentBlock::AnthropicThinking { thinking, .. } => {
                        *thinking = crate::message::redact_secrets(thinking);
                    }
                    ContentBlock::OpenAIReasoning { summary, .. } => {
                        for item in summary {
                            *item = crate::message::redact_secrets(item);
                        }
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        *content = crate::message::redact_secrets(content);
                    }
                    ContentBlock::ToolUse { input, .. } => redact_json_value(input),
                    ContentBlock::Image { .. } => {}
                    ContentBlock::OpenAICompaction { .. } => {}
                }
            }
        }
        for event in &mut redacted.replay_events {
            match &mut event.kind {
                StoredReplayEventKind::DisplayMessage { title, content, .. } => {
                    if let Some(title) = title.as_mut() {
                        *title = crate::message::redact_secrets(title);
                    }
                    *content = crate::message::redact_secrets(content);
                }
                StoredReplayEventKind::SwarmStatus { members } => {
                    for member in members {
                        if let Some(detail) = member.detail.as_mut() {
                            *detail = crate::message::redact_secrets(detail);
                        }
                    }
                }
                StoredReplayEventKind::SwarmPlan { items, reason, .. } => {
                    if let Some(reason) = reason.as_mut() {
                        *reason = crate::message::redact_secrets(reason);
                    }
                    for item in items {
                        item.content = crate::message::redact_secrets(&item.content);
                    }
                }
            }
        }
        // The graph is rebuildable derived state and can duplicate credentials
        // from raw history in model-written prose. Exporting it adds no recovery
        // value, so omit it rather than relying on secret-pattern coverage.
        redacted.context_nodes.clear();
        redacted.context_frontier = None;
        redacted.last_context_op_id = None;
        redacted.last_context_op_sha256 = None;
        redacted.persist_state.pending_context_transaction = None;
        redacted
    }

    pub fn token_usage_totals(&self) -> crate::protocol::TokenUsageTotals {
        let mut totals = crate::protocol::TokenUsageTotals::default();
        for message in &self.messages {
            let Some(usage) = message.token_usage.as_ref() else {
                continue;
            };
            totals.messages_with_token_usage = totals.messages_with_token_usage.saturating_add(1);
            totals.input_tokens = totals.input_tokens.saturating_add(usage.input_tokens);
            totals.output_tokens = totals.output_tokens.saturating_add(usage.output_tokens);
            if usage.cache_read_input_tokens.is_some()
                || usage.cache_creation_input_tokens.is_some()
            {
                totals.cache_reported_input_tokens = totals
                    .cache_reported_input_tokens
                    .saturating_add(usage.input_tokens);
            }
            totals.cache_read_input_tokens = totals
                .cache_read_input_tokens
                .saturating_add(usage.cache_read_input_tokens.unwrap_or(0));
            totals.cache_creation_input_tokens = totals
                .cache_creation_input_tokens
                .saturating_add(usage.cache_creation_input_tokens.unwrap_or(0));
        }
        totals
    }

    pub fn add_message(&mut self, role: Role, content: Vec<ContentBlock>) -> String {
        self.add_message_ext_with_display_role(role, content, None, None, None)
    }

    pub fn add_message_with_duration(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        tool_duration_ms: Option<u64>,
    ) -> String {
        self.add_message_ext_with_display_role(role, content, tool_duration_ms, None, None)
    }

    pub fn add_message_with_display_role(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        display_role: Option<StoredDisplayRole>,
    ) -> String {
        self.add_message_ext_with_display_role(role, content, None, None, display_role)
    }

    pub fn add_message_ext(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        tool_duration_ms: Option<u64>,
        token_usage: Option<StoredTokenUsage>,
    ) -> String {
        self.add_message_ext_with_display_role(role, content, tool_duration_ms, token_usage, None)
    }

    pub fn add_message_ext_with_display_role(
        &mut self,
        role: Role,
        content: Vec<ContentBlock>,
        tool_duration_ms: Option<u64>,
        token_usage: Option<StoredTokenUsage>,
        display_role: Option<StoredDisplayRole>,
    ) -> String {
        let id = new_id("message");
        self.append_stored_message(StoredMessage {
            id: id.clone(),
            role,
            content,
            display_role,
            timestamp: Some(Utc::now()),
            tool_duration_ms,
            token_usage,
        });
        id
    }

    pub fn append_stored_message(&mut self, message: StoredMessage) {
        self.memory_profile_cache.messages_count += 1;
        self.memory_profile_cache.messages_json_bytes += estimate_json_bytes(&message);
        self.memory_profile_cache
            .message_stats
            .merge_from(&summarize_blocks(&message.content));
        self.messages.push(message);
        self.mark_messages_append_dirty();
    }

    pub fn insert_message(&mut self, index: usize, message: StoredMessage) {
        self.messages.insert(index, message);
        self.compaction = None;
        self.clear_context_graph_state();
        self.mark_memory_profile_dirty();
        self.mark_messages_full_dirty();
    }

    pub fn replace_messages(&mut self, messages: Vec<StoredMessage>) {
        self.messages = messages;
        self.compaction = None;
        self.clear_context_graph_state();
        self.mark_memory_profile_dirty();
        self.mark_messages_full_dirty();
    }

    pub fn truncate_messages(&mut self, len: usize) {
        if len < self.messages.len() {
            if let Err(error) = self.retain_context_graph_prefix(len) {
                crate::logging::warn(&format!(
                    "Failed to retain valid LCM transcript prefix; falling back to raw history: {error}"
                ));
                self.compaction = None;
                self.clear_context_graph_state();
            }
            self.messages.truncate(len);
            self.mark_memory_profile_dirty();
            self.mark_messages_full_dirty();
        }
    }

    /// Drop oversized inline images from the stored transcript, oldest-first,
    /// until the total remaining base64 image payload fits within
    /// `target_total_chars`. Used to recover from provider HTTP 413
    /// "request too large" errors, which are driven by base64 image payload size
    /// rather than the token context window.
    ///
    /// Mutates and persists the authoritative transcript (replacing each dropped
    /// image with a short text marker) and invalidates the provider-message
    /// cache so the next API call reflects the reduced payload. Returns the
    /// number of images that were stripped.
    pub fn strip_oversized_images(&mut self, target_total_chars: usize) -> usize {
        let mut contents: Vec<&mut Vec<ContentBlock>> =
            self.messages.iter_mut().map(|m| &mut m.content).collect();
        let stripped = jcode_compaction_core::strip_large_images_in_contents(
            &mut contents,
            target_total_chars,
        );
        if stripped > 0 {
            self.compaction = None;
            self.clear_context_graph_state();
            self.mark_memory_profile_dirty();
            self.mark_messages_full_dirty();
        }
        stripped
    }

    pub fn visible_conversation_message_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| is_visible_conversation_message(message))
            .count()
    }

    pub fn visible_conversation_messages(&self) -> Vec<&StoredMessage> {
        self.messages
            .iter()
            .filter(|message| is_visible_conversation_message(message))
            .collect()
    }

    pub fn stored_len_for_visible_conversation_message(
        &self,
        visible_index: usize,
    ) -> Option<usize> {
        if visible_index == 0 {
            return None;
        }

        let mut count = 0usize;
        for (stored_index, message) in self.messages.iter().enumerate() {
            if is_visible_conversation_message(message) {
                count += 1;
                if count == visible_index {
                    return Some(stored_index + 1);
                }
            }
        }
        None
    }

    /// Stored-message indices of the rewind targets shown in the TUI's
    /// numbered `/rewind` list, in display order.
    ///
    /// The TUI numbers user/assistant *transcript entries* (what the user
    /// actually sees), not raw stored messages. Stored tool-result messages
    /// and tool-call-only assistant messages render as tool cards or nothing,
    /// so counting raw stored messages diverges wildly from the on-screen
    /// numbering in tool-heavy sessions (issue #432). Deriving targets from
    /// the same rendering used for the transcript keeps `/rewind N` aligned
    /// with the numbers `/rewind` prints.
    ///
    /// A single stored message can produce multiple transcript entries (text
    /// split around a tool result); each entry keeps its own number and maps
    /// to the same stored index so numbering matches the visible list exactly.
    pub fn rewind_target_stored_indices(&self) -> Vec<usize> {
        render_messages(self)
            .into_iter()
            .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
            .filter_map(|message| message.stored_index)
            .collect()
    }

    /// Number of `/rewind` targets (see [`Self::rewind_target_stored_indices`]).
    pub fn rewind_target_count(&self) -> usize {
        self.rewind_target_stored_indices().len()
    }

    /// Record a memory injection event for replay visualization
    pub fn record_memory_injection(
        &mut self,
        summary: String,
        content: String,
        count: u32,
        age_ms: u64,
        memory_ids: Vec<String>,
    ) {
        let injection = StoredMemoryInjection {
            summary,
            content,
            count,
            memory_ids,
            age_ms: Some(age_ms),
            before_message: Some(self.messages.len()),
            timestamp: Utc::now(),
        };
        self.memory_profile_cache.memory_injections_count += 1;
        self.memory_profile_cache.memory_injections_json_bytes += estimate_json_bytes(&injection);
        self.memory_injections.push(injection);
        self.mark_memory_injections_append_dirty();
    }

    pub fn injected_memory_ids(&self) -> Vec<String> {
        let mut ids = HashSet::new();
        for injection in &self.memory_injections {
            ids.extend(injection.memory_ids.iter().cloned());
        }
        ids.into_iter().collect()
    }

    pub fn record_replay_display_message(
        &mut self,
        role: impl Into<String>,
        title: Option<String>,
        content: impl Into<String>,
    ) {
        let event = StoredReplayEvent {
            timestamp: Utc::now(),
            kind: StoredReplayEventKind::DisplayMessage {
                role: role.into(),
                title,
                content: content.into(),
            },
        };
        self.memory_profile_cache.replay_events_count += 1;
        self.memory_profile_cache.replay_events_json_bytes += estimate_json_bytes(&event);
        self.replay_events.push(event);
        self.mark_replay_events_append_dirty();
    }

    pub fn record_swarm_status_event(&mut self, members: Vec<crate::protocol::SwarmMemberStatus>) {
        let kind = StoredReplayEventKind::SwarmStatus { members };
        if self
            .replay_events
            .last()
            .is_some_and(|last| last.kind == kind)
        {
            return;
        }
        let event = StoredReplayEvent {
            timestamp: Utc::now(),
            kind,
        };
        self.memory_profile_cache.replay_events_count += 1;
        self.memory_profile_cache.replay_events_json_bytes += estimate_json_bytes(&event);
        self.replay_events.push(event);
        self.mark_replay_events_append_dirty();
    }

    pub fn record_swarm_plan_event(
        &mut self,
        swarm_id: String,
        version: u64,
        items: Vec<crate::plan::PlanItem>,
        participants: Vec<String>,
        reason: Option<String>,
    ) {
        let kind = StoredReplayEventKind::SwarmPlan {
            swarm_id,
            version,
            items,
            participants,
            reason,
        };
        if self
            .replay_events
            .last()
            .is_some_and(|last| last.kind == kind)
        {
            return;
        }
        let event = StoredReplayEvent {
            timestamp: Utc::now(),
            kind,
        };
        self.memory_profile_cache.replay_events_count += 1;
        self.memory_profile_cache.replay_events_json_bytes += estimate_json_bytes(&event);
        self.replay_events.push(event);
        self.mark_replay_events_append_dirty();
    }

    pub fn provider_messages(&mut self) -> &[Message] {
        let needs_full_rebuild = self.provider_messages_cache_mode == PersistVectorMode::Full
            || self.provider_messages_cache_len > self.messages.len();

        if needs_full_rebuild {
            self.provider_messages_cache.clear();
            self.provider_message_prefix_hashes_cache.clear();
            self.provider_messages_cache.reserve(self.messages.len());
            self.provider_message_prefix_hashes_cache
                .reserve(self.messages.len());
            for index in 0..self.messages.len() {
                let message = self.messages[index].to_message();
                self.push_provider_message_cache_entry(message);
            }
            self.provider_messages_cache_len = self.messages.len();
            self.provider_messages_cache_mode = PersistVectorMode::Clean;
            return &self.provider_messages_cache;
        }

        if self.provider_messages_cache_mode == PersistVectorMode::Append
            && self.provider_messages_cache_len < self.messages.len()
        {
            let appended_len = self.messages.len() - self.provider_messages_cache_len;
            self.provider_messages_cache.reserve(appended_len);
            self.provider_message_prefix_hashes_cache
                .reserve(appended_len);
            for index in self.provider_messages_cache_len..self.messages.len() {
                let message = self.messages[index].to_message();
                self.push_provider_message_cache_entry(message);
            }
            self.provider_messages_cache_len = self.messages.len();
            self.provider_messages_cache_mode = PersistVectorMode::Clean;
        }

        &self.provider_messages_cache
    }

    pub fn provider_message_prefix_hashes(&mut self) -> &[u64] {
        let _ = self.provider_messages();
        &self.provider_message_prefix_hashes_cache
    }

    pub fn messages_for_provider_uncached(&self) -> Vec<Message> {
        stored_messages_to_messages(&self.messages)
    }

    pub fn messages_for_provider(&mut self) -> Vec<Message> {
        self.provider_messages().to_vec()
    }

    /// Drop heavyweight transcript vectors after remote startup has rendered the
    /// optimistic local history. The authoritative transcript comes from the
    /// server once the connection is established, so keeping another owned copy
    /// in the client only inflates memory during idle remote sessions.
    pub fn strip_transcript_for_remote_client(&mut self) {
        self.messages.clear();
        self.compaction = None;
        self.clear_context_graph_state();
        self.env_snapshots.clear();
        self.memory_injections.clear();
        self.replay_events.clear();
        self.rebuild_memory_profile_cache();
        self.reset_provider_messages_cache();
        self.reset_persist_state(true);
    }

    /// Remove all ToolUse content blocks from a specific message.
    /// Used when tool calls are discarded (e.g. due to truncated output / max_tokens).
    pub fn remove_tool_use_blocks(&mut self, message_id: &str) {
        let mut changed = false;
        for msg in &mut self.messages {
            if msg.id == *message_id {
                let old_len = msg.content.len();
                msg.content
                    .retain(|block| !matches!(block, ContentBlock::ToolUse { .. }));
                changed = msg.content.len() != old_len;
                break;
            }
        }
        if changed {
            let invalidates_graph = self.context_nodes.iter().any(|node| {
                node.source_message_ids
                    .iter()
                    .any(|source_id| source_id == message_id)
            });
            if invalidates_graph {
                self.compaction = None;
                self.clear_context_graph_state();
            }
            self.mark_memory_profile_dirty();
            self.mark_messages_full_dirty();
        }
    }
}

fn redact_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            *s = crate::message::redact_secrets(s);
        }
        serde_json::Value::Array(values) => {
            for entry in values {
                redact_json_value(entry);
            }
        }
        serde_json::Value::Object(map) => {
            for entry in map.values_mut() {
                redact_json_value(entry);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Deserialize)]
struct RemoteStartupSessionSnapshot {
    id: String,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    custom_title: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    messages: Vec<StoredMessage>,
    #[serde(default)]
    journal_sequence: u64,
    #[serde(default)]
    journal_watermark: u64,
    #[serde(default)]
    context_nodes: Vec<StoredContextNode>,
    #[serde(default)]
    context_frontier: Option<StoredContextFrontier>,
    #[serde(default)]
    compaction: Option<StoredCompactionState>,
    #[serde(default)]
    provider_session_id: Option<String>,
    #[serde(default)]
    provider_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    route_api_method: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    subagent_model: Option<String>,
    #[serde(default)]
    improve_mode: Option<SessionImproveMode>,
    #[serde(default)]
    autoreview_enabled: Option<bool>,
    #[serde(default)]
    autojudge_enabled: Option<bool>,
    #[serde(default)]
    is_canary: bool,
    #[serde(default)]
    testing_build: Option<String>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    short_name: Option<String>,
    #[serde(default)]
    status: SessionStatus,
    #[serde(default)]
    last_pid: Option<u32>,
    #[serde(default)]
    last_active_at: Option<DateTime<Utc>>,
    #[serde(default)]
    is_debug: bool,
    #[serde(default)]
    saved: bool,
    #[serde(default)]
    save_label: Option<String>,
}

#[cfg(test)]
#[path = "session_tests/mod.rs"]
mod tests;
