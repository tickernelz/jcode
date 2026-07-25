use super::*;

impl CompactionManager {
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
        self.engine
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

    pub(super) fn mode_trigger_label(&self) -> &'static str {
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

    pub(super) fn cached_semantic_embedding(&mut self, text: &str) -> Option<Vec<f32>> {
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

    pub(super) fn insert_semantic_embedding_cache(
        &mut self,
        key: u64,
        embedding: Option<Vec<f32>>,
    ) {
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
