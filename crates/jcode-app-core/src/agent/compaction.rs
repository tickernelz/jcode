use super::*;

impl Agent {
    pub(super) fn note_compaction_applied(&mut self) {
        self.cache_tracker.reset();
        self.locked_tools = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.session.provider_session_identity = None;
    }

    pub fn poll_compaction_completion_event(&mut self) -> Option<CompactionEvent> {
        let compaction = self.registry.compaction();
        let (event, lcm) = match compaction.try_write() {
            Ok(mut manager) => {
                if manager.synchronize_engine(crate::config::config().compaction.engine) {
                    self.session.deactivate_context_graph_state();
                    self.note_compaction_applied();
                    self.persist_session_best_effort("compaction engine switch");
                }
                if manager.engine() == crate::config::CompactionEngine::Lcm {
                    match manager.materialize_lcm_context(&mut self.session) {
                        Ok((_, event)) => (event, true),
                        Err(error) => {
                            logging::error(&format!("LCM durable commit failed: {error}"));
                            (None, true)
                        }
                    }
                } else {
                    let provider_messages = self.session.messages_for_provider();
                    let event = manager.poll_compaction_event_with(&provider_messages);
                    if event.is_some() {
                        self.sync_session_compaction_state_from_manager(&manager);
                    }
                    (event, false)
                }
            }
            Err(_) => return None,
        };

        if event.is_some() {
            self.note_compaction_applied();
            if !lcm {
                self.persist_session_best_effort("compaction completion");
            }
        }

        event
    }

    pub fn request_manual_compaction(&mut self) -> (String, bool) {
        let lcm_configured =
            crate::config::config().compaction.engine == crate::config::CompactionEngine::Lcm;
        if !lcm_configured && !self.provider.supports_compaction() {
            return (
                "Manual compaction is not available for this provider.".to_string(),
                false,
            );
        }

        let provider = self.provider.clone();
        let messages = self.session.messages_for_provider();
        let compaction = self.registry.compaction();

        match compaction.try_write() {
            Ok(mut manager) => {
                if manager.synchronize_engine(crate::config::config().compaction.engine) {
                    self.session.deactivate_context_graph_state();
                    self.note_compaction_applied();
                    self.persist_session_best_effort("compaction engine switch");
                }
                let stats = manager.stats_with(&messages);
                let status_msg = format!(
                    "**Context Status:**\n\
                    • Messages: {} (active), {} (total history)\n\
                    • Token usage: ~{}k (estimate ~{}k) / {}k ({:.1}%)\n\
                    • Has summary: {}\n\
                    • Compacting: {}",
                    stats.active_messages,
                    stats.total_turns,
                    stats.effective_tokens / 1000,
                    stats.token_estimate / 1000,
                    manager.token_budget() / 1000,
                    stats.context_usage * 100.0,
                    if stats.has_summary { "yes" } else { "no" },
                    if stats.is_compacting {
                        "in progress..."
                    } else {
                        "no"
                    }
                );

                let start = if manager.engine() == crate::config::CompactionEngine::Lcm {
                    manager.force_lcm_compact_with(&self.session, provider)
                } else {
                    manager.force_compact_with(&messages, provider)
                };
                match start {
                    Ok(()) => (
                        format!(
                            "{}\n\n📦 **Compacting context** (manual) — summarizing older messages in the background to stay within the context window.\n\
                            The summary will be applied automatically when ready.",
                            status_msg
                        ),
                        true,
                    ),
                    Err(reason) => (
                        format!("{status_msg}\n\n⚠ **Cannot compact:** {reason}"),
                        false,
                    ),
                }
            }
            Err(_) => (
                "⚠ Cannot access compaction manager (lock held)".to_string(),
                false,
            ),
        }
    }

    fn is_context_limit_error(error: &str) -> bool {
        let lower = error.to_lowercase();
        lower.contains("context length")
            || lower.contains("context window")
            || lower.contains("maximum context")
            || lower.contains("max context")
            || lower.contains("token limit")
            || lower.contains("too many tokens")
            || lower.contains("prompt is too long")
            || lower.contains("input is too long")
            || lower.contains("request too large")
            || lower.contains("length limit")
            || lower.contains("maximum tokens")
            || (lower.contains("exceeded") && lower.contains("tokens"))
    }

