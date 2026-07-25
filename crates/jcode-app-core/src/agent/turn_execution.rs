use super::*;

mod session_and_repl;

fn restore_provider_identity(
    provider: &dyn Provider,
    route_request: &str,
    reasoning_effort: Option<&str>,
) -> Result<()> {
    let route_result = crate::provider::set_model_with_auth_refresh(provider, route_request)
        .map_err(|error| anyhow::anyhow!("route rollback via '{route_request}' failed: {error}"));
    let rollback_effort = reasoning_effort.unwrap_or("");
    let effort_result = if reasoning_effort.is_some() || provider.reasoning_effort().is_some() {
        provider
            .set_reasoning_effort(rollback_effort)
            .map_err(|error| {
                anyhow::anyhow!("reasoning rollback to '{rollback_effort}' failed: {error}")
            })
    } else {
        Ok(())
    };
    match (route_result, effort_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(route), Ok(())) => Err(route),
        (Ok(()), Err(effort)) => Err(effort),
        (Err(route), Err(effort)) => Err(anyhow::anyhow!("{route}; {effort}")),
    }
}

impl Agent {
    async fn ensure_provider_identity_ready(&mut self) -> Result<()> {
        crate::provider::ensure_no_account_transition()?;
        // A startup reconciliation failure remains gated, but every later turn
        // owns a retry so a transient storage/provider error cannot wedge this
        // process until restart.
        crate::server::provider_control::reconcile_pending_account_transition_for_admission_async(
            Arc::clone(&self.provider),
        )
        .await?;
        Ok(())
    }

    async fn ensure_provider_identity_ready_under_admission(&mut self) -> Result<()> {
        crate::provider::ensure_no_account_transition()?;
        crate::server::ensure_no_pending_account_reconciliation()?;
        let previous = self.session.exact_runtime_identity.clone();
        if previous.as_ref().is_some_and(|identity| {
            matches!(
                identity.route.runtime_key,
                jcode_provider_core::RuntimeKey::ClaudeOAuth
                    | jcode_provider_core::RuntimeKey::OpenAIOAuth
            )
        }) {
            self.provider.ensure_credentials_current().await?;
        }
        if self.provider.exact_runtime_identity().as_ref() != previous.as_ref() {
            self.reconcile_exact_runtime_identity_after_request_open(previous.as_ref())?;
        }
        if let Some(error) = self.provider_identity_error.as_deref() {
            anyhow::bail!("Provider identity is not safe for a model turn: {error}");
        }
        Ok(())
    }

    /// Run a single turn with the given user message
    pub async fn run_once(&mut self, user_message: &str) -> Result<()> {
        self.ensure_provider_identity_ready().await?;
        let _account_admission =
            crate::server::acquire_account_reconciliation_admission_lock().await?;
        crate::session::with_account_transition_admission(self.run_once_admitted(user_message))
            .await
    }

