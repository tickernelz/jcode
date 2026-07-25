use super::*;

impl Agent {
    pub fn restore_session(&mut self, session_id: &str) -> Result<SessionStatus> {
        self.restore_session_with_working_dir(session_id, None)
    }

    pub(crate) fn restore_session_with_working_dir(
        &mut self,
        session_id: &str,
        working_dir: Option<&str>,
    ) -> Result<SessionStatus> {
        let restore_start = Instant::now();
        let load_start = Instant::now();
        let mut session = Session::load(session_id)?;
        if let Some(working_dir) = working_dir {
            session.working_dir = Some(working_dir.to_string());
            session.refresh_initial_session_context_message();
        }
        let load_ms = load_start.elapsed().as_millis();
        logging::info(&format!(
            "Restoring session '{}' with {} messages, provider_session_id: {:?}, status: {}",
            session_id,
            session.messages.len(),
            session.provider_session_id,
            session.status.display()
        ));
        let previous_status = session.status.clone();
        let previous_session_id = self.session.id.clone();
        let previous_model = self
            .session
            .model
            .clone()
            .unwrap_or_else(|| self.provider.model());
        let previous_route_request =
            crate::provider::MultiProvider::model_switch_request_for_session_route(
                &previous_model,
                self.session.provider_key.as_deref(),
                self.session.route_api_method.as_deref(),
            );
        let previous_effort = self.provider.reasoning_effort();
        let model_start = Instant::now();
        if let Some(model) = session.model.clone() {
            let model_request =
                crate::provider::MultiProvider::model_switch_request_for_session_route(
                    &model,
                    session.provider_key.as_deref(),
                    session.route_api_method.as_deref(),
                );
            if let Err(error) =
                crate::provider::set_model_with_auth_refresh(self.provider.as_ref(), &model_request)
            {
                let rollback = restore_provider_identity(
                    self.provider.as_ref(),
                    &previous_route_request,
                    previous_effort.as_deref(),
                );
                let rollback = rollback.map_or_else(
                    |rollback| {
                        let message = rollback.to_string();
                        self.provider_identity_error = Some(message.clone());
                        message
                    },
                    |()| "ok".to_string(),
                );
                return Err(anyhow::anyhow!(
                    "Failed to restore exact session model '{model}' via '{model_request}': {error}; provider rollback: {rollback}"
                ));
            }
        } else {
            session.model = Some(self.provider.model());
        }
        let target_effort = session.reasoning_effort.as_deref().unwrap_or("");
        let should_apply_effort = session.reasoning_effort.is_some()
            || previous_effort.is_some()
            || !self.provider.available_efforts().is_empty();
        if should_apply_effort && let Err(error) = self.provider.set_reasoning_effort(target_effort)
        {
            let rollback = restore_provider_identity(
                self.provider.as_ref(),
                &previous_route_request,
                previous_effort.as_deref(),
            );
            let rollback = rollback.map_or_else(
                |rollback| {
                    let message = rollback.to_string();
                    self.provider_identity_error = Some(message.clone());
                    message
                },
                |()| "ok".to_string(),
            );
            return Err(anyhow::anyhow!(
                "Failed to restore reasoning effort '{target_effort}': {error}; provider rollback: {}",
                rollback
            ));
        }
        let effective_identity = self.provider.exact_runtime_identity();
        let identity_error = match (
            session.exact_runtime_identity.as_ref(),
            effective_identity.as_ref(),
        ) {
            (Some(expected), Some(actual)) if expected != actual => Some(format!(
                "exact runtime identity mismatch: expected {expected:?}, effective {actual:?}"
            )),
            (Some(_), None) => Some(format!(
                "provider {} cannot verify the persisted exact runtime identity",
                self.provider.name()
            )),
            _ => None,
        };
        if let Some(identity_error) = identity_error {
            let rollback = restore_provider_identity(
                self.provider.as_ref(),
                &previous_route_request,
                previous_effort.as_deref(),
            )
            .map_or_else(
                |rollback_error| {
                    let message = rollback_error.to_string();
                    self.provider_identity_error = Some(message.clone());
                    message
                },
                |()| "ok".to_string(),
            );
            return Err(anyhow::anyhow!(
                "Failed to restore session {session_id}: {identity_error}; provider rollback: {rollback}"
            ));
        }
        if session.exact_runtime_identity.is_none() {
            // Missing identity cannot prove that old resume or compaction state
            // belongs to the account now active. Enter the new ownership domain
            // only after discarding every unowned derived projection.
            session.provider_session_id = None;
            session.provider_session_identity = None;
            session.reset_context_graph_for_identity_transition();
            session.exact_runtime_identity = effective_identity.clone();
        }
        if session.provider_session_id.is_some()
            && (!effective_identity
                .as_ref()
                .is_some_and(|identity| identity.has_verifiable_account_binding())
                || session.provider_session_identity != effective_identity)
        {
            logging::warn(&format!(
                "Discarding stale or unbound provider session id while restoring {session_id}"
            ));
            session.provider_session_id = None;
            session.provider_session_identity = None;
        }
        let model_ms = model_start.elapsed().as_millis();

        let mut closed_previous = None;
        if previous_session_id != session.id {
            crate::session::begin_session_handoff(&self.session, &session)?;
            let mut candidate = self.session.clone();
            candidate.status = SessionStatus::Closed;
            candidate.last_active_at = Some(chrono::Utc::now());
            if let Err(error) = candidate.save() {
                let rollback = restore_provider_identity(
                    self.provider.as_ref(),
                    &previous_route_request,
                    previous_effort.as_deref(),
                );
                let rollback = rollback.map_or_else(
                    |rollback| {
                        let message = rollback.to_string();
                        self.provider_identity_error = Some(message.clone());
                        message
                    },
                    |()| "ok".to_string(),
                );
                crate::session::finish_session_handoff(&previous_session_id);
                return Err(anyhow::anyhow!(
                    "Failed to close previous session before restore: {error}; provider rollback: {}",
                    rollback
                ));
            }
            closed_previous = Some(candidate);
        }

        let mark_active_start = Instant::now();
        session.status = SessionStatus::Active;
        session.last_pid = Some(std::process::id());
        session.last_active_at = Some(chrono::Utc::now());
        let mark_active_ms = mark_active_start.elapsed().as_millis();

        let save_start = Instant::now();
        if let Err(error) = session.save() {
            let mut previous_rollback_failed = false;
            let rollback = if let Some(mut previous) = closed_previous {
                previous.status = self.session.status.clone();
                previous.last_pid = self.session.last_pid;
                previous.last_active_at = self.session.last_active_at;
                match previous.save() {
                    Ok(()) => {
                        self.session = previous;
                        "ok".to_string()
                    }
                    Err(rollback_error) => {
                        previous_rollback_failed = true;
                        crate::storage::unregister_active_pid(&previous_session_id);
                        rollback_error.to_string()
                    }
                }
            } else {
                "not needed".to_string()
            };
            let provider_rollback = restore_provider_identity(
                self.provider.as_ref(),
                &previous_route_request,
                previous_effort.as_deref(),
            );
            let provider_rollback = provider_rollback.map_or_else(
                |rollback| {
                    let message = rollback.to_string();
                    self.provider_identity_error = Some(message.clone());
                    message
                },
                |()| "ok".to_string(),
            );
            if previous_rollback_failed {
                self.provider_identity_error = Some(format!(
                    "Restored-session activation and previous-session rollback both failed for {previous_session_id}; startup handoff reconciliation is required"
                ));
            } else {
                crate::session::finish_session_handoff(&previous_session_id);
            }
            return Err(anyhow::anyhow!(
                "Failed to activate restored session: {error}; previous-session rollback: {rollback}; provider rollback: {}",
                provider_rollback
            ));
        }
        let save_ms = save_start.elapsed().as_millis();
        let assign_start = Instant::now();
        self.provider_session_id = session.provider_session_id.clone();
        self.session = session;
        self.provider_identity_error = None;
        crate::storage::unregister_active_pid(&previous_session_id);
        self.session.publish_active_presence();
        crate::session::finish_session_handoff(&previous_session_id);
        crate::tool::clear_session_tool_policy(&previous_session_id);
        crate::tool::set_session_tool_policy(
            &self.session.id,
            self.allowed_tools.clone(),
            self.disabled_tools.clone(),
        );
        let assign_ms = assign_start.elapsed().as_millis();

        let reset_start = Instant::now();
        self.reset_runtime_state_for_session_change();
        let restored_soft_interrupts = self.restore_persisted_soft_interrupts();
        let reset_ms = reset_start.elapsed().as_millis();
        crate::session_effort::record_session_effort(
            &self.session.id,
            self.session.reasoning_effort.as_deref(),
        );
        self.sync_memory_dedup_state_from_session();

        logging::info(&format!(
            "restore_session: loaded session {} with {} messages, calling seed_compaction",
            session_id,
            self.session.messages.len()
        ));
        let compaction_start = Instant::now();
        self.seed_compaction_from_session();
        let compaction_ms = compaction_start.elapsed().as_millis();

        let env_snapshot_start = Instant::now();
        self.log_env_snapshot("resume");
        let env_snapshot_ms = env_snapshot_start.elapsed().as_millis();
        self.fire_session_lifecycle_hook("session_start", "resume");

        logging::info(&format!(
            "[TIMING] restore_session: session={}, messages={}, restored_soft_interrupts={}, load={}ms, assign={}ms, reset={}ms, model={}ms, mark_active={}ms, compaction={}ms, env_snapshot={}ms, save={}ms, total={}ms",
            session_id,
            self.session.messages.len(),
            restored_soft_interrupts,
            load_ms,
            assign_ms,
            reset_ms,
            model_ms,
            mark_active_ms,
            compaction_ms,
            env_snapshot_ms,
            save_ms,
            restore_start.elapsed().as_millis(),
        ));
        logging::info(&format!(
            "Session restored: {} messages in session",
            self.session.messages.len()
        ));
        Ok(previous_status)
    }

