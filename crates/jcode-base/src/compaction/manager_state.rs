use super::*;

impl CompactionManager {
    pub fn new() -> Self {
        let cfg = crate::config::config().compaction.clone();
        let mode = cfg.mode.clone();
        Self {
            engine: cfg.engine,
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

    pub(super) fn cancel_pending_work(&mut self) {
        if let Some(source) = self.pending_lcm_source.as_ref() {
            emit_lcm_attempt(
                &source.attempt_context("leaf"),
                LcmAttemptOutcome::Cancelled,
                LcmAttemptReason::ManagerResetOrDrop,
                None,
                None,
                None,
                0,
            );
        }
        if let Some(prepared) = self.prepared_lcm_context.as_ref() {
            emit_lcm_attempt(
                &prepared.attempt,
                LcmAttemptOutcome::Cancelled,
                LcmAttemptReason::PreparedCandidateDiscarded,
                None,
                Some(prepared.duration_ms),
                None,
                0,
            );
        }
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

    /// Restore a projection already proven to be owned by the durable native
    /// LCM graph. The ordinary restore path deliberately refuses arbitrary
    /// rolling/provider-native state when LCM is active.
    pub fn restore_native_lcm_stored_state_with(
        &mut self,
        state: &crate::session::StoredCompactionState,
        all_messages: &[crate::session::StoredMessage],
    ) {
        self.restore_persisted_stored_state_with(state, all_messages);
        if self.engine != crate::config::CompactionEngine::Lcm {
            return;
        }
        self.compacted_count = state.compacted_count.min(all_messages.len());
        self.active_summary = Some(Summary {
            text: lcm_redact_uncertain_secrets(&state.summary_text),
            openai_encrypted_content: None,
            covers_up_to_turn: state.covers_up_to_turn,
            original_turn_count: state.original_turn_count,
        });
        self.active_chars.set_exact(
            all_messages[self.compacted_count..]
                .iter()
                .map(|message| content_char_count(&message.content))
                .sum(),
        );
        self.suppress_compaction_until_new_message = !all_messages.is_empty();
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
    pub(super) fn anti_signals_block(&self, all_messages: &[Message]) -> bool {
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
    pub(super) fn should_compact_proactively(&self, all_messages: &[Message]) -> bool {
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
    pub(super) fn should_compact_semantic(&self, all_messages: &[Message]) -> bool {
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
    pub(super) fn semantic_cutoff(&mut self, active: &[Message]) -> usize {
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
    pub(super) fn active_messages<'a>(&self, all_messages: &'a [Message]) -> &'a [Message] {
        // If session restore/replay leaves the manager with bookkeeping from a
        // longer message vector, never fall back to the full transcript. That
        // makes already-compacted messages active again and can drive repeated
        // emergency compaction loops. Clamp to the end instead: all available
        // messages are covered by the summary until new turns arrive.
        let start = self.compacted_count.min(all_messages.len());
        &all_messages[start..]
    }

    pub(super) fn clamp_compacted_count_to_messages(
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

    pub(super) fn log_compaction_state(
        &self,
        phase: &str,
        trigger: &str,
        all_messages: &[Message],
    ) {
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

    pub(super) fn log_compaction_outcome(&self, outcome: CompactionOutcomeLog<'_>) {
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

    pub(super) fn active_message_chars_with(&self, all_messages: &[Message]) -> usize {
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
}
