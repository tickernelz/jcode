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
use futures::{StreamExt, TryStreamExt};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use tokio::task::JoinHandle;

#[cfg(test)]
use lcm_helpers::{
    LCM_OMITTED_SOURCE, LCM_REQUIRED_SECTIONS, LCM_SAFE_PROMPT_CHARS, LCM_SUMMARY_SYSTEM_PROMPT,
};

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;

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

fn lcm_source_matches_canonical_session(
    source: &PendingLcmSource,
    session: &crate::session::Session,
) -> bool {
    let messages = session.active_stored_messages();
    let source_end = source.covered_message_count;
    let Some(source_start) = source_end.checked_sub(source.source_message_ids.len()) else {
        return false;
    };
    if source.source_message_ids.is_empty() || source_end > messages.len() {
        return false;
    }
    let current_source = &messages[source_start..source_end];
    current_source
        .iter()
        .map(|message| message.id.as_str())
        .eq(source.source_message_ids.iter().map(String::as_str))
        && stored_message_prefix_sha256(current_source)
            .is_ok_and(|sha256| sha256 == source.source_sha256)
        && stored_message_prefix_sha256(&messages[..source_end])
            .is_ok_and(|sha256| sha256 == source.source_prefix_sha256)
}

fn critical_lcm_cutoff(active: &[Message], target_tokens: usize) -> usize {
    if active.len() <= MIN_TURNS_TO_KEEP {
        return 0;
    }
    let active_char_counts: Vec<usize> = active.iter().map(message_char_count).collect();
    let mut remaining_suffix_chars = vec![0usize; active_char_counts.len() + 1];
    for index in (0..active_char_counts.len()).rev() {
        remaining_suffix_chars[index] =
            remaining_suffix_chars[index + 1].saturating_add(active_char_counts[index]);
    }
    let mut turns_to_keep = RECENT_TURNS_TO_KEEP.min(active.len().saturating_sub(1));
    loop {
        let candidate = safe_compaction_cutoff(active, active.len().saturating_sub(turns_to_keep));
        if candidate > 0 && remaining_suffix_chars[candidate] / CHARS_PER_TOKEN <= target_tokens {
            return candidate;
        }
        if turns_to_keep <= MIN_TURNS_TO_KEEP {
            return safe_compaction_cutoff(active, active.len().saturating_sub(MIN_TURNS_TO_KEEP));
        }
        turns_to_keep = (turns_to_keep / 2).max(MIN_TURNS_TO_KEEP);
    }
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
        session.provider_key.as_deref().unwrap_or(""),
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
    let bytes = match serde_json::to_vec(&serde_json::json!({
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
    })) {
        Ok(bytes) => bytes,
        Err(error) => {
            crate::logging::warn(&format!(
                "Failed to serialize LCM route policy fingerprint: {error}"
            ));
            b"lcm-route-policy-serialization-error".to_vec()
        }
    };
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
const LCM_CHUNK_CONCURRENCY: usize = 2;
const LCM_INLINE_SOURCE_CHARS: usize = 1_024;
const LCM_CRITICAL_PROMPT_CHARS: usize = 32_000;

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
    source_runtime_identity_sha256: String,
    source_prefix_sha256: String,
    covered_through_message_id: String,
    prior_active_nodes: Vec<crate::session::StoredContextNode>,
    node_level: u32,
    child_nodes: Vec<crate::session::StoredContextNode>,
    summarizer_model: String,
    summarizer_provider: String,
    summarizer_route: String,
    summarizer_runtime_identity_sha256: String,
    configured_model: Option<String>,
    cutoff: usize,
    source_fingerprint: Option<u64>,
    pre_tokens: u64,
    trigger: String,
    route_policy_fingerprint: String,
    attempt_terminal_emitted: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
struct LcmAttemptContext {
    attempt_id: String,
    session_id: String,
    trigger: String,
    stage: String,
    configured_route: Option<String>,
    effective_route: String,
    source_messages: usize,
    pre_tokens: u64,
    terminal_emitted: Arc<std::sync::atomic::AtomicBool>,
}

impl LcmAttemptContext {
    fn child(&self, stage: impl Into<String>) -> Self {
        let stage = stage.into();
        let mut child = self.clone();
        child.stage = stage;
        child.source_messages = 4;
        child
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LcmAttemptOutcome {
    Queued,
    Running,
    Retrying,
    Generated,
    PersistenceRetry,
    Published,
    Failed,
    Cancelled,
    Superseded,
}

impl LcmAttemptOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Retrying => "retrying",
            Self::Generated => "generated",
            Self::PersistenceRetry => "persistence_retry",
            Self::Published => "published",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
        }
    }

    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Published | Self::Failed | Self::Cancelled | Self::Superseded
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LcmAttemptReason {
    ManagerResetOrDrop,
    PreparedCandidateDiscarded,
    RoutePolicyChanged,
    CanonicalSourceChanged,
    CandidateValidationFailed,
    TaskJoinFailed,
    RoutePolicyChangedBeforeCommit,
    DurableCommitFailed,
    DurableCommitSucceeded,
    SchedulerWait,
    SchedulerQueueTimeout,
    SchedulerClosed,
    SchedulerAdmitted,
    ProviderTimeout,
    ContextLimit,
    ContextLimitAdaptiveRetry,
    SecretOutputRejected,
    OutputValidationFailed,
    ProviderError,
    CandidateReady,
}

impl LcmAttemptReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ManagerResetOrDrop => "manager_reset_or_drop",
            Self::PreparedCandidateDiscarded => "prepared_candidate_discarded",
            Self::RoutePolicyChanged => "route_policy_changed",
            Self::CanonicalSourceChanged => "canonical_source_changed",
            Self::CandidateValidationFailed => "candidate_validation_failed",
            Self::TaskJoinFailed => "task_join_failed",
            Self::RoutePolicyChangedBeforeCommit => "route_policy_changed_before_commit",
            Self::DurableCommitFailed => "durable_commit_failed",
            Self::DurableCommitSucceeded => "durable_commit_succeeded",
            Self::SchedulerWait => "scheduler_wait",
            Self::SchedulerQueueTimeout => "scheduler_queue_timeout",
            Self::SchedulerClosed => "scheduler_closed",
            Self::SchedulerAdmitted => "scheduler_admitted",
            Self::ProviderTimeout => "provider_timeout",
            Self::ContextLimit => "context_limit",
            Self::ContextLimitAdaptiveRetry => "context_limit_adaptive_retry",
            Self::SecretOutputRejected => "secret_output_rejected",
            Self::OutputValidationFailed => "output_validation_failed",
            Self::ProviderError => "provider_error",
            Self::CandidateReady => "candidate_ready",
        }
    }
}