    /// Get conversation history for sync
    pub fn get_history(&self) -> Vec<HistoryMessage> {
        crate::session::render_messages(&self.session)
            .into_iter()
            .map(|msg| HistoryMessage {
                role: msg.role,
                content: msg.content,
                tool_calls: if msg.tool_calls.is_empty() {
                    None
                } else {
                    Some(msg.tool_calls)
                },
                tool_data: msg.tool_data,
            })
            .collect()
    }

    pub fn get_history_and_rendered_images(
        &self,
    ) -> (Vec<HistoryMessage>, Vec<crate::session::RenderedImage>) {
        let (messages, images) = crate::session::render_messages_and_images(&self.session);
        let history = messages
            .into_iter()
            .map(|msg| HistoryMessage {
                role: msg.role,
                content: msg.content,
                tool_calls: if msg.tool_calls.is_empty() {
                    None
                } else {
                    Some(msg.tool_calls)
                },
                tool_data: msg.tool_data,
            })
            .collect();
        (history, images)
    }

    pub fn get_history_and_rendered_images_with_compacted_history(
        &self,
        compacted_history_visible: usize,
    ) -> (
        Vec<HistoryMessage>,
        Vec<crate::session::RenderedImage>,
        Option<crate::session::RenderedCompactedHistoryInfo>,
    ) {
        let (messages, images, compacted_info) =
            crate::session::render_messages_and_images_with_compacted_history(
                &self.session,
                compacted_history_visible,
            );
        let history = messages
            .into_iter()
            .map(|msg| HistoryMessage {
                role: msg.role,
                content: msg.content,
                tool_calls: if msg.tool_calls.is_empty() {
                    None
                } else {
                    Some(msg.tool_calls)
                },
                tool_data: msg.tool_data,
            })
            .collect();
        (history, images, compacted_info)
    }

