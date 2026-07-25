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
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;
mod account_transition_persistence;
mod crash;
mod journal;
mod maintenance;
mod memory_profile;
mod model;
mod persistence;
mod render;
mod storage_paths;
pub use account_transition_persistence::{
    AccountTransitionAdmission, AccountTransitionFileLock, account_transition_admission_held,
    capture_account_transition_admission, record_completed_account_transition,
    with_account_transition_admission, with_inherited_account_transition_admission,
};
pub use crash::{
    CrashedSessionsInfo, begin_session_handoff, detect_crashed_sessions,
    find_recent_crashed_sessions, find_session_by_name_or_id, finish_session_handoff,
    recover_crashed_sessions, recover_crashed_sessions_by_ids,
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
    /// Canonical messages excluded from the active conversation branch by
    /// rewind. Raw messages remain append-only and searchable; provider and
    /// transcript projections filter this set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub archived_message_ids: Vec<String>,
    /// Provider-only recovery projection for HTTP 413 payload-size failures.
    ///
    /// The raw transcript in `messages` remains canonical and append-only. When
    /// set, provider requests materialize a reduced clone with oldest inline
    /// images replaced by markers until the total image payload fits this budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_image_char_budget: Option<usize>,
    /// Provider-only recovery projection for assistant messages whose tool calls
    /// were truncated by the provider. The stored ToolUse bytes remain canonical;
    /// provider requests omit ToolUse blocks for these message ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_tool_use_suppressed_message_ids: Vec<String>,
    /// Highest journal sequence incorporated into this in-memory session.
    #[serde(default)]
    pub journal_sequence: u64,
    /// Highest journal sequence covered by this installed snapshot.
    #[serde(default)]
    pub journal_watermark: u64,
    /// Monotonic revision advanced by every durable session write, including
    /// graph-neutral full checkpoints that do not append a journal entry.
    #[serde(default)]
    pub persistence_revision: u64,
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
    /// Exact runtime identity under which `provider_session_id` was issued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_identity: Option<jcode_provider_core::ExactRuntimeIdentity>,
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
    /// Exact non-secret route/account identity expected on restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_runtime_identity: Option<jcode_provider_core::ExactRuntimeIdentity>,
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
    archived_message_ids: Vec<String>,
    #[serde(default)]
    persistence_revision: u64,
    #[serde(default)]
    compaction: Option<StoredCompactionState>,
    #[serde(default)]
    provider_session_id: Option<String>,
    #[serde(default)]
    provider_session_identity: Option<jcode_provider_core::ExactRuntimeIdentity>,
    #[serde(default)]
    provider_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    route_api_method: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    exact_runtime_identity: Option<jcode_provider_core::ExactRuntimeIdentity>,
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

pub(crate) fn native_lcm_projection_text(
    nodes: &[StoredContextNode],
    frontier: &StoredContextFrontier,
) -> Option<String> {
    let by_id: HashMap<_, _> = nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    frontier
        .active_node_ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            by_id.get(id.as_str()).map(|node| {
                format!(
                    "[LCM context node {} level {}]\n{}",
                    index + 1,
                    node.level,
                    node.summary_text
                )
            })
        })
        .collect::<Option<Vec<_>>>()
        .map(|sections| sections.join("\n\n"))
}

pub(crate) fn stored_context_node_id(node: &StoredContextNode) -> String {
    let mut digest = Sha256::new();
    digest.update(b"jcode-lcm-node-v2");
    {
        let mut field = |bytes: &[u8]| {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        field(&node.schema_version.to_le_bytes());
        field(&node.level.to_le_bytes());
        field(node.source_session_id.as_bytes());
        field(&(node.source_message_ids.len() as u64).to_le_bytes());
        for id in &node.source_message_ids {
            field(id.as_bytes());
        }
        field(node.source_sha256.as_bytes());
        field(node.source_runtime_identity_sha256.as_bytes());
        field(&(node.child_node_ids.len() as u64).to_le_bytes());
        for id in &node.child_node_ids {
            field(id.as_bytes());
        }
        field(node.summary_text.as_bytes());
        field(node.summarizer_model.as_bytes());
        field(node.summarizer_provider.as_bytes());
        field(node.summarizer_route.as_bytes());
        field(node.summarizer_runtime_identity_sha256.as_bytes());
        field(&node.prompt_schema_version.to_le_bytes());
    }
    format!("lcm-{:x}", digest.finalize())
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_stored_context_node_id(node: &StoredContextNode) -> String {
    stored_context_node_id(node)
}

pub(crate) fn exact_runtime_identity_sha256(
    identity: &jcode_provider_core::ExactRuntimeIdentity,
) -> anyhow::Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(identity)?)
    ))
}

