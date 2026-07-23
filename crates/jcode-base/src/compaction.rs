//! Background compaction for conversation context management
//!
//! When context reaches 80% of the limit, kicks off background summarization.
//! User continues chatting while summary is generated. When ready, seamlessly
//! swaps in the compacted context.
//!
//! The CompactionManager does NOT store its own copy of messages. Instead,
//! callers pass `&[Message]` references when needed. The manager tracks how
//! many messages from the front have been compacted via `compacted_count`.
//!
//! ## Compaction Modes
//!
//! - **Reactive** (default): compact when context hits a fixed threshold (80%).
//! - **Proactive**: compact early based on predicted EWMA token growth rate.
//! - **Semantic**: compact based on embedding-detected topic shifts and
//!   relevance scoring. Falls back to proactive if embeddings are unavailable.

use crate::message::{ContentBlock, Message, Role};
use crate::provider::Provider;
use crate::provider::openai_request::{
    openai_encrypted_content_fallback_summary, openai_encrypted_content_is_sendable,
};
use anyhow::Result;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use tokio::task::JoinHandle;

fn message_fingerprint(messages: &[Message]) -> Option<u64> {
    messages
        .iter()
        .map(jcode_message_types::stable_message_hash)
        .reduce(jcode_message_types::extend_stable_hash)
}

fn stored_message_prefix_sha256(messages: &[crate::session::StoredMessage]) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(messages)?)
    ))
}

fn lcm_node_id(
    source: &PendingLcmSource,
    summary_text: &str,
    prompt_schema_version: u32,
) -> String {
    let mut digest = Sha256::new();
    digest.update(source.session_id.as_bytes());
    digest.update(source.generation.to_le_bytes());
    digest.update(source.next_node_sequence.to_le_bytes());
    digest.update(source.source_sha256.as_bytes());
    digest.update(source.summarizer_route.as_bytes());
    digest.update(prompt_schema_version.to_le_bytes());
    digest.update(summary_text.as_bytes());
    format!("lcm-{:x}", digest.finalize())
}

fn lcm_atomic_parent_id(
    generation: u64,
    sequence: u64,
    children_sha256: &str,
    summarizer_route: &str,
    summary_text: &str,
    prompt_schema_version: u32,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"jcode-lcm-parent-v1");
    digest.update(generation.to_le_bytes());
    digest.update(sequence.to_le_bytes());
    digest.update(children_sha256.as_bytes());
    digest.update(summarizer_route.as_bytes());
    digest.update(prompt_schema_version.to_le_bytes());
    digest.update(summary_text.as_bytes());
    format!("lcm-{:x}", digest.finalize())
}

fn lcm_route_spec(
    session: &crate::session::Session,
    inherited_model: &str,
    configured_model: Option<&str>,
) -> String {
    configured_model.map(str::to_string).unwrap_or_else(|| {
        crate::provider::MultiProvider::model_switch_request_for_session_route(
            session.model.as_deref().unwrap_or(inherited_model),
            session.provider_key.as_deref(),
            session.route_api_method.as_deref(),
        )
    })
}

fn lcm_inherited_route_selection(
    session: &crate::session::Session,
    inherited_model: &str,
) -> Option<crate::provider::RouteSelection> {
    let api_method = session.route_api_method.as_deref()?.trim();
    if api_method.is_empty() {
        return None;
    }
    let method = crate::provider::ModelRouteApiMethod::parse(api_method);
    let runtime_key = crate::provider::RuntimeKey::from_api_method(
        &method,
        session.provider_key.as_deref().unwrap_or_default(),
    );
    let mut model = session
        .model
        .as_deref()
        .unwrap_or(inherited_model)
        .trim()
        .to_string();
    let mut provider_label = session.provider_key.clone().unwrap_or_default();
    let openrouter_preference = matches!(runtime_key, crate::provider::RuntimeKey::OpenRouter)
        .then(|| {
            model
                .rsplit_once('@')
                .map(|(catalog, provider)| (catalog.to_string(), provider.to_string()))
        })
        .flatten()
        .filter(|(_, provider)| !provider.trim().is_empty());
    if let Some((catalog_model, preferred_provider)) = openrouter_preference {
        model = catalog_model;
        provider_label = preferred_provider;
    }
    Some(crate::provider::RouteSelection {
        model,
        runtime_key,
        api_method: api_method.to_string(),
        provider_label,
        detail: String::new(),
    })
}

fn set_lcm_inherited_route(
    provider: &dyn Provider,
    selection: &crate::provider::RouteSelection,
) -> Result<()> {
    match provider.set_route_selection(selection) {
        Ok(()) => Ok(()),
        Err(first_error) => {
            provider.on_auth_changed_preserve_current_provider();
            provider
                .set_route_selection(selection)
                .map_err(|second_error| {
                    anyhow::anyhow!(
                        "{} (typed route retried after auth refresh: {})",
                        first_error,
                        second_error
                    )
                })
        }
    }
}

fn lcm_route_policy_fingerprint(
    session: &crate::session::Session,
    configured_model: Option<&str>,
) -> String {
    lcm_route_policy_fingerprint_with_auth_generation(
        session,
        configured_model,
        crate::provider::pricing::auth_pricing_generation(),
    )
}

fn lcm_route_policy_fingerprint_with_auth_generation(
    session: &crate::session::Session,
    configured_model: Option<&str>,
    auth_generation: u64,
) -> String {
    lcm_route_policy_fingerprint_with_runtime_identity(
        session,
        configured_model,
        auth_generation,
        crate::auth::claude::active_account_label().as_deref(),
        crate::auth::codex::active_account_label().as_deref(),
    )
}

fn lcm_route_policy_fingerprint_with_runtime_identity(
    session: &crate::session::Session,
    configured_model: Option<&str>,
    auth_generation: u64,
    active_anthropic_account: Option<&str>,
    active_openai_account: Option<&str>,
) -> String {
    let config = crate::config::config();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "configured_model": configured_model,
        "engine": config.compaction.engine,
        "provider_config": config.provider,
        "named_providers": config.providers,
        "auth_generation": auth_generation,
        "active_anthropic_account": active_anthropic_account,
        "active_openai_account": active_openai_account,
        "session_model": session.model,
        "session_provider_key": session.provider_key,
        "session_api_method": session.route_api_method,
    }))
    .unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

fn lcm_summary_with_retrieval_anchor(
    summary: &str,
    source_session_id: &str,
    source_message_ids: &[String],
) -> String {
    let range = match (source_message_ids.first(), source_message_ids.last()) {
        (Some(first), Some(last)) => format!("{first}..{last}"),
        _ => "imported-session-root".to_string(),
    };
    format!(
        "{}\n\n## Retrieval anchor\nUse `conversation_search` against source session `{source_session_id}` and canonical message range `{range}` when exact raw details are needed.",
        summary.trim()
    )
}

pub use jcode_compaction_core::{
    CHARS_PER_TOKEN, COMPACTION_THRESHOLD, CRITICAL_THRESHOLD, CompactionAction, CompactionEvent,
    CompactionStats, DEFAULT_TOKEN_BUDGET, EMBED_MAX_CHARS_PER_MSG, EMBEDDING_HISTORY_WINDOW,
    EMERGENCY_IMAGE_MAX_CHARS, EMERGENCY_TOOL_RESULT_MAX_CHARS, MANUAL_COMPACT_MIN_THRESHOLD,
    MIN_TURNS_TO_KEEP, PAYLOAD_IMAGE_CHAR_BUDGET, RECENT_TURNS_TO_KEEP,
    SEMANTIC_EMBED_CACHE_CAPACITY, SUMMARY_PROMPT, SYSTEM_OVERHEAD_TOKENS, Summary,
    TOKEN_HISTORY_WINDOW, build_compaction_conversation_text, build_compaction_prompt,
    build_emergency_summary_text, compacted_summary_text_block, content_char_count,
    effective_context_tokens_from_usage, emergency_strip_large_images,
    emergency_truncate_large_payloads, estimate_compaction_tokens,
    is_request_payload_too_large_error, mean_embedding, message_char_count, safe_compaction_cutoff,
    semantic_cache_key, semantic_goal_text, semantic_message_text, strip_large_images_in_contents,
    summary_payload_char_count,
};

#[cfg(not(test))]
const HARD_THRESHOLD_PENDING_WAIT_MS: u64 = 15_000;
#[cfg(test)]
const HARD_THRESHOLD_PENDING_WAIT_MS: u64 = 350;
#[cfg(not(test))]
const HARD_THRESHOLD_PENDING_POLL_MS: u64 = 50;
#[cfg(test)]
const HARD_THRESHOLD_PENDING_POLL_MS: u64 = 10;

/// Result from background compaction task
struct CompactionResult {
    summary_text: String,
    atomic_parent_summaries: Vec<String>,
    openai_encrypted_content: Option<String>,
    covers_up_to_turn: usize,
    duration_ms: u64,
    summarized_messages: usize,
}

struct PendingLcmSource {
    session_id: String,
    base_generation: u64,
    generation: u64,
    next_node_sequence: u64,
    covered_message_count: usize,
    source_message_ids: Vec<String>,
    input_proof_ids: Vec<String>,
    source_sha256: String,
    source_prefix_sha256: String,
    covered_through_message_id: String,
    prior_active_nodes: Vec<crate::session::StoredContextNode>,
    node_level: u32,
    child_nodes: Vec<crate::session::StoredContextNode>,
    summarizer_model: String,
    summarizer_provider: String,
    summarizer_route: String,
    configured_model: Option<String>,
    cutoff: usize,
    source_fingerprint: Option<u64>,
    pre_tokens: u64,
    trigger: String,
    route_policy_fingerprint: String,
}

impl PendingLcmSource {
    fn matches_runtime_policy(
        &self,
        session: &crate::session::Session,
        policy: &crate::config::CompactionConfig,
    ) -> bool {
        if policy.engine != crate::config::CompactionEngine::Lcm {
            return false;
        }
        if policy.model != self.configured_model {
            return false;
        }
        if self.trigger == "critical_active_route" {
            return lcm_route_spec(
                session,
                session
                    .model
                    .as_deref()
                    .unwrap_or(self.summarizer_model.as_str()),
                None,
            ) == self.summarizer_route;
        }
        self.configured_model.is_some()
            || lcm_route_spec(
                session,
                session
                    .model
                    .as_deref()
                    .unwrap_or(self.summarizer_model.as_str()),
                None,
            ) == self.summarizer_route
    }
}

struct PreparedLcmContext {
    transaction: crate::session::ContextGraphTransaction,
    projection: crate::session::StoredCompactionState,
    summary: Summary,
    covered_message_count: usize,
    cutoff: usize,
    pre_tokens: u64,
    duration_ms: u64,
    trigger: String,
    messages_dropped: Option<usize>,
    configured_model: Option<String>,
    effective_route: String,
    allow_active_route_fallback: bool,
    route_policy_fingerprint: String,
}

struct CompactionOutcomeLog<'a> {
    trigger: &'a str,
    pre_tokens: u64,
    post_tokens: u64,
    messages_compacted: usize,
    messages_dropped: Option<usize>,
    duration_ms: u64,
    all_messages: &'a [Message],
}

struct HardThresholdWait {
    waited_ms: u64,
    applied: bool,
    timed_out: bool,
}

/// Rolling character-count estimate for the active (non-compacted) message
/// suffix.
///
/// Token estimation needs the size of the live message tail without rescanning
/// the entire history on every call, so this caches that sum next to a dirty
/// flag. The value and the flag must always move together: previously they were
/// two independent `CompactionManager` fields, and a code path that updated one
/// without the other silently corrupted token accounting. Keeping the raw
/// fields private and forcing every mutation through these named operations
/// makes that class of bug unrepresentable.
#[derive(Debug, Clone, Default)]
struct ActiveCharEstimate {
    chars: usize,
    dirty: bool,
}

impl ActiveCharEstimate {
    /// The currently cached character count. Only trustworthy when not dirty;
    /// readers must consult [`Self::is_dirty`] (and any external invariants)
    /// before relying on it.
    fn value(&self) -> usize {
        self.chars
    }

    /// Whether the cached value is stale and must be recomputed from history.
    fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the cached value stale so the next read recomputes from history.
    fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Record an exact, freshly computed count as the trusted value.
    fn set_exact(&mut self, chars: usize) {
        self.chars = chars;
        self.dirty = false;
    }

    /// Extend a trusted count by a newly appended message's characters.
    ///
    /// Mirrors the append-only fast path: the prior value is assumed accurate,
    /// so the running sum stays trusted (dirty cleared).
    fn append_exact(&mut self, chars: usize) {
        self.chars = self.chars.saturating_add(chars);
        self.dirty = false;
    }

    /// Reset to zero after a restore/clamp. Stays dirty when there may be active
    /// messages whose characters have not been measured yet.
    fn reset_pending(&mut self, maybe_has_active: bool) {
        self.chars = 0;
        self.dirty = maybe_has_active;
    }
}

/// Manages background compaction of conversation context.
///
/// Does NOT own message data. The caller owns the messages and passes
/// references into methods that need them. After compaction, the manager
/// records `compacted_count` — the number of leading messages that have
/// been summarized and should be skipped when building API payloads.
pub struct CompactionManager {
    /// Storage/materialization engine. Trigger policy remains in `mode`.
    engine: crate::config::CompactionEngine,

    /// Number of leading messages that have been compacted into the summary.
    /// When building API messages, skip the first `compacted_count` messages.
    compacted_count: usize,

    /// Active summary (if we've compacted before)
    active_summary: Option<Summary>,

    /// Rolling char estimate for the active (non-compacted) message suffix.
    ///
    /// In the common append-only case this is maintained incrementally, so token
    /// estimation does not need to rescan the entire active history every time.
    /// Bundled with its own dirty flag so the value and staleness can never
    /// drift apart (see [`ActiveCharEstimate`]).
    active_chars: ActiveCharEstimate,

    /// Background compaction task handle
    pending_task: Option<JoinHandle<Result<CompactionResult>>>,

    /// User-facing trigger label for the currently running background compaction.
    pending_trigger: Option<String>,

    /// Turn index (relative to uncompacted messages) where pending compaction will cut off
    pending_cutoff: usize,

    /// Stable cache-relevant fingerprint of the exact source prefix captured by
    /// the pending task. Message count alone cannot detect same-length rewrites.
    pending_source_fingerprint: Option<u64>,

    /// Canonical source captured for an in-flight LCM leaf job.
    pending_lcm_source: Option<PendingLcmSource>,

    /// Completed LCM candidate waiting for durable commit. It is never visible
    /// through provider materialization before commit succeeds.
    prepared_lcm_context: Option<PreparedLcmContext>,

    /// Total turns seen (for tracking)
    total_turns: usize,

    /// When true, session restore/reseed has just loaded old history and
    /// compaction must stay disabled until a genuinely new message is added.
    suppress_compaction_until_new_message: bool,

    /// Token budget
    token_budget: usize,

    /// Provider-reported input token usage from the latest request.
    /// Used to trigger compaction with real token counts instead of only heuristics.
    observed_input_tokens: Option<u64>,

    /// Last compaction event (if any)
    last_compaction: Option<CompactionEvent>,

    // ── Mode & strategy ────────────────────────────────────────────────────
    /// Active compaction mode (set from config at construction)
    mode: crate::config::CompactionMode,

    /// Config snapshot for mode-specific parameters
    compaction_config: crate::config::CompactionConfig,

    // ── Proactive mode state ───────────────────────────────────────────────
    /// Rolling window of observed token counts, one entry per turn snapshot.
    /// Used to compute EWMA growth rate for proactive compaction.
    token_history: VecDeque<u64>,

    /// Total turns elapsed since the last successful compaction.
    /// Used as a cooldown anti-signal.
    turns_since_last_compact: usize,

    // ── Semantic mode state ────────────────────────────────────────────────
    /// Per-turn embedding snapshots for topic-shift detection.
    /// Each entry is the L2-normalized embedding of the last assistant message
    /// of that turn (truncated to EMBED_MAX_CHARS_PER_MSG for speed).
    embedding_history: VecDeque<Vec<f32>>,

    /// Local cache for semantic compaction embeddings keyed by truncated-text hash.
    /// Stores both successful embeddings and failed lookups (`None`) so repeated
    /// semantic scans do not redo the same work.
    semantic_embed_cache: HashMap<u64, (Option<Vec<f32>>, u64)>,