    pub fn get_tool_call_summaries(&self, limit: usize) -> Vec<crate::protocol::ToolCallSummary> {
        crate::session::summarize_tool_calls(&self.session, limit)
    }

    /// Start an interactive REPL
    pub async fn repl(&mut self) -> Result<()> {
        println!("J-Code - Coding Agent");
        println!("Type your message, or 'quit' to exit.");

        // Show available skills
        let skills = self.current_skills_snapshot();
        let skill_list = skills.list();
        if !skill_list.is_empty() {
            println!(
                "Available skills: {}",
                skill_list
                    .iter()
                    .map(|s| format!("/{}", s.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!();

        loop {
            print!("> ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            let input = input.trim();
            if input.is_empty() {
                continue;
            }

            if input == "quit" || input == "exit" {
                break;
            }

            if input == "clear" {
                match self.clear() {
                    Ok(()) => println!("Conversation cleared."),
                    Err(error) => eprintln!("Failed to clear conversation: {error}"),
                }
                continue;
            }

            // Check for skill invocation
            if let Some(invocation) = SkillRegistry::parse_invocation(input) {
                if let Some(skill) = skills.get(invocation.name) {
                    println!("Activating skill: {}", skill.name);
                    println!("{}\n", skill.description);
                    self.active_skill = Some(invocation.name.to_string());
                    if let Some(prompt) = invocation.prompt {
                        if let Err(e) = self.run_once(prompt).await {
                            eprintln!("\nError: {}\n", e);
                        }
                        println!();
                    }
                    continue;
                } else {
                    println!("Unknown skill: /{}", invocation.name);
                    println!(
                        "Available: {}",
                        skills
                            .list()
                            .iter()
                            .map(|s| format!("/{}", s.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    continue;
                }
            }

            if let Err(e) = self.run_once(input).await {
                eprintln!("\nError: {}\n", e);
            }

            println!();
        }

        // Extract memories from session before exiting
        self.extract_session_memories().await;

        Ok(())
    }

    /// Extract memories from the session transcript
    /// Returns the number of memories extracted, or 0 if none/skipped
    pub async fn extract_session_memories(&self) -> usize {
        if !self.memory_enabled {
            return 0;
        }

        let active_messages = self.active_messages();
        // Need at least 4 active messages for meaningful extraction.
        if active_messages.len() < 4 {
            return 0;
        }

        logging::info(&format!(
            "Extracting memories from {} messages",
            active_messages.len()
        ));

        // Build transcript
        let mut transcript = String::new();
        for msg in active_messages {
            let role = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            };
            transcript.push_str(&format!("**{}:**\n", role));
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text, .. } => {
                        transcript.push_str(text);
                        transcript.push('\n');
                    }
                    ContentBlock::ToolUse { name, .. } => {
                        transcript.push_str(&format!("[Used tool: {}]\n", name));
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        let preview = if content.len() > 200 {
                            format!("{}...", crate::util::truncate_str(content, 200))
                        } else {
                            content.clone()
                        };
                        transcript.push_str(&format!("[Result: {}]\n", preview));
                    }
                    ContentBlock::Reasoning { .. }
                    | ContentBlock::ReasoningTrace { .. }
                    | ContentBlock::AnthropicThinking { .. }
                    | ContentBlock::OpenAIReasoning { .. } => {}
                    ContentBlock::Image { .. } => {
                        transcript.push_str("[Image]\n");
                    }
                    ContentBlock::OpenAICompaction { .. } => {
                        transcript.push_str("[OpenAI native compaction]\n");
                    }
                }
            }
            transcript.push('\n');
        }

        if !crate::memory::memory_llm_judge_available() {
            logging::info("Memory extraction skipped: LLM judge unavailable");
            return 0;
        }

        // Extract using sidecar
        let sidecar = crate::sidecar::Sidecar::new();
        match sidecar.extract_memories(&transcript).await {
            Ok(extracted) if !extracted.is_empty() => {
                let manager = self
                    .session
                    .working_dir
                    .as_deref()
                    .map(|dir| crate::memory::MemoryManager::new().with_project_dir(dir))
                    .unwrap_or_default();
                let mut stored_count = 0;

                for memory in &extracted {
                    let category = crate::memory::MemoryCategory::from_extracted(&memory.category);

                    let trust = match memory.trust.as_str() {
                        "high" => crate::memory::TrustLevel::High,
                        "low" => crate::memory::TrustLevel::Low,
                        _ => crate::memory::TrustLevel::Medium,
                    };

                    let entry = crate::memory::MemoryEntry::new(category, &memory.content)
                        .with_source(&self.session.id)
                        .with_trust(trust);

                    if manager.remember_project(entry).is_ok() {
                        stored_count += 1;
                    }
                }

                if stored_count > 0 {
                    logging::info(&format!("Extracted {} memories from session", stored_count));
                }
                stored_count
            }
            Ok(_) => 0,
            Err(e) => {
                logging::info(&format!("Memory extraction skipped: {}", e));
                0
            }
        }
    }
}
