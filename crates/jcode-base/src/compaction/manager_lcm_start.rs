use super::*;

impl CompactionManager {
    /// Start background compaction if needed
    pub fn maybe_start_compaction_with(
        &mut self,
        all_messages: &[Message],
        provider: Arc<dyn Provider>,
    ) {
        if !provider
            .exact_runtime_identity()
            .as_ref()
            .is_some_and(|identity| identity.has_verifiable_account_binding())
        {
            crate::logging::error(
                "Compaction not started because effective provider account identity is opaque or incomplete",
            );
            return;
        }
        if !self.should_compact_with(all_messages) {
            return;
        }

        let active = self.active_messages(all_messages);

        // Calculate cutoff within active messages.
        // Semantic mode uses relevance scoring; other modes use recency.
        let mut cutoff = match self.mode {
            crate::config::CompactionMode::Semantic => self.semantic_cutoff(active),
            _ => active.len().saturating_sub(RECENT_TURNS_TO_KEEP),
        };
        if cutoff == 0 {
            return;
        }

        // Adjust cutoff to not split tool call/result pairs
        cutoff = safe_compaction_cutoff(active, cutoff);
        if cutoff == 0 {
            return;
        }

        // Snapshot messages to summarize (must clone for the async task)
        let messages_to_summarize: Vec<Message> = active[..cutoff].to_vec();
        let msg_count = messages_to_summarize.len();
        let existing_summary = self.active_summary.clone();
        let mode_label = self.mode_trigger_label().to_string();
        let estimated_tokens = self.effective_token_count_with(all_messages);
        crate::logging::info(&format!(
            "[TIMING] compaction_start: trigger={}, active_messages={}, cutoff={}, estimated_tokens={}, has_existing_summary={}",
            mode_label,
            active.len(),
            cutoff,
            estimated_tokens,
            existing_summary.is_some(),
        ));

        self.pending_cutoff = cutoff;
        self.pending_source_fingerprint = message_fingerprint(&active[..cutoff]);
        self.pending_trigger = Some(mode_label.clone());

        // Spawn background task that notifies via Bus when done
        self.pending_task = Some(tokio::spawn(async move {
            let start = std::time::Instant::now();
            let result =
                generate_compaction_artifact(provider, messages_to_summarize, existing_summary)
                    .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            crate::logging::info(&format!(
                "Compaction ({}) finished in {:.2}s ({} messages summarized)",
                mode_label,
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
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "captures one immutable LCM source identity tuple"
    )]
    pub(super) fn capture_lcm_source(
        &self,
        session: &crate::session::Session,
        cutoff: usize,
        summarizer_model: String,
        summarizer_provider: String,
        summarizer_route: String,
        configured_model: Option<String>,
        pre_tokens: u64,
        trigger: String,
    ) -> Result<PendingLcmSource> {
        let route_policy_fingerprint =
            lcm_route_policy_fingerprint(session, configured_model.as_deref());
        let source_runtime_identity_sha256 = crate::session::exact_runtime_identity_sha256(
            session
                .exact_runtime_identity
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("LCM source runtime identity is missing"))?,
        )?;
        let messages = session.active_stored_messages();
        let covered_message_count = self.compacted_count.saturating_add(cutoff);
        if covered_message_count == 0 || covered_message_count > messages.len() {
            anyhow::bail!("LCM cutoff does not map to canonical session history");
        }
        let prior_frontier = session.context_frontier.as_ref();
        let source_start = prior_frontier
            .map_or(0, |frontier| frontier.covered_message_count)
            .min(covered_message_count);
        let source = &messages[source_start..covered_message_count];
        let source_prefix = &messages[..covered_message_count];
        if source.is_empty() {
            anyhow::bail!("LCM leaf source is empty");
        }
        let prior_active_nodes = prior_frontier
            .into_iter()
            .flat_map(|frontier| frontier.active_node_ids.iter())
            .map(|id| {
                session
                    .context_nodes
                    .iter()
                    .find(|node| node.id == *id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("LCM frontier node {id} is missing"))
            })
            .collect::<Result<Vec<_>>>()?;
        let current_generation = session
            .context_frontier
            .as_ref()
            .map_or(0, |frontier| frontier.generation);
        let covered_through_message_id = source
            .last()
            .map(|message| message.id.clone())
            .ok_or_else(|| anyhow::anyhow!("LCM covered prefix must not be empty"))?;
        Ok(PendingLcmSource {
            session_id: session.id.clone(),
            base_generation: current_generation,
            generation: current_generation.saturating_add(1),
            next_node_sequence: session
                .context_frontier
                .as_ref()
                .map_or(1, |frontier| frontier.next_node_sequence),
            covered_message_count,
            source_message_ids: source.iter().map(|message| message.id.clone()).collect(),
            input_proof_ids: source.iter().map(|message| message.id.clone()).collect(),
            source_sha256: stored_message_prefix_sha256(source)?,
            source_runtime_identity_sha256: source_runtime_identity_sha256.clone(),
            source_prefix_sha256: stored_message_prefix_sha256(source_prefix)?,
            covered_through_message_id,
            prior_active_nodes,
            node_level: 0,
            child_nodes: Vec::new(),
            summarizer_model,
            summarizer_provider,
            summarizer_route,
            // Replaced by the production dispatcher with the resolved
            // compactor identity. The source identity is a deterministic
            // default for direct preparation tests.
            summarizer_runtime_identity_sha256: source_runtime_identity_sha256,
            configured_model,
            cutoff,
            source_fingerprint: message_fingerprint(
                &messages[source_start..covered_message_count]
                    .iter()
                    .map(crate::session::StoredMessage::to_message)
                    .collect::<Vec<_>>(),
            ),
            pre_tokens,
            trigger,
            route_policy_fingerprint,
            attempt_terminal_emitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    pub(super) fn lcm_provider_for_session(
        &self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> Result<LcmProviderResolution> {
        let configured_model = crate::config::config().compaction.model.clone();
        self.lcm_provider_for_session_with_model(session, provider, configured_model)
    }

    pub(super) fn lcm_provider_for_session_with_model(
        &self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
        configured_model: Option<String>,
    ) -> Result<LcmProviderResolution> {
        if !session
            .exact_runtime_identity
            .as_ref()
            .is_some_and(|identity| identity.has_verifiable_account_binding())
        {
            anyhow::bail!(
                "LCM is disabled because the session credential identity is opaque or incomplete"
            );
        }
        let compactor = provider.fork();
        let inherited_model = session.model.clone().unwrap_or_else(|| provider.model());
        let route = lcm_route_spec(session, &inherited_model, configured_model.as_deref());
        if configured_model.is_some() {
            crate::provider::set_model_with_auth_refresh(compactor.as_ref(), &route)?;
        } else if let Some(selection) = lcm_inherited_route_selection(session, &inherited_model) {
            set_lcm_inherited_route(compactor.as_ref(), &selection)?;
        } else {
            crate::provider::set_model_with_auth_refresh(compactor.as_ref(), &route)?;
        }
        if !compactor
            .exact_runtime_identity()
            .as_ref()
            .is_some_and(|identity| identity.has_verifiable_account_binding())
        {
            anyhow::bail!(
                "LCM is disabled because the effective compactor credential identity is opaque or incomplete"
            );
        }
        let model = compactor.model();
        let provider_name = compactor.name().to_string();
        Ok((compactor, model, provider_name, route, configured_model))
    }

    /// Fork and route a provider exactly as native LCM would for this session.
    /// Lifecycle callers use this to avoid silently reverting to the active chat
    /// route or provider-native compaction while creating a transferred child.
    pub fn portable_provider_for_session(
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> Result<Arc<dyn Provider>> {
        let manager = Self::new();
        let (provider, _, _, _, _) = manager.lcm_provider_for_session(session, provider)?;
        Ok(provider)
    }

    pub(super) fn maybe_start_lcm_with(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> bool {
        if self.pending_task.is_none()
            && self.prepared_lcm_context.is_none()
            && match self.maybe_start_lcm_condensation(session, provider.clone()) {
                Ok(started) => started,
                Err(error) => {
                    crate::logging::error(&format!("LCM hierarchy start failed: {error}"));
                    false
                }
            }
        {
            return true;
        }
        let all_messages = session.messages_for_provider_uncached();
        if !self.should_compact_with(&all_messages) {
            return false;
        }
        let active = self.active_messages(&all_messages);
        let mut cutoff = match self.mode {
            crate::config::CompactionMode::Semantic => self.semantic_cutoff(active),
            _ => active.len().saturating_sub(RECENT_TURNS_TO_KEEP),
        };
        cutoff = safe_compaction_cutoff(active, cutoff);
        if cutoff == 0 {
            return false;
        }

        let trigger = self.mode_trigger_label().to_string();
        match self.start_lcm_job(session, provider, &all_messages, cutoff, trigger) {
            Ok(()) => true,
            Err(error) => {
                crate::logging::error(&format!(
                    "LCM job start failed; keeping canonical history: {error}"
                ));
                false
            }
        }
    }

    pub(super) fn maybe_start_lcm_condensation(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
    ) -> Result<bool> {
        const FANOUT: usize = 4;
        let Some(frontier) = session.context_frontier.as_ref() else {
            return Ok(false);
        };
        let all_messages = session.messages_for_provider_uncached();
        if !self.should_compact_with(&all_messages) {
            return Ok(false);
        }
        let active = self.active_messages(&all_messages);
        let active_chars = active.iter().map(message_char_count).sum();
        let tail_tokens = estimate_compaction_tokens(None, active_chars, self.token_budget);
        if (tail_tokens as f64 / self.token_budget.max(1) as f64) >= f64::from(COMPACTION_THRESHOLD)
        {
            // The raw tail itself needs a new leaf. Condensing the frontier first
            // would cause two provider-prefix changes where one leaf generation
            // is the actual fit operation.
            return Ok(false);
        }
        let active_nodes = frontier
            .active_node_ids
            .iter()
            .map(|id| {
                session
                    .context_nodes
                    .iter()
                    .find(|node| node.id == *id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("LCM frontier node {id} is missing"))
            })
            .collect::<Result<Vec<_>>>()?;
        if active_nodes.len() < FANOUT {
            return Ok(false);
        }
        let children = active_nodes[active_nodes.len() - FANOUT..].to_vec();
        let child_level = children[0].level;
        if children.iter().any(|node| node.level != child_level) {
            return Ok(false);
        }

        let (compactor, model, provider_name, route, configured_model) =
            self.lcm_provider_for_session(session, provider)?;
        let budget_route = route.clone();
        let route_policy_fingerprint =
            lcm_route_policy_fingerprint(session, configured_model.as_deref());
        let child_bytes = serde_json::to_vec(&children)?;
        let source_runtime_identity_sha256 = crate::session::exact_runtime_identity_sha256(
            session.exact_runtime_identity.as_ref().ok_or_else(|| {
                anyhow::anyhow!("LCM hierarchy source runtime identity is missing")
            })?,
        )?;
        let summarizer_runtime_identity_sha256 = crate::session::exact_runtime_identity_sha256(
            &compactor
                .exact_runtime_identity()
                .ok_or_else(|| anyhow::anyhow!("LCM hierarchy summarizer identity is missing"))?,
        )?;
        let source_sha256 = format!("{:x}", Sha256::digest(child_bytes));
        let mut seen_message_ids = std::collections::HashSet::new();
        let source_message_ids = children
            .iter()
            .flat_map(|node| node.source_message_ids.iter().cloned())
            .filter(|id| seen_message_ids.insert(id.clone()))
            .collect::<Vec<_>>();
        let input_proof_ids = children.iter().map(|node| node.id.clone()).collect();
        let source = PendingLcmSource {
            session_id: session.id.clone(),
            base_generation: frontier.generation,
            generation: frontier.generation.saturating_add(1),
            next_node_sequence: frontier.next_node_sequence,
            covered_message_count: frontier.covered_message_count,
            source_message_ids,
            input_proof_ids,
            source_sha256,
            source_runtime_identity_sha256,
            source_prefix_sha256: frontier.source_prefix_sha256.clone(),
            covered_through_message_id: frontier
                .covered_through_message_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("LCM frontier lacks covered message id"))?,
            prior_active_nodes: active_nodes[..active_nodes.len() - FANOUT].to_vec(),
            node_level: child_level.saturating_add(1),
            child_nodes: children.clone(),
            summarizer_model: model,
            summarizer_provider: provider_name,
            summarizer_route: route,
            summarizer_runtime_identity_sha256,
            configured_model,
            cutoff: 0,
            source_fingerprint: None,
            pre_tokens: self.effective_token_count_with(&session.messages_for_provider_uncached())
                as u64,
            trigger: "hierarchy".to_string(),
            route_policy_fingerprint,
            attempt_terminal_emitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let messages_to_summarize = children
            .iter()
            .map(|node| Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!(
                        "[LCM child {} level {}]\n{}",
                        node.id, node.level, node.summary_text
                    ),
                    cache_control: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            })
            .collect::<Vec<_>>();
        let message_count = messages_to_summarize.len();
        let attempt = source.attempt_context("hierarchy");
        self.pending_cutoff = 0;
        self.pending_source_fingerprint = None;
        self.pending_trigger = Some("hierarchy".to_string());
        self.pending_lcm_source = Some(source);
        self.pending_task = Some(tokio::spawn(async move {
            let start = Instant::now();
            let result = generate_lcm_compaction_artifact_with_attempt(
                compactor,
                messages_to_summarize,
                None,
                Some(budget_route),
                LcmJobPriority::Background,
                attempt,
            )
            .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            crate::bus::Bus::global().publish(crate::bus::BusEvent::CompactionFinished);
            result.map(|mut result| {
                result.duration_ms = duration_ms;
                result.summarized_messages = message_count;
                result
            })
        }));
        Ok(true)
    }

    pub(super) fn start_lcm_job(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
        all_messages: &[Message],
        cutoff: usize,
        trigger: String,
    ) -> Result<()> {
        let policy_model = crate::config::config().compaction.model.clone();
        self.start_lcm_job_with_model(
            session,
            provider,
            all_messages,
            cutoff,
            trigger,
            policy_model.clone(),
            policy_model,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "starts one fully specified LCM route transaction"
    )]
    pub(super) fn start_lcm_job_with_model(
        &mut self,
        session: &crate::session::Session,
        provider: Arc<dyn Provider>,
        all_messages: &[Message],
        cutoff: usize,
        trigger: String,
        route_model: Option<String>,
        policy_model: Option<String>,
    ) -> Result<()> {
        let active = self.active_messages(all_messages);
        if cutoff == 0 || cutoff > active.len() {
            anyhow::bail!("LCM cutoff is outside the active message suffix");
        }
        let (compactor, model, provider_name, route, _) =
            self.lcm_provider_for_session_with_model(session, provider, route_model)?;
        let budget_route = route.clone();
        let pre_tokens = self.effective_token_count_with(all_messages) as u64;
        let mut source = self.capture_lcm_source(
            session,
            cutoff,
            model,
            provider_name,
            route,
            policy_model,
            pre_tokens,
            trigger.clone(),
        )?;
        let summarizer_identity = compactor
            .exact_runtime_identity()
            .ok_or_else(|| anyhow::anyhow!("LCM summarizer runtime identity is missing"))?;
        source.summarizer_runtime_identity_sha256 =
            crate::session::exact_runtime_identity_sha256(&summarizer_identity)?;
        let messages_to_summarize = active[..cutoff].to_vec();
        let existing_summary = if session.context_frontier.is_none() {
            self.active_summary.clone()
        } else {
            None
        };
        let message_count = messages_to_summarize.len();
        let attempt = source.attempt_context("leaf");
        const ATOMIC_PARENT_PRIOR_CHILDREN: usize = 3;
        let mut carry_frontier = source.prior_active_nodes.clone();
        let mut carry_level = source.node_level;
        let mut atomic_carry_tiers = Vec::new();
        #[allow(
            clippy::while_let_loop,
            reason = "the candidate slice must be validated before mutating its source frontier"
        )]
        loop {
            let Some(children) = carry_frontier.get(
                carry_frontier
                    .len()
                    .saturating_sub(ATOMIC_PARENT_PRIOR_CHILDREN)..,
            ) else {
                break;
            };
            if children.len() != ATOMIC_PARENT_PRIOR_CHILDREN
                || children.iter().any(|node| node.level != carry_level)
            {
                break;
            }
            atomic_carry_tiers.push(children.to_vec());
            carry_frontier.truncate(
                carry_frontier
                    .len()
                    .saturating_sub(ATOMIC_PARENT_PRIOR_CHILDREN),
            );
            carry_level = carry_level.saturating_add(1);
        }

        self.pending_cutoff = cutoff;
        self.pending_source_fingerprint = source.source_fingerprint;
        self.pending_trigger = Some(trigger.clone());
        self.pending_lcm_source = Some(source);
        let priority = if trigger.starts_with("critical_") || trigger == "hard_compact" {
            LcmJobPriority::Critical
        } else {
            LcmJobPriority::Background
        };
        self.pending_task = Some(tokio::spawn(async move {
            let start = Instant::now();
            let mut result = generate_lcm_compaction_artifact_with_attempt(
                Arc::clone(&compactor),
                messages_to_summarize,
                existing_summary,
                Some(budget_route.clone()),
                priority,
                attempt.clone(),
            )
            .await?;
            let mut carried_summary = result.summary_text.clone();
            for (carry_index, carry_children) in atomic_carry_tiers.into_iter().enumerate() {
                let child_level = carry_children[0].level;
                let mut parent_messages = carry_children
                    .iter()
                    .map(|node| Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: format!(
                                "[LCM child {} level {}]\n{}",
                                node.id, node.level, node.summary_text
                            ),
                            cache_control: None,
                        }],
                        timestamp: None,
                        tool_duration_ms: None,
                    })
                    .collect::<Vec<_>>();
                parent_messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: format!("[LCM new child level {child_level}]\n{carried_summary}"),
                        cache_control: None,
                    }],
                    timestamp: None,
                    tool_duration_ms: None,
                });
                let parent = generate_lcm_compaction_artifact_with_attempt(
                    Arc::clone(&compactor),
                    parent_messages,
                    None,
                    Some(budget_route.clone()),
                    priority,
                    attempt.child(format!("carry-{carry_index}")),
                )
                .await?;
                carried_summary = parent.summary_text.clone();
                result.atomic_parent_summaries.push(parent.summary_text);
            }
            let duration_ms = start.elapsed().as_millis() as u64;
            crate::bus::Bus::global().publish(crate::bus::BusEvent::CompactionFinished);
            result.duration_ms = duration_ms;
            result.summarized_messages = message_count;
            Ok(result)
        }));
        Ok(())
    }

    pub(super) fn prepare_lcm_context(
        source: PendingLcmSource,
        result: CompactionResult,
    ) -> Result<PreparedLcmContext> {
        let attempt = source.attempt_context(if source.trigger == "hierarchy" {
            "hierarchy"
        } else {
            "leaf"
        });
        const PROMPT_SCHEMA_VERSION: u32 = 1;
        if result.summary_text.trim().is_empty() {
            anyhow::bail!("LCM compactor returned an empty summary");
        }
        let durable_summary = lcm_summary_with_retrieval_anchor(
            &result.summary_text,
            &source.session_id,
            &source.source_message_ids,
        );
        let mut node = crate::session::StoredContextNode {
            id: String::new(),
            schema_version: 2,
            level: source.node_level,
            source_session_id: source.session_id.clone(),
            source_message_ids: source.source_message_ids.clone(),
            source_sha256: source.source_sha256.clone(),
            source_runtime_identity_sha256: source.source_runtime_identity_sha256.clone(),
            child_node_ids: source
                .child_nodes
                .iter()
                .map(|node| node.id.clone())
                .collect(),
            summary_text: durable_summary.clone(),
            summary_sha256: Some(format!("{:x}", Sha256::digest(durable_summary.as_bytes()))),
            estimated_tokens: durable_summary.len().div_ceil(CHARS_PER_TOKEN).max(1) as u64,
            summarizer_model: source.summarizer_model.clone(),
            summarizer_provider: source.summarizer_provider.clone(),
            summarizer_route: source.summarizer_route.clone(),
            summarizer_runtime_identity_sha256: source.summarizer_runtime_identity_sha256.clone(),
            prompt_schema_version: PROMPT_SCHEMA_VERSION,
            created_at: Utc::now(),
        };
        node.id = crate::session::stored_context_node_id(&node);
        let node_id = node.id.clone();
        let mut active_nodes = source.prior_active_nodes.clone();
        let mut append_context_nodes = vec![node.clone()];
        let mut carried_node = node;
        for parent_summary in &result.atomic_parent_summaries {
            const FANOUT: usize = 4;
            if active_nodes.len() < FANOUT - 1 {
                anyhow::bail!(
                    "LCM atomic carry candidate does not have three prior level-{} nodes",
                    carried_node.level
                );
            }
            let mut children = active_nodes.split_off(active_nodes.len() - (FANOUT - 1));
            children.push(carried_node.clone());
            if children.len() != FANOUT
                || children
                    .iter()
                    .any(|child| child.level != carried_node.level)
            {
                anyhow::bail!(
                    "LCM atomic parent children are not four adjacent level-{} nodes",
                    carried_node.level
                );
            }
            let children_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&children)?));
            let mut seen = std::collections::HashSet::new();
            let source_message_ids = children
                .iter()
                .flat_map(|child| child.source_message_ids.iter().cloned())
                .filter(|id| seen.insert(id.clone()))
                .collect::<Vec<_>>();
            let parent_durable_summary = lcm_summary_with_retrieval_anchor(
                parent_summary,
                &source.session_id,
                &source_message_ids,
            );
            let mut parent = crate::session::StoredContextNode {
                id: String::new(),
                schema_version: 2,
                level: carried_node.level.saturating_add(1),
                source_session_id: source.session_id.clone(),
                source_message_ids,
                source_sha256: children_sha256,
                source_runtime_identity_sha256: source.source_runtime_identity_sha256.clone(),
                child_node_ids: children.iter().map(|child| child.id.clone()).collect(),
                summary_text: parent_durable_summary.clone(),
                summary_sha256: Some(format!(
                    "{:x}",
                    Sha256::digest(parent_durable_summary.as_bytes())
                )),
                estimated_tokens: parent_durable_summary
                    .len()
                    .div_ceil(CHARS_PER_TOKEN)
                    .max(1) as u64,
                summarizer_model: source.summarizer_model.clone(),
                summarizer_provider: source.summarizer_provider.clone(),
                summarizer_route: source.summarizer_route.clone(),
                summarizer_runtime_identity_sha256: source
                    .summarizer_runtime_identity_sha256
                    .clone(),
                prompt_schema_version: PROMPT_SCHEMA_VERSION,
                created_at: Utc::now(),
            };
            parent.id = crate::session::stored_context_node_id(&parent);
            append_context_nodes.push(parent.clone());
            carried_node = parent;
        }
        active_nodes.push(carried_node);
        let active_node_ids = active_nodes.iter().map(|node| node.id.clone()).collect();
        let frontier = crate::session::StoredContextFrontier {
            schema_version: 1,
            generation: source.generation,
            active_node_ids,
            covered_message_count: source.covered_message_count,
            covered_through_message_id: Some(source.covered_through_message_id),
            source_prefix_sha256: source.source_prefix_sha256.clone(),
            next_node_sequence: source
                .next_node_sequence
                .saturating_add(append_context_nodes.len() as u64),
        };
        let projection_text = crate::session::native_lcm_projection_text(&active_nodes, &frontier)
            .ok_or_else(|| anyhow::anyhow!("LCM frontier references a missing context node"))?;
        let transaction = crate::session::ContextGraphTransaction {
            schema_version: 1,
            op_id: format!("lcm-op-{}-{node_id}", source.generation),
            base_generation: source.base_generation,
            generation: source.generation,
            append_context_nodes,
            frontier,
            input_proof: crate::session::ContextGraphInputProof {
                schema_version: 1,
                source_session_id: source.session_id,
                source_message_ids: source.input_proof_ids,
                source_sha256: source.source_sha256,
            },
        };
        let projection = crate::session::StoredCompactionState {
            summary_text: projection_text.clone(),
            openai_encrypted_content: None,
            covers_up_to_turn: source.covered_message_count,
            original_turn_count: source.covered_message_count,
            compacted_count: source.covered_message_count,
        };
        Ok(PreparedLcmContext {
            attempt,
            transaction,
            projection,
            summary: Summary {
                text: projection_text,
                openai_encrypted_content: None,
                covers_up_to_turn: source.covered_message_count,
                original_turn_count: source.covered_message_count,
            },
            covered_message_count: source.covered_message_count,
            cutoff: source.cutoff,
            pre_tokens: source.pre_tokens,
            duration_ms: result.duration_ms,
            trigger: source.trigger.clone(),
            messages_dropped: None,
            configured_model: source.configured_model,
            effective_route: source.summarizer_route,
            allow_active_route_fallback: source.trigger == "critical_active_route",
            route_policy_fingerprint: source.route_policy_fingerprint,
        })
    }
}