    /// Monotonic recency counter for the semantic embedding cache LRU.
    semantic_embed_cache_counter: u64,
}

impl CompactionManager {
    pub fn new() -> Self {
        let cfg = crate::config::config().compaction.clone();
        let mode = cfg.mode.clone();
        Self {
            engine: cfg.engine.clone(),
            compacted_count: 0,
            active_summary: None,
            active_chars: ActiveCharEstimate::default(),
            pending_task: None,
            pending_trigger: None,
            pending_cutoff: 0,
            pending_source_fingerprint: None,
            pending_lcm_source: None,
            prepared_lcm_context: None,
            total_turns: 0,
            suppress_compaction_until_new_message: false,
            token_budget: DEFAULT_TOKEN_BUDGET,
            observed_input_tokens: None,
            last_compaction: None,
            mode,
            compaction_config: cfg,
            token_history: VecDeque::with_capacity(TOKEN_HISTORY_WINDOW + 1),
            turns_since_last_compact: 0,
            embedding_history: VecDeque::with_capacity(EMBEDDING_HISTORY_WINDOW + 1),
            semantic_embed_cache: HashMap::with_capacity(SEMANTIC_EMBED_CACHE_CAPACITY),
            semantic_embed_cache_counter: 0,
        }
    }

    /// Reset all compaction state
    pub fn reset(&mut self) {
        self.cancel_pending_work();
        *self = Self::new();
    }

    fn cancel_pending_work(&mut self) {
        if let Some(task) = self.pending_task.take() {
            task.abort();
        }
        self.pending_trigger = None;
        self.pending_cutoff = 0;
        self.pending_source_fingerprint = None;
        self.pending_lcm_source = None;
        self.prepared_lcm_context = None;
    }

    pub fn with_budget(mut self, budget: usize) -> Self {
        self.token_budget = budget;
        self
    }

    /// Update the token budget (e.g., when model changes)
    pub fn set_budget(&mut self, budget: usize) {
        self.token_budget = budget;
    }

    /// Get current token budget
    pub fn token_budget(&self) -> usize {
        self.token_budget
    }

    /// Notify the manager that a message was added.
    ///
    /// Legacy callers that do not provide the message content keep turn counts
    /// correct, but mark the rolling char estimate dirty so the next token
    /// estimate will resync from the provided history slice.
    pub fn notify_message_added(&mut self) {
        self.total_turns += 1;
        self.suppress_compaction_until_new_message = false;
        self.active_chars.invalidate();
    }

    /// Notify the manager that a message was added and update the rolling char
    /// estimate incrementally.
    pub fn notify_message_added_with(&mut self, message: &Message) {
        self.notify_message_added_blocks(&message.content);
    }

    pub fn notify_message_added_blocks(&mut self, content: &[ContentBlock]) {
        self.total_turns += 1;
        self.suppress_compaction_until_new_message = false;
        self.active_chars.append_exact(content_char_count(content));
    }

    /// Backward-compatible alias for `notify_message_added`.
    /// Accepts (and ignores) the message — callers that haven't been
    /// updated yet can still call `add_message(msg)`.
    pub fn add_message(&mut self, message: Message) {
        self.notify_message_added_with(&message);
    }

    /// Seed the manager from already-existing history that was restored from
    /// disk or otherwise replayed into memory.
    ///
    /// This updates turn counts but deliberately suppresses compaction until a
    /// genuinely new message is added after the restore. Restoring history must
    /// not itself trigger compaction.
    pub fn seed_restored_messages(&mut self, count: usize) {
        self.total_turns = count;
        self.suppress_compaction_until_new_message = count > 0;
        self.active_chars.reset_pending(count > 0);
    }

    /// Seed the manager from already-existing history with an exact rolling char
    /// estimate for the active suffix.
    pub fn seed_restored_messages_with(&mut self, all_messages: &[Message]) {
        self.total_turns = all_messages.len();
        self.suppress_compaction_until_new_message = !all_messages.is_empty();
        self.active_chars
            .set_exact(all_messages.iter().map(message_char_count).sum());
    }

    pub fn seed_restored_stored_messages_with(
        &mut self,
        all_messages: &[crate::session::StoredMessage],
    ) {
        self.total_turns = all_messages.len();
        self.suppress_compaction_until_new_message = !all_messages.is_empty();
        self.active_chars.set_exact(
            all_messages
                .iter()
                .map(|message| content_char_count(&message.content))
                .sum(),
        );
    }

    /// Restore a previously persisted compacted view.
    pub fn restore_persisted_state(
        &mut self,
        state: &crate::session::StoredCompactionState,
        total_messages: usize,
    ) {
        if let Some(task) = self.pending_task.take() {
            task.abort();
        }
        self.pending_trigger = None;
        self.pending_cutoff = 0;
        self.pending_source_fingerprint = None;
        self.pending_lcm_source = None;
        self.prepared_lcm_context = None;
        self.observed_input_tokens = None;
        self.last_compaction = None;
        self.token_history.clear();
        self.turns_since_last_compact = 0;
        self.embedding_history.clear();
        self.semantic_embed_cache.clear();
        self.semantic_embed_cache_counter = 0;
        self.total_turns = total_messages;
        if self.engine == crate::config::CompactionEngine::Lcm {
            // A process may restart after config was already changed to LCM, so
            // `synchronize_engine` will not observe an engine transition. Never
            // restore a rolling or provider-native projection in that case. The
            // raw journal remains canonical and the first LCM leaf starts at zero.
            self.compacted_count = 0;
            self.active_summary = None;
            self.active_chars.reset_pending(total_messages > 0);
            self.suppress_compaction_until_new_message = total_messages > 0;
            return;
        }
        self.compacted_count = state.compacted_count.min(total_messages);
        self.active_chars
            .reset_pending(total_messages > self.compacted_count);
        self.active_summary = Some(Summary {
            text: state.summary_text.clone(),
            openai_encrypted_content: state.openai_encrypted_content.clone(),
            covers_up_to_turn: state.covers_up_to_turn,
            original_turn_count: state.original_turn_count,
        });
        self.suppress_compaction_until_new_message = total_messages > 0;
    }

    /// Restore persisted compaction state and compute the active-suffix char
    /// estimate from the provided full message list.
    pub fn restore_persisted_state_with(
        &mut self,
        state: &crate::session::StoredCompactionState,
        all_messages: &[Message],
    ) {
        self.restore_persisted_state(state, all_messages.len());
        self.active_chars.set_exact(
            self.active_messages(all_messages)
                .iter()
                .map(message_char_count)
                .sum(),
        );
    }

    pub fn restore_persisted_stored_state_with(
        &mut self,
        state: &crate::session::StoredCompactionState,
        all_messages: &[crate::session::StoredMessage],
    ) {
        self.restore_persisted_state(state, all_messages.len());
        let start = self.compacted_count.min(all_messages.len());
        self.active_chars.set_exact(
            all_messages[start..]
                .iter()
                .map(|message| content_char_count(&message.content))
                .sum(),
        );
    }

    /// Export the currently active compacted view for persistence.
    pub fn persisted_state(&self) -> Option<crate::session::StoredCompactionState> {
        self.active_summary
            .as_ref()
            .map(|summary| crate::session::StoredCompactionState {
                summary_text: summary.text.clone(),
                openai_encrypted_content: summary.openai_encrypted_content.clone(),
                covers_up_to_turn: summary.covers_up_to_turn,
                original_turn_count: summary.original_turn_count,
                compacted_count: self.compacted_count,
            })
    }

    /// Drop provider-native OpenAI compaction state when it can no longer be
    /// replayed within OpenAI's per-string request limit. The compacted prefix
    /// remains compacted, but future requests use a small text fallback instead
    /// of bricking the session with an oversized `encrypted_content` field.
    pub fn discard_oversized_openai_native_compaction(&mut self) -> bool {
        let Some(summary) = self.active_summary.as_mut() else {
            return false;
        };
        let Some(encrypted_content) = summary.openai_encrypted_content.as_ref() else {
            return false;
        };
        if openai_encrypted_content_is_sendable(encrypted_content) {
            return false;
        }

        let encrypted_content_len = encrypted_content.len();
        crate::logging::warn(&format!(
            "[compaction] Discarding oversized OpenAI native compaction payload ({} chars)",
            encrypted_content_len,
        ));
        summary.openai_encrypted_content = None;
        let fallback = openai_encrypted_content_fallback_summary(encrypted_content_len);
        if summary.text.trim().is_empty() {
            summary.text = fallback;
        } else if !summary
            .text
            .contains("OpenAI native compaction state was discarded")
        {
            summary.text.push_str("\n\n");
            summary.text.push_str(&fallback);
        }
        self.observed_input_tokens = None;
        true
    }

    // ── Token snapshot (proactive mode) ────────────────────────────────────

    /// Record the observed token count after a completed turn.
    ///
    /// Called by the agent after `update_compaction_usage_from_stream`.
    /// Pushes the value into the rolling history window used by the proactive
    /// and semantic modes. Also increments the cooldown counter.
    pub fn push_token_snapshot(&mut self, tokens: u64) {
        self.token_history.push_back(tokens);
        if self.token_history.len() > TOKEN_HISTORY_WINDOW {
            self.token_history.pop_front();
        }
        self.turns_since_last_compact += 1;
    }

    /// Record an embedding snapshot for the current turn (semantic mode).
    ///
    /// `text` should be a short representation of the turn's assistant output
    /// (first EMBED_MAX_CHARS_PER_MSG chars). Silently skipped if the
    /// embedding model is unavailable.
    pub fn push_embedding_snapshot(&mut self, text: &str) {
        let snippet: String = text.chars().take(EMBED_MAX_CHARS_PER_MSG).collect();
        if let Some(emb) = self.cached_semantic_embedding(&snippet) {
            self.embedding_history.push_back(emb);
            if self.embedding_history.len() > EMBEDDING_HISTORY_WINDOW {
                self.embedding_history.pop_front();
            }
        }
    }

    // ── Anti-signal guard (shared by proactive + semantic) ──────────────────

    /// Returns `true` when any anti-signal fires and we should NOT compact
    /// proactively right now.
    ///
    /// Anti-signals are universal guards applied before the mode-specific
    /// trigger logic. They prevent wasted work and respect user intent.
    fn anti_signals_block(&self, all_messages: &[Message]) -> bool {
        let cfg = &self.compaction_config;

        // 1. Already compacting — never double-trigger.
        if self.pending_task.is_some() {
            return true;
        }

        // 2. Context below the proactive floor — too early regardless of trend.
        let usage = self.context_usage_with(all_messages);
        if usage < cfg.proactive_floor {
            return true;
        }

        // 3. Not enough token history to project from.
        if self.token_history.len() < cfg.min_samples {
            return true;
        }

        // 4. Growth has stalled: last stall_window snapshots show no increase.
        //    If tokens haven't grown, there's no urgency.
        if self.token_history.len() >= cfg.stall_window {
            let recent: Vec<u64> = self
                .token_history
                .iter()
                .rev()
                .take(cfg.stall_window)
                .cloned()
                .collect();
            let oldest = recent[recent.len() - 1];
            let newest = recent[0];
            if newest <= oldest {
                return true;
            }
        }

        // 5. Cooldown: too soon after the last compaction.
        if self.turns_since_last_compact < cfg.min_turns_between_compactions {
            return true;
        }

        false
    }

    // ── Proactive mode trigger ──────────────────────────────────────────────

    /// Returns `true` if the proactive strategy thinks we should compact now.
    ///
    /// Uses an EWMA over the token history to project forward `lookahead_turns`
    /// turns. If the projected token count would exceed the 80% threshold,
    /// it's time to compact before we get there.
    fn should_compact_proactively(&self, all_messages: &[Message]) -> bool {
        if self.anti_signals_block(all_messages) {
            return false;
        }

        let cfg = &self.compaction_config;
        let budget = self.token_budget as f64;
        let threshold = COMPACTION_THRESHOLD as f64 * budget;

        // Compute EWMA of per-turn token deltas.
        // We need at least 2 snapshots to get a delta.
        let snapshots: Vec<u64> = self.token_history.iter().cloned().collect();
        if snapshots.len() < 2 {
            return false;
        }

        let alpha = cfg.ewma_alpha as f64;
        let mut ewma_delta: f64 = (snapshots[1] as f64) - (snapshots[0] as f64);
        ewma_delta = ewma_delta.max(0.0);
        for i in 2..snapshots.len() {
            let delta = ((snapshots[i] as f64) - (snapshots[i - 1] as f64)).max(0.0);
            ewma_delta = alpha * delta + (1.0 - alpha) * ewma_delta;
        }
        let Some(current) = snapshots.last().copied().map(|value| value as f64) else {
            return false;
        };
        let projected = current + ewma_delta * cfg.lookahead_turns as f64;

        crate::logging::info(&format!(
            "[compaction/proactive] current={:.0} ewma_delta={:.1}/turn projected@{}turns={:.0} threshold={:.0}",
            current, ewma_delta, cfg.lookahead_turns, projected, threshold
        ));

        projected >= threshold
    }

    // ── Semantic mode trigger ───────────────────────────────────────────────

    /// Returns `true` if the semantic strategy detects a topic shift or
    /// predicts we should compact now.
    ///
    /// Topic-shift detection: compares the mean embedding of the oldest half
    /// of the history window against the newest half. A low cosine similarity
    /// between the two clusters indicates a topic boundary was crossed —
    /// the previous topic is complete and safe to summarize.
    ///
    /// Falls back to proactive logic if embeddings are unavailable.
    fn should_compact_semantic(&self, all_messages: &[Message]) -> bool {
        if self.anti_signals_block(all_messages) {
            return false;
        }

        // Need enough embedding history to split into two halves.
        let history_len = self.embedding_history.len();
        if history_len < 4 {
            // Fall back to proactive trigger.
            return self.should_compact_proactively(all_messages);
        }

        let cfg = &self.compaction_config;
        let half = history_len / 2;

        let old_embeddings: Vec<&Vec<f32>> = self.embedding_history.iter().take(half).collect();
        let new_embeddings: Vec<&Vec<f32>> = self.embedding_history.iter().skip(half).collect();

        let dim = old_embeddings[0].len();

        // Compute mean embedding for each half.
        let mean_old = mean_embedding(&old_embeddings, dim);
        let mean_new = mean_embedding(&new_embeddings, dim);

        let similarity = crate::embedding::cosine_similarity(&mean_old, &mean_new);

        crate::logging::info(&format!(
            "[compaction/semantic] topic similarity (old vs new half) = {:.3} (threshold={:.2})",
            similarity, cfg.topic_shift_threshold
        ));

        if similarity < cfg.topic_shift_threshold {
            crate::logging::info(
                "[compaction/semantic] Topic shift detected — triggering proactive compaction",
            );
            return true;
        }

        // No topic shift — still fall back to proactive growth check.
        self.should_compact_proactively(all_messages)
    }

    /// Build a relevance-scored keep set for semantic compaction.
    ///
    /// Embeds the last `goal_window_turns` messages to represent the current
    /// goal, then scores all active messages by cosine similarity. Returns the
    /// cutoff index: messages before the cutoff will be summarized, messages at
    /// or after are kept verbatim.
    ///
    /// Messages above `relevance_keep_threshold` anywhere in the history are
    /// pulled out of the summarize set. Falls back to the standard recency
    /// cutoff if embeddings fail.
    fn semantic_cutoff(&mut self, active: &[Message]) -> usize {
        let goal_window_turns = self.compaction_config.goal_window_turns;
        let relevance_keep_threshold = self.compaction_config.relevance_keep_threshold;
        let standard_cutoff = active.len().saturating_sub(RECENT_TURNS_TO_KEEP);
        if standard_cutoff == 0 {
            return 0;
        }

        // Build goal text from recent turns.
        let goal_turns = goal_window_turns.min(active.len());
        let goal_text = semantic_goal_text(&active[active.len() - goal_turns..]);

        if goal_text.is_empty() {
            return standard_cutoff;
        }

        let goal_emb = match self.cached_semantic_embedding(&goal_text) {
            Some(embedding) => embedding,
            None => return standard_cutoff,
        };

        // Score each candidate message (those before standard_cutoff).
        let mut high_relevance_count = 0usize;
        let mut earliest_high_relevance = standard_cutoff;

        for (idx, msg) in active[..standard_cutoff].iter().enumerate() {
            let text = semantic_message_text(msg);

            if text.is_empty() {
                continue;
            }

            if let Some(embedding) = self.cached_semantic_embedding(&text) {
                let sim = crate::embedding::cosine_similarity(&goal_emb, &embedding);
                if sim >= relevance_keep_threshold {
                    high_relevance_count += 1;
                    earliest_high_relevance = earliest_high_relevance.min(idx);
                }
            }
        }

        if high_relevance_count == 0 {
            return standard_cutoff;
        }

        // Find the latest high-relevance message before standard_cutoff.
        // We can't have gaps in the summarized range (tool call integrity),
        // so we move the cutoff up to just before the earliest high-relevance
        // message in the tail of the compaction range.
        let adjusted_cutoff = earliest_high_relevance;

        // Ensure we actually compact something meaningful.
        if adjusted_cutoff < 2 {
            return standard_cutoff;
        }

        crate::logging::info(&format!(
            "[compaction/semantic] relevance scoring: {} high-relevance msgs kept, cutoff {} -> {}",
            high_relevance_count, standard_cutoff, adjusted_cutoff
        ));

        adjusted_cutoff
    }