fn validate_context_node_source_proofs(
    current_session_id: &str,
    current_runtime_identity: Option<&jcode_provider_core::ExactRuntimeIdentity>,
    messages: &[StoredMessage],
    nodes: &[StoredContextNode],
    frontier: Option<&StoredContextFrontier>,
) -> Result<(), String> {
    let expected_runtime_identity_sha256 = current_runtime_identity
        .ok_or_else(|| "context graph owner runtime identity is missing".to_string())
        .and_then(|identity| {
            exact_runtime_identity_sha256(identity).map_err(|err| err.to_string())
        })?;
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<std::collections::HashMap<_, _>>();
    let mut active_reachable = std::collections::HashSet::new();
    fn mark_active_reachable<'a>(
        id: &'a str,
        by_id: &std::collections::HashMap<&'a str, &'a StoredContextNode>,
        reachable: &mut std::collections::HashSet<&'a str>,
    ) {
        if !reachable.insert(id) {
            return;
        }
        if let Some(node) = by_id.get(id) {
            for child in &node.child_node_ids {
                mark_active_reachable(child, by_id, reachable);
            }
        }
    }
    if let Some(frontier) = frontier {
        for id in &frontier.active_node_ids {
            mark_active_reachable(id, &by_id, &mut active_reachable);
        }
    }

    for node in nodes {
        let summary_sha256 = node
            .summary_sha256
            .as_deref()
            .ok_or_else(|| format!("context node {} has no portable summary proof", node.id))?;
        let actual = format!("{:x}", Sha256::digest(node.summary_text.as_bytes()));
        if summary_sha256 != actual {
            return Err(format!(
                "context node {} portable summary proof is invalid",
                node.id
            ));
        }
        // Rewind may remove an archived leaf's raw source from the active
        // transcript, but every committed node remains content-addressed. Check
        // its immutable identity before any active-branch-only source checks.
        let expected_id = stored_context_node_id(node);
        if node.id != expected_id {
            return Err(format!(
                "context node {} has an invalid immutable identity",
                node.id
            ));
        }
        // Inactive nodes may belong to a previous identity domain and remain
        // durable as immutable forensic evidence. Only the active projection
        // must prove ownership by the session's current exact identity.
        if active_reachable.contains(node.id.as_str())
            && node.source_runtime_identity_sha256 != expected_runtime_identity_sha256
        {
            return Err(format!(
                "context node {} source runtime identity does not match its owning session",
                node.id
            ));
        }
        if node.level == 0 {
            if node.source_message_ids.is_empty() {
                if node.source_session_id != current_session_id {
                    continue;
                }
                return Err(format!("context leaf {} has no canonical source", node.id));
            }
            // Rewind moves only the active branch/frontier. Archived leaves may
            // refer to canonical raw messages excluded from the active projection,
            // so active-history proof checks apply only to nodes reachable from the
            // frontier. Structural and summary proofs still cover every immutable
            // committed node.
            if !active_reachable.contains(node.id.as_str()) {
                continue;
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

fn active_reachable_context_node_ids<'a>(
    nodes: &'a [StoredContextNode],
    frontier: Option<&'a StoredContextFrontier>,
) -> std::collections::HashSet<&'a str> {
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<std::collections::HashMap<_, _>>();
    let mut reachable = std::collections::HashSet::new();
    fn mark<'a>(
        id: &'a str,
        by_id: &std::collections::HashMap<&'a str, &'a StoredContextNode>,
        reachable: &mut std::collections::HashSet<&'a str>,
    ) {
        if !reachable.insert(id) {
            return;
        }
        if let Some(node) = by_id.get(id) {
            for child in &node.child_node_ids {
                mark(child, by_id, reachable);
            }
        }
    }
    if let Some(frontier) = frontier {
        for id in &frontier.active_node_ids {
            mark(id, &by_id, &mut reachable);
        }
    }
    reachable
}

