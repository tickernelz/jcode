use super::*;

impl Session {
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

    pub fn replace_messages(&mut self, messages: Vec<StoredMessage>) {
        self.messages = messages;
        self.archived_message_ids.clear();
        self.provider_image_char_budget = None;
        self.provider_tool_use_suppressed_message_ids.clear();
        self.compaction = None;
        self.deactivate_context_graph_state();
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
                self.deactivate_context_graph_state();
            }
            self.messages.truncate(len);
            let retained = self
                .messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<HashSet<_>>();
            self.archived_message_ids
                .retain(|id| retained.contains(id.as_str()));
            self.mark_memory_profile_dirty();
            self.mark_messages_full_dirty();
        }
    }

    /// Persist provider-only oversized-image suppression metadata.
    ///
    /// Raw `StoredMessage` bytes remain canonical. Provider materialization uses
    /// this budget to clone-and-strip oversized inline images oldest-first.
    pub fn suppress_oversized_images_for_provider(&mut self, target_total_chars: usize) -> usize {
        if self
            .provider_image_char_budget
            .is_some_and(|current| current <= target_total_chars)
        {
            return 0;
        }
        let stripped = self.projected_oversized_image_strip_count(target_total_chars);
        if stripped > 0 {
            self.provider_image_char_budget = Some(target_total_chars);
            self.reset_provider_messages_cache();
            // The projection fields are snapshot metadata. Force a checkpoint
            // without changing canonical messages or pre-advancing the durable
            // CAS revision that `save` verifies against disk.
            self.mark_messages_full_dirty();
        }
        stripped
    }

    pub fn projected_oversized_image_strip_count(&self, target_total_chars: usize) -> usize {
        let mut messages = stored_messages_to_messages(&self.active_stored_messages());
        let mut contents: Vec<&mut Vec<ContentBlock>> =
            messages.iter_mut().map(|m| &mut m.content).collect();
        jcode_compaction_core::strip_large_images_in_contents(&mut contents, target_total_chars)
    }

    pub fn visible_conversation_message_count(&self) -> usize {
        self.active_stored_message_entries()
            .into_iter()
            .map(|(_, message)| message)
            .filter(|message| is_visible_conversation_message(message))
            .count()
    }

    pub fn visible_conversation_messages(&self) -> Vec<&StoredMessage> {
        self.active_stored_message_entries()
            .into_iter()
            .map(|(_, message)| message)
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
        for (stored_index, message) in self.active_stored_message_entries() {
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
            self.provider_messages_cache
                .reserve(self.active_message_count());
            self.provider_message_prefix_hashes_cache
                .reserve(self.active_message_count());
            let messages = self.project_provider_messages_from_active_stored();
            for message in messages {
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
                let message = self.project_stored_message_for_provider(&self.messages[index]);
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
        self.project_provider_messages_from_active_stored()
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
        self.archived_message_ids.clear();
        self.provider_image_char_budget = None;
        self.provider_tool_use_suppressed_message_ids.clear();
        self.compaction = None;
        self.clear_context_graph_state();
        self.env_snapshots.clear();
        self.memory_injections.clear();
        self.replay_events.clear();
        self.rebuild_memory_profile_cache();
        self.reset_provider_messages_cache();
        self.reset_persist_state(true);
    }

    /// Omit ToolUse blocks from this message in provider requests without
    /// mutating canonical stored history.
    pub fn suppress_tool_use_blocks_for_provider(&mut self, message_id: &str) {
        if self
            .provider_tool_use_suppressed_message_ids
            .iter()
            .any(|id| id == message_id)
        {
            return;
        }
        self.provider_tool_use_suppressed_message_ids
            .push(message_id.to_string());
        self.reset_provider_messages_cache();
        self.mark_messages_full_dirty();
    }

    fn project_provider_messages_from_active_stored(&self) -> Vec<Message> {
        let mut messages = self
            .active_stored_message_entries()
            .into_iter()
            .map(|(_, message)| self.project_stored_message_for_provider(message))
            .collect::<Vec<_>>();
        if let Some(target_total_chars) = self.provider_image_char_budget {
            let mut contents: Vec<&mut Vec<ContentBlock>> =
                messages.iter_mut().map(|m| &mut m.content).collect();
            jcode_compaction_core::strip_large_images_in_contents(
                &mut contents,
                target_total_chars,
            );
        }
        messages
    }

    fn project_stored_message_for_provider(&self, message: &StoredMessage) -> Message {
        let mut projected = message.to_message();
        if self
            .provider_tool_use_suppressed_message_ids
            .iter()
            .any(|id| id == &message.id)
        {
            projected
                .content
                .retain(|block| !matches!(block, ContentBlock::ToolUse { .. }));
        }
        projected
    }
}