    /// Get the active (uncompacted) messages from a full message list.
    /// Skips the first `compacted_count` messages.
    fn active_messages<'a>(&self, all_messages: &'a [Message]) -> &'a [Message] {
        // If session restore/replay leaves the manager with bookkeeping from a
        // longer message vector, never fall back to the full transcript. That
        // makes already-compacted messages active again and can drive repeated
        // emergency compaction loops. Clamp to the end instead: all available
        // messages are covered by the summary until new turns arrive.
        let start = self.compacted_count.min(all_messages.len());
        &all_messages[start..]
    }

    fn clamp_compacted_count_to_messages(
        &mut self,
        all_messages: &[Message],
        reason: &str,
    ) -> bool {
        // Some backward-compatible call paths intentionally poll/apply without
        // caller-owned message history. An empty slice there means "unknown",
        // not necessarily an empty transcript, so do not treat it as an
        // authoritative upper bound.
        if all_messages.is_empty() {
            return false;
        }
        if self.compacted_count <= all_messages.len() {
            return false;
        }

        crate::logging::warn(&format!(
            "[compaction/invariant] compacted_count_exceeded_messages reason={} compacted_count={} messages_len={} total_turns={} has_summary={} summary_chars={} observed_input_tokens={:?}",
            reason,
            self.compacted_count,
            all_messages.len(),
            self.total_turns,
            self.active_summary.is_some(),
            self.summary_chars(),
            self.observed_input_tokens,
        ));
        self.compacted_count = all_messages.len();
        self.active_chars.set_exact(0);
        true
    }

    fn log_compaction_state(&self, phase: &str, trigger: &str, all_messages: &[Message]) {
        let active_len = self.active_messages(all_messages).len();
        crate::logging::info(&format!(
            "[compaction/state] phase={} trigger={} messages_len={} active_messages={} compacted_count={} total_turns={} token_budget={} token_estimate={} effective_tokens={} observed_input_tokens={:?} has_summary={} summary_chars={} pending_cutoff={} is_compacting={}",
            phase,
            trigger,
            all_messages.len(),
            active_len,
            self.compacted_count,
            self.total_turns,
            self.token_budget,
            self.token_estimate_with(all_messages),
            self.effective_token_count_with(all_messages),
            self.observed_input_tokens,
            self.active_summary.is_some(),
            self.summary_chars(),
            self.pending_cutoff,
            self.pending_task.is_some(),
        ));
    }

    fn log_compaction_outcome(&self, outcome: CompactionOutcomeLog<'_>) {
        let tokens_saved = outcome.pre_tokens.saturating_sub(outcome.post_tokens);
        let grew = outcome.post_tokens > outcome.pre_tokens;
        let level = if grew { "warn" } else { "info" };
        let line = format!(
            "[compaction/outcome] level={} trigger={} duration_ms={} pre_tokens={} post_tokens={} tokens_saved={} grew={} messages_len={} active_messages={} compacted_count={} total_turns={} messages_compacted={} messages_dropped={} summary_chars={} observed_input_tokens={:?}",
            level,
            outcome.trigger,
            outcome.duration_ms,
            outcome.pre_tokens,
            outcome.post_tokens,
            tokens_saved,
            grew,
            outcome.all_messages.len(),
            self.active_messages(outcome.all_messages).len(),
            self.compacted_count,
            self.total_turns,
            outcome.messages_compacted,
            outcome.messages_dropped.unwrap_or(0),
            self.summary_chars(),
            self.observed_input_tokens,
        );
        if grew {
            crate::logging::warn(&line);
        } else {
            crate::logging::info(&line);
        }
    }

    fn active_message_chars_with(&self, all_messages: &[Message]) -> usize {
        // Recompute from history when the cache is stale, or when the
        // display-side turn estimate disagrees with the real active slice
        // length (the two can diverge across restore/clamp/compaction paths,
        // and trusting a mismatched cache is exactly what corrupts token
        // accounting).
        if self.active_chars.is_dirty()
            || self.active_messages_count() != self.active_messages(all_messages).len()
        {
            self.active_messages(all_messages)
                .iter()
                .map(message_char_count)
                .sum()
        } else {
            self.active_chars.value()
        }
    }

    /// Get current token estimate using the caller's message list
    pub fn token_estimate_with(&self, all_messages: &[Message]) -> usize {
        estimate_compaction_tokens(
            self.active_summary.as_ref(),
            self.active_message_chars_with(all_messages),
            self.token_budget,
        )
    }

    /// Get current token estimate (backward compat — uses 0 messages, only summary + observed)
    pub fn token_estimate(&self) -> usize {
        estimate_compaction_tokens(self.active_summary.as_ref(), 0, self.token_budget)
    }

    /// Store provider-reported input token usage for compaction decisions.
    pub fn update_observed_input_tokens(&mut self, tokens: u64) {
        self.observed_input_tokens = Some(tokens);
    }

    /// Best-effort current token count using the caller's messages.
    pub fn effective_token_count_with(&self, all_messages: &[Message]) -> usize {
        let estimate = self.token_estimate_with(all_messages);
        let observed = self
            .observed_input_tokens
            .and_then(|tokens| usize::try_from(tokens).ok())
            .unwrap_or(0);
        estimate.max(observed)
    }

    /// Best-effort token count without message data (uses only observed tokens)
    pub fn effective_token_count(&self) -> usize {
        let estimate = self.token_estimate();
        let observed = self
            .observed_input_tokens
            .and_then(|tokens| usize::try_from(tokens).ok())
            .unwrap_or(0);
        estimate.max(observed)
    }

    /// Get current context usage as percentage (using caller's messages)
    pub fn context_usage_with(&self, all_messages: &[Message]) -> f32 {
        self.effective_token_count_with(all_messages) as f32 / self.token_budget as f32
    }

    /// Get current context usage (without messages, uses observed tokens only)
    pub fn context_usage(&self) -> f32 {
        self.effective_token_count() as f32 / self.token_budget as f32
    }

    /// Check if we should start compaction
    pub fn should_compact_with(&self, all_messages: &[Message]) -> bool {
        use crate::config::CompactionMode;
        if self.suppress_compaction_until_new_message
            || self.pending_task.is_some()
            || self.prepared_lcm_context.is_some()
        {
            return false;
        }
        let active = self.active_messages(all_messages);
        match self.mode {
            CompactionMode::Reactive => {
                self.pending_task.is_none()
                    && self.context_usage_with(all_messages) >= COMPACTION_THRESHOLD
                    && active.len() > RECENT_TURNS_TO_KEEP
            }
            CompactionMode::Proactive => {
                active.len() > RECENT_TURNS_TO_KEEP && self.should_compact_proactively(all_messages)
            }
            CompactionMode::Semantic => {
                active.len() > RECENT_TURNS_TO_KEEP && self.should_compact_semantic(all_messages)
            }
        }
    }

    /// Start background compaction if needed
    pub fn maybe_start_compaction_with(
        &mut self,
        all_messages: &[Message],
        provider: Arc<dyn Provider>,
    ) {
        if !self.should_compact_with(all_messages) {
            return;
        }

        let active = self.active_messages(all_messages);

        // Calculate cutoff within active messages.
        // Semantic mode uses relevance scoring; other modes use recency.
        let mut cutoff = match self.mode {
            crate::config::CompactionMode::Semantic => self.semantic_cutoff(active),
            _ => active.len().saturating_sub(RECENT_TURNS_TO_KEEP),
        };
        if cutoff == 0 {
            return;
        }

        // Adjust cutoff to not split tool call/result pairs
        cutoff = safe_compaction_cutoff(active, cutoff);
        if cutoff == 0 {
            return;
        }

        // Snapshot messages to summarize (must clone for the async task)
        let messages_to_summarize: Vec<Message> = active[..cutoff].to_vec();
        let msg_count = messages_to_summarize.len();
        let existing_summary = self.active_summary.clone();
        let mode_label = self.mode_trigger_label().to_string();
        let estimated_tokens = self.effective_token_count_with(all_messages);
        crate::logging::info(&format!(
            "[TIMING] compaction_start: trigger={}, active_messages={}, cutoff={}, estimated_tokens={}, has_existing_summary={}",
            mode_label,
            active.len(),
            cutoff,
            estimated_tokens,
            existing_summary.is_some(),
        ));

        self.pending_cutoff = cutoff;
        self.pending_source_fingerprint = message_fingerprint(&active[..cutoff]);
        self.pending_trigger = Some(mode_label.clone());

        // Spawn background task that notifies via Bus when done
        self.pending_task = Some(tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result =
                generate_compaction_artifact(provider, messages_to_summarize, existing_summary)
                    .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            crate::logging::info(&format!(
                "Compaction ({}) finished in {:.2}s ({} messages summarized)",
                mode_label,
                duration_ms as f64 / 1000.0,
                msg_count,
            ));
            crate::bus::Bus::global().publish(crate::bus::BusEvent::CompactionFinished);
            result.map(|mut result| {
                result.duration_ms = duration_ms;
                result.summarized_messages = msg_count;
                result
            })
        }));
    }

    fn capture_lcm_source(
        &self,
        session: &crate::session::Session,
        cutoff: usize,
        summarizer_model: String,
        summarizer_provider: String,
        summarizer_route: String,
        configured_model: Option<String>,
        pre_tokens: u64,
        trigger: String,
    ) -> Result<PendingLcmSource> {
        let route_policy_fingerprint =
            lcm_route_policy_fingerprint(session, configured_model.as_deref());
        let covered_message_count = self.compacted_count.saturating_add(cutoff);
        if covered_message_count == 0 || covered_message_count > session.messages.len() {
            anyhow::bail!("LCM cutoff does not map to canonical session history");
        }
        let prior_frontier = session.context_frontier.as_ref();
        let source_start = prior_frontier
            .map_or(0, |frontier| frontier.covered_message_count)
            .min(covered_message_count);
        let source = &session.messages[source_start..covered_message_count];
        let source_prefix = &session.messages[..covered_message_count];
        if source.is_empty() {
            anyhow::bail!("LCM leaf source is empty");
        }
        let prior_active_nodes = prior_frontier
            .into_iter()
            .flat_map(|frontier| frontier.active_node_ids.iter())
            .map(|id| {
                session
                    .context_nodes
                    .iter()
                    .find(|node| node.id == *id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("LCM frontier node {id} is missing"))
            })
            .collect::<Result<Vec<_>>>()?;
        let current_generation = session
            .context_frontier
            .as_ref()
            .map_or(0, |frontier| frontier.generation);
        Ok(PendingLcmSource {
            session_id: session.id.clone(),
            base_generation: current_generation,
            generation: current_generation.saturating_add(1),
            next_node_sequence: session
                .context_frontier
                .as_ref()
                .map_or(1, |frontier| frontier.next_node_sequence),
            covered_message_count,
            source_message_ids: source.iter().map(|message| message.id.clone()).collect(),
            input_proof_ids: source.iter().map(|message| message.id.clone()).collect(),
            source_sha256: stored_message_prefix_sha256(source)?,
            source_prefix_sha256: stored_message_prefix_sha256(source_prefix)?,
            covered_through_message_id: source
                .last()
                .expect("covered prefix is non-empty")
                .id
                .clone(),
            prior_active_nodes,
            node_level: 0,
            child_nodes: Vec::new(),
            summarizer_model,
            summarizer_provider,
            summarizer_route,
            configured_model,
            cutoff,
            source_fingerprint: message_fingerprint(
                &session.messages[source_start..covered_message_count]
                    .iter()
                    .map(crate::session::StoredMessage::to_message)
                    .collect::<Vec<_>>(),
            ),
            pre_tokens,
            trigger,
            route_policy_fingerprint,
        })
    }

    fn lcm_provider_for_session(
        &self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> Result<(Arc<dyn Provider>, String, String, String, Option<String>)> {
        let configured_model = crate::config::config().compaction.model.clone();
        self.lcm_provider_for_session_with_model(session, provider, configured_model)
    }

    fn lcm_provider_for_session_with_model(
        &self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
        configured_model: Option<String>,
    ) -> Result<(Arc<dyn Provider>, String, String, String, Option<String>)> {
        let compactor = provider.fork();
        let inherited_model = session.model.clone().unwrap_or_else(|| provider.model());
        let route = lcm_route_spec(session, &inherited_model, configured_model.as_deref());
        if configured_model.is_some() {
            crate::provider::set_model_with_auth_refresh(compactor.as_ref(), &route)?;
        } else if let Some(selection) = lcm_inherited_route_selection(session, &inherited_model) {
            set_lcm_inherited_route(compactor.as_ref(), &selection)?;
        } else {
            crate::provider::set_model_with_auth_refresh(compactor.as_ref(), &route)?;
        }
        let model = compactor.model();
        let provider_name = compactor.name().to_string();
        Ok((compactor, model, provider_name, route, configured_model))
    }

    /// Fork and route a provider exactly as native LCM would for this session.
    /// Lifecycle callers use this to avoid silently reverting to the active chat
    /// route or provider-native compaction while creating a transferred child.
    pub fn portable_provider_for_session(
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> Result<Arc<dyn Provider>> {
        let manager = Self::new();
        let (provider, _, _, _, _) = manager.lcm_provider_for_session(session, provider)?;
        Ok(provider)
    }

    fn maybe_start_lcm_with(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> bool {
        if self.pending_task.is_none()
            && self.prepared_lcm_context.is_none()
            && match self.maybe_start_lcm_condensation(session, provider.clone()) {
                Ok(started) => started,
                Err(error) => {
                    crate::logging::error(&format!("LCM hierarchy start failed: {error}"));
                    false
                }
            }
        {
            return true;
        }
        let all_messages = session.messages_for_provider_uncached();
        if !self.should_compact_with(&all_messages) {
            return false;
        }
        let active = self.active_messages(&all_messages);
        let mut cutoff = match self.mode {
            crate::config::CompactionMode::Semantic => self.semantic_cutoff(active),
            _ => active.len().saturating_sub(RECENT_TURNS_TO_KEEP),
        };
        cutoff = safe_compaction_cutoff(active, cutoff);
        if cutoff == 0 {
            return false;
        }

        let trigger = self.mode_trigger_label().to_string();
        match self.start_lcm_job(session, provider, &all_messages, cutoff, trigger) {
            Ok(()) => true,
            Err(error) => {
                crate::logging::error(&format!(
                    "LCM job start failed; keeping canonical history: {error}"
                ));
                false
            }
        }
    }

    fn maybe_start_lcm_condensation(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> Result<bool> {
        const FANOUT: usize = 4;
        let Some(frontier) = session.context_frontier.as_ref() else {
            return Ok(false);
        };
        let all_messages = session.messages_for_provider_uncached();
        if !self.should_compact_with(&all_messages) {
            return Ok(false);
        }
        let active = self.active_messages(&all_messages);
        let active_chars = active.iter().map(message_char_count).sum();
        let tail_tokens = estimate_compaction_tokens(None, active_chars, self.token_budget);
        if (tail_tokens as f64 / self.token_budget.max(1) as f64) >= f64::from(COMPACTION_THRESHOLD)
        {
            // The raw tail itself needs a new leaf. Condensing the frontier first
            // would cause two provider-prefix changes where one leaf generation
            // is the actual fit operation.
            return Ok(false);
        }
        let active_nodes = frontier
            .active_node_ids
            .iter()
            .map(|id| {
                session
                    .context_nodes
                    .iter()
                    .find(|node| node.id == *id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("LCM frontier node {id} is missing"))
            })
            .collect::<Result<Vec<_>>>()?;
        if active_nodes.len() < FANOUT {
            return Ok(false);
        }
        let children = active_nodes[active_nodes.len() - FANOUT..].to_vec();
        let child_level = children[0].level;
        if children.iter().any(|node| node.level != child_level) {
            return Ok(false);
        }

        let (compactor, model, provider_name, route, configured_model) =
            self.lcm_provider_for_session(session, provider)?;
        let budget_route = route.clone();
        let route_policy_fingerprint =
            lcm_route_policy_fingerprint(session, configured_model.as_deref());
        let child_bytes = serde_json::to_vec(&children)?;
        let source_sha256 = format!("{:x}", Sha256::digest(child_bytes));
        let mut seen_message_ids = std::collections::HashSet::new();
        let source_message_ids = children
            .iter()
            .flat_map(|node| node.source_message_ids.iter().cloned())
            .filter(|id| seen_message_ids.insert(id.clone()))
            .collect::<Vec<_>>();
        let input_proof_ids = children.iter().map(|node| node.id.clone()).collect();
        let source = PendingLcmSource {
            session_id: session.id.clone(),
            base_generation: frontier.generation,
            generation: frontier.generation.saturating_add(1),
            next_node_sequence: frontier.next_node_sequence,
            covered_message_count: frontier.covered_message_count,
            source_message_ids,
            input_proof_ids,
            source_sha256,
            source_prefix_sha256: frontier.source_prefix_sha256.clone(),
            covered_through_message_id: frontier
                .covered_through_message_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("LCM frontier lacks covered message id"))?,
            prior_active_nodes: active_nodes[..active_nodes.len() - FANOUT].to_vec(),
            node_level: child_level.saturating_add(1),
            child_nodes: children.clone(),
            summarizer_model: model,
            summarizer_provider: provider_name,
            summarizer_route: route,
            configured_model,
            cutoff: 0,
            source_fingerprint: None,
            pre_tokens: self.effective_token_count_with(&session.messages_for_provider_uncached())
                as u64,
            trigger: "hierarchy".to_string(),
            route_policy_fingerprint,
        };
        let messages_to_summarize = children
            .iter()
            .map(|node| Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!(
                        "[LCM child {} level {}]\n{}",
                        node.id, node.level, node.summary_text
                    ),
                    cache_control: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            })
            .collect::<Vec<_>>();
        let message_count = messages_to_summarize.len();
        self.pending_cutoff = 0;
        self.pending_source_fingerprint = None;
        self.pending_trigger = Some("hierarchy".to_string());
        self.pending_lcm_source = Some(source);
        self.pending_task = Some(tokio::spawn(async move {
            let start = Instant::now();
            let result = generate_lcm_compaction_artifact(
                compactor,
                messages_to_summarize,
                None,
                Some(budget_route),
                LcmJobPriority::Background,
            )
            .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            crate::bus::Bus::global().publish(crate::bus::BusEvent::CompactionFinished);
            result.map(|mut result| {
                result.duration_ms = duration_ms;
                result.summarized_messages = message_count;
                result
            })
        }));
        Ok(true)
    }

    fn start_lcm_job(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
        all_messages: &[Message],
        cutoff: usize,
        trigger: String,
    ) -> Result<()> {
        let policy_model = crate::config::config().compaction.model.clone();
        self.start_lcm_job_with_model(
            session,
            provider,
            all_messages,
            cutoff,
            trigger,
            policy_model.clone(),
            policy_model,
        )
    }

    fn start_lcm_job_with_model(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
        all_messages: &[Message],
        cutoff: usize,
        trigger: String,
        route_model: Option<String>,
        policy_model: Option<String>,
    ) -> Result<()> {
        let active = self.active_messages(all_messages);
        if cutoff == 0 || cutoff > active.len() {
            anyhow::bail!("LCM cutoff is outside the active message suffix");
        }
        let (compactor, model, provider_name, route, _) =
            self.lcm_provider_for_session_with_model(session, provider, route_model)?;
        let budget_route = route.clone();
        let pre_tokens = self.effective_token_count_with(all_messages) as u64;
        let source = self.capture_lcm_source(
            session,
            cutoff,
            model,
            provider_name,
            route,
            policy_model,
            pre_tokens,
            trigger.clone(),
        )?;
        let messages_to_summarize = active[..cutoff].to_vec();
        let existing_summary = if session.context_frontier.is_none() {
            self.active_summary.clone()
        } else {
            None
        };
        let message_count = messages_to_summarize.len();
        const ATOMIC_PARENT_PRIOR_CHILDREN: usize = 3;
        let mut carry_frontier = source.prior_active_nodes.clone();
        let mut carry_level = source.node_level;
        let mut atomic_carry_tiers = Vec::new();
        loop {
            let Some(children) = carry_frontier.get(
                carry_frontier
                    .len()
                    .saturating_sub(ATOMIC_PARENT_PRIOR_CHILDREN)..,
            ) else {
                break;
            };
            if children.len() != ATOMIC_PARENT_PRIOR_CHILDREN
                || children.iter().any(|node| node.level != carry_level)
            {
                break;
            }
            atomic_carry_tiers.push(children.to_vec());
            carry_frontier.truncate(
                carry_frontier
                    .len()
                    .saturating_sub(ATOMIC_PARENT_PRIOR_CHILDREN),
            );
            carry_level = carry_level.saturating_add(1);
        }

        self.pending_cutoff = cutoff;
        self.pending_source_fingerprint = source.source_fingerprint;
        self.pending_trigger = Some(trigger.clone());
        self.pending_lcm_source = Some(source);
        let priority = if trigger.starts_with("critical_") || trigger == "hard_compact" {
            LcmJobPriority::Critical
        } else {
            LcmJobPriority::Background
        };
        self.pending_task = Some(tokio::spawn(async move {
            let start = Instant::now();
            let mut result = generate_lcm_compaction_artifact(
                Arc::clone(&compactor),
                messages_to_summarize,
                existing_summary,
                Some(budget_route.clone()),
                priority,
            )
            .await?;
            let mut carried_summary = result.summary_text.clone();
            for carry_children in atomic_carry_tiers {
                let child_level = carry_children[0].level;
                let mut parent_messages = carry_children
                    .iter()
                    .map(|node| Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: format!(
                                "[LCM child {} level {}]\n{}",
                                node.id, node.level, node.summary_text
                            ),
                            cache_control: None,
                        }],
                        timestamp: None,
                        tool_duration_ms: None,
                    })
                    .collect::<Vec<_>>();
                parent_messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: format!("[LCM new child level {child_level}]\n{carried_summary}"),
                        cache_control: None,
                    }],
                    timestamp: None,
                    tool_duration_ms: None,
                });
                let parent = generate_lcm_compaction_artifact(
                    Arc::clone(&compactor),
                    parent_messages,
                    None,
                    Some(budget_route.clone()),
                    priority,
                )
                .await?;
                carried_summary = parent.summary_text.clone();
                result.atomic_parent_summaries.push(parent.summary_text);
            }
            let duration_ms = start.elapsed().as_millis() as u64;
            crate::bus::Bus::global().publish(crate::bus::BusEvent::CompactionFinished);
            result.duration_ms = duration_ms;
            result.summarized_messages = message_count;
            Ok(result)
        }));
        Ok(())
    }

    fn prepare_lcm_context(
        source: PendingLcmSource,
        result: CompactionResult,
    ) -> Result<PreparedLcmContext> {
        const PROMPT_SCHEMA_VERSION: u32 = 1;
        if result.summary_text.trim().is_empty() {
            anyhow::bail!("LCM compactor returned an empty summary");
        }
        let node_id = lcm_node_id(&source, &result.summary_text, PROMPT_SCHEMA_VERSION);
        let durable_summary = lcm_summary_with_retrieval_anchor(
            &result.summary_text,
            &source.session_id,
            &source.source_message_ids,
        );
        let node = crate::session::StoredContextNode {
            id: node_id.clone(),
            schema_version: 1,
            level: source.node_level,
            source_session_id: source.session_id.clone(),
            source_message_ids: source.source_message_ids.clone(),
            source_sha256: source.source_sha256.clone(),
            child_node_ids: source
                .child_nodes
                .iter()
                .map(|node| node.id.clone())
                .collect(),
            summary_text: durable_summary.clone(),
            summary_sha256: Some(format!("{:x}", Sha256::digest(durable_summary.as_bytes()))),
            estimated_tokens: ((durable_summary.len() + CHARS_PER_TOKEN - 1) / CHARS_PER_TOKEN)
                .max(1) as u64,
            summarizer_model: source.summarizer_model.clone(),
            summarizer_provider: source.summarizer_provider.clone(),
            summarizer_route: source.summarizer_route.clone(),
            prompt_schema_version: PROMPT_SCHEMA_VERSION,
            created_at: Utc::now(),
        };
        let mut active_nodes = source.prior_active_nodes.clone();
        let mut append_context_nodes = vec![node.clone()];
        let mut carried_node = node;
        for parent_summary in &result.atomic_parent_summaries {
            const FANOUT: usize = 4;
            if active_nodes.len() < FANOUT - 1 {
                anyhow::bail!(
                    "LCM atomic carry candidate does not have three prior level-{} nodes",
                    carried_node.level
                );
            }
            let mut children = active_nodes.split_off(active_nodes.len() - (FANOUT - 1));
            children.push(carried_node.clone());
            if children.len() != FANOUT
                || children
                    .iter()
                    .any(|child| child.level != carried_node.level)
            {
                anyhow::bail!(
                    "LCM atomic parent children are not four adjacent level-{} nodes",
                    carried_node.level
                );
            }
            let children_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&children)?));
            let mut seen = std::collections::HashSet::new();
            let source_message_ids = children
                .iter()
                .flat_map(|child| child.source_message_ids.iter().cloned())
                .filter(|id| seen.insert(id.clone()))
                .collect::<Vec<_>>();
            let parent_durable_summary = lcm_summary_with_retrieval_anchor(
                parent_summary,
                &source.session_id,
                &source_message_ids,
            );
            let parent_id = lcm_atomic_parent_id(
                source.generation,
                source
                    .next_node_sequence
                    .saturating_add(append_context_nodes.len() as u64),
                &children_sha256,
                &source.summarizer_route,
                parent_summary,
                PROMPT_SCHEMA_VERSION,
            );
            let parent = crate::session::StoredContextNode {
                id: parent_id,
                schema_version: 1,
                level: carried_node.level.saturating_add(1),
                source_session_id: source.session_id.clone(),
                source_message_ids,
                source_sha256: children_sha256,
                child_node_ids: children.iter().map(|child| child.id.clone()).collect(),
                summary_text: parent_durable_summary.clone(),
                summary_sha256: Some(format!(
                    "{:x}",
                    Sha256::digest(parent_durable_summary.as_bytes())
                )),
                estimated_tokens: ((parent_durable_summary.len() + CHARS_PER_TOKEN - 1)
                    / CHARS_PER_TOKEN)
                    .max(1) as u64,
                summarizer_model: source.summarizer_model.clone(),
                summarizer_provider: source.summarizer_provider.clone(),
                summarizer_route: source.summarizer_route.clone(),
                prompt_schema_version: PROMPT_SCHEMA_VERSION,
                created_at: Utc::now(),
            };
            append_context_nodes.push(parent.clone());
            carried_node = parent;
        }
        active_nodes.push(carried_node);
        let active_node_ids = active_nodes.iter().map(|node| node.id.clone()).collect();
        let projection_text = active_nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                format!(
                    "[LCM context node {} level {}]\n{}",
                    index + 1,
                    node.level,
                    node.summary_text
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let frontier = crate::session::StoredContextFrontier {
            schema_version: 1,
            generation: source.generation,
            active_node_ids,
            covered_message_count: source.covered_message_count,
            covered_through_message_id: Some(source.covered_through_message_id),
            source_prefix_sha256: source.source_prefix_sha256.clone(),
            next_node_sequence: source
                .next_node_sequence
                .saturating_add(append_context_nodes.len() as u64),
        };
        let transaction = crate::session::ContextGraphTransaction {
            schema_version: 1,
            op_id: format!("lcm-op-{}-{node_id}", source.generation),
            base_generation: source.base_generation,
            generation: source.generation,
            append_context_nodes,
            frontier,
            input_proof: crate::session::ContextGraphInputProof {
                schema_version: 1,
                source_session_id: source.session_id,
                source_message_ids: source.input_proof_ids,
                source_sha256: source.source_sha256,
            },
        };
        let projection = crate::session::StoredCompactionState {
            summary_text: projection_text.clone(),
            openai_encrypted_content: None,
            covers_up_to_turn: source.covered_message_count,
            original_turn_count: source.covered_message_count,
            compacted_count: source.covered_message_count,
        };
        Ok(PreparedLcmContext {
            transaction,
            projection,
            summary: Summary {
                text: projection_text,
                openai_encrypted_content: None,
                covers_up_to_turn: source.covered_message_count,
                original_turn_count: source.covered_message_count,
            },
            covered_message_count: source.covered_message_count,
            cutoff: source.cutoff,
            pre_tokens: source.pre_tokens,
            duration_ms: result.duration_ms,
            trigger: source.trigger.clone(),
            messages_dropped: None,
            configured_model: source.configured_model,
            effective_route: source.summarizer_route,
            allow_active_route_fallback: source.trigger == "critical_active_route",
            route_policy_fingerprint: source.route_policy_fingerprint,
        })
    }

    fn poll_lcm_candidate(&mut self, session: &crate::session::Session) {
        if self.prepared_lcm_context.is_some() {
            return;
        }
        let Some(task) = self.pending_task.take() else {
            return;
        };
        if !task.is_finished() {
            self.pending_task = Some(task);
            return;
        }
        let Some(source) = self.pending_lcm_source.take() else {
            task.abort();
            crate::logging::error("LCM task completed without its canonical source snapshot");
            return;
        };

        self.pending_cutoff = 0;
        self.pending_source_fingerprint = None;
        self.pending_trigger = None;

        let current_policy = crate::config::config().compaction.clone();
        if !source.matches_runtime_policy(session, &current_policy) {
            crate::logging::warn(
                "Discarding completed LCM job because compaction engine/model/route policy changed",
            );
            return;
        }
        let all_messages = session.messages_for_provider_uncached();
        let source_end = source.covered_message_count.min(all_messages.len());
        let source_start = self.compacted_count.min(source_end);
        if source_end != source.covered_message_count
            || message_fingerprint(&all_messages[source_start..source_end])
                != source.source_fingerprint
        {
            crate::logging::warn(
                "Discarding completed LCM job because canonical source history changed",
            );
            return;
        }

        match futures::executor::block_on(task) {
            Ok(Ok(result)) => match Self::prepare_lcm_context(source, result) {
                Ok(prepared) => self.prepared_lcm_context = Some(prepared),
                Err(error) => crate::logging::error(&format!(
                    "Failed to prepare completed LCM transaction: {error}"
                )),
            },
            Ok(Err(error)) => {
                crate::logging::error(&format!("LCM summary generation failed: {error}"));
            }
            Err(error) => {
                crate::logging::error(&format!("LCM summary task panicked: {error}"));
            }
        }
    }

    fn commit_prepared_lcm(
        &mut self,
        session: &mut crate::session::Session,
    ) -> Result<Option<CompactionEvent>> {
        let Some(prepared) = self.prepared_lcm_context.take() else {
            return Ok(None);
        };
        let policy = &crate::config::config().compaction;
        let inherited_model = session
            .model
            .as_deref()
            .unwrap_or_else(|| prepared.effective_route.as_str());
        let active_route = lcm_route_spec(session, inherited_model, None);
        let policy_matches = policy.engine == crate::config::CompactionEngine::Lcm
            && prepared.route_policy_fingerprint
                == lcm_route_policy_fingerprint(session, prepared.configured_model.as_deref())
            && if matches!(
                prepared.trigger.as_str(),
                "critical_legacy_import" | "critical_local_emergency_chain" | "hard_compact"
            ) {
                true
            } else if prepared.allow_active_route_fallback {
                active_route == prepared.effective_route
            } else {
                policy.model == prepared.configured_model
                    && prepared.configured_model.as_ref().map_or_else(
                        || active_route == prepared.effective_route,
                        |configured| configured == &prepared.effective_route,
                    )
            };
        if !policy_matches {
            anyhow::bail!("prepared LCM candidate route policy became stale before commit");
        }
        let commit = session.commit_context_graph_transaction_with_compaction(
            prepared.transaction.clone(),
            Some(prepared.projection.clone()),
        );
        if let Err(error) = commit {
            self.prepared_lcm_context = Some(prepared);
            return Err(error);
        }

        let all_messages = session.messages_for_provider_uncached();
        self.compacted_count = prepared.covered_message_count.min(all_messages.len());
        self.active_chars.set_exact(
            all_messages[self.compacted_count..]
                .iter()
                .map(message_char_count)
                .sum(),
        );
        self.active_summary = Some(prepared.summary);
        self.observed_input_tokens = None;
        self.turns_since_last_compact = 0;
        let post_tokens = self.effective_token_count_with(&all_messages) as u64;
        let leaf_count = session
            .context_nodes
            .iter()
            .filter(|node| node.level == 0)
            .count();
        let parent_count = session.context_nodes.len().saturating_sub(leaf_count);
        let max_node_level = session.context_nodes.iter().map(|node| node.level).max();
        let effective_route = prepared
            .transaction
            .append_context_nodes
            .last()
            .map(|node| node.summarizer_route.clone());
        let fallback_reason = match prepared.trigger.as_str() {
            "critical_active_route" => {
                Some("selected_route_failed>active_route_succeeded".to_string())
            }
            "critical_legacy_import" => Some(
                "selected_route_failed>active_route_failed_or_unavailable>legacy_summary_succeeded"
                    .to_string(),
            ),
            "critical_local_emergency_chain" => Some(
                "selected_route_failed>active_route_failed_or_unavailable>legacy_summary_rejected_or_unavailable>local_emergency_succeeded"
                    .to_string(),
            ),
            "hard_compact" => Some("critical_local_emergency".to_string()),
            _ => None,
        };
        let event = CompactionEvent {
            trigger: prepared.trigger,
            engine: Some("lcm".to_string()),
            ownership: Some("lcm".to_string()),
            configured_route: crate::config::config().compaction.model.clone(),
            effective_route,
            fallback_reason,
            leaf_count: Some(leaf_count),
            parent_count: Some(parent_count),
            frontier_size: session
                .context_frontier
                .as_ref()
                .map(|frontier| frontier.active_node_ids.len()),
            max_node_level,
            graph_generation: session
                .context_frontier
                .as_ref()
                .map(|frontier| frontier.generation),
            pre_tokens: Some(prepared.pre_tokens),
            post_tokens: Some(post_tokens),
            tokens_saved: Some(prepared.pre_tokens.saturating_sub(post_tokens)),
            duration_ms: Some(prepared.duration_ms),
            messages_dropped: prepared.messages_dropped,
            messages_compacted: Some(prepared.cutoff),
            summary_chars: self
                .active_summary
                .as_ref()
                .map(|summary| summary.text.len()),
            active_messages: Some(self.active_messages_count()),
        };
        self.last_compaction = Some(event.clone());
        Ok(Some(event))
    }

    /// Shared Agent/local-TUI LCM completion facade. It validates the captured
    /// source, commits graph plus projection durably, and only then exposes the
    /// new summary in provider context.
    pub fn materialize_lcm_context(
        &mut self,
        session: &mut crate::session::Session,
    ) -> Result<(Vec<Message>, Option<CompactionEvent>)> {
        self.poll_lcm_candidate(session);
        let event = self.commit_prepared_lcm(session)?;
        if event.is_some() {
            // The event is returned directly by this facade. Do not leave a
            // duplicate for a later rolling-style `take_compaction_event` call.
            self.last_compaction = None;
        }
        let all_messages = session.messages_for_provider_uncached();
        let active = self.active_messages(&all_messages);
        let frontier_summaries = session
            .context_frontier
            .as_ref()
            .into_iter()
            .flat_map(|frontier| frontier.active_node_ids.iter())
            .filter_map(|id| {
                session
                    .context_nodes
                    .iter()
                    .find(|node| node.id == *id)
                    .map(|node| node.summary_text.as_str())
            })
            .collect::<Vec<_>>();
        let messages = if !frontier_summaries.is_empty() {
            let mut messages = Vec::with_capacity(active.len() + frontier_summaries.len());
            for summary in frontier_summaries {
                messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: compacted_summary_text_block(summary),
                        cache_control: None,
                    }],
                    timestamp: None,
                    tool_duration_ms: None,
                });
            }
            messages.extend(active.iter().cloned());
            messages
        } else if let Some(summary) = self.active_summary.as_ref() {
            let mut messages = Vec::with_capacity(active.len() + 1);
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: compacted_summary_text_block(&summary.text),
                    cache_control: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            });
            messages.extend(active.iter().cloned());
            messages
        } else {
            active.to_vec()
        };
        Ok((messages, event))
    }

    /// Apply unchanged Jcode trigger thresholds to the LCM engine. Critical
    /// synchronous recovery remains the next Phase 3 gate; below 95%, this
    /// starts one source-validated background leaf job.
    pub fn ensure_lcm_context_fits(
        &mut self,
        session: &mut crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> CompactionAction {
        let all_messages = session.messages_for_provider_uncached();
        if self.context_usage_with(&all_messages) >= CRITICAL_THRESHOLD {
            let critical_policy_model = crate::config::config().compaction.model.clone();
            let active = self.active_messages(&all_messages);
            let cutoff =
                safe_compaction_cutoff(active, active.len().saturating_sub(RECENT_TURNS_TO_KEEP));
            if cutoff > 0 {
                if self.pending_task.is_none() && self.prepared_lcm_context.is_none() {
                    let _ = self.start_lcm_job_with_model(
                        session,
                        Arc::clone(&provider),
                        &all_messages,
                        cutoff,
                        "critical_selected_route".to_string(),
                        critical_policy_model.clone(),
                        critical_policy_model.clone(),
                    );
                }
                if self.pending_task.is_some() || self.prepared_lcm_context.is_some() {
                    match self.wait_for_critical_lcm_attempt(session) {
                        Ok(Some(compacted)) => return CompactionAction::HardCompacted(compacted),
                        Ok(None) => {}
                        Err(error) => crate::logging::warn(&format!(
                            "Selected LCM critical route failed; trying active route: {error}"
                        )),
                    }
                }

                if critical_policy_model.is_some() {
                    if let Err(error) = self.start_lcm_job_with_model(
                        session,
                        Arc::clone(&provider),
                        &all_messages,
                        cutoff,
                        "critical_active_route".to_string(),
                        None,
                        critical_policy_model,
                    ) {
                        crate::logging::warn(&format!(
                            "Active-session LCM critical route could not start: {error}"
                        ));
                    } else {
                        match self.wait_for_critical_lcm_attempt(session) {
                            Ok(Some(compacted)) => {
                                return CompactionAction::HardCompacted(compacted);
                            }
                            Ok(None) => {}
                            Err(error) => crate::logging::warn(&format!(
                                "Active-session LCM critical route failed; trying legacy projection: {error}"
                            )),
                        }
                    }
                }

                if let Ok(Some(compacted)) = self.install_critical_legacy_summary(session, cutoff) {
                    return CompactionAction::HardCompacted(compacted);
                }
            }
            return match self
                .hard_lcm_compact_with_trigger(session, "critical_local_emergency_chain")
            {
                Ok(dropped) => CompactionAction::HardCompacted(dropped),
                Err(error) => {
                    crate::logging::error(&format!("Critical LCM recovery failed: {error}"));
                    CompactionAction::None
                }
            };
        }
        if self.maybe_start_lcm_with(session, provider) {
            CompactionAction::BackgroundStarted {
                trigger: self.mode_trigger_label().to_string(),
            }
        } else {
            CompactionAction::None
        }
    }

    fn wait_for_critical_lcm_attempt(
        &mut self,
        session: &mut crate::session::Session,
    ) -> Result<Option<usize>> {
        if let Ok(handle) = tokio::runtime::Handle::try_current()
            && !matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            )
        {
            // A synchronous wait on a current-thread runtime prevents the
            // spawned provider future from ever advancing. Fail this route
            // immediately so the deterministic local recovery can run.
            self.cancel_pending_work();
            anyhow::bail!("critical LCM provider wait requires a multi-thread Tokio runtime");
        }
        let start = Instant::now();
        let timeout = std::time::Duration::from_millis(HARD_THRESHOLD_PENDING_WAIT_MS);
        let poll = std::time::Duration::from_millis(HARD_THRESHOLD_PENDING_POLL_MS);
        loop {
            if self
                .pending_task
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
            {
                self.poll_lcm_candidate(session);
            }
            if self.prepared_lcm_context.is_some() {
                let event = self.commit_prepared_lcm(session)?;
                return Ok(event.and_then(|event| event.messages_compacted));
            }
            if self.pending_task.is_none() {
                return Ok(None);
            }
            if start.elapsed() >= timeout {
                self.cancel_pending_work();
                anyhow::bail!(
                    "critical LCM route timed out after {} ms",
                    start.elapsed().as_millis()
                );
            }
            // Tell Tokio this worker is intentionally blocking so it can lend a
            // replacement worker to the spawned compactor instead of deadlocking
            // under concurrent critical sessions.
            tokio::task::block_in_place(|| std::thread::sleep(poll));
        }
    }

    fn install_critical_legacy_summary(
        &mut self,
        session: &mut crate::session::Session,
        maximum_cutoff: usize,
    ) -> Result<Option<usize>> {
        if self.compacted_count != 0
            || session.context_frontier.is_some()
            || !session.context_nodes.is_empty()
        {
            // Legacy import is only valid before LCM has established ownership.
            // Re-importing an iterative LCM projection would add the covered
            // count twice and bind its summary to the wrong raw source range.
            return Ok(None);
        }
        let Some(legacy) = session.compaction.as_ref().filter(|state| {
            !state.summary_text.trim().is_empty()
                && state.openai_encrypted_content.is_none()
                && state.compacted_count > 0
                && state.compacted_count == state.covers_up_to_turn
                && state.compacted_count == state.original_turn_count
        }) else {
            return Ok(None);
        };
        let all_messages = session.messages_for_provider_uncached();
        if legacy.compacted_count > maximum_cutoff || legacy.compacted_count > all_messages.len() {
            return Ok(None);
        }
        let cutoff = legacy.compacted_count;
        if cutoff == 0 || safe_compaction_cutoff(&all_messages, cutoff) != cutoff {
            return Ok(None);
        }
        let summary_text = crate::message::redact_secrets(&legacy.summary_text);
        let pre_tokens = self.effective_token_count_with(&all_messages) as u64;
        let source = self.capture_lcm_source(
            session,
            cutoff,
            "legacy-rolling-import-v1".to_string(),
            "jcode".to_string(),
            "legacy:rolling".to_string(),
            crate::config::config().compaction.model.clone(),
            pre_tokens,
            "critical_legacy_import".to_string(),
        )?;
        self.prepared_lcm_context = Some(Self::prepare_lcm_context(
            source,
            CompactionResult {
                summary_text,
                atomic_parent_summaries: Vec::new(),
                openai_encrypted_content: None,
                covers_up_to_turn: cutoff,
                duration_ms: 0,
                summarized_messages: cutoff,
            },
        )?);
        let event = self.commit_prepared_lcm(session)?;
        Ok(event.and_then(|event| event.messages_compacted))
    }

    pub fn hard_lcm_compact_with(
        &mut self,
        session: &mut crate::session::Session,
    ) -> std::result::Result<usize, String> {
        self.hard_lcm_compact_with_trigger(session, "hard_compact")
    }

    fn hard_lcm_compact_with_trigger(
        &mut self,
        session: &mut crate::session::Session,
        trigger: &str,
    ) -> std::result::Result<usize, String> {
        let all_messages = session.messages_for_provider_uncached();
        let active = self.active_messages(&all_messages);
        if active.len() <= MIN_TURNS_TO_KEEP {
            return Err(format!(
                "Not enough messages to compact (have {}, need more than {})",
                active.len(),
                MIN_TURNS_TO_KEEP
            ));
        }
        let pre_tokens = self.effective_token_count_with(&all_messages) as u64;
        let active_char_counts: Vec<usize> = active.iter().map(message_char_count).collect();
        let mut remaining_suffix_chars = vec![0usize; active_char_counts.len() + 1];
        for index in (0..active_char_counts.len()).rev() {
            remaining_suffix_chars[index] =
                remaining_suffix_chars[index + 1].saturating_add(active_char_counts[index]);
        }
        let mut turns_to_keep = RECENT_TURNS_TO_KEEP.min(active.len().saturating_sub(1));
        let cutoff = loop {
            let candidate =
                safe_compaction_cutoff(active, active.len().saturating_sub(turns_to_keep));
            if candidate > 0
                && remaining_suffix_chars[candidate] / CHARS_PER_TOKEN <= self.token_budget
            {
                break candidate;
            }
            if turns_to_keep <= MIN_TURNS_TO_KEEP {
                break safe_compaction_cutoff(
                    active,
                    active.len().saturating_sub(MIN_TURNS_TO_KEEP),
                );
            }
            turns_to_keep = (turns_to_keep / 2).max(MIN_TURNS_TO_KEEP);
        };
        if cutoff == 0 {
            return Err("Cannot compact - would split tool call/result pairs".to_string());
        }

        let source = self
            .capture_lcm_source(
                session,
                cutoff,
                "jcode-emergency-v1".to_string(),
                "jcode".to_string(),
                "local:emergency".to_string(),
                crate::config::config().compaction.model.clone(),
                pre_tokens,
                trigger.to_string(),
            )
            .map_err(|error| error.to_string())?;
        let summary_text = build_emergency_summary_text(
            if session.context_frontier.is_none() {
                self.active_summary
                    .as_ref()
                    .map(|summary| summary.text.as_str())
            } else {
                None
            },
            cutoff,
            pre_tokens,
            self.token_budget,
            &active[..cutoff],
        );
        if let Some(task) = self.pending_task.take() {
            task.abort();
        }
        self.pending_lcm_source = None;
        self.pending_cutoff = 0;
        self.pending_source_fingerprint = None;
        self.pending_trigger = None;
        self.prepared_lcm_context = None;

        let result = CompactionResult {
            summary_text,
            atomic_parent_summaries: Vec::new(),
            openai_encrypted_content: None,
            covers_up_to_turn: cutoff,
            duration_ms: 0,
            summarized_messages: cutoff,
        };
        let mut prepared =
            Self::prepare_lcm_context(source, result).map_err(|error| error.to_string())?;
        prepared.messages_dropped = Some(cutoff);
        self.prepared_lcm_context = Some(prepared);
        self.commit_prepared_lcm(session)
            .map_err(|error| error.to_string())?;
        Ok(cutoff)
    }

    pub fn force_lcm_compact_with(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> std::result::Result<(), String> {
        if self.pending_task.is_some() || self.prepared_lcm_context.is_some() {
            return Err("Compaction already in progress".to_string());
        }
        let all_messages = session.messages_for_provider_uncached();
        let active = self.active_messages(&all_messages);
        if active.len() <= RECENT_TURNS_TO_KEEP {
            return Err(format!(
                "Not enough messages to compact (need more than {}, have {})",
                RECENT_TURNS_TO_KEEP,
                active.len()
            ));
        }
        if self.context_usage_with(&all_messages) < MANUAL_COMPACT_MIN_THRESHOLD {
            return Err(format!(
                "Context usage too low ({:.1}%) - nothing to compact",
                self.context_usage_with(&all_messages) * 100.0
            ));
        }
        let cutoff =
            safe_compaction_cutoff(active, active.len().saturating_sub(RECENT_TURNS_TO_KEEP));
        if cutoff == 0 {
            return Err("Cannot compact - would split tool call/result pairs".to_string());
        }
        self.start_lcm_job(
            session,
            provider,
            &all_messages,
            cutoff,
            "manual".to_string(),
        )
        .map_err(|error| error.to_string())
    }

    /// Ensure context fits before an API call.
    ///
    /// Starts background compaction if above 80%. If context is critically full
    /// (>=95%), also performs an immediate hard-compact (drops old messages) so
    /// the next API call doesn't fail with "prompt too long".
    pub fn ensure_context_fits(
        &mut self,
        all_messages: &[Message],
        provider: Arc<dyn Provider>,
    ) -> CompactionAction {
        // If we're already critically full, hard-compact synchronously *before*
        // kicking off any background compaction. Starting a background task here
        // would only get aborted by the hard compact (its summary is computed
        // against the pre-hard-compact offsets), so skip the wasted work and the
        // risk of a stale `pending_cutoff` being applied later.
        let usage = self.context_usage_with(all_messages);
        if usage >= CRITICAL_THRESHOLD {
            if self.pending_task.is_some() {
                crate::logging::warn(&format!(
                    "[compaction] Context at {:.1}% with background compaction in flight — waiting up to {}ms before hard compact",
                    usage * 100.0,
                    HARD_THRESHOLD_PENDING_WAIT_MS,
                ));
                let waited = self.wait_for_pending_compaction_at_hard_threshold(all_messages);
                let post_wait_usage = self.context_usage_with(all_messages);
                crate::logging::info(&format!(
                    "[compaction] Hard-threshold wait complete: waited_ms={}, applied={}, timed_out={}, usage_now={:.1}%",
                    waited.waited_ms,
                    waited.applied,
                    waited.timed_out,
                    post_wait_usage * 100.0,
                ));
                if post_wait_usage < CRITICAL_THRESHOLD {
                    // We may still be above the soft threshold. Let the normal
                    // path below decide whether another async compaction should
                    // start, but avoid dropping context now that the hard
                    // threshold has been cleared.
                } else {
                    crate::logging::warn(&format!(
                        "[compaction] Context still at {:.1}% after waiting for in-flight compaction; escalating to hard compact",
                        post_wait_usage * 100.0,
                    ));
                    match self.hard_compact_with(all_messages) {
                        Ok(dropped) => {
                            let post_usage = self.context_usage_with(all_messages);
                            crate::logging::info(&format!(
                                "[compaction] Hard compact dropped {} messages, context now at {:.1}%",
                                dropped,
                                post_usage * 100.0,
                            ));
                            return CompactionAction::HardCompacted(dropped);
                        }
                        Err(reason) => {
                            crate::logging::error(&format!(
                                "[compaction] Hard compact failed at critical threshold: {}",
                                reason
                            ));
                        }
                    }
                }
            } else {
                crate::logging::warn(&format!(
                    "[compaction] Context at {:.1}% (critical threshold {:.0}%) — performing synchronous hard compact",
                    usage * 100.0,
                    CRITICAL_THRESHOLD * 100.0,
                ));
                match self.hard_compact_with(all_messages) {
                    Ok(dropped) => {
                        let post_usage = self.context_usage_with(all_messages);
                        crate::logging::info(&format!(
                            "[compaction] Hard compact dropped {} messages, context now at {:.1}%",
                            dropped,
                            post_usage * 100.0,
                        ));
                        return CompactionAction::HardCompacted(dropped);
                    }
                    Err(reason) => {
                        crate::logging::error(&format!(
                            "[compaction] Hard compact failed at critical threshold: {}",
                            reason
                        ));
                    }
                }
            }
        }

        let was_compacting = self.is_compacting();
        self.maybe_start_compaction_with(all_messages, provider);
        let bg_started = !was_compacting && self.is_compacting();

        if bg_started {
            CompactionAction::BackgroundStarted {
                trigger: self
                    .pending_trigger
                    .clone()
                    .unwrap_or_else(|| self.mode_trigger_label().to_string()),
            }
        } else {
            CompactionAction::None
        }
    }

    fn wait_for_pending_compaction_at_hard_threshold(
        &mut self,
        all_messages: &[Message],
    ) -> HardThresholdWait {
        let start = Instant::now();
        let timeout = std::time::Duration::from_millis(HARD_THRESHOLD_PENDING_WAIT_MS);
        let poll = std::time::Duration::from_millis(HARD_THRESHOLD_PENDING_POLL_MS);

        while start.elapsed() < timeout {
            if self
                .pending_task
                .as_ref()
                .map(|task| task.is_finished())
                .unwrap_or(false)
            {
                self.check_and_apply_compaction_with(all_messages);
                return HardThresholdWait {
                    waited_ms: start.elapsed().as_millis() as u64,
                    applied: self.last_compaction.is_some(),
                    timed_out: false,
                };
            }
            std::thread::sleep(poll);
        }

        if self
            .pending_task
            .as_ref()
            .map(|task| task.is_finished())
            .unwrap_or(false)
        {
            self.check_and_apply_compaction_with(all_messages);
            return HardThresholdWait {
                waited_ms: start.elapsed().as_millis() as u64,
                applied: self.last_compaction.is_some(),
                timed_out: false,
            };
        }

        HardThresholdWait {
            waited_ms: start.elapsed().as_millis() as u64,
            applied: false,
            timed_out: true,
        }
    }

    /// Force immediate compaction (for manual /compact command).
    pub fn force_compact_with(
        &mut self,
        all_messages: &[Message],
        provider: Arc<dyn Provider>,
    ) -> Result<(), String> {
        if self.engine == crate::config::CompactionEngine::Lcm {
            return Err("LCM owns context; use force_lcm_compact_with".to_string());
        }
        if self.pending_task.is_some() {
            return Err("Compaction already in progress".to_string());
        }

        let active = self.active_messages(all_messages);

        if active.len() <= RECENT_TURNS_TO_KEEP {
            return Err(format!(
                "Not enough messages to compact (need more than {}, have {})",
                RECENT_TURNS_TO_KEEP,
                active.len()
            ));
        }

        if self.context_usage_with(all_messages) < MANUAL_COMPACT_MIN_THRESHOLD {
            return Err(format!(
                "Context usage too low ({:.1}%) - nothing to compact",
                self.context_usage_with(all_messages) * 100.0
            ));
        }

        let mut cutoff = active.len().saturating_sub(RECENT_TURNS_TO_KEEP);
        if cutoff == 0 {
            return Err("No messages available to compact after keeping recent turns".to_string());
        }

        cutoff = safe_compaction_cutoff(active, cutoff);
        if cutoff == 0 {
            return Err("Cannot compact - would split tool call/result pairs".to_string());
        }

        let messages_to_summarize: Vec<Message> = active[..cutoff].to_vec();
        let msg_count = messages_to_summarize.len();
        let existing_summary = self.active_summary.clone();

        self.pending_cutoff = cutoff;
        self.pending_source_fingerprint = message_fingerprint(&active[..cutoff]);
        self.pending_trigger = Some("manual".to_string());

        self.pending_task = Some(tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result =
                generate_compaction_artifact(provider, messages_to_summarize, existing_summary)
                    .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            crate::logging::info(&format!(
                "Compaction finished in {:.2}s ({} messages summarized)",
                duration_ms as f64 / 1000.0,
                msg_count,
            ));
            crate::bus::Bus::global().publish(crate::bus::BusEvent::CompactionFinished);
            result.map(|mut result| {
                result.duration_ms = duration_ms;
                result.summarized_messages = msg_count;
                result
            })
        }));

        Ok(())
    }

    /// Check if background compaction is done and apply it, updating rolling
    /// token-estimate state from the provided full message list.
    pub fn check_and_apply_compaction_with(&mut self, all_messages: &[Message]) {
        if self.engine == crate::config::CompactionEngine::Lcm {
            return;
        }
        self.clamp_compacted_count_to_messages(all_messages, "check_and_apply_start");
        let task = match self.pending_task.take() {
            Some(task) => task,
            None => return,
        };

        // Check if done without blocking
        if !task.is_finished() {
            // Not done yet, put it back
            self.pending_task = Some(task);
            return;
        }

        // Get result
        match futures::executor::block_on(task) {
            Ok(Ok(result)) => {
                let trigger = self
                    .pending_trigger
                    .clone()
                    .unwrap_or_else(|| self.mode_trigger_label().to_string());
                self.log_compaction_state("apply_start", &trigger, all_messages);

                // Defense-in-depth: `pending_cutoff` was computed against the
                // active slice as it existed when the background task started. If
                // the active slice has since shrunk (e.g. an interleaving hard
                // compaction advanced `compacted_count`), the produced summary no
                // longer aligns with the current offsets, and applying the stale
                // cutoff would over-advance `compacted_count` and wipe out live
                // messages (observed as "kept 0 recent messages"). A soft
                // compaction must always leave a healthy active tail, so detect
                // the mismatch and discard the stale result instead of applying
                // it. Hard compacts already abort the pending task, so this is a
                // belt-and-suspenders guard.
                let active_len = self.active_messages(all_messages).len();
                let source_matches = message_fingerprint(
                    &self.active_messages(all_messages)[..self.pending_cutoff.min(active_len)],
                ) == self.pending_source_fingerprint;
                let leaves_no_healthy_tail =
                    self.pending_cutoff > active_len.saturating_sub(MIN_TURNS_TO_KEEP);
                if !all_messages.is_empty() && (leaves_no_healthy_tail || !source_matches) {
                    crate::logging::warn(&format!(
                        "[compaction] Discarding stale background compaction result (pending_cutoff={}, active_len={}, trigger={}) — context changed since it started",
                        self.pending_cutoff, active_len, trigger,
                    ));
                    self.pending_cutoff = 0;
                    self.pending_source_fingerprint = None;
                    self.pending_trigger = None;
                    return;
                }

                let pre_tokens = self.effective_token_count_with(all_messages) as u64;
                let compacted_chars: usize = self
                    .active_messages(all_messages)
                    .iter()
                    .take(self.pending_cutoff)
                    .map(message_char_count)
                    .sum();
                let summary = Summary {
                    text: result.summary_text,
                    openai_encrypted_content: result.openai_encrypted_content,
                    covers_up_to_turn: result.covers_up_to_turn,
                    original_turn_count: self.pending_cutoff,
                };

                // Advance the compacted count — these messages are now summarized
                self.compacted_count = self.compacted_count.saturating_add(self.pending_cutoff);
                if !all_messages.is_empty() {
                    self.compacted_count = self.compacted_count.min(all_messages.len());
                }
                self.active_chars.set_exact(
                    self.active_message_chars_with(all_messages)
                        .saturating_sub(compacted_chars),
                );

                // Store summary
                self.active_summary = Some(summary);
                self.discard_oversized_openai_native_compaction();
                self.observed_input_tokens = None;
                let post_tokens = self.effective_token_count_with(all_messages) as u64;
                let ownership = if self
                    .active_summary
                    .as_ref()
                    .and_then(|summary| summary.openai_encrypted_content.as_ref())
                    .is_some()
                {
                    "native"
                } else {
                    "rolling"
                };
                let summary_chars = self
                    .active_summary
                    .as_ref()
                    .map(|summary| summary.text.len());
                let active_messages = self.active_messages_count();
                self.last_compaction = Some(CompactionEvent {
                    trigger: trigger.clone(),
                    engine: Some("rolling".to_string()),
                    ownership: Some(ownership.to_string()),
                    pre_tokens: Some(pre_tokens),
                    post_tokens: Some(post_tokens),
                    tokens_saved: Some(pre_tokens.saturating_sub(post_tokens)),
                    duration_ms: Some(result.duration_ms),
                    messages_dropped: None,
                    messages_compacted: Some(result.summarized_messages),
                    summary_chars,
                    active_messages: Some(active_messages),
                    ..CompactionEvent::default()
                });
                crate::logging::info(&format!(
                    "[TIMING] compaction_complete: trigger={}, duration={}ms, pre_tokens={}, post_tokens={}, tokens_saved={}, messages_compacted={}, summary_chars={}, active_messages={}",
                    self.last_compaction
                        .as_ref()
                        .map(|event| event.trigger.as_str())
                        .unwrap_or("unknown"),
                    result.duration_ms,
                    pre_tokens,
                    post_tokens,
                    pre_tokens.saturating_sub(post_tokens),
                    result.summarized_messages,
                    self.active_summary
                        .as_ref()
                        .map(|summary| summary.text.len())
                        .unwrap_or(0),
                    self.active_messages_count(),
                ));
                self.log_compaction_outcome(CompactionOutcomeLog {
                    trigger: &trigger,
                    pre_tokens,
                    post_tokens,
                    messages_compacted: result.summarized_messages,
                    messages_dropped: None,
                    duration_ms: result.duration_ms,
                    all_messages,
                });

                // Reset cooldown counter so proactive/semantic modes don't
                // fire again immediately after a successful compaction.
                self.turns_since_last_compact = 0;

                self.pending_cutoff = 0;
                self.pending_source_fingerprint = None;
                self.pending_trigger = None;
            }
            Ok(Err(e)) => {
                crate::logging::error(&format!("[compaction] Failed to generate summary: {}", e));
                self.pending_trigger = None;
                self.pending_cutoff = 0;
                self.pending_source_fingerprint = None;
            }
            Err(e) => {
                crate::logging::error(&format!("[compaction] Task panicked: {}", e));
                self.pending_trigger = None;
                self.pending_cutoff = 0;
                self.pending_source_fingerprint = None;
            }
        }
    }

    /// Backward-compatible completion check without caller history.
    pub fn check_and_apply_compaction(&mut self) {
        self.check_and_apply_compaction_with(&[]);
        self.active_chars.invalidate();
    }

    /// Take the last compaction event (if any)
    pub fn take_compaction_event(&mut self) -> Option<CompactionEvent> {
        self.last_compaction.take()
    }

    /// Get messages for API call (with summary if compacted).
    /// Takes the full message list from the caller.
    pub fn messages_for_api_with(&mut self, all_messages: &[Message]) -> Vec<Message> {
        self.check_and_apply_compaction_with(all_messages);
        self.discard_oversized_openai_native_compaction();

        let active = self.active_messages(all_messages);

        match &self.active_summary {
            Some(summary) => {
                let summary_block = summary
                    .openai_encrypted_content
                    .as_ref()
                    .map(|encrypted_content| ContentBlock::OpenAICompaction {
                        encrypted_content: encrypted_content.clone(),
                    })
                    .unwrap_or_else(|| ContentBlock::Text {
                        text: compacted_summary_text_block(&summary.text),
                        cache_control: None,
                    });

                let mut result = Vec::with_capacity(active.len() + 1);

                result.push(Message {
                    role: Role::User,
                    content: vec![summary_block],
                    timestamp: None,
                    tool_duration_ms: None,
                });

                // Clone only the active (non-compacted) messages
                result.extend(active.iter().cloned());

                result
            }
            None => active.to_vec(),
        }
    }

    /// Check if compaction is in progress
    pub fn is_compacting(&self) -> bool {
        self.pending_task.is_some() || self.prepared_lcm_context.is_some()
    }

    pub fn engine(&self) -> crate::config::CompactionEngine {
        self.engine.clone()
    }

    /// Synchronize a long-lived manager with the authoritative runtime policy.
    /// Changing engines invalidates every pending result because rolling and LCM
    /// candidates have different ownership and publication contracts.
    pub fn synchronize_engine(&mut self, engine: crate::config::CompactionEngine) -> bool {
        if self.engine == engine {
            return false;
        }
        self.cancel_pending_work();
        if engine == crate::config::CompactionEngine::Lcm {
            // A rolling/native projection may be encrypted, lossy, or tied to
            // another provider. The first LCM leaf must therefore summarize the
            // canonical raw prefix from message zero rather than claiming that
            // an opaque legacy projection proves source it never exposed.
            self.compacted_count = 0;
            self.active_summary = None;
            self.active_chars.invalidate();
            self.observed_input_tokens = None;
        }
        self.engine = engine;
        true
    }

    /// Get the active compaction mode
    pub fn mode(&self) -> crate::config::CompactionMode {
        self.mode.clone()
    }

    /// Change the active compaction mode for this session at runtime.
    pub fn set_mode(&mut self, mode: crate::config::CompactionMode) {
        self.mode = mode.clone();
        self.compaction_config.mode = mode;
    }

    fn mode_trigger_label(&self) -> &'static str {
        self.mode.as_str()
    }

    /// Get the number of compacted (summarized) messages
    pub fn compacted_count(&self) -> usize {
        self.compacted_count
    }

    /// Get the character count of the active summary (0 if none)
    pub fn summary_chars(&self) -> usize {
        self.active_summary
            .as_ref()
            .map(summary_payload_char_count)
            .unwrap_or(0)
    }

    /// Get the current number of active, un-compacted messages.
    pub fn active_messages_count(&self) -> usize {
        self.total_turns.saturating_sub(self.compacted_count)
    }

    /// Get stats about current state (without message data)
    pub fn stats(&self) -> CompactionStats {
        CompactionStats {
            total_turns: self.total_turns,
            active_messages: 0, // unknown without messages
            has_summary: self.active_summary.is_some(),
            is_compacting: self.is_compacting(),
            token_estimate: self.token_estimate(),
            effective_tokens: self.effective_token_count(),
            observed_input_tokens: self.observed_input_tokens,
            context_usage: self.context_usage(),
        }
    }

    /// Get stats with full message data
    pub fn stats_with(&self, all_messages: &[Message]) -> CompactionStats {
        let active = self.active_messages(all_messages);
        CompactionStats {
            total_turns: self.total_turns,
            active_messages: active.len(),
            has_summary: self.active_summary.is_some(),
            is_compacting: self.is_compacting(),
            token_estimate: self.token_estimate_with(all_messages),
            effective_tokens: self.effective_token_count_with(all_messages),
            observed_input_tokens: self.observed_input_tokens,
            context_usage: self.context_usage_with(all_messages),
        }
    }

    fn cached_semantic_embedding(&mut self, text: &str) -> Option<Vec<f32>> {
        let key = semantic_cache_key(text);

        if let Some((cached, recency)) = self.semantic_embed_cache.get_mut(&key) {
            let counter = self.semantic_embed_cache_counter;
            self.semantic_embed_cache_counter = counter.wrapping_add(1);
            *recency = counter;
            return cached.clone();
        }

        let embedding = crate::embedding::embed(text).ok();
        self.insert_semantic_embedding_cache(key, embedding.clone());
        embedding
    }

    fn insert_semantic_embedding_cache(&mut self, key: u64, embedding: Option<Vec<f32>>) {
        if self.semantic_embed_cache.len() >= SEMANTIC_EMBED_CACHE_CAPACITY {
            let oldest_key = self
                .semantic_embed_cache
                .iter()
                .min_by_key(|(_, (_, recency))| *recency)
                .map(|(&key, _)| key);
            if let Some(oldest_key) = oldest_key {
                self.semantic_embed_cache.remove(&oldest_key);
            }
        }

        let counter = self.semantic_embed_cache_counter;
        self.semantic_embed_cache_counter = counter.wrapping_add(1);
        self.semantic_embed_cache.insert(key, (embedding, counter));
    }

    /// Poll for compaction completion and return an event if one was applied.
    pub fn poll_compaction_event_with(
        &mut self,
        all_messages: &[Message],
    ) -> Option<CompactionEvent> {
        self.check_and_apply_compaction_with(all_messages);
        self.take_compaction_event()
    }

    /// Emergency hard compaction: drop old messages without summarizing.
    /// Takes the caller's full message list to inspect content.
    ///
    /// When the remaining turns (after keeping `RECENT_TURNS_TO_KEEP`) still
    /// exceed the token budget, progressively keeps fewer turns down to
    /// `MIN_TURNS_TO_KEEP`.
    pub fn hard_compact_with(&mut self, all_messages: &[Message]) -> Result<usize, String> {
        if self.engine == crate::config::CompactionEngine::Lcm {
            return Err("LCM owns context; use hard_lcm_compact_with".to_string());
        }
        if self.clamp_compacted_count_to_messages(all_messages, "hard_compact_start") {
            self.log_compaction_state("hard_compact_clamped", "hard_compact", all_messages);
        }

        let active = self.active_messages(all_messages);

        if active.len() <= MIN_TURNS_TO_KEEP {
            return Err(format!(
                "Not enough messages to compact (have {}, need more than {})",
                active.len(),
                MIN_TURNS_TO_KEEP
            ));
        }

        let pre_tokens = self.effective_token_count_with(all_messages) as u64;
        self.log_compaction_state("hard_compact_start", "hard_compact", all_messages);
        let active_char_counts: Vec<usize> = active.iter().map(message_char_count).collect();
        let mut remaining_suffix_chars = vec![0usize; active_char_counts.len() + 1];
        for idx in (0..active_char_counts.len()).rev() {
            remaining_suffix_chars[idx] =
                remaining_suffix_chars[idx + 1].saturating_add(active_char_counts[idx]);
        }

        let mut turns_to_keep = RECENT_TURNS_TO_KEEP.min(active.len().saturating_sub(1));
        let mut cutoff;
        loop {
            cutoff = active.len().saturating_sub(turns_to_keep);
            cutoff = safe_compaction_cutoff(active, cutoff);

            if cutoff > 0 {
                let remaining_tokens = remaining_suffix_chars[cutoff] / CHARS_PER_TOKEN;
                if remaining_tokens <= self.token_budget {
                    break;
                }
            }

            if turns_to_keep <= MIN_TURNS_TO_KEEP {
                cutoff = active.len().saturating_sub(MIN_TURNS_TO_KEEP);
                cutoff = safe_compaction_cutoff(active, cutoff);
                break;
            }
            turns_to_keep = (turns_to_keep / 2).max(MIN_TURNS_TO_KEEP);
        }

        if cutoff == 0 {
            return Err("Cannot compact — would split tool call/result pairs".to_string());
        }

        // This hard compact will advance `compacted_count` and supersede any
        // in-flight background (reactive/proactive/semantic) compaction. That
        // background task summarized messages relative to the *old*
        // `compacted_count`; if it completed afterwards, `check_and_apply_*`
        // would add its stale `pending_cutoff` on top of the already-advanced
        // `compacted_count`, double-compacting and wiping out all live messages
        // (observed as "kept 0 recent messages"). Abort and discard it now that
        // we're committed to the hard compact.
        if let Some(task) = self.pending_task.take() {
            task.abort();
            crate::logging::warn(&format!(
                "[compaction] Aborting in-flight background compaction (pending_cutoff={}, trigger={:?}) — superseded by hard compact",
                self.pending_cutoff, self.pending_trigger,
            ));
            self.pending_cutoff = 0;
            self.pending_source_fingerprint = None;
            self.pending_trigger = None;
        }

        let dropped_count = cutoff;
        let summary_text = build_emergency_summary_text(
            self.active_summary
                .as_ref()
                .map(|summary| summary.text.as_str()),
            dropped_count,
            pre_tokens,
            self.token_budget,
            &active[..cutoff],
        );

        let summary = Summary {
            text: summary_text,
            openai_encrypted_content: None,
            covers_up_to_turn: cutoff,
            original_turn_count: cutoff,
        };

        self.compacted_count = self
            .compacted_count
            .saturating_add(cutoff)
            .min(all_messages.len());
        self.active_chars.set_exact(remaining_suffix_chars[cutoff]);
        self.active_summary = Some(summary);
        self.observed_input_tokens = None;
        let post_tokens = self.effective_token_count_with(all_messages) as u64;
        self.last_compaction = Some(CompactionEvent {
            trigger: "hard_compact".to_string(),
            engine: Some("rolling".to_string()),
            ownership: Some("rolling".to_string()),
            fallback_reason: Some("critical_local_emergency".to_string()),
            pre_tokens: Some(pre_tokens),
            post_tokens: Some(post_tokens),
            tokens_saved: Some(pre_tokens.saturating_sub(post_tokens)),
            duration_ms: Some(0),
            messages_dropped: Some(dropped_count),
            messages_compacted: Some(dropped_count),
            summary_chars: self
                .active_summary
                .as_ref()
                .map(|summary| summary.text.len()),
            active_messages: Some(self.active_messages_count()),
            ..CompactionEvent::default()
        });
        self.log_compaction_outcome(CompactionOutcomeLog {
            trigger: "hard_compact",
            pre_tokens,
            post_tokens,
            messages_compacted: dropped_count,
            messages_dropped: Some(dropped_count),
            duration_ms: 0,
            all_messages,
        });

        Ok(dropped_count)
    }

    /// Emergency truncation: shorten large tool results in active messages.
    ///
    /// When hard compaction isn't sufficient (the remaining few turns are
    /// individually too large), this truncates tool result content so the
    /// conversation can fit within the token budget.
    ///
    /// Returns the number of tool results that were truncated.
    pub fn emergency_truncate_with(&mut self, all_messages: &mut [Message]) -> usize {
        let start = self.compacted_count.min(all_messages.len());
        let active = &mut all_messages[start..];
        let truncated = emergency_truncate_large_payloads(
            active,
            EMERGENCY_TOOL_RESULT_MAX_CHARS,
            EMERGENCY_IMAGE_MAX_CHARS,
        );

        if truncated > 0 {
            self.observed_input_tokens = None;
            self.active_chars.invalidate();
        }
        truncated
    }

    /// Synchronously force the context back under budget without waiting for a
    /// background summary.
    ///
    /// This is the shared escalation policy used by every emergency-recovery
    /// caller: drop old turns via [`hard_compact_with`], then — only if the
    /// context is *still* over budget — shorten oversized tool results via
    /// [`emergency_truncate_with`]. Previously each caller open-coded this
    /// sequence with subtly different escalation (one retried after a hard
    /// compact without re-checking the budget), so centralizing it both removes
    /// the duplication and guarantees consistent behavior.
    ///
    /// Returns a structured outcome so callers can render their own
    /// user-facing message. `pre_usage` is the context usage fraction observed
    /// before recovery (captured here so the report matches what triggered it).
    pub fn recover_within_budget(&mut self, all_messages: &mut [Message]) -> EmergencyRecovery {
        let pre_usage = self.context_usage_with(all_messages);

        let dropped = match self.hard_compact_with(all_messages) {
            Ok(dropped) => Some(dropped),
            Err(reason) => {
                crate::logging::warn(&format!(
                    "[compaction] recover_within_budget: hard compact failed ({reason})"
                ));
                None
            }
        };

        // Only escalate to truncation when dropping turns did not get us under
        // budget (or could not run at all).
        let still_over_budget = self.context_usage_with(all_messages) > 1.0 || dropped.is_none();
        let truncated = if still_over_budget {
            self.emergency_truncate_with(all_messages)
        } else {
            0
        };

        EmergencyRecovery {
            pre_usage,
            dropped,
            truncated,
        }
    }
}

