use super::*;

impl CompactionManager {
    pub(super) fn poll_lcm_candidate(&mut self, session: &crate::session::Session) {
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
            emit_lcm_attempt(
                &source.attempt_context("leaf"),
                LcmAttemptOutcome::Superseded,
                LcmAttemptReason::RoutePolicyChanged,
                None,
                None,
                None,
                0,
            );
            crate::logging::warn(
                "Discarding completed LCM job because compaction engine/model/route policy changed",
            );
            return;
        }
        if !lcm_source_matches_canonical_session(&source, session) {
            emit_lcm_attempt(
                &source.attempt_context("leaf"),
                LcmAttemptOutcome::Superseded,
                LcmAttemptReason::CanonicalSourceChanged,
                None,
                None,
                None,
                0,
            );
            crate::logging::warn(
                "Discarding completed LCM job because canonical source history changed",
            );
            return;
        }

        let attempt = source.attempt_context(if source.trigger == "hierarchy" {
            "hierarchy"
        } else {
            "leaf"
        });
        match futures::executor::block_on(task) {
            Ok(Ok(result)) => match Self::prepare_lcm_context(source, result) {
                Ok(prepared) => self.prepared_lcm_context = Some(prepared),
                Err(error) => {
                    emit_lcm_attempt(
                        &attempt,
                        LcmAttemptOutcome::Failed,
                        LcmAttemptReason::CandidateValidationFailed,
                        None,
                        None,
                        None,
                        0,
                    );
                    crate::logging::error(&format!(
                        "Failed to prepare completed LCM transaction: {error}"
                    ));
                }
            },
            Ok(Err(error)) => {
                crate::logging::error(&format!("LCM summary generation failed: {error}"));
            }
            Err(error) => {
                emit_lcm_attempt(
                    &attempt,
                    LcmAttemptOutcome::Failed,
                    LcmAttemptReason::TaskJoinFailed,
                    None,
                    None,
                    None,
                    0,
                );
                crate::logging::error(&format!("LCM summary task panicked: {error}"));
            }
        }
    }

    pub(super) fn commit_prepared_lcm(
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
            .unwrap_or(prepared.effective_route.as_str());
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
            emit_lcm_attempt(
                &prepared.attempt,
                LcmAttemptOutcome::Superseded,
                LcmAttemptReason::RoutePolicyChangedBeforeCommit,
                None,
                Some(prepared.duration_ms),
                None,
                0,
            );
            anyhow::bail!("prepared LCM candidate route policy became stale before commit");
        }
        let persistence_start = Instant::now();
        let commit = session.commit_context_graph_transaction_with_compaction(
            prepared.transaction.clone(),
            Some(prepared.projection.clone()),
        );
        if let Err(error) = commit {
            emit_lcm_attempt(
                &prepared.attempt,
                LcmAttemptOutcome::PersistenceRetry,
                LcmAttemptReason::DurableCommitFailed,
                None,
                Some(prepared.duration_ms),
                Some(persistence_start.elapsed().as_millis() as u64),
                0,
            );
            self.prepared_lcm_context = Some(prepared);
            return Err(error);
        }
        let persistence_ms = persistence_start.elapsed().as_millis() as u64;
        let attempt = prepared.attempt.clone();

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
        emit_lcm_attempt(
            &attempt,
            LcmAttemptOutcome::Published,
            LcmAttemptReason::DurableCommitSucceeded,
            None,
            event.duration_ms,
            Some(persistence_ms),
            0,
        );
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
                    .map(|node| lcm_redact_uncertain_secrets(&node.summary_text))
            })
            .collect::<Vec<_>>();
        let messages = if !frontier_summaries.is_empty() {
            let mut messages = Vec::with_capacity(active.len() + frontier_summaries.len());
            for summary in frontier_summaries {
                messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: compacted_summary_text_block(&summary),
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
            let summary_text = lcm_redact_uncertain_secrets(&summary.text);
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: compacted_summary_text_block(&summary_text),
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
            let target_tokens =
                (self.token_budget as f64 * f64::from(COMPACTION_THRESHOLD)) as usize;
            let cutoff = critical_lcm_cutoff(active, target_tokens);
            if cutoff > 0 {
                if self.pending_task.is_none()
                    && self.prepared_lcm_context.is_none()
                    && let Err(error) = self.start_lcm_job_with_model(
                        session,
                        Arc::clone(&provider),
                        &all_messages,
                        cutoff,
                        "critical_selected_route".to_string(),
                        critical_policy_model.clone(),
                        critical_policy_model.clone(),
                    )
                {
                    crate::logging::warn(&format!(
                        "Failed to start selected LCM critical route: {error}"
                    ));
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

    pub(super) fn wait_for_critical_lcm_attempt(
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

    pub(super) fn install_critical_legacy_summary(
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
        let summary_text = lcm_redact_uncertain_secrets(&legacy.summary_text);
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

    pub(super) fn hard_lcm_compact_with_trigger(
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
        let target_tokens = (self.token_budget as f64 * f64::from(COMPACTION_THRESHOLD)) as usize;
        let cutoff = critical_lcm_cutoff(active, target_tokens);
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
        let existing_summary = if session.context_frontier.is_none() {
            self.active_summary
                .as_ref()
                .map(|summary| lcm_redact_uncertain_secrets(&summary.text))
        } else {
            None
        };
        let safe_messages = lcm_safe_messages(&active[..cutoff]);
        let summary_text = lcm_redact_uncertain_secrets(&build_emergency_summary_text(
            existing_summary.as_deref(),
            cutoff,
            pre_tokens,
            self.token_budget,
            &safe_messages,
        ));
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
                    "[compaction] Context at {:.1}% with background compaction in flight — polling once before hard compact",
                    usage * 100.0,
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

    pub(super) fn wait_for_pending_compaction_at_hard_threshold(
        &mut self,
        all_messages: &[Message],
    ) -> HardThresholdWait {
        let start = Instant::now();
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
            // "timed_out" is retained in the existing diagnostic shape. At
            // hard threshold it now means the in-flight result was not ready at
            // the single nonblocking poll, so hard fallback must proceed.
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
        if !provider
            .exact_runtime_identity()
            .as_ref()
            .is_some_and(|identity| identity.has_verifiable_account_binding())
        {
            return Err(
                "Compaction is disabled because effective provider account identity is opaque or incomplete"
                    .to_string(),
            );
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
}