    /// Best-effort emergency recovery after a context-limit error.
    ///
    /// Performs a synchronous hard compaction and resets provider session state,
    /// allowing the caller to retry the same turn immediately.
    pub(super) fn try_auto_compact_after_context_limit(
        &mut self,
        error: &str,
    ) -> Option<CompactionEvent> {
        if crate::provider::openai_request::is_openai_encrypted_content_too_large_error(error)
            && self.try_recover_oversized_openai_native_compaction()
        {
            return Some(CompactionEvent {
                trigger: "auto_recovery_native".to_string(),
                engine: Some("provider-native".to_string()),
                ownership: Some("provider".to_string()),
                ..CompactionEvent::default()
            });
        }
        // A provider HTTP 413 ("request too large") is a *byte-size* failure
        // driven by inline base64 images, not a token-context overflow. Token
        // accounting deliberately undercounts images, so ordinary compaction
        // would not shrink the payload and the retry would 413 again. Strip
        // oversized images first.
        if self.try_recover_after_payload_too_large(error) {
            return Some(CompactionEvent {
                trigger: "auto_recovery_payload".to_string(),
                engine: Some("payload-truncation".to_string()),
                ownership: Some("jcode".to_string()),
                ..CompactionEvent::default()
            });
        }
        if !Self::is_context_limit_error(error) {
            return None;
        }
        let lcm_configured =
            crate::config::config().compaction.engine == crate::config::CompactionEngine::Lcm;
        if !lcm_configured && !self.provider.supports_compaction() {
            return None;
        }

        let context_limit = self.provider.context_window() as u64;
        let compaction = self.registry.compaction();

        let (dropped, usage_pct, compaction_event) = match compaction.try_write() {
            Ok(mut manager) => {
                if manager.synchronize_engine(crate::config::config().compaction.engine) {
                    self.session.deactivate_context_graph_state();
                    self.note_compaction_applied();
                    self.persist_session_best_effort("compaction engine switch");
                }
                manager.update_observed_input_tokens(context_limit);
                let all_messages = self.session.messages_for_provider_uncached();
                let usage_pct = manager.context_usage_with(&all_messages) * 100.0;
                let dropped = if manager.engine() == crate::config::CompactionEngine::Lcm {
                    match manager
                        .ensure_lcm_context_fits(&mut self.session, Arc::clone(&self.provider))
                    {
                        crate::compaction::CompactionAction::HardCompacted(dropped) => dropped,
                        _ => {
                            logging::warn(
                                "Context-limit auto-recovery failed: LCM fallback chain did not compact",
                            );
                            return None;
                        }
                    }
                } else {
                    let dropped = match manager.hard_compact_with(&all_messages) {
                        Ok(dropped) => dropped,
                        Err(reason) => {
                            logging::warn(&format!(
                                "Context-limit auto-recovery failed: hard compact failed ({})",
                                reason
                            ));
                            return None;
                        }
                    };
                    self.sync_session_compaction_state_from_manager(&manager);
                    dropped
                };
                let event = manager.take_compaction_event();
                (dropped, usage_pct, event)
            }
            Err(_) => {
                logging::warn("Context-limit auto-recovery skipped: compaction manager lock busy");
                return None;
            }
        };

        self.cache_tracker.reset();
        self.locked_tools = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.session.provider_session_identity = None;

        logging::warn(&format!(
            "Context limit exceeded; auto-compacted and retrying (dropped {} messages, usage was {:.1}%)",
            dropped, usage_pct
        ));
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "auto_compaction_applied",
                "context_limit_auto_compaction",
            )
            .with_session_id(self.session.id.clone())
            .with_detail(format!(
                "dropped_messages={dropped},usage_pct={usage_pct:.1}"
            ))
            .force_attribution(),
        );

        Some(compaction_event.unwrap_or_else(|| {
            CompactionEvent {
                trigger: "auto_recovery".to_string(),
                engine: Some(
                    crate::config::config()
                        .compaction
                        .engine
                        .as_str()
                        .to_string(),
                ),
                ownership: Some("jcode".to_string()),
                messages_dropped: Some(dropped),
                ..CompactionEvent::default()
            }
        }))
    }

    /// Best-effort recovery after a provider HTTP 413 "request too large" error.
    ///
    /// This failure is caused by the serialized request body (dominated by inline
    /// base64 images) exceeding the provider's size cap, which is independent of
    /// the token context window. We persist an oldest-first provider projection
    /// budget without changing canonical history, then reset provider state so
    /// the caller can retry the same turn immediately.
    fn try_recover_after_payload_too_large(&mut self, error: &str) -> bool {
        if !crate::compaction::is_request_payload_too_large_error(error) {
            return false;
        }

        let stripped = self
            .session
            .suppress_oversized_images_for_provider(crate::compaction::PAYLOAD_IMAGE_CHAR_BUDGET);
        if stripped == 0 {
            logging::warn(
                "Request-too-large recovery skipped: no oversized inline images to strip",
            );
            return false;
        }

        // The provider projection changed; reseed compaction bookkeeping and reset
        // provider session/cache state so the retry sends the reduced payload.
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            let provider_messages = self.session.messages_for_provider();
            manager.reset();
            manager.set_budget(self.provider.context_window());
            if let Some(state) = self.session.compaction.as_ref() {
                manager.restore_persisted_state_with(state, &provider_messages);
            } else {
                manager.seed_restored_messages_with(&provider_messages);
            }
            self.sync_session_compaction_state_from_manager(&manager);
        }

        self.cache_tracker.reset();
        self.locked_tools = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.session.provider_session_identity = None;
        if let Err(error) = self.session.save() {
            logging::error(&format!(
                "Request-too-large recovery could not persist its provider projection: {error:#}"
            ));
            return false;
        }

        logging::warn(&format!(
            "Request body exceeded provider size limit; suppressed {} oversized inline image(s) in provider projection and retrying",
            stripped
        ));
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "payload_too_large_recovered",
                "request_payload_too_large",
            )
            .with_session_id(self.session.id.clone())
            .with_detail(format!("images_suppressed={stripped}"))
            .force_attribution(),
        );

        true
    }

    fn try_recover_oversized_openai_native_compaction(&mut self) -> bool {
        let compaction = self.registry.compaction();
        let recovered = match compaction.try_write() {
            Ok(mut manager) => {
                if !manager.discard_oversized_openai_native_compaction() {
                    return false;
                }
                self.sync_session_compaction_state_from_manager(&manager);
                true
            }
            Err(_) => {
                logging::warn(
                    "OpenAI native compaction recovery skipped: compaction manager lock busy",
                );
                false
            }
        };

        if !recovered {
            return false;
        }

        self.cache_tracker.reset();
        self.locked_tools = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.session.provider_session_identity = None;

        logging::warn(
            "OpenAI native compaction payload exceeded provider size limit; discarded native state and retrying with text fallback",
        );
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "native_compaction_payload_recovered",
                "openai_encrypted_content_too_large",
            )
            .with_session_id(self.session.id.clone())
            .force_attribution(),
        );

        true
    }

    fn effective_context_tokens_from_usage(
        &self,
        input_tokens: u64,
        cache_read_input_tokens: Option<u64>,
        cache_creation_input_tokens: Option<u64>,
    ) -> u64 {
        // Shared heuristic (jcode-compaction-core): keeps the compaction
        // manager's observed-token feed consistent with the client-side
        // context display.
        crate::compaction::effective_context_tokens_from_usage(
            self.provider.name(),
            input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        )
    }

    pub(super) fn update_compaction_usage_from_stream(
        &mut self,
        input_tokens: u64,
        cache_read_input_tokens: Option<u64>,
        cache_creation_input_tokens: Option<u64>,
    ) {
        if !self.provider.uses_jcode_compaction() || input_tokens == 0 {
            return;
        }
        let observed = self.effective_context_tokens_from_usage(
            input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        );
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.update_observed_input_tokens(observed);
            manager.push_token_snapshot(observed);
        };
    }

    /// Push an embedding snapshot for the semantic compaction mode.
    /// Called after each assistant turn with a short text snippet.
    /// No-op if the embedding model is unavailable or mode is not semantic.
    pub(super) fn push_embedding_snapshot_if_semantic(&mut self, text: &str) {
        use crate::config::CompactionMode;
        let is_semantic = {
            let compaction = self.registry.compaction();
            compaction
                .try_read()
                .map(|m| m.mode() == CompactionMode::Semantic)
                .unwrap_or(false)
        };
        if !is_semantic {
            return;
        }
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.push_embedding_snapshot(text);
        };
    }
}