/// Outcome of [`CompactionManager::recover_within_budget`].
#[derive(Debug, Clone, Copy)]
pub struct EmergencyRecovery {
    /// Context usage fraction (1.0 == full budget) observed before recovery.
    pub pre_usage: f32,
    /// Messages dropped by the hard compact, or `None` if it could not run.
    pub dropped: Option<usize>,
    /// Number of oversized tool results that were truncated as a fallback.
    pub truncated: usize,
}

impl EmergencyRecovery {
    /// Whether any space-reclaiming action actually happened.
    pub fn did_anything(&self) -> bool {
        self.dropped.unwrap_or(0) > 0 || self.truncated > 0
    }

    /// A user-facing description of what recovery did, without a trailing
    /// call to action (callers append their own, e.g. "Retrying..." or
    /// "You can continue."). `trigger_usage` is the usage fraction that
    /// triggered recovery (rendered as a percentage).
    pub fn summary_line(&self, trigger_usage: f32) -> String {
        let pct = trigger_usage * 100.0;
        match (self.dropped, self.truncated) {
            (Some(dropped), 0) => format!(
                "⚡ Emergency compaction: dropped {dropped} old messages (context was at {pct:.0}%).",
            ),
            (Some(dropped), truncated) => format!(
                "⚡ Emergency compaction: dropped {dropped} old messages and truncated {truncated} tool result(s) (context was at {pct:.0}%).",
            ),
            (None, truncated) => format!(
                "⚡ Emergency truncation: shortened {truncated} large tool result(s) to fit context.",
            ),
        }
    }
}