fn emit_lcm_attempt(
    attempt: &LcmAttemptContext,
    outcome: LcmAttemptOutcome,
    reason_code: LcmAttemptReason,
    queue_ms: Option<u64>,
    execution_ms: Option<u64>,
    persistence_ms: Option<u64>,
    adaptive_retries: usize,
) {
    use std::sync::atomic::Ordering;
    if outcome.is_terminal() {
        if attempt.terminal_emitted.swap(true, Ordering::AcqRel) {
            crate::logging::error(&format!(
                "Suppressing duplicate terminal LCM attempt event {} ({})",
                attempt.attempt_id,
                outcome.as_str()
            ));
            return;
        }
    } else if attempt.terminal_emitted.load(Ordering::Acquire) {
        crate::logging::error(&format!(
            "Suppressing post-terminal LCM attempt event {} ({})",
            attempt.attempt_id,
            outcome.as_str()
        ));
        return;
    }
    let fields = vec![
        ("attempt_id".to_string(), attempt.attempt_id.clone()),
        ("session_id".to_string(), attempt.session_id.clone()),
        ("trigger".to_string(), attempt.trigger.clone()),
        ("stage".to_string(), attempt.stage.clone()),
        (
            "configured_route".to_string(),
            crate::message::redact_uncertain_secrets(
                &attempt
                    .configured_route
                    .clone()
                    .unwrap_or_else(|| "active-session-route".to_string()),
            ),
        ),
        (
            "effective_route".to_string(),
            crate::message::redact_uncertain_secrets(&attempt.effective_route),
        ),
        ("outcome".to_string(), outcome.as_str().to_string()),
        ("reason_code".to_string(), reason_code.as_str().to_string()),
        (
            "source_messages".to_string(),
            attempt.source_messages.to_string(),
        ),
        ("pre_tokens".to_string(), attempt.pre_tokens.to_string()),
        (
            "queue_ms".to_string(),
            queue_ms.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
        ),
        (
            "execution_ms".to_string(),
            execution_ms.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
        ),
        (
            "persistence_ms".to_string(),
            persistence_ms.map_or_else(|| "unknown".to_string(), |value| value.to_string()),
        ),
        ("adaptive_retries".to_string(), adaptive_retries.to_string()),
    ];
    #[cfg(test)]
    lcm_attempt_test_events()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(fields.clone());
    crate::logging::event_info("LCM_ATTEMPT", fields);
}