fn validate_imported_context_ancestor_proofs(session: &Session) -> Result<(), String> {
    let active_reachable = active_reachable_context_node_ids(
        &session.context_nodes,
        session.context_frontier.as_ref(),
    );
    for node in &session.context_nodes {
        if node.level != 0
            || node.source_session_id == session.id
            || !active_reachable.contains(node.id.as_str())
        {
            continue;
        }
        validate_recorded_ancestor_chain(
            &session.id,
            session.parent_id.as_deref(),
            &node.source_session_id,
        )
        .map_err(|error| format!("imported context node {} {error}", node.id))?;
        let parent = Session::load(&node.source_session_id).map_err(|error| {
            format!(
                "imported context node {} ancestor session {} is unavailable: {error}",
                node.id, node.source_session_id
            )
        })?;
        if parent.id != node.source_session_id {
            return Err(format!(
                "imported context node {} loaded ancestor identity mismatch",
                node.id
            ));
        }
        let parent_identity = parent.exact_runtime_identity.as_ref().ok_or_else(|| {
            format!(
                "imported context node {} ancestor identity is missing",
                node.id
            )
        })?;
        let current_identity = session.exact_runtime_identity.as_ref().ok_or_else(|| {
            format!(
                "imported context node {} current identity is missing",
                node.id
            )
        })?;
        if parent_identity != current_identity {
            return Err(format!(
                "imported context node {} ancestor runtime identity does not match current session",
                node.id
            ));
        }
        let parent_identity_sha256 =
            exact_runtime_identity_sha256(parent_identity).map_err(|error| error.to_string())?;
        if node.source_runtime_identity_sha256 != parent_identity_sha256 {
            return Err(format!(
                "imported context node {} recorded ancestor runtime identity proof is invalid",
                node.id
            ));
        }
        let parent_messages = parent.active_stored_messages();
        let parent_message_ids = parent_messages
            .iter()
            .map(|message| message.id.clone())
            .collect::<Vec<_>>();
        if node.source_message_ids != parent_message_ids {
            return Err(format!(
                "imported context node {} ancestor message order proof is invalid",
                node.id
            ));
        }
        let parent_sha256 =
            stored_messages_sha256(&parent_messages).map_err(|error| error.to_string())?;
        if node.source_sha256 != parent_sha256 {
            return Err(format!(
                "imported context node {} ancestor raw byte proof is invalid",
                node.id
            ));
        }
    }
    Ok(())
}

fn validate_recorded_ancestor_chain(
    current_session_id: &str,
    parent_id: Option<&str>,
    source_session_id: &str,
) -> Result<(), String> {
    let mut next = parent_id.map(str::to_string).ok_or_else(|| {
        format!(
            "source {source_session_id} is not in the current session's recorded ancestor chain"
        )
    })?;
    let mut seen = std::collections::HashSet::new();
    seen.insert(current_session_id.to_string());
    loop {
        if next == source_session_id {
            return Ok(());
        }
        if !seen.insert(next.clone()) {
            return Err(format!(
                "source {source_session_id} is behind a cyclic recorded ancestor chain"
            ));
        }
        let ancestor = Session::load(&next).map_err(|error| {
            format!(
                "source {source_session_id} recorded ancestor chain cannot load {next}: {error}"
            )
        })?;
        next = ancestor.parent_id.ok_or_else(|| {
            format!(
                "source {source_session_id} is not in the current session's recorded ancestor chain"
            )
        })?;
    }
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

mod context_graph;
mod lifecycle;
mod messages;
mod persistence_state;

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
    archived_message_ids: Vec<String>,
    #[serde(default)]
    provider_image_char_budget: Option<usize>,
    #[serde(default)]
    provider_tool_use_suppressed_message_ids: Vec<String>,
    #[serde(default)]
    journal_sequence: u64,
    #[serde(default)]
    journal_watermark: u64,
    #[serde(default)]
    persistence_revision: u64,
    #[serde(default)]
    context_nodes: Vec<StoredContextNode>,
    #[serde(default)]
    context_frontier: Option<StoredContextFrontier>,
    #[serde(default)]
    compaction: Option<StoredCompactionState>,
    #[serde(default)]
    provider_session_id: Option<String>,
    #[serde(default)]
    provider_session_identity: Option<jcode_provider_core::ExactRuntimeIdentity>,
    #[serde(default)]
    provider_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    route_api_method: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default)]
    exact_runtime_identity: Option<jcode_provider_core::ExactRuntimeIdentity>,
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