impl Default for CompactionManager {
    fn default() -> Self {
        Self::new()
    }
}

static LCM_SAFE_PROMPT_CHARS: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LCM_SCHEDULER: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
// Background hierarchy/leaf work may use at most three slots. Critical context
// recovery and user-waiting transfers can therefore always enter the shared
// FIFO scheduler without waiting behind a newly admitted background job.
static LCM_BACKGROUND_SCHEDULER: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(3)));
#[cfg(not(test))]
const LCM_PROVIDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
#[cfg(test)]
const LCM_PROVIDER_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(not(test))]
const LCM_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(test)]
const LCM_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LcmJobPriority {
    Background,
    Critical,
}

impl LcmJobPriority {
    fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Critical => "critical",
        }
    }
}

struct LcmSchedulerPermit {
    _shared: tokio::sync::OwnedSemaphorePermit,
    _background: Option<tokio::sync::OwnedSemaphorePermit>,
}

async fn acquire_lcm_scheduler(priority: LcmJobPriority) -> Result<LcmSchedulerPermit> {
    acquire_lcm_scheduler_from(
        Arc::clone(&LCM_SCHEDULER),
        Arc::clone(&LCM_BACKGROUND_SCHEDULER),
        priority,
        LCM_QUEUE_TIMEOUT,
    )
    .await
}