#[cfg(test)]
type LcmAttemptFields = Vec<(String, String)>;
#[cfg(test)]
type LcmAttemptEvents = std::sync::Mutex<Vec<LcmAttemptFields>>;

#[cfg(test)]
fn lcm_attempt_test_events() -> &'static LcmAttemptEvents {
    static EVENTS: std::sync::OnceLock<LcmAttemptEvents> = std::sync::OnceLock::new();
    EVENTS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

#[cfg(test)]
fn take_lcm_attempt_test_events(attempt_id: &str) -> Vec<Vec<(String, String)>> {
    let mut events = lcm_attempt_test_events()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut matching = Vec::new();
    events.retain(|fields| {
        let matches = fields
            .iter()
            .any(|(key, value)| key == "attempt_id" && value == attempt_id);
        if matches {
            matching.push(fields.clone());
        }
        !matches
    });
    matching
}

fn lcm_attempt_failure_reason(error: &anyhow::Error) -> LcmAttemptReason {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("scheduler queue timed out") {
        LcmAttemptReason::SchedulerQueueTimeout
    } else if message.contains("scheduler is closed") {
        LcmAttemptReason::SchedulerClosed
    } else if message.contains("provider call timed out") || message.contains("rewrite timed out") {
        LcmAttemptReason::ProviderTimeout
    } else if is_lcm_context_limit_error(error) {
        LcmAttemptReason::ContextLimit
    } else if message.contains("secret-shaped") {
        LcmAttemptReason::SecretOutputRejected
    } else if message.contains("ground")
        || message.contains("section")
        || message.contains("concise")
        || message.contains("reduced safely")
        || message.contains("summarizable")
    {
        LcmAttemptReason::OutputValidationFailed
    } else {
        LcmAttemptReason::ProviderError
    }
}

type LcmProviderResolution = (Arc<dyn Provider>, String, String, String, Option<String>);

impl PendingLcmSource {
    fn attempt_context(&self, stage: impl Into<String>) -> LcmAttemptContext {
        let stage = stage.into();
        LcmAttemptContext {
            attempt_id: format!(
                "{}:{}:{}:{}",
                self.session_id, self.generation, self.next_node_sequence, stage
            ),
            session_id: self.session_id.clone(),
            trigger: self.trigger.clone(),
            stage,
            configured_route: self.configured_model.clone(),
            effective_route: self.summarizer_route.clone(),
            source_messages: self.source_message_ids.len(),
            pre_tokens: self.pre_tokens,
            terminal_emitted: Arc::clone(&self.attempt_terminal_emitted),
        }
    }

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
    attempt: LcmAttemptContext,
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

impl Drop for CompactionManager {
    fn drop(&mut self) {
        self.cancel_pending_work();
    }
}

mod lcm_helpers;
mod manager_apply;
mod manager_lcm_commit;
mod manager_lcm_start;
mod manager_state;

use lcm_helpers::*;

pub use lcm_helpers::build_transfer_compaction_state;
