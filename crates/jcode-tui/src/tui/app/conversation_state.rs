use super::*;

impl App {
    pub(super) fn ensure_provider_messages_hydrated(&mut self) {
        if !self.is_remote || !self.messages.is_empty() || self.session.messages.is_empty() {
            return;
        }

        let provider_messages = self.session.messages_for_provider_uncached();
        self.replace_provider_messages(provider_messages);
    }

    pub(super) fn materialized_provider_messages(&self) -> Vec<Message> {
        if self.is_remote || !self.messages.is_empty() {
            self.messages.clone()
        } else {
            self.session.messages_for_provider_uncached()
        }
    }

    pub(super) fn local_transcript_message_count(&self) -> usize {
        if self.is_remote {
            self.messages.len()
        } else {
            self.session.messages.len()
        }
    }

    pub(super) fn format_compaction_strategy_label(trigger: &str) -> &'static str {
        match trigger {
            "manual" => "manual",
            "proactive" => "proactive",
            "semantic" => "semantic",
            "reactive" => "reactive",
            "auto_recovery" => "automatic recovery",
            "hard_compact" => "emergency",
            _ => "automatic",
        }
    }

    pub(super) fn format_compaction_started_message(trigger: &str) -> String {
        let strategy = Self::format_compaction_strategy_label(trigger);
        format!(
            "📦 Compacting context ({}) - summarizing older messages in the background to stay within the context window.",
            strategy
        )
    }

    pub(super) fn format_compaction_progress_notice(elapsed: std::time::Duration) -> String {
        const BAR_WIDTH: usize = 12;
        const PULSE_WIDTH: usize = 4;
        let max_start = BAR_WIDTH.saturating_sub(PULSE_WIDTH);
        let frame = (elapsed.as_millis() / 180) as usize;
        let period = (max_start * 2).max(1);
        let phase = frame % period;
        let start = if phase <= max_start {
            phase
        } else {
            period - phase
        };
        let mut bar = String::with_capacity(BAR_WIDTH);
        for idx in 0..BAR_WIDTH {
            if (start..start + PULSE_WIDTH).contains(&idx) {
                bar.push('█');
            } else {
                bar.push('░');
            }
        }
        format!("Compacting context [{}] {:.0}s", bar, elapsed.as_secs_f32())
    }

    pub(super) fn format_compaction_complete_message(
        event: &crate::compaction::CompactionEvent,
        context_limit: u64,
    ) -> String {
        if event.trigger == "hard_compact" {
            return Self::format_emergency_compaction_message(event, context_limit);
        }

        let reason = match event.trigger.as_str() {
            "auto_recovery" => "after the context window filled up",
            _ => "to stay within the context window",
        };
        let strategy = Self::format_compaction_strategy_label(&event.trigger);
        let mut message = format!(
            "📦 Context compacted ({}) - older messages were summarized {}.",
            strategy, reason
        );
        let details = Self::format_compaction_detail_segments(event, context_limit, false);
        if !details.is_empty() {
            message.push_str("\n\n");
            message.push_str(&details.join(" · "));
        }
        message
    }

    pub(super) fn format_emergency_compaction_message(
        event: &crate::compaction::CompactionEvent,
        context_limit: u64,
    ) -> String {
        let mut message =
            "📦 Emergency compaction - older messages were dropped to recover from context pressure. Recent context was kept.".to_string();
        let details = Self::format_compaction_detail_segments(event, context_limit, true);
        if !details.is_empty() {
            message.push_str("\n\n");
            message.push_str(&details.join(" · "));
        }
        message
    }

    fn format_compaction_detail_segments(
        event: &crate::compaction::CompactionEvent,
        context_limit: u64,
        emergency: bool,
    ) -> Vec<String> {
        let mut details = Vec::new();

        if let Some(duration_ms) = event.duration_ms {
            details.push(format!(
                "Took {}",
                crate::message::Message::format_duration(duration_ms)
            ));
        }
        if let Some(engine) = event.engine.as_deref() {
            let ownership = event
                .ownership
                .as_deref()
                .map_or_else(String::new, |owner| format!(" via {owner}"));
            details.push(format!("engine {engine}{ownership}"));
        }
        if let Some(route) = event.effective_route.as_deref() {
            details.push(format!("route {route}"));
        }
        if let Some(reason) = event.fallback_reason.as_deref() {
            details.push(format!("fallback {reason}"));
        }
        if let Some(frontier_size) = event.frontier_size {
            let mut graph = format!("frontier {frontier_size}");
            if let (Some(leaves), Some(parents)) = (event.leaf_count, event.parent_count) {
                let leaf_noun = if leaves == 1 { "leaf" } else { "leaves" };
                let parent_noun = if parents == 1 { "parent" } else { "parents" };
                graph.push_str(&format!(" ({leaves} {leaf_noun}, {parents} {parent_noun})"));
            }
            if let Some(level) = event.max_node_level {
                graph.push_str(&format!(", level {level}"));
            }
            if let Some(generation) = event.graph_generation {
                graph.push_str(&format!(", generation {generation}"));
            }
            details.push(graph);
        }
        if let Some(tokens) = event.pre_tokens {
            details.push(format!(
                "before ~{} tokens",
                Self::format_compaction_number(tokens)
            ));
        }
        if let Some(tokens) = event.post_tokens {
            let mut segment = format!("now ~{} tokens", Self::format_compaction_number(tokens));
            if context_limit > 0 {
                segment.push_str(&format!(
                    " ({})",
                    Self::format_compaction_usage(tokens, context_limit)
                ));
            }
            details.push(segment);
        }
        if let Some(saved) = event.tokens_saved.filter(|saved| *saved > 0) {
            details.push(format!(
                "saved ~{} tokens",
                Self::format_compaction_number(saved)
            ));
        }

        let message_count = event.messages_dropped.or(event.messages_compacted);
        if let Some(count) = message_count {
            let noun = if count == 1 { "message" } else { "messages" };
            let verb = if emergency { "dropped" } else { "summarized" };
            details.push(format!(
                "{} {} {}",
                verb,
                Self::format_compaction_number(count as u64),
                noun
            ));
        }

        if let Some(summary_chars) = event.summary_chars.filter(|chars| *chars > 0) {
            details.push(format!(
                "summary {} chars",
                Self::format_compaction_number(summary_chars as u64)
            ));
        }

        if let Some(active_messages) = event.active_messages {
            let noun = if active_messages == 1 {
                "recent message"
            } else {
                "recent messages"
            };
            details.push(format!(
                "kept {} {} live",
                Self::format_compaction_number(active_messages as u64),
                noun
            ));
        }

        details
    }

    fn format_compaction_usage(tokens: u64, context_limit: u64) -> String {
        let percent = (tokens as f64 / context_limit.max(1) as f64) * 100.0;
        if percent >= 10.0 {
            format!("{percent:.0}% of window")
        } else {
            format!("{percent:.1}% of window")
        }
    }

    pub(super) fn format_compaction_number(value: u64) -> String {
        let digits = value.to_string();
        let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
        for (idx, ch) in digits.chars().rev().enumerate() {
            if idx > 0 && idx % 3 == 0 {
                formatted.push(',');
            }
            formatted.push(ch);
        }
        formatted.chars().rev().collect()
    }

    pub(super) fn add_provider_message(&mut self, message: Message) {
        if self.is_remote {
            self.ensure_provider_messages_hydrated();
            self.messages.push(message.clone());
        }
        let lcm_configured =
            crate::config::config().compaction.engine == crate::config::CompactionEngine::Lcm;
        if self.is_remote || (!self.provider.uses_jcode_compaction() && !lcm_configured) {
            return;
        }
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.notify_message_added_with(&message);
        };
    }

    pub(super) fn replace_provider_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
        self.last_injected_memory_signature = None;
        self.reset_tool_output_tracking();
        self.reseed_compaction_from_provider_messages();
        self.note_runtime_memory_event_force("provider_messages_replaced", "provider_view_reset");
    }

    pub(super) fn clear_provider_messages(&mut self) {
        self.messages.clear();
        self.last_injected_memory_signature = None;
        self.reset_tool_output_tracking();
        self.reseed_compaction_from_provider_messages();
        self.note_runtime_memory_event_force("provider_messages_cleared", "provider_view_cleared");
    }

    pub(super) fn reset_tool_output_tracking(&mut self) {
        self.tool_call_ids.clear();
        self.tool_result_ids.clear();
        self.tool_output_scan_index = 0;
    }

    pub(super) fn reseed_compaction_from_provider_messages(&mut self) {
        let lcm_configured =
            crate::config::config().compaction.engine == crate::config::CompactionEngine::Lcm;
        if self.is_remote
            || (!self.provider.uses_jcode_compaction()
                && self.session.compaction.is_none()
                && !lcm_configured)
        {
            return;
        }
        if let Err(error) = self.reseed_compaction_from_provider_messages_strict() {
            crate::logging::warn(&format!(
                "Could not reseed compaction from provider messages: {error}"
            ));
        }
    }

    pub(super) fn reseed_compaction_from_provider_messages_strict(&mut self) -> anyhow::Result<()> {
        let provider_messages = self.materialized_provider_messages();
        let active_stored_messages = self.session.active_stored_messages().into_owned();
        let compaction = self.registry.compaction();
        let mut manager = compaction.try_write().map_err(|_| {
            anyhow::anyhow!("compaction state is busy during provider identity transition")
        })?;
        manager.reset();
        manager.set_budget(self.context_limit as usize);
        if let Some(state) = self.session.compaction.as_ref() {
            if self.session.has_owned_native_lcm_projection() {
                manager.restore_native_lcm_stored_state_with(state, &active_stored_messages);
            } else {
                manager.restore_persisted_state_with(state, &provider_messages);
            }
        } else {
            manager.seed_restored_messages_with(&provider_messages);
        }
        if manager.discard_oversized_openai_native_compaction() {
            self.sync_session_compaction_state_from_manager(&manager);
        }
        Ok(())
    }

    pub(super) fn reset_provider_view_after_identity_transition(&mut self) -> anyhow::Result<()> {
        // Never seed the new identity from `self.messages`: in local mode that
        // vector may still be the old identity's compacted provider projection.
        // Re-materialize only from the canonical durable journal after its
        // projection/frontier were deactivated.
        self.messages = self.session.messages_for_provider_uncached();
        self.last_injected_memory_signature = None;
        self.reset_tool_output_tracking();
        self.reseed_compaction_from_provider_messages_strict()
    }

    pub(super) fn sync_session_compaction_state_from_manager(
        &mut self,
        manager: &crate::compaction::CompactionManager,
    ) {
        let new_state = manager.persisted_state();
        if self.session.compaction != new_state {
            self.session.compaction = new_state;
            if let Err(err) = self.session.save() {
                crate::logging::error(&format!(
                    "Failed to persist compaction state for session {}: {}",
                    self.session.id, err
                ));
            }
        }
    }

    pub(super) fn synchronize_compaction_engine(
        &mut self,
        manager: &mut crate::compaction::CompactionManager,
    ) -> bool {
        if !manager.synchronize_engine(crate::config::config().compaction.engine) {
            return false;
        }
        self.session.deactivate_context_graph_state();
        self.invalidate_kv_cache_after_compaction();
        if let Err(error) = self.session.save() {
            crate::logging::error(&format!(
                "Failed to persist compaction engine switch for session {}: {error}",
                self.session.id
            ));
        }
        true
    }

    pub(super) fn apply_openai_native_compaction(
        &mut self,
        encrypted_content: String,
        compacted_count: usize,
    ) -> anyhow::Result<()> {
        let encrypted_content_len = encrypted_content.len();
        let (summary_text, openai_encrypted_content) =
            if crate::provider::openai_request::openai_encrypted_content_is_sendable(
                &encrypted_content,
            ) {
                (String::new(), Some(encrypted_content))
            } else {
                crate::logging::warn(&format!(
                    "Discarding oversized OpenAI native compaction payload before TUI persist ({} chars)",
                    encrypted_content_len,
                ));
                (
                    crate::provider::openai_request::openai_encrypted_content_fallback_summary(
                        encrypted_content_len,
                    ),
                    None,
                )
            };
        let state = crate::session::StoredCompactionState {
            summary_text,
            openai_encrypted_content,
            covers_up_to_turn: compacted_count,
            original_turn_count: compacted_count,
            compacted_count,
        };

        self.session.compaction = Some(state.clone());
        let provider_messages = self.materialized_provider_messages();
        let compaction = self.registry.compaction();
        if let Ok(mut manager) = compaction.try_write() {
            manager.set_budget(self.context_limit as usize);
            manager.restore_persisted_state_with(&state, &provider_messages);
        }

        self.invalidate_kv_cache_after_compaction();
        self.session.save()?;
        Ok(())
    }

    pub(super) fn messages_for_provider(&mut self) -> (Vec<Message>, Option<CompactionEvent>) {
        self.ensure_provider_messages_hydrated();

        if self.is_remote {
            return (self.messages.clone(), None);
        }
        let base_messages = self.materialized_provider_messages();
        let lcm_configured =
            crate::config::config().compaction.engine == crate::config::CompactionEngine::Lcm;
        if !lcm_configured
            && !self.provider.supports_compaction()
            && self.session.compaction.is_none()
        {
            return (base_messages, None);
        }
        let compaction = self.registry.compaction();
        match compaction.try_write() {
            Ok(mut manager) => {
                self.synchronize_compaction_engine(&mut manager);
                if manager.engine() == crate::config::CompactionEngine::Lcm {
                    let (mut messages, mut event) = match manager
                        .materialize_lcm_context(&mut self.session)
                    {
                        Ok(materialized) => materialized,
                        Err(error) => {
                            crate::logging::error(&format!(
                                "LCM durable commit failed; retaining prior provider context: {error}"
                            ));
                            (manager.messages_for_api_with(&base_messages), None)
                        }
                    };
                    let action =
                        manager.ensure_lcm_context_fits(&mut self.session, self.provider.clone());
                    match action {
                        crate::compaction::CompactionAction::BackgroundStarted { trigger } => {
                            self.push_display_message(DisplayMessage::system(
                                Self::format_compaction_started_message(&trigger),
                            ));
                            self.set_status_notice("Compacting context");
                        }
                        crate::compaction::CompactionAction::HardCompacted(_) => {
                            if let Ok((committed, _)) =
                                manager.materialize_lcm_context(&mut self.session)
                            {
                                messages = committed;
                            }
                            event = event.or_else(|| manager.take_compaction_event());
                        }
                        crate::compaction::CompactionAction::None => {}
                    }
                    return (messages, event);
                }
                let discarded_oversized_native =
                    manager.discard_oversized_openai_native_compaction();
                if self.provider.uses_jcode_compaction() {
                    let action = manager.ensure_context_fits(&base_messages, self.provider.clone());
                    match action {
                        crate::compaction::CompactionAction::BackgroundStarted { trigger } => {
                            self.push_display_message(DisplayMessage::system(
                                Self::format_compaction_started_message(&trigger),
                            ));
                            self.set_status_notice("Compacting context");
                        }
                        crate::compaction::CompactionAction::HardCompacted(_) => {}
                        crate::compaction::CompactionAction::None => {}
                    }
                }
                let messages = manager.messages_for_api_with(&base_messages);
                let event = manager.take_compaction_event();
                if event.is_some() || discarded_oversized_native {
                    self.sync_session_compaction_state_from_manager(&manager);
                }
                (messages, event)
            }
            Err(_) => (base_messages, None),
        }
    }

    pub(super) fn poll_compaction_completion(&mut self) -> bool {
        let lcm_configured =
            crate::config::config().compaction.engine == crate::config::CompactionEngine::Lcm;
        if self.is_remote
            || (!lcm_configured
                && !self.provider.supports_compaction()
                && self.session.compaction.is_none())
        {
            return false;
        }
        let compaction = self.registry.compaction();
        let event = if let Ok(mut manager) = compaction.try_write() {
            self.synchronize_compaction_engine(&mut manager);
            if manager.engine() == crate::config::CompactionEngine::Lcm {
                match manager.materialize_lcm_context(&mut self.session) {
                    Ok((_, event)) => event,
                    Err(error) => {
                        crate::logging::error(&format!("LCM durable commit failed: {error}"));
                        None
                    }
                }
            } else {
                let provider_messages = self.materialized_provider_messages();
                let event = manager.poll_compaction_event_with(&provider_messages);
                if event.is_some() {
                    self.sync_session_compaction_state_from_manager(&manager);
                }
                event
            }
        } else {
            None
        };
        if let Some(event) = event {
            self.handle_compaction_event(event);
            return true;
        }
        false
    }

    pub(super) fn handle_compaction_event(&mut self, event: CompactionEvent) {
        self.invalidate_kv_cache_after_compaction();
        if let Err(err) = self.session.save() {
            crate::logging::warn(&format!(
                "Failed to persist provider session reset after compaction for session {}: {}",
                self.session.id, err
            ));
        }
        let message = if event.messages_dropped.is_some() {
            self.set_status_notice("Emergency compaction");
            Self::format_emergency_compaction_message(&event, self.context_limit)
        } else {
            self.set_status_notice("Context compacted");
            Self::format_compaction_complete_message(&event, self.context_limit)
        };
        self.push_display_message(DisplayMessage::system(message));
    }

    pub(super) fn invalidate_kv_cache_after_compaction(&mut self) {
        // Compaction intentionally replaces the provider-facing transcript
        // (typically hundreds of messages become summary + recent tail). The
        // previous request is therefore not a valid append-only cache baseline.
        // Advance the generation as well as clearing the current baseline so a
        // pre-compaction request that completes later cannot restore stale state.
        self.kv_cache.cache_generation = self.kv_cache.cache_generation.wrapping_add(1);
        self.kv_cache.kv_cache_baseline = None;
        self.kv_cache.cold_cache_warned_baseline_completed_at = None;
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.context_warning_shown = false;
        // The sidebar/status context figure is derived from the last
        // provider-reported stream usage, which described the *pre-compaction*
        // message list. Mark it stale so the display falls back to the local
        // estimate over the new (summary + recent) active messages until the
        // next provider usage report arrives (issue #441). The raw counters
        // are kept intact for turn footers and cost accounting.
        self.streaming.streaming_context_stale = true;
        self.streaming.streaming_usage_call_reset_pending = true;
        self.bump_context_revision();
    }

    pub fn set_status_notice(&mut self, text: impl Into<String>) {
        self.status_notice = Some((text.into(), Instant::now()));
    }

    /// Stash a persistent startup notice card and show it immediately.
    ///
    /// The card is also re-applied once the remote History bootstrap clears the
    /// transcript for a brand-new session, so launch-hotkey / welcome tips stay
    /// visible on the idle screen instead of flashing for a moment and vanishing.
    pub fn set_pending_startup_notice(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
    ) {
        let title = title.into();
        let message = message.into();
        self.push_display_message(
            DisplayMessage::system(message.clone()).with_title(title.clone()),
        );
        self.pending_startup_notice = Some((title, message));
    }

    /// Re-apply the stashed startup notice card if it is no longer present in the
    /// transcript (e.g. after the History bootstrap reset the display history).
    /// Scoped to the idle screen: once a real conversation has started the notice
    /// is consumed so it never reappears (and never leaks into a switched-to
    /// session).
    pub(crate) fn reapply_pending_startup_notice_if_cleared(&mut self) {
        let Some((title, message)) = self.pending_startup_notice.clone() else {
            return;
        };
        let conversation_started = self
            .display_messages
            .iter()
            .any(|m| matches!(m.role.as_str(), "user" | "assistant" | "tool" | "reasoning"));
        if conversation_started {
            self.pending_startup_notice = None;
            return;
        }
        let already_present = self
            .display_messages
            .iter()
            .any(|m| m.role == "system" && m.content == message);
        if !already_present {
            self.push_display_message(DisplayMessage::system(message).with_title(title));
        }
    }

    pub(crate) fn set_remote_startup_phase(&mut self, phase: super::RemoteStartupPhase) {
        let changed = self.remote_startup_phase.as_ref() != Some(&phase);
        self.remote_startup_phase = Some(phase);
        if changed || self.remote_startup_phase_started.is_none() {
            self.remote_startup_phase_started = Some(Instant::now());
        }
    }

    pub(crate) fn clear_remote_startup_phase(&mut self) {
        self.remote_startup_phase = None;
        self.remote_startup_phase_started = None;
    }

    /// Begin (or restart) the per-connection history-recovery budget.
    ///
    /// Called when a remote connection starts waiting for the bootstrap
    /// `History` payload. Each fresh connection gets a clean budget so a stall on
    /// one connection does not exhaust the retries available to the next.
    pub(crate) fn begin_remote_history_wait(&mut self) {
        self.remote_history_wait_started = Some(Instant::now());
        self.remote_history_recovery_attempts = 0;
        self.remote_history_recovery_last_attempt = None;
    }

    /// Clear the history-recovery watchdog once history has loaded (or the
    /// connection is no longer waiting on it).
    pub(crate) fn clear_remote_history_wait(&mut self) {
        self.remote_history_wait_started = None;
        self.remote_history_recovery_attempts = 0;
        self.remote_history_recovery_last_attempt = None;
    }

    pub(super) fn set_memory_feature_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if !enabled {
            crate::memory::clear_pending_memory(&self.session.id);
            crate::memory::clear_activity();
            crate::memory_agent::reset();
            self.last_injected_memory_signature = None;
        }
    }

    pub(super) fn set_autoreview_feature_enabled(&mut self, enabled: bool) {
        self.autoreview_enabled = enabled;
        self.session.autoreview_enabled = Some(enabled);
    }

    pub(super) fn set_autojudge_feature_enabled(&mut self, enabled: bool) {
        self.autojudge_enabled = enabled;
        self.session.autojudge_enabled = Some(enabled);
    }

    pub(super) fn trigger_save_memory_extraction(&self) {
        let provider_messages = self.materialized_provider_messages();
        if self.is_remote || !self.memory_enabled || provider_messages.len() < 4 {
            return;
        }

        let transcript = crate::memory_agent::build_transcript_for_extraction(&provider_messages);
        crate::memory_agent::trigger_final_extraction_with_dir(
            transcript,
            self.session.id.clone(),
            self.session.working_dir.clone(),
        );
    }

    pub(super) fn memory_prompt_signature(prompt: &str) -> String {
        prompt
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<String>>()
            .join("\n")
    }

    pub(super) fn should_inject_memory_context(&mut self, prompt: &str) -> bool {
        let signature = Self::memory_prompt_signature(prompt);
        let now = Instant::now();
        if let Some((last_signature, last_injected_at)) =
            self.last_injected_memory_signature.as_ref()
            && *last_signature == signature
            && now.duration_since(*last_injected_at).as_secs() < MEMORY_INJECTION_SUPPRESSION_SECS
        {
            return false;
        }
        self.last_injected_memory_signature = Some((signature, now));
        true
    }

    pub(in crate::tui::app) fn clear_active_experimental_feature_notice(&mut self) {
        self.active_experimental_feature_notice = None;
    }

    pub(in crate::tui::app) fn note_experimental_feature_use(
        &mut self,
        key: &'static str,
    ) -> Option<&'static str> {
        const NOTICE: &str = "experimental feature";
        if self
            .experimental_feature_warnings_seen
            .insert(key.to_string())
        {
            self.active_experimental_feature_notice = Some(NOTICE.to_string());
            Some(NOTICE)
        } else {
            None
        }
    }

    pub(in crate::tui::app) fn experimental_feature_key_for_tool(
        tool: &crate::message::ToolCall,
    ) -> Option<&'static str> {
        if tool.name != "swarm" {
            return None;
        }

        let action = tool.input.get("action").and_then(|value| value.as_str());
        let spawns_agents = matches!(action, Some("spawn") | Some("fill_slots"))
            || matches!(action, Some("assign_task") | Some("assign_next"))
                && (tool
                    .input
                    .get("spawn_if_needed")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
                    || tool
                        .input
                        .get("prefer_spawn")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false));

        spawns_agents.then_some("swarm_spawn")
    }

    pub(super) fn set_swarm_feature_enabled(&mut self, enabled: bool) {
        self.swarm_enabled = enabled;
        if !enabled {
            self.remote_swarm_members.clear();
        }
    }

    pub(super) fn extract_thought_line(text: &str) -> Option<String> {
        let trimmed = text.trim();
        if trimmed.starts_with("Thought for ") && trimmed.ends_with('s') {
            Some(trimmed.to_string())
        } else {
            None
        }
    }

    /// Handle quit request (Ctrl+C/Ctrl+D). Returns true if should actually quit.
    pub(super) fn handle_quit_request(&mut self) -> bool {
        const QUIT_TIMEOUT: Duration = Duration::from_secs(2);

        if let Some(pending_time) = self.quit_pending
            && pending_time.elapsed() < QUIT_TIMEOUT
        {
            let mut candidate = self.session.clone();
            candidate.provider_session_id = self.provider_session_id.clone();
            candidate.status = crate::session::SessionStatus::Closed;
            if let Err(error) = candidate.save() {
                self.push_display_message(DisplayMessage::error(format!(
                    "Failed to persist session before quit: {error}"
                )));
                self.set_status_notice("Quit cancelled: session save failed");
                return false;
            }
            crate::storage::unregister_active_pid(&candidate.id);
            self.session = candidate;
            crate::telemetry::end_session_with_reason(
                self.provider.name(),
                &self.provider.model(),
                crate::telemetry::SessionEndReason::NormalExit,
            );
            self.should_quit = true;
            return true;
        }

        // First press or timeout expired - show warning
        self.quit_pending = Some(Instant::now());
        self.set_status_notice("Press Ctrl+C again to quit");
        false
    }

    fn collect_missing_tool_outputs_since_last_scan(&mut self) -> Vec<(usize, Vec<String>)> {
        let message_len = self.local_transcript_message_count();
        if self.tool_output_scan_index > message_len {
            self.reset_tool_output_tracking();
        }

        let scan_start = self.tool_output_scan_index;
        let mut new_result_ids = Vec::new();
        let mut assistant_tool_uses: Vec<(usize, Vec<String>)> = Vec::new();

        if self.is_remote {
            for (index, msg) in self.messages.iter().enumerate().skip(scan_start) {
                match msg.role {
                    Role::User => {
                        for block in &msg.content {
                            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                                new_result_ids.push(tool_use_id.clone());
                            }
                        }
                    }
                    Role::Assistant => {
                        let tool_uses = msg
                            .content
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        if !tool_uses.is_empty() {
                            assistant_tool_uses.push((index, tool_uses));
                        }
                    }
                }
            }
        } else {
            for (index, msg) in self.session.messages.iter().enumerate().skip(scan_start) {
                if self.session.archived_message_ids.contains(&msg.id) {
                    continue;
                }
                match msg.role {
                    Role::User => {
                        for block in &msg.content {
                            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                                new_result_ids.push(tool_use_id.clone());
                            }
                        }
                    }
                    Role::Assistant => {
                        let tool_uses = msg
                            .content
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        if !tool_uses.is_empty() {
                            assistant_tool_uses.push((index, tool_uses));
                        }
                    }
                }
            }
        }

        self.tool_result_ids.extend(new_result_ids);

        let mut missing_repairs = Vec::new();
        for (index, tool_uses) in assistant_tool_uses {
            let mut missing_for_message = Vec::new();
            for id in tool_uses {
                self.tool_call_ids.insert(id.clone());
                if !self.tool_result_ids.contains(&id) {
                    missing_for_message.push(id);
                }
            }
            if !missing_for_message.is_empty() {
                missing_repairs.push((index, missing_for_message));
            }
        }

        self.tool_output_scan_index = message_len;
        missing_repairs
    }

    pub(super) fn missing_tool_result_ids(&mut self) -> Vec<String> {
        self.collect_missing_tool_outputs_since_last_scan();
        self.tool_call_ids
            .difference(&self.tool_result_ids)
            .cloned()
            .collect::<Vec<_>>()
    }

    pub(super) fn summarize_tool_results_missing(&mut self) -> Option<String> {
        let missing = self.missing_tool_result_ids();
        if missing.is_empty() {
            return None;
        }
        let sample = missing
            .iter()
            .take(3)
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let count = missing.len();
        let suffix = if count > 3 { "..." } else { "" };
        Some(format!(
            "Missing tool outputs for {} call(s): {}{}",
            count, sample, suffix
        ))
    }

    pub(super) fn repair_missing_tool_outputs(&mut self) -> usize {
        let missing_repairs = self.collect_missing_tool_outputs_since_last_scan();
        let mut repaired = 0usize;
        for (_index, missing_for_message) in missing_repairs {
            for id in &missing_for_message {
                let tool_block = ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: TOOL_OUTPUT_MISSING_TEXT.to_string(),
                    is_error: Some(true),
                };
                let inserted_message = Message {
                    role: Role::User,
                    content: vec![tool_block.clone()],
                    timestamp: None,
                    tool_duration_ms: None,
                };
                let stored_message = crate::session::StoredMessage {
                    id: id::new_id("message"),
                    role: Role::User,
                    content: vec![tool_block],
                    display_role: None,
                    timestamp: Some(chrono::Utc::now()),
                    tool_duration_ms: None,
                    token_usage: None,
                };
                if self.is_remote || !self.messages.is_empty() {
                    self.messages.push(inserted_message);
                }
                // Canonical history is append-only. Provider request builders
                // normalize delayed tool results back beside their calls.
                self.session.append_stored_message(stored_message);
                self.tool_result_ids.insert(id.clone());
                repaired += 1;
            }
        }

        self.tool_output_scan_index = self.local_transcript_message_count();

        if repaired > 0 {
            self.reseed_compaction_from_provider_messages();
            let _ = self.session.save();
        }

        repaired
    }

    /// Rebuild current session into a new one without tool calls
    pub(super) fn recover_session_without_tools(&mut self) {
        let old_session = self.session.clone();
        let old_messages = old_session.active_stored_messages().into_owned();

        let new_session_id = format!("session_recovery_{}", id::new_id("rec"));
        let mut new_session =
            Session::create_with_id(new_session_id, Some(old_session.id.clone()), None);
        new_session.title = old_session.title.clone();
        new_session.custom_title = old_session.custom_title.clone();
        new_session.provider_session_id = None;
        new_session.provider_session_identity = None;
        new_session.exact_runtime_identity = old_session.exact_runtime_identity.clone();
        new_session.provider_key = old_session.provider_key.clone();
        new_session.route_api_method = old_session.route_api_method.clone();
        new_session.model = old_session.model.clone();
        new_session.reasoning_effort = old_session.reasoning_effort.clone();
        new_session.is_canary = old_session.is_canary;
        new_session.testing_build = old_session.testing_build.clone();
        new_session.is_debug = old_session.is_debug;
        new_session.saved = old_session.saved;
        new_session.save_label = old_session.save_label.clone();
        new_session.working_dir = old_session.working_dir.clone();

        self.clear_provider_messages();
        self.clear_display_messages();
        // Ctrl+R is reachable mid-stream (turn.rs key handling); drop the
        // in-flight streaming render state (including the ephemeral mermaid
        // preview slot) so it cannot leak into the recovered session's
        // transcript. ACTIVE_DIAGRAMS deliberately survives: recovery keeps
        // every text block, so registered diagrams still back retained
        // messages, and body-cache prefix reuse (ui_prepare.rs) would skip
        // re-registering them if we cleared the registry here.
        self.clear_streaming_render_state();
        self.queued_messages.clear();
        self.pasted_contents.clear();
        self.pending_images.clear();
        self.active_skill = None;
        self.provider_session_id = None;
        self.session = new_session;
        self.set_side_panel_snapshot(
            crate::side_panel::snapshot_for_session(&self.session.id).unwrap_or_default(),
        );

        for msg in old_messages {
            let role = msg.role.clone();
            let kept_blocks: Vec<ContentBlock> = msg
                .content
                .into_iter()
                .filter(|block| matches!(block, ContentBlock::Text { .. }))
                .collect();
            if kept_blocks.is_empty() {
                continue;
            }
            self.add_provider_message(Message {
                role: role.clone(),
                content: kept_blocks.clone(),
                timestamp: None,
                tool_duration_ms: None,
            });
            self.push_display_message(DisplayMessage {
                role: match role {
                    Role::User => "user".to_string(),
                    Role::Assistant => "assistant".to_string(),
                },
                content: kept_blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                tool_calls: vec![],
                duration_secs: None,
                title: None,
                tool_data: None,
            });
            let _ = self.session.add_message(role, kept_blocks);
        }
        let _ = self.session.save();

        self.push_display_message(DisplayMessage::system(format!(
            "Recovery complete. New session: {}. Tool calls stripped; context preserved.",
            self.session.id
        )));
        self.set_status_notice("Recovered session");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolCall;

    #[test]
    fn experimental_feature_key_marks_swarm_spawn_actions() {
        let tool = ToolCall {
            id: "tc".to_string(),
            name: "swarm".to_string(),
            input: serde_json::json!({"action": "spawn", "prompt": "try it"}),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(
            App::experimental_feature_key_for_tool(&tool),
            Some("swarm_spawn")
        );
    }

    #[test]
    fn experimental_feature_key_marks_spawn_if_needed_assignment() {
        let tool = ToolCall {
            id: "tc".to_string(),
            name: "swarm".to_string(),
            input: serde_json::json!({"action": "assign_task", "spawn_if_needed": true}),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(
            App::experimental_feature_key_for_tool(&tool),
            Some("swarm_spawn")
        );
    }

    #[test]
    fn experimental_feature_key_ignores_non_spawning_swarm_actions() {
        let tool = ToolCall {
            id: "tc".to_string(),
            name: "swarm".to_string(),
            input: serde_json::json!({"action": "status"}),
            intent: None,
            thought_signature: None,
        };

        assert_eq!(App::experimental_feature_key_for_tool(&tool), None);
    }
}