async fn acquire_lcm_scheduler_from(
    shared_scheduler: Arc<tokio::sync::Semaphore>,
    background_scheduler: Arc<tokio::sync::Semaphore>,
    priority: LcmJobPriority,
    queue_timeout: std::time::Duration,
) -> Result<LcmSchedulerPermit> {
    tokio::time::timeout(queue_timeout, async move {
        let background = if priority == LcmJobPriority::Background {
            Some(
                background_scheduler
                    .acquire_owned()
                    .await
                    .map_err(|_| anyhow::anyhow!("LCM background scheduler is closed"))?,
            )
        } else {
            None
        };
        let shared = shared_scheduler
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("LCM scheduler is closed"))?;
        Ok(LcmSchedulerPermit {
            _shared: shared,
            _background: background,
        })
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "LCM {} scheduler queue timed out after {} ms",
            priority.as_str(),
            queue_timeout.as_millis()
        )
    })?
}

const LCM_SUMMARY_SYSTEM_PROMPT: &str = r#"You are the Jcode LCM context compactor.
Return only stable Markdown with these sections: Objective and user intent; Explicit constraints and prohibited actions; Decisions and rationale; Repository state and exact paths/symbols/branches/commits; Changes actually completed; Commands and tests with actual outcomes; Failures, diagnosis, and unresolved blockers; Open questions and next steps; Retrieval anchors and source range.
Separate observed facts from plans or assumptions. Preserve newer corrections and mark superseded decisions. Every non-placeholder content line must be `- [source] ` followed by one exact, contiguous excerpt copied verbatim from the observed source. Do not paraphrase or combine excerpts. Use `None observed.` when a section has no useful excerpt. Never claim an edit, commit, command, or test happened unless an exact source excerpt says it did. Do not include chain-of-thought, credentials, tokens, or raw tool blobs. Do not invent missing details."#;

