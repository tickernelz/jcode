use super::*;

impl Session {
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
            archived_message_ids: Vec::new(),
            provider_image_char_budget: None,
            provider_tool_use_suppressed_message_ids: Vec::new(),
            journal_sequence: 0,
            journal_watermark: 0,
            persistence_revision: 0,
            context_nodes: Vec::new(),
            context_frontier: None,
            compaction: None,
            provider_session_id: None,
            provider_session_identity: None,
            provider_key: None,
            model: None,
            route_api_method: None,
            reasoning_effort: None,
            exact_runtime_identity: None,
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
            archived_message_ids: Vec::new(),
            provider_image_char_budget: None,
            provider_tool_use_suppressed_message_ids: Vec::new(),
            journal_sequence: 0,
            journal_watermark: 0,
            persistence_revision: 0,
            context_nodes: Vec::new(),
            context_frontier: None,
            compaction: None,
            provider_session_id: None,
            provider_session_identity: None,
            provider_key: None,
            model: None,
            route_api_method: None,
            reasoning_effort: None,
            exact_runtime_identity: None,
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
                    self.deactivate_context_graph_state();
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

    /// Publish presence after an already-active session candidate has been
    /// durably committed. Lifecycle transactions use this to avoid exposing a
    /// new binding before its snapshot succeeds.
    pub fn publish_active_presence(&self) {
        if self.status == SessionStatus::Active {
            register_active_pid(&self.id, self.last_pid.unwrap_or_else(std::process::id));
            self.sync_internal_presence_flag();
        }
    }

    /// Keep the on-disk internal-session flag in sync with this session's
    /// role. Debug/test sessions and spawned children (swarm workers,
    /// subagents) are internal: they stay tracked for lifecycle purposes but
    /// are hidden from user-facing presence UIs like the menu bar (issue
    /// #508).
    pub(super) fn sync_internal_presence_flag(&self) {
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
}