    async fn run_once_admitted(&mut self, user_message: &str) -> Result<()> {
        self.ensure_provider_identity_ready_under_admission()
            .await?;
        self.rewind_undo_snapshot = None;
        self.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: user_message.to_string(),
                cache_control: None,
            }],
        );
        self.session.save()?;
        if trace_enabled() {
            eprintln!("[trace] session_id {}", self.session.id);
        }
        let _ = self.run_turn(true).await?;
        Ok(())
    }

    pub async fn run_once_capture(&mut self, user_message: &str) -> Result<String> {
        self.ensure_provider_identity_ready().await?;
        let _account_admission =
            crate::server::acquire_account_reconciliation_admission_lock().await?;
        crate::session::with_account_transition_admission(
            self.run_once_capture_admitted(user_message),
        )
        .await
    }

    async fn run_once_capture_admitted(&mut self, user_message: &str) -> Result<String> {
        self.ensure_provider_identity_ready_under_admission()
            .await?;
        self.rewind_undo_snapshot = None;
        self.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: user_message.to_string(),
                cache_control: None,
            }],
        );
        self.session.save()?;
        if trace_enabled() {
            eprintln!("[trace] session_id {}", self.session.id);
        }
        self.run_turn(false).await
    }

    /// Run one conversation turn with streaming events via mpsc channel (per-client)
    pub async fn run_once_streaming_mpsc(
        &mut self,
        user_message: &str,
        images: Vec<(String, String)>,
        system_reminder: Option<String>,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()> {
        self.ensure_provider_identity_ready().await?;
        let _account_admission =
            crate::server::acquire_account_reconciliation_admission_lock().await?;
        crate::session::with_account_transition_admission(self.run_once_streaming_mpsc_admitted(
            user_message,
            images,
            system_reminder,
            event_tx,
        ))
        .await
    }

    async fn run_once_streaming_mpsc_admitted(
        &mut self,
        user_message: &str,
        images: Vec<(String, String)>,
        system_reminder: Option<String>,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()> {
        self.ensure_provider_identity_ready_under_admission()
            .await?;
        self.rewind_undo_snapshot = None;
        // Inject any pending notifications before the user message
        let alerts = self.take_alerts();
        if !alerts.is_empty() {
            let alert_text = format!(
                "[NOTIFICATION]\nYou received {} notification(s) from other agents working in this codebase:\n\n{}\n\nUse the communicate tool to coordinate with other agents (prefer dm; broadcast reaches only your spawned subtree).",
                alerts.len(),
                alerts.join("\n\n---\n\n")
            );
            self.add_message(
                Role::User,
                vec![ContentBlock::Text {
                    text: alert_text,
                    cache_control: None,
                }],
            );
        }

        self.current_turn_system_reminder =
            system_reminder.filter(|value| !value.trim().is_empty());

        let mut blocks: Vec<ContentBlock> = images
            .into_iter()
            .map(|(media_type, data)| ContentBlock::Image { media_type, data })
            .collect();
        blocks.push(ContentBlock::Text {
            text: user_message.to_string(),
            cache_control: None,
        });

        if blocks.len() > 1 {
            crate::logging::info(&format!(
                "Agent received message with {} image(s)",
                blocks.len() - 1
            ));
        }

        self.add_message(Role::User, blocks);
        crate::telemetry::record_turn();
        self.session.save()?;
        let turn_started_at = Instant::now();
        let start_message_index = self.message_count();
        self.fire_turn_start_hook("chat");
        let result = self.run_turn_streaming_mpsc(event_tx).await;
        self.current_turn_system_reminder = None;
        self.fire_turn_end_hook(&result, turn_started_at, start_message_index);
        result
    }

    /// Fire the `turn_start` observer hook when a turn begins, before the model
    /// starts generating (and before the first `pre_tool`). This lets external
    /// integrations (terminal multiplexers, status bars) detect that the agent
    /// is actively working during the otherwise-invisible window between prompt
    /// submission and the first tool call. No-op (without building the payload)
    /// when the hook is not configured.
    fn fire_turn_start_hook(&self, source: &str) {
        if !crate::hooks::hook_configured("turn_start") {
            return;
        }
        let mut event = crate::hooks::HookEvent::new("turn_start")
            .session_id(self.session.id.clone())
            .field("MODEL", self.provider_model())
            .field("SOURCE", source.to_string());
        if let Some(cwd) = self.working_dir() {
            event = event.cwd(cwd);
        }
        crate::hooks::dispatch_observer(event);
    }

    /// Fire the `turn_end` observer hook with turn outcome metadata.
    /// No-op (without building the payload) when the hook is not configured.
    fn fire_turn_end_hook(
        &self,
        result: &Result<()>,
        started_at: Instant,
        start_message_index: usize,
    ) {
        if !crate::hooks::hook_configured("turn_end") {
            return;
        }
        let status = if result.is_ok() { "ok" } else { "error" };
        let mut event = crate::hooks::HookEvent::new("turn_end")
            .session_id(self.session.id.clone())
            .field("STATUS", status)
            .field("DURATION_MS", started_at.elapsed().as_millis().to_string())
            .field("MODEL", self.provider_model());
        if let Some(cwd) = self.working_dir() {
            event = event.cwd(cwd);
        }
        if let Some(text) = self.latest_assistant_text_after(start_message_index) {
            const LAST_TEXT_LIMIT: usize = 4000;
            let snippet: String = text.chars().take(LAST_TEXT_LIMIT).collect();
            event = event.field("LAST_ASSISTANT_TEXT", snippet);
        }
        if let Err(error) = result {
            const ERROR_LIMIT: usize = 1000;
            let message: String = error.to_string().chars().take(ERROR_LIMIT).collect();
            event = event.field("ERROR", message);
        }
        crate::hooks::dispatch_observer(event);
    }

    /// Clear conversation history
    pub fn clear(&mut self) -> Result<(), String> {
        self.clear_with_canary(None)
    }

    pub(crate) fn clear_with_canary(&mut self, force_canary: Option<&str>) -> Result<(), String> {
        self.clear_with_canary_session(force_canary, Session::create(None, None))
    }

    pub(crate) fn clear_with_canary_session(
        &mut self,
        force_canary: Option<&str>,
        mut new_session: Session,
    ) -> Result<(), String> {
        let old_session_id = self.session.id.clone();
        let preserve_canary = self.session.is_canary;
        let preserve_testing_build = self.session.testing_build.clone();
        let preserve_debug = self.session.is_debug;
        let preserve_working_dir = self.session.working_dir.clone();

        new_session.model = Some(self.provider.model());
        new_session.provider_key =
            crate::session::derive_session_provider_key(self.provider.name());
        new_session.is_canary = preserve_canary;
        new_session.testing_build = preserve_testing_build;
        if let Some(build_hash) = force_canary {
            new_session.set_canary(build_hash);
        }
        new_session.is_debug = preserve_debug;
        new_session.working_dir = preserve_working_dir;
        new_session.ensure_initial_session_context_message();

        // Prepare an inert durable replacement before closing the live
        // session. No PID marker or in-memory binding points at it yet.
        new_session.status = SessionStatus::Closed;
        new_session.last_pid = None;
        new_session
            .save()
            .map_err(|error| format!("Failed to create cleared session: {error}"))?;
        crate::session::begin_session_handoff(&self.session, &new_session)
            .map_err(|error| format!("Failed to prepare cleared-session handoff: {error}"))?;

        let now = chrono::Utc::now();
        let mut closed_old = self.session.clone();
        closed_old.status = SessionStatus::Closed;
        closed_old.last_active_at = Some(now);
        if let Err(error) = closed_old.save() {
            crate::session::finish_session_handoff(&old_session_id);
            return Err(format!("Failed to close cleared session: {error}"));
        }

        new_session.status = SessionStatus::Active;
        new_session.last_pid = Some(std::process::id());
        new_session.last_active_at = Some(now);
        if let Err(error) = new_session.save() {
            closed_old.status = SessionStatus::Active;
            closed_old.last_pid = Some(std::process::id());
            let rollback = closed_old.save();
            if rollback.is_ok() {
                self.session = closed_old;
                crate::session::finish_session_handoff(&old_session_id);
            } else {
                crate::storage::unregister_active_pid(&old_session_id);
                self.provider_identity_error = Some(format!(
                    "Cleared-session activation and source rollback both failed for {old_session_id}; startup handoff reconciliation is required"
                ));
            }
            return Err(format!(
                "Failed to activate cleared session: {error}; old-session rollback: {}",
                rollback
                    .err()
                    .map_or_else(|| "ok".to_string(), |rollback| rollback.to_string())
            ));
        }

        new_session.publish_active_presence();
        crate::storage::unregister_active_pid(&old_session_id);
        crate::session::finish_session_handoff(&old_session_id);
        self.session = new_session;
        self.provider_identity_error = None;
        self.reset_runtime_state_for_session_change();
        self.soft_interrupt_queue = Arc::new(std::sync::Mutex::new(Vec::new()));
        self.background_tool_signal = InterruptSignal::new();
        self.graceful_shutdown = InterruptSignal::new();
        crate::tool::clear_session_tool_policy(&old_session_id);
        crate::tool::set_session_tool_policy(
            &self.session.id,
            self.allowed_tools.clone(),
            self.disabled_tools.clone(),
        );
        self.provider_session_id = None;
        self.seed_compaction_from_session();
        Ok(())
    }

    /// Clear provider session so the next turn sends full context.
    pub fn reset_provider_session(&mut self) -> Result<()> {
        let mut candidate = self.session.clone();
        candidate.provider_session_id = None;
        candidate.provider_session_identity = None;
        candidate.save()?;
        self.session = candidate;
        self.provider_session_id = None;
        Ok(())
    }

    /// Rewind the conversation to a 1-based visible transcript message index.
    ///
    /// The index is interpreted against the same rendered transcript the TUI
    /// numbers in `/rewind` (user/assistant entries only, tool cards and
    /// system notices excluded). Mapping through raw stored messages instead
    /// would count tool-result messages the UI never numbers, sending
    /// `/rewind N` far earlier than the on-screen message N (issue #432).
    ///
    /// Provider-side resumable sessions are reset so the next request sends the
    /// truncated context from scratch instead of continuing from a stale upstream
    /// conversation.
    pub fn rewind_to_message(&mut self, message_index: usize) -> Result<usize, String> {
        let targets = self.session.rewind_target_stored_indices();
        let message_count = targets.len();
        if message_index == 0 || message_index > message_count {
            return Err(format!(
                "Invalid message number: {}. Valid range: 1-{}",
                message_index, message_count
            ));
        }
        let target_raw_index = targets[message_index - 1];

        let removed = message_count - message_index;
        let undo_snapshot = RewindUndoSnapshot {
            archived_message_ids: self.session.archived_message_ids.clone(),
            raw_message_count: self.session.messages.len(),
            compaction: self.session.compaction.clone(),
            context_graph: self.session.context_graph_state(),
            provider_session_id: self.provider_session_id.clone(),
            session_provider_session_id: self.session.provider_session_id.clone(),
            session_provider_session_identity: self.session.provider_session_identity.clone(),
            visible_message_count: message_count,
        };
        let mut candidate = self.session.clone();
        candidate
            .rewind_active_branch_through(target_raw_index)
            .map_err(|error| format!("Failed to update active rewind branch: {error}"))?;
        candidate.updated_at = chrono::Utc::now();
        candidate.provider_session_id = None;
        candidate.provider_session_identity = None;
        candidate
            .save()
            .map_err(|error| format!("Failed to persist conversation rewind: {error}"))?;
        self.rewind_undo_snapshot = Some(undo_snapshot);
        self.session = candidate;
        self.provider_session_id = None;
        self.cache_tracker.reset();
        self.locked_tools = None;
        self.reset_tool_output_tracking();
        self.seed_compaction_from_session();
        Ok(removed)
    }

    pub fn undo_rewind(&mut self) -> Result<usize, String> {
        let Some(snapshot) = self.rewind_undo_snapshot.clone() else {
            return Err("No rewind to undo.".to_string());
        };

        let current_count = self.session.rewind_target_count();
        let restored = snapshot.visible_message_count.saturating_sub(current_count);
        let mut candidate = self.session.clone();
        candidate.restore_active_branch(snapshot.archived_message_ids, snapshot.raw_message_count);
        candidate.compaction = snapshot.compaction;
        candidate.restore_context_graph_state(snapshot.context_graph);
        candidate.provider_session_id = snapshot.session_provider_session_id;
        candidate.provider_session_identity = snapshot.session_provider_session_identity;
        candidate.updated_at = chrono::Utc::now();
        candidate
            .save()
            .map_err(|error| format!("Failed to persist conversation rewind undo: {error}"))?;
        self.rewind_undo_snapshot = None;
        self.session = candidate;
        self.provider_session_id = snapshot.provider_session_id;
        self.cache_tracker.reset();
        self.locked_tools = None;
        self.reset_tool_output_tracking();
        self.seed_compaction_from_session();
        Ok(restored)
    }

    /// Unlock the tool list so the next API request picks up any new tools.
    /// Called after MCP reload or when the user explicitly wants new tools.
    pub fn unlock_tools(&mut self) {
        if self.locked_tools.is_some() {
            logging::info("Tool list unlocked — next request will pick up current tools");
            self.locked_tools = None;
            self.cache_tracker.reset();
        }
        // Allow the late-MCP-registration recheck to fire once for the next
        // snapshot (e.g. after an explicit `mcp` reload).
        self.mcp_late_register_resolved = false;
    }

    /// Unlock tools if a tool execution may have changed the registry
    /// (e.g., mcp connect/disconnect/reload)
    pub(super) fn unlock_tools_if_needed(&mut self, tool_name: &str) {
        if tool_name == "mcp" {
            self.unlock_tools();
        }
    }

    pub fn is_canary(&self) -> bool {
        self.session.is_canary
    }

    pub fn is_debug(&self) -> bool {
        self.session.is_debug
    }

    pub fn set_canary(&mut self, build_hash: &str) {
        self.session.set_canary(build_hash);
        if let Err(err) = self.session.save() {
            logging::error(&format!("Failed to persist canary session state: {}", err));
        }
    }

    /// Mark this session as a debug/test session
    /// Set a custom system prompt override (used by ambient mode).
    /// When set, this replaces the normal system prompt entirely.
    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.system_prompt_override = Some(prompt.to_string());
    }

    pub fn set_debug(&mut self, is_debug: bool) {
        self.session.set_debug(is_debug);
        if let Err(err) = self.session.save() {
            logging::error(&format!("Failed to persist debug session state: {}", err));
        }
    }

    /// Enable or disable memory features for this session.
    pub fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if !enabled {
            crate::memory::clear_pending_memory(&self.session.id);
        }
    }

    /// Mark this session as an inline swarm worker. When enabled, the streaming
    /// loop publishes a throttled output tail to the global bus so a
    /// coordinator can render a live inline gallery viewport for it.
    pub fn set_inline_output_tap(&mut self, enabled: bool) {
        self.inline_output_tap = enabled;
    }

    /// Whether this session streams an inline output tail to the bus.
    pub(crate) fn inline_output_tap(&self) -> bool {
        self.inline_output_tap
    }

    /// Publish the current rolling activity tail to the bus for the
    /// coordinator's inline gallery. No-op unless the inline tap is enabled.
    pub(crate) fn publish_inline_tail(&self) {
        if !self.inline_output_tap {
            return;
        }
        crate::bus::Bus::global().publish(crate::bus::BusEvent::SwarmOutputTail(
            crate::bus::SwarmOutputTail {
                session_id: self.session.id.clone(),
                tail: self.inline_tail.render(),
            },
        ));
    }

    /// Check whether memory features are enabled for this session.
    pub fn memory_enabled(&self) -> bool {
        self.memory_enabled
    }

    /// Set the stdin request channel for interactive stdin forwarding
    pub fn set_stdin_request_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::tool::StdinInputRequest>,
    ) {
        self.stdin_request_tx = Some(tx);
    }

    pub(super) async fn tool_definitions(&mut self) -> Vec<ToolDefinition> {
        if self.session.is_canary {
            self.registry.register_selfdev_tools().await;
        }

        // Return locked tools if available (prevents cache invalidation from
        // tools arriving asynchronously after the first API request).
        //
        // Exception: MCP servers connect on a background task and register
        // `mcp__*` tools seconds after the session starts — typically *after*
        // the first turn has already locked the snapshot. We deliberately do
        // NOT block the first turn on MCP connection: servers can be slow or
        // hang, and we want the user to be able to talk to the agent the moment
        // the session spawns. The price is that the first locked snapshot is
        // missing MCP tools, and the only other unlock path fires when the model
        // calls the `mcp` management tool — which it cannot do without first
        // seeing MCP tools (#206).
        //
        // So, exactly once per locked snapshot, if MCP tools have since appeared
        // in the registry, we rebuild. This is a single intentional provider
        // prompt-cache miss (the turn MCP tools first appear). The
        // `mcp_late_register_resolved` flag makes this a one-shot check so we do
        // not rescan the registry on every subsequent turn.
        if let Some(ref locked) = self.locked_tools {
            if self.mcp_late_register_resolved {
                return locked.clone();
            }
            if self.registry_has_new_mcp_tools(locked).await {
                logging::info(
                    "MCP tools registered after first turn locked the tool snapshot — \
                     rebuilding once to expose them. This is one intentional prompt-cache \
                     miss; we accept it so the agent is reachable immediately at spawn \
                     instead of blocking on MCP connection (#206).",
                );
                // Latch the one-shot guard and drop the stale snapshot directly.
                // We intentionally do NOT call `unlock_tools()` here, because that
                // re-arms the guard (it is the explicit-reload path) and would let
                // the recheck fire again on every later turn.
                self.mcp_late_register_resolved = true;
                self.locked_tools = None;
                self.cache_tracker.reset();
            } else {
                // No MCP tools have appeared. They may still be connecting, so
                // leave the guard unset and re-check on the next turn. Once they
                // appear (or never do, after the registry settles) we stop.
                return locked.clone();
            }
        }

        let tools = self.build_filtered_tool_definitions().await;

        // Lock the tool list to prevent cache invalidation when more tools
        // arrive asynchronously mid-session.
        logging::info(&format!(
            "Locking tool list at {} tools for cache stability",
            tools.len()
        ));
        self.locked_tools = Some(tools.clone());
        tools
    }

    /// Build the agent's tool definitions from the registry, applying the
    /// session's `allowed_tools`, `disabled_tools`, and self-dev filters.
    async fn build_filtered_tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = self.registry.definitions(self.allowed_tools.as_ref()).await;
        if !self.disabled_tools.is_empty() {
            tools.retain(|tool| !self.disabled_tools.contains(&tool.name));
        }
        Self::apply_selfdev_tool_surface(&mut tools, self.session.is_canary);
        tools
    }

    /// Tailor the `selfdev` tool definition to the session mode.
    ///
    /// The registry stores a single shared `selfdev` tool with a default
    /// (non-self-dev) schema. Self-dev sessions get the full build/test/reload
    /// surface; every other session keeps the lightweight on-ramp surface
    /// (`enter`, `setup`, `reload`, `status`, `find-config`). The tool stays
    /// available in all sessions so the agent can always enter self-dev mode.
    fn apply_selfdev_tool_surface(tools: &mut [ToolDefinition], is_canary: bool) {
        for tool in tools.iter_mut() {
            if tool.name == "selfdev" {
                tool.description =
                    crate::tool::selfdev::SelfDevTool::description_for(is_canary).to_string();
                tool.input_schema = crate::tool::selfdev::SelfDevTool::schema_for(is_canary);
            }
        }
    }

    /// Returns true if the registry contains `mcp__*` tools (subject to the
    /// session's `allowed_tools` filter) that are not present in the currently
    /// locked snapshot. Used to detect the async MCP-registration race (#206).
    async fn registry_has_new_mcp_tools(&self, locked: &[ToolDefinition]) -> bool {
        let registry_names = self.registry.tool_names().await;
        let allowed = self.allowed_tools.as_ref();
        registry_names.iter().any(|name| {
            name.starts_with("mcp__")
                && allowed.map(|set| set.contains(name)).unwrap_or(true)
                && !self.disabled_tools.contains(name)
                && !locked.iter().any(|t| &t.name == name)
        })
    }

    pub async fn tool_names(&self) -> Vec<String> {
        self.tool_definitions_for_debug()
            .await
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    /// Get full tool definitions for debug introspection (bypasses lock)
    pub async fn tool_definitions_for_debug(&self) -> Vec<crate::message::ToolDefinition> {
        if self.session.is_canary {
            self.registry.register_selfdev_tools().await;
        }
        let mut tools = self.registry.definitions(self.allowed_tools.as_ref()).await;
        if !self.disabled_tools.is_empty() {
            tools.retain(|tool| !self.disabled_tools.contains(&tool.name));
        }
        Self::apply_selfdev_tool_surface(&mut tools, self.session.is_canary);
        tools
    }

    pub async fn execute_tool(
        &self,
        name: &str,
        input: serde_json::Value,
    ) -> Result<crate::tool::ToolOutput> {
        self.ensure_tool_identity_ready()?;
        self.validate_tool_allowed(name)?;

        let call_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| format!("debug-{}", d.as_millis()))
            .unwrap_or_else(|_| "debug".to_string());
        let ctx = ToolContext {
            session_id: self.session.id.clone(),
            message_id: self.session.id.clone(),
            tool_call_id: call_id,
            working_dir: self.working_dir().map(PathBuf::from),
            stdin_request_tx: self.stdin_request_tx.clone(),
            graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
            execution_mode: ToolExecutionMode::Direct,
        };
        self.registry.execute(name, input, ctx).await
    }

    pub fn add_manual_tool_use(
        &mut self,
        tool_call_id: String,
        tool_name: String,
        input: serde_json::Value,
    ) -> Result<String> {
        let message_id = self.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: tool_call_id,
                name: tool_name,
                input,
                thought_signature: None,
            }],
        );
        self.session.save()?;
        Ok(message_id)
    }

    pub fn add_manual_tool_result(
        &mut self,
        tool_call_id: String,
        output: crate::tool::ToolOutput,
        duration_ms: u64,
    ) -> Result<()> {
        let blocks = tool_output_to_content_blocks(tool_call_id, output);
        self.add_message_with_duration(Role::User, blocks, Some(duration_ms));
        self.session.save()?;
        Ok(())
    }

    pub fn add_manual_tool_error(
        &mut self,
        tool_call_id: String,
        error: String,
        duration_ms: u64,
    ) -> Result<()> {
        self.add_message_with_duration(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: tool_call_id,
                content: error,
                is_error: Some(true),
            }],
            Some(duration_ms),
        );
        self.session.save()?;
        Ok(())
    }

    pub(super) fn validate_tool_allowed(&self, name: &str) -> Result<()> {
        if let Some(allowed) = self.allowed_tools.as_ref()
            && !allowed.contains(name)
        {
            return Err(anyhow::anyhow!("Tool '{}' is not allowed", name));
        }
        if self.disabled_tools.contains(name) {
            return Err(anyhow::anyhow!("Tool '{}' is disabled", name));
        }
        Ok(())
    }
}