const LCM_REQUIRED_SECTIONS: [&str; 9] = [
    "Objective and user intent",
    "Explicit constraints and prohibited actions",
    "Decisions and rationale",
    "Repository state and exact paths/symbols/branches/commits",
    "Changes actually completed",
    "Commands and tests with actual outcomes",
    "Failures, diagnosis, and unresolved blockers",
    "Open questions and next steps",
    "Retrieval anchors and source range",
];

fn compactor_safe_messages(messages: &[Message]) -> Vec<Message> {
    let mut safe_messages = messages.to_vec();
    for message in &mut safe_messages {
        for block in &mut message.content {
            match block {
                ContentBlock::ToolUse { input, .. } => {
                    *input = serde_json::json!({
                        "payload": "omitted from compactor context; use conversation_search for canonical details"
                    });
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let status = match is_error {
                        Some(true) => "error",
                        Some(false) => "success",
                        None => "unknown",
                    };
                    *content = format!(
                        "[tool result payload omitted from compactor context; status={status}; use conversation_search for canonical details]"
                    );
                }
                _ => {}
            }
        }
    }
    safe_messages
}

fn lcm_safe_source_text(messages: &[Message], existing_summary: Option<&Summary>) -> String {
    let safe_messages = compactor_safe_messages(messages);
    crate::message::redact_secrets(&build_compaction_conversation_text(
        &safe_messages,
        existing_summary,
    ))
}

fn build_lcm_compaction_prompt(
    messages: &[Message],
    existing_summary: Option<&Summary>,
    max_prompt_chars: usize,
) -> String {
    let mut source = lcm_safe_source_text(messages, existing_summary);
    const INSTRUCTION: &str = "Select only exact source excerpts according to the section schema in the system instruction. Prefix every excerpt with `- [source] ` and use every required heading even when its value is `None observed.`.";
    const OMISSION: &str =
        "\n\n... [middle source omitted from this chunk; use canonical retrieval anchors] ...\n\n";
    let overhead = INSTRUCTION.len() + OMISSION.len() + 9;
    if source.len().saturating_add(overhead) > max_prompt_chars && max_prompt_chars > overhead {
        let budget = max_prompt_chars - overhead;
        let head_budget = budget / 2;
        let tail_budget = budget.saturating_sub(head_budget);
        let head = jcode_compaction_core::truncate_str_boundary(&source, head_budget);
        let mut tail_start = source.len().saturating_sub(tail_budget);
        while tail_start < source.len() && !source.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        source = format!("{head}{OMISSION}{}", &source[tail_start..]);
    }
    format!("{source}\n\n---\n\n{INSTRUCTION}")
}

fn lcm_output_has_required_sections(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    LCM_REQUIRED_SECTIONS
        .iter()
        .all(|section| lower.contains(&format!("# {}", section.to_ascii_lowercase())))
}

fn lcm_output_is_grounded(summary: &str, source: &str) -> bool {
    summary.lines().all(|line| {
        let line = line.trim();
        if line.is_empty() {
            return true;
        }
        if line.starts_with('#') {
            let heading = line.trim_start_matches('#').trim();
            return LCM_REQUIRED_SECTIONS
                .iter()
                .any(|required| heading.eq_ignore_ascii_case(required));
        }
        let content = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line)
            .trim();
        if content.eq_ignore_ascii_case("none observed.")
            || content.eq_ignore_ascii_case("none observed")
        {
            return true;
        }
        content.strip_prefix("[source] ").is_some_and(|excerpt| {
            let excerpt = excerpt.trim();
            !excerpt.is_empty()
                && source
                    .lines()
                    .any(|source_line| source_line.trim() == excerpt)
        })
    })
}

fn is_lcm_context_limit_error(error: &anyhow::Error) -> bool {
    let lower = error.to_string().to_ascii_lowercase();
    (lower.contains("context")
        && (lower.contains("limit")
            || lower.contains("length")
            || lower.contains("window")
            || lower.contains("too long")))
        || lower.contains("too many tokens")
        || lower.contains("prompt is too long")
        || lower.contains("prompt too long")
        || lower.contains("input is too long")
        || lower.contains("maximum token")
        || lower.contains("max token")
}

fn lcm_prompt_budget(
    provider: &dyn Provider,
    route_identity: Option<&str>,
) -> (String, usize, usize) {
    let context_tokens = provider.context_window().max(4_096);
    let output_reserve = (context_tokens / 8).clamp(1_024, 8_192);
    let safety_reserve = (context_tokens / 10).max(1_024);
    let prompt_chars = context_tokens
        .saturating_sub(output_reserve)
        .saturating_sub(safety_reserve)
        .max(1_024)
        .saturating_mul(CHARS_PER_TOKEN);
    let route_key = route_identity
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}:{}", provider.name(), provider.model()));
    let cached = LCM_SAFE_PROMPT_CHARS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&route_key)
        .copied();
    (
        route_key,
        cached.map_or(prompt_chars, |safe| safe.min(prompt_chars)),
        output_reserve.saturating_mul(CHARS_PER_TOKEN),
    )
}

fn lcm_output_is_concise(summary: &str, source_chars: usize, output_budget_chars: usize) -> bool {
    let length = summary.trim().len();
    length > 0 && length <= output_budget_chars && (length < source_chars || length <= 1_024)
}

fn lcm_message_chunks(messages: Vec<Message>, max_chars: usize) -> Vec<Vec<Message>> {
    // A boundary after message `n - 1` is unsafe when a tool call is on its
    // left and the corresponding result is on its right. This preserves exact
    // IDs and ordering for parallel calls without forcing unrelated turns into
    // the same chunk. Oversized individual transactions remain intact and rely
    // on the bounded prompt renderer's per-result truncation.
    let mut tool_calls = HashMap::new();
    let mut resolved_tool_calls = std::collections::HashSet::new();
    let mut unsafe_boundaries = vec![false; messages.len() + 1];
    for (index, message) in messages.iter().enumerate() {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    tool_calls.entry(id.clone()).or_insert(index);
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    if let Some(call_index) = tool_calls.get(tool_use_id).copied() {
                        resolved_tool_calls.insert(tool_use_id.clone());
                        let transaction_end = if messages
                            .get(index + 1)
                            .is_some_and(|next| next.role == Role::Assistant)
                        {
                            index + 1
                        } else {
                            index
                        };
                        for boundary in call_index.saturating_add(1)..=transaction_end {
                            unsafe_boundaries[boundary] = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for (tool_use_id, call_index) in &tool_calls {
        if !resolved_tool_calls.contains(tool_use_id) && call_index + 1 < messages.len() {
            unsafe_boundaries[call_index + 1] = true;
        }
    }

    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut current_chars: usize = 0;
    for (index, message) in messages.into_iter().enumerate() {
        let chars = message_char_count(&message);
        if !current.is_empty()
            && current_chars.saturating_add(chars) > max_chars
            && !unsafe_boundaries[index]
        {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current_chars = current_chars.saturating_add(chars);
        current.push(message);
        if current_chars >= max_chars && !unsafe_boundaries[index + 1] {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

async fn complete_lcm_bounded(
    provider: &Arc<dyn Provider>,
    messages: &[Message],
    existing_summary: Option<&Summary>,
    max_prompt_chars: usize,
    output_budget_chars: usize,
) -> Result<String> {
    let source_chars = messages.iter().map(message_char_count).sum::<usize>()
        + existing_summary
            .map(summary_payload_char_count)
            .unwrap_or(0);
    let canonical_source = lcm_safe_source_text(messages, existing_summary);
    let prompt = build_lcm_compaction_prompt(messages, existing_summary, max_prompt_chars);
    let summary = tokio::time::timeout(
        LCM_PROVIDER_TIMEOUT,
        provider.complete_simple(&prompt, LCM_SUMMARY_SYSTEM_PROMPT),
    )
    .await
    .map_err(|_| anyhow::anyhow!("LCM compactor provider call timed out"))??;
    let summary = crate::message::redact_secrets(&summary);
    if lcm_output_is_concise(&summary, source_chars, output_budget_chars)
        && lcm_output_has_required_sections(&summary)
        && lcm_output_is_grounded(&summary, &canonical_source)
    {
        return Ok(summary.trim().to_string());
    }
    let target_chars = output_budget_chars
        .min(source_chars.saturating_sub(1))
        .max(128);
    let rewrite_prompt = format!(
        "Rewrite the following candidate into at most {target_chars} characters. Use all nine exact required Markdown headings from the system instruction. Every retained content line must be `- [source] ` plus one exact contiguous excerpt from the original observed source; otherwise replace it with `None observed.`. Remove unsupported completion claims and secrets. Return only the rewrite.\n\n{summary}"
    );
    let rewritten = tokio::time::timeout(
        LCM_PROVIDER_TIMEOUT,
        provider.complete_simple(&rewrite_prompt, LCM_SUMMARY_SYSTEM_PROMPT),
    )
    .await
    .map_err(|_| anyhow::anyhow!("LCM compactor rewrite timed out"))??;
    let rewritten = crate::message::redact_secrets(&rewritten);
    if !lcm_output_is_concise(&rewritten, source_chars, output_budget_chars)
        || !lcm_output_has_required_sections(&rewritten)
        || !lcm_output_is_grounded(&rewritten, &canonical_source)
    {
        anyhow::bail!(
            "LCM compactor output remained invalid, unsupported, or oversized after one rewrite"
        );
    }
    Ok(rewritten.trim().to_string())
}

async fn summarize_lcm_source_with_budget(
    provider: &Arc<dyn Provider>,
    messages: Vec<Message>,
    existing_summary: Option<Summary>,
    max_prompt_chars: usize,
    output_budget_chars: usize,
) -> Result<String> {
    let chunk_chars = (max_prompt_chars * 3 / 4).max(1_024);
    let source_chars = messages.iter().map(message_char_count).sum::<usize>()
        + existing_summary
            .as_ref()
            .map(summary_payload_char_count)
            .unwrap_or(0);
    if source_chars <= chunk_chars {
        return complete_lcm_bounded(
            provider,
            &messages,
            existing_summary.as_ref(),
            max_prompt_chars,
            output_budget_chars,
        )
        .await;
    }

    let mut summaries = Vec::new();
    for (index, chunk) in lcm_message_chunks(messages, chunk_chars)
        .into_iter()
        .enumerate()
    {
        summaries.push(
            complete_lcm_bounded(
                provider,
                &chunk,
                (index == 0).then_some(existing_summary.as_ref()).flatten(),
                max_prompt_chars,
                output_budget_chars,
            )
            .await?,
        );
    }
    if summaries.is_empty()
        && let Some(existing) = existing_summary
    {
        summaries.push(existing.text);
    }

    while summaries.len() > 1 {
        let summary_messages = summaries
            .into_iter()
            .enumerate()
            .map(|(index, summary)| Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("[chronological LCM chunk {}]\n{summary}", index + 1),
                    cache_control: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            })
            .collect::<Vec<_>>();
        let groups = lcm_message_chunks(summary_messages, chunk_chars);
        let mut reduced = Vec::with_capacity(groups.len());
        for group in groups {
            reduced.push(
                complete_lcm_bounded(
                    provider,
                    &group,
                    None,
                    max_prompt_chars,
                    output_budget_chars,
                )
                .await?,
            );
        }
        summaries = reduced;
    }
    summaries
        .pop()
        .ok_or_else(|| anyhow::anyhow!("LCM source did not contain summarizable context"))
}

/// Generate summary using the provider
async fn generate_compaction_artifact(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    mut existing_summary: Option<Summary>,
) -> Result<CompactionResult> {
    let start = Instant::now();
    if let Some(summary) = existing_summary.as_mut()
        && let Some(encrypted_content) = summary.openai_encrypted_content.as_ref()
        && !openai_encrypted_content_is_sendable(encrypted_content)
    {
        let encrypted_content_len = encrypted_content.len();
        crate::logging::warn(&format!(
            "[compaction] Existing OpenAI native compaction payload is oversized ({} chars); falling back to text summary",
            encrypted_content_len,
        ));
        summary.openai_encrypted_content = None;
        let fallback = openai_encrypted_content_fallback_summary(encrypted_content_len);
        if summary.text.trim().is_empty() {
            summary.text = fallback;
        } else if !summary
            .text
            .contains("OpenAI native compaction state was discarded")
        {
            summary.text.push_str("\n\n");
            summary.text.push_str(&fallback);
        }
    }

    if let Ok(native) = provider
        .native_compact(
            &messages,
            existing_summary
                .as_ref()
                .map(|summary| summary.text.as_str()),
            existing_summary
                .as_ref()
                .and_then(|summary| summary.openai_encrypted_content.as_deref()),
        )
        .await
    {
        if let Some(encrypted_content) = native.openai_encrypted_content.as_ref()
            && !openai_encrypted_content_is_sendable(encrypted_content)
        {
            crate::logging::warn(&format!(
                "[compaction] OpenAI native compaction returned oversized encrypted_content ({} chars); falling back to text summary",
                encrypted_content.len(),
            ));
        } else {
            return Ok(CompactionResult {
                summary_text: native.summary_text.unwrap_or_default(),
                atomic_parent_summaries: Vec::new(),
                openai_encrypted_content: native.openai_encrypted_content,
                covers_up_to_turn: messages.len(),
                duration_ms: start.elapsed().as_millis() as u64,
                summarized_messages: messages.len(),
            });
        }
    }

    let max_prompt_chars = provider.context_window().saturating_sub(4000) * CHARS_PER_TOKEN;
    let safe_messages = compactor_safe_messages(&messages);
    let prompt = crate::message::redact_secrets(&build_compaction_prompt(
        &safe_messages,
        existing_summary.as_ref(),
        max_prompt_chars,
    ));

    // Generate summary using simple completion
    let summary = provider
        .complete_simple(
            &prompt,
            "You are a helpful assistant that summarizes conversations.",
        )
        .await?;
    let summary = crate::message::redact_secrets(&summary);

    Ok(CompactionResult {
        summary_text: summary,
        atomic_parent_summaries: Vec::new(),
        openai_encrypted_content: None,
        covers_up_to_turn: messages.len(),
        duration_ms: start.elapsed().as_millis() as u64,
        summarized_messages: messages.len(),
    })
}

/// Generate a portable LCM leaf. Provider-native encrypted compaction is
/// intentionally not consulted because graph nodes must remain replayable on
/// every provider route.
async fn generate_lcm_compaction_artifact(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    existing_summary: Option<Summary>,
    route_identity: Option<String>,
    priority: LcmJobPriority,
) -> Result<CompactionResult> {
    let start = Instant::now();
    let queued_at = Instant::now();
    let permit = acquire_lcm_scheduler(priority).await?;
    let queue_ms = queued_at.elapsed().as_millis() as u64;
    crate::logging::event_info(
        "LCM_SCHEDULER",
        vec![
            ("provider", provider.name().to_string()),
            ("model", provider.model()),
            ("priority", priority.as_str().to_string()),
            ("queue_ms", queue_ms.to_string()),
        ],
    );
    let (route_key, mut max_prompt_chars, output_budget_chars) =
        lcm_prompt_budget(provider.as_ref(), route_identity.as_deref());
    let mut attempts = 0;
    let summary = loop {
        match summarize_lcm_source_with_budget(
            &provider,
            messages.clone(),
            existing_summary.clone(),
            max_prompt_chars,
            output_budget_chars,
        )
        .await
        {
            Ok(summary) => break summary,
            Err(error) if attempts < 2 && is_lcm_context_limit_error(&error) => {
                attempts += 1;
                max_prompt_chars = (max_prompt_chars / 2).max(1_024);
                LCM_SAFE_PROMPT_CHARS
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(route_key.clone(), max_prompt_chars);
                crate::logging::warn(&format!(
                    "LCM compactor route {route_key} exceeded its declared context; retrying with {max_prompt_chars} prompt chars"
                ));
            }
            Err(error) => return Err(error),
        }
    };
    drop(permit);
    Ok(CompactionResult {
        summary_text: summary.trim().to_string(),
        atomic_parent_summaries: Vec::new(),
        openai_encrypted_content: None,
        covers_up_to_turn: messages.len(),
        duration_ms: start.elapsed().as_millis() as u64,
        summarized_messages: messages.len(),
    })
}

pub async fn build_transfer_compaction_state(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    existing_state: Option<crate::session::StoredCompactionState>,
    engine: crate::config::CompactionEngine,
) -> Result<Option<crate::session::StoredCompactionState>> {
    let existing_summary = existing_state.as_ref().map(|state| Summary {
        text: state.summary_text.clone(),
        openai_encrypted_content: state.openai_encrypted_content.clone(),
        covers_up_to_turn: state.covers_up_to_turn,
        original_turn_count: state.original_turn_count,
    });

    if messages.is_empty() {
        return Ok(existing_state.map(|mut state| {
            state.compacted_count = 0;
            state
        }));
    }

    let prior_turns = existing_state
        .as_ref()
        .map(|state| state.original_turn_count.max(state.covers_up_to_turn))
        .unwrap_or(0);
    let result = if engine == crate::config::CompactionEngine::Lcm {
        generate_lcm_compaction_artifact(
            provider,
            messages.clone(),
            existing_summary,
            None,
            LcmJobPriority::Critical,
        )
        .await?
    } else {
        generate_compaction_artifact(provider, messages.clone(), existing_summary).await?
    };
    let total_turns = prior_turns + messages.len();

    Ok(Some(crate::session::StoredCompactionState {
        summary_text: result.summary_text,
        openai_encrypted_content: result.openai_encrypted_content,
        covers_up_to_turn: total_turns,
        original_turn_count: total_turns,
        compacted_count: 0,
    }))
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
