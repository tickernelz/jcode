use super::*;

impl Session {
    pub(super) fn archived_message_id_set(&self) -> HashSet<&str> {
        self.archived_message_ids
            .iter()
            .map(String::as_str)
            .collect()
    }

    pub fn active_stored_message_entries(&self) -> Vec<(usize, &StoredMessage)> {
        if self.archived_message_ids.is_empty() {
            return self.messages.iter().enumerate().collect();
        }
        let archived = self.archived_message_id_set();
        self.messages
            .iter()
            .enumerate()
            .filter(|(_, message)| !archived.contains(message.id.as_str()))
            .collect()
    }

    pub fn active_stored_messages(&self) -> Cow<'_, [StoredMessage]> {
        if self.archived_message_ids.is_empty() {
            Cow::Borrowed(&self.messages)
        } else {
            Cow::Owned(
                self.active_stored_message_entries()
                    .into_iter()
                    .map(|(_, message)| message.clone())
                    .collect(),
            )
        }
    }

    pub fn active_message_count(&self) -> usize {
        if self.archived_message_ids.is_empty() {
            self.messages.len()
        } else {
            self.active_stored_message_entries().len()
        }
    }

    /// Move the active branch through `raw_index` without deleting canonical
    /// messages. New messages appended later remain active and form a new branch.
    pub fn rewind_active_branch_through(&mut self, raw_index: usize) -> anyhow::Result<()> {
        let active = self.active_stored_message_entries();
        let active_position = active
            .iter()
            .position(|(index, _)| *index == raw_index)
            .ok_or_else(|| anyhow::anyhow!("rewind target is not on the active branch"))?;
        let retained = active_position + 1;
        let to_archive = active[retained..]
            .iter()
            .map(|(_, message)| message.id.clone())
            .collect::<Vec<_>>();
        if let Err(error) = self.retain_context_graph_prefix(retained) {
            crate::logging::warn(&format!(
                "Failed to retain valid LCM rewind prefix; deactivating the frontier: {error}"
            ));
            self.deactivate_context_graph_state();
        }
        let mut archived = self
            .archived_message_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        for id in to_archive {
            if archived.insert(id.clone()) {
                self.archived_message_ids.push(id);
            }
        }
        self.reset_provider_messages_cache();
        Ok(())
    }

    /// Restore a prior active branch while archiving any canonical messages
    /// appended after the snapshot was taken.
    pub fn restore_active_branch(
        &mut self,
        previous_archived_message_ids: Vec<String>,
        snapshot_raw_message_count: usize,
    ) {
        let mut archived = previous_archived_message_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        self.archived_message_ids = previous_archived_message_ids;
        for message in self.messages.iter().skip(snapshot_raw_message_count) {
            if archived.insert(message.id.clone()) {
                self.archived_message_ids.push(message.id.clone());
            }
        }
        self.reset_provider_messages_cache();
    }

    pub fn has_owned_native_lcm_projection(&self) -> bool {
        let Some(state) = self.compaction.as_ref() else {
            return false;
        };
        let messages = self.active_stored_messages();
        state.openai_encrypted_content.is_none()
            && self.context_frontier.as_ref().is_some_and(|frontier| {
                frontier.covered_message_count == state.compacted_count
                    && frontier.covered_message_count == state.covers_up_to_turn
                    && !frontier.active_node_ids.is_empty()
                    && frontier.covered_message_count <= messages.len()
                    && stored_messages_sha256(&messages[..frontier.covered_message_count])
                        .is_ok_and(|digest| digest == frontier.source_prefix_sha256)
                    && frontier.covered_through_message_id.as_deref()
                        == messages[..frontier.covered_message_count]
                            .last()
                            .map(|message| message.id.as_str())
                    && native_lcm_projection_text(&self.context_nodes, frontier)
                        .is_some_and(|projection| projection == state.summary_text)
            })
    }

    pub(crate) fn adopt_persistence_base(&mut self, durable: &Self) {
        self.persistence_revision = durable.persistence_revision;
        self.journal_sequence = durable.journal_sequence;
        self.journal_watermark = durable.journal_watermark;
        self.persist_state.context_generation = durable
            .context_frontier
            .as_ref()
            .map_or(0, |frontier| frontier.generation);
        self.persist_state.context_op_id = durable.last_context_op_id.clone();
    }

    pub fn context_graph_state(&self) -> ContextGraphState {
        ContextGraphState {
            context_nodes: self.context_nodes.clone(),
            context_frontier: self.context_frontier.clone(),
            last_context_op_id: self.last_context_op_id.clone(),
            last_context_op_sha256: self.last_context_op_sha256.clone(),
        }
    }

    pub fn restore_context_graph_state(&mut self, state: ContextGraphState) {
        let current_generation = self.context_frontier.as_ref().map_or(
            self.persist_state.context_generation,
            |frontier| {
                frontier
                    .generation
                    .max(self.persist_state.context_generation)
            },
        );
        let mut restored_nodes = state.context_nodes;
        for node in self.context_nodes.drain(..) {
            if !restored_nodes.iter().any(|restored| restored.id == node.id) {
                restored_nodes.push(node);
            }
        }
        self.context_nodes = restored_nodes;
        self.context_frontier = state.context_frontier.map(|mut frontier| {
            frontier.generation = current_generation.saturating_add(1);
            frontier
        });
        self.last_context_op_id = state.last_context_op_id;
        self.last_context_op_sha256 = state.last_context_op_sha256;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
        self.persist_state.context_generation = current_generation;
    }

    pub(super) fn clear_context_graph_state(&mut self) {
        self.context_nodes.clear();
        self.context_frontier = None;
        self.last_context_op_id = None;
        self.last_context_op_sha256 = None;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
    }

    /// Exact runtime/account transitions establish a new graph ownership
    /// domain. Preserve immutable nodes for forensics, but remove every active
    /// projection/frontier reference so old-owner summaries cannot be reused.
    pub fn reset_context_graph_for_identity_transition(&mut self) {
        self.deactivate_context_graph_state();
    }

    /// Deactivate the mutable frontier without garbage-collecting immutable
    /// nodes. Rewind, repair, and canonical transcript replacement use this;
    /// actual node deletion belongs to a separately validated GC operation.
    pub fn deactivate_context_graph_state(&mut self) {
        self.compaction = None;
        // The journal meta does not carry context frontier state. Force a full
        // snapshot even though immutable node count is unchanged, otherwise a
        // restart can replay archive metadata over the old active frontier and
        // discard the otherwise-valid graph as inconsistent.
        self.persist_state.context_nodes_len = usize::MAX;
        if self.context_nodes.is_empty() {
            self.context_frontier = None;
            self.last_context_op_id = None;
            self.last_context_op_sha256 = None;
        } else {
            let previous = self.context_frontier.take();
            let generation = match previous.as_ref() {
                Some(frontier) => match frontier.generation.checked_add(1) {
                    Some(generation) => generation,
                    None => {
                        self.context_frontier = None;
                        self.persist_state.pending_context_transaction = None;
                        return;
                    }
                },
                None => 1,
            };
            let next_node_sequence =
                previous
                    .as_ref()
                    .map_or(self.context_nodes.len() as u64 + 1, |frontier| {
                        frontier
                            .next_node_sequence
                            .max(self.context_nodes.len() as u64 + 1)
                    });
            self.context_frontier = Some(StoredContextFrontier {
                schema_version: 1,
                generation,
                active_node_ids: Vec::new(),
                covered_message_count: 0,
                covered_through_message_id: None,
                source_prefix_sha256: format!("{:x}", Sha256::digest(b"[]")),
                next_node_sequence,
            });
        }
        self.persist_state.pending_context_transaction = None;
    }

    /// Retain the maximal chronological graph prefix fully covered by the first
    /// `new_len` active messages. Parents that straddle the rewind boundary are
    /// expanded into their immutable children. Nodes remain immutable; only the
    /// active frontier changes.
    pub fn retain_context_graph_prefix(&mut self, new_len: usize) -> anyhow::Result<()> {
        let Some(old_frontier) = self.context_frontier.clone() else {
            self.compaction = None;
            return Ok(());
        };
        if new_len >= old_frontier.covered_message_count {
            return Ok(());
        }
        let active_messages = self.active_stored_messages();
        let positions = active_messages
            .iter()
            .enumerate()
            .map(|(index, message)| (message.id.clone(), index))
            .collect::<std::collections::HashMap<_, _>>();
        let nodes_by_id = self
            .context_nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<std::collections::HashMap<_, _>>();

        fn retain_node(
            id: &str,
            new_len: usize,
            current_session_id: &str,
            positions: &std::collections::HashMap<String, usize>,
            nodes: &std::collections::HashMap<String, StoredContextNode>,
            selected: &mut Vec<String>,
        ) -> bool {
            let node = &nodes[id];
            let local_positions = node
                .source_message_ids
                .iter()
                .filter_map(|message_id| positions.get(message_id.as_str()).copied())
                .collect::<Vec<_>>();
            let imported =
                node.source_session_id != current_session_id && local_positions.is_empty();
            if imported
                || (!local_positions.is_empty()
                    && local_positions.iter().all(|position| *position < new_len))
            {
                selected.push(node.id.clone());
                return true;
            }
            if node.child_node_ids.is_empty() {
                return false;
            }
            for child_id in &node.child_node_ids {
                if !retain_node(
                    child_id,
                    new_len,
                    current_session_id,
                    positions,
                    nodes,
                    selected,
                ) {
                    return false;
                }
            }
            false
        }

        let mut selected = Vec::new();
        for id in &old_frontier.active_node_ids {
            if !retain_node(
                id,
                new_len,
                &self.id,
                &positions,
                &nodes_by_id,
                &mut selected,
            ) {
                break;
            }
        }
        if selected.is_empty() {
            drop(active_messages);
            self.deactivate_context_graph_state();
            return Ok(());
        }

        let covered = selected
            .iter()
            .filter_map(|id| nodes_by_id.get(id.as_str()))
            .flat_map(|node| node.source_message_ids.iter())
            .filter_map(|message_id| positions.get(message_id.as_str()).copied())
            .max()
            .map_or(0, |position| position + 1)
            .min(new_len);
        let next_frontier = StoredContextFrontier {
            schema_version: 1,
            generation: old_frontier.generation.saturating_add(1),
            active_node_ids: selected,
            covered_message_count: covered,
            covered_through_message_id: covered
                .checked_sub(1)
                .map(|index| active_messages[index].id.clone()),
            source_prefix_sha256: if covered == 0 {
                old_frontier.source_prefix_sha256
            } else {
                stored_messages_sha256(&active_messages[..covered])?
            },
            next_node_sequence: old_frontier.next_node_sequence,
        };
        let projection_text = native_lcm_projection_text(&self.context_nodes, &next_frontier)
            .ok_or_else(|| anyhow::anyhow!("rewind frontier references a missing context node"))?;
        drop(active_messages);
        self.context_frontier = Some(next_frontier);
        self.compaction = Some(StoredCompactionState {
            summary_text: projection_text,
            openai_encrypted_content: None,
            covers_up_to_turn: covered,
            original_turn_count: covered,
            compacted_count: covered,
        });
        self.last_context_op_id = None;
        self.last_context_op_sha256 = None;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
        Ok(())
    }

    /// Inherit rebuildable LCM graph state when a lifecycle operation copies the
    /// complete canonical transcript into a new child session. Immutable node
    /// identities retain their original source-session namespace; the child only
    /// receives nodes whose proofs validate against its copied transcript.
    pub fn inherit_context_graph_from(&mut self, parent: &Session) -> anyhow::Result<()> {
        let Some(parent_frontier) = parent.context_frontier.as_ref() else {
            self.clear_context_graph_state();
            return Ok(());
        };
        if !self
            .exact_runtime_identity
            .as_ref()
            .is_some_and(|identity| identity.has_verifiable_account_binding())
            || !parent
                .exact_runtime_identity
                .as_ref()
                .is_some_and(|identity| identity.has_verifiable_account_binding())
        {
            anyhow::bail!("LCM graph inheritance requires verified account identities");
        }
        if self.exact_runtime_identity != parent.exact_runtime_identity {
            anyhow::bail!("LCM graph inheritance requires an exact runtime/account identity match");
        }
        if self.messages.len() != parent.messages.len()
            || self.archived_message_ids != parent.archived_message_ids
            || stored_messages_sha256(&self.messages)? != stored_messages_sha256(&parent.messages)?
        {
            anyhow::bail!("LCM graph inheritance requires an exact canonical transcript copy");
        }

        let nodes = parent.context_nodes.clone();
        let frontier = parent_frontier.clone();
        validate_context_graph(&nodes, Some(&frontier)).map_err(anyhow::Error::msg)?;
        let active_messages = self.active_stored_messages();
        validate_context_node_source_proofs(
            &self.id,
            self.exact_runtime_identity.as_ref(),
            &active_messages,
            &nodes,
            Some(&frontier),
        )
        .map_err(anyhow::Error::msg)?;
        validate_context_frontier_coverage(&active_messages, &nodes, &frontier)
            .map_err(anyhow::Error::msg)?;
        drop(active_messages);
        self.context_nodes = nodes;
        self.context_frontier = Some(frontier);
        self.last_context_op_id = None;
        self.last_context_op_sha256 = None;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
        Ok(())
    }

    /// Copy the canonical transcript and all state that defines its active
    /// context projection into a lifecycle child. Provider resume bindings are
    /// intentionally excluded because a new session may never reuse them.
    /// Keeping this operation atomic at the API level prevents callers from
    /// copying raw messages while forgetting archive metadata and resurrecting
    /// a branch that the parent had rewound.
    pub fn inherit_context_continuity_from(&mut self, parent: &Session) -> anyhow::Result<()> {
        self.replace_messages(parent.messages.clone());
        self.archived_message_ids = parent.archived_message_ids.clone();
        self.exact_runtime_identity = parent.exact_runtime_identity.clone();
        self.compaction = parent.compaction.clone();
        self.inherit_context_graph_from(parent)
    }

    /// Install a self-contained transfer summary while retaining an auditable
    /// proof and lineage back to the canonical parent transcript. Transfer
    /// children intentionally do not duplicate parent raw messages.
    pub fn install_imported_context_root(
        &mut self,
        parent: &Session,
        compaction: StoredCompactionState,
        summarizer_identity: &jcode_provider_core::ExactRuntimeIdentity,
    ) -> anyhow::Result<()> {
        let Some(child_identity) = self.exact_runtime_identity.as_ref() else {
            anyhow::bail!(
                "LCM imported root requires verified child and parent account identities"
            );
        };
        let Some(parent_identity) = parent.exact_runtime_identity.as_ref() else {
            anyhow::bail!(
                "LCM imported root requires verified child and parent account identities"
            );
        };
        if !child_identity.has_verifiable_account_binding()
            || !parent_identity.has_verifiable_account_binding()
        {
            anyhow::bail!(
                "LCM imported root requires verified child and parent account identities"
            );
        }
        if child_identity != parent_identity {
            anyhow::bail!(
                "LCM imported root requires exact child and parent runtime identity equality"
            );
        }
        if !summarizer_identity.has_verifiable_account_binding() {
            anyhow::bail!("LCM imported root requires verified summarizer account identity");
        }
        if !self.messages.is_empty() {
            anyhow::bail!("LCM imported root requires an empty child transcript");
        }
        if self.parent_id.as_deref() != Some(parent.id.as_str()) {
            anyhow::bail!("LCM imported root source must be the child's recorded parent");
        }
        if compaction.summary_text.trim().is_empty()
            || compaction.openai_encrypted_content.is_some()
        {
            anyhow::bail!("LCM imported root requires a portable text summary");
        }
        let parent_messages = parent.active_stored_messages();
        let source_sha256 = stored_messages_sha256(&parent_messages)?;
        let source_identity_sha256 = exact_runtime_identity_sha256(parent_identity)?;
        let summarizer_identity_sha256 = exact_runtime_identity_sha256(summarizer_identity)?;
        let imported_summary = format!(
            "{}\n\n## Retrieval anchor\nUse `conversation_search` against recorded ancestor session `{}` when exact canonical parent details are needed.",
            compaction.summary_text.trim(),
            parent.id
        );
        let mut node = StoredContextNode {
            id: String::new(),
            schema_version: 2,
            level: 0,
            source_session_id: parent.id.clone(),
            source_message_ids: parent_messages
                .iter()
                .map(|message| message.id.clone())
                .collect(),
            source_sha256: source_sha256.clone(),
            source_runtime_identity_sha256: source_identity_sha256,
            child_node_ids: Vec::new(),
            summary_text: imported_summary.clone(),
            summary_sha256: Some(format!("{:x}", Sha256::digest(imported_summary.as_bytes()))),
            estimated_tokens: imported_summary.len().div_ceil(4).max(1) as u64,
            summarizer_model: summarizer_identity.route.model.clone(),
            summarizer_provider: summarizer_identity.provider_key.clone(),
            summarizer_route: serde_json::to_string(&summarizer_identity.route)?,
            summarizer_runtime_identity_sha256: summarizer_identity_sha256,
            prompt_schema_version: 1,
            created_at: Utc::now(),
        };
        node.id = stored_context_node_id(&node);
        let node_id = node.id.clone();
        let frontier = StoredContextFrontier {
            schema_version: 1,
            generation: 1,
            active_node_ids: vec![node_id],
            covered_message_count: 0,
            covered_through_message_id: None,
            // Frontier coverage is over the child's canonical transcript. The
            // imported ancestor proof lives on the node itself and must not be
            // mistaken for a covered prefix of this empty transfer child.
            source_prefix_sha256: stored_messages_sha256(&[])?,
            next_node_sequence: 2,
        };
        validate_context_graph(std::slice::from_ref(&node), Some(&frontier))
            .map_err(anyhow::Error::msg)?;
        validate_context_node_source_proofs(
            &self.id,
            self.exact_runtime_identity.as_ref(),
            &self.messages,
            std::slice::from_ref(&node),
            Some(&frontier),
        )
        .map_err(anyhow::Error::msg)?;
        validate_context_frontier_coverage(&self.messages, std::slice::from_ref(&node), &frontier)
            .map_err(anyhow::Error::msg)?;
        let projection = native_lcm_projection_text(std::slice::from_ref(&node), &frontier)
            .ok_or_else(|| anyhow::anyhow!("LCM imported root projection is incomplete"))?;
        self.compaction = Some(StoredCompactionState {
            summary_text: projection,
            openai_encrypted_content: None,
            covers_up_to_turn: 0,
            original_turn_count: 0,
            compacted_count: 0,
        });
        self.context_nodes = vec![node];
        self.context_frontier = Some(frontier);
        self.last_context_op_id = None;
        self.last_context_op_sha256 = None;
        self.persist_state.pending_context_transaction = None;
        self.persist_state.context_nodes_len = usize::MAX;
        Ok(())
    }

    pub(super) fn session_from_startup_stub(stub: SessionStartupStub) -> Self {
        let mut session = Self::create_with_id(stub.id, stub.parent_id, stub.title);
        session.custom_title = stub.custom_title;
        session.created_at = stub.created_at;
        session.updated_at = stub.updated_at;
        session.archived_message_ids = stub.archived_message_ids;
        session.persistence_revision = stub.persistence_revision;
        session.compaction = stub.compaction;
        session.provider_session_id = stub.provider_session_id;
        session.provider_session_identity = stub.provider_session_identity;
        session.provider_key = stub.provider_key;
        session.model = stub.model;
        session.route_api_method = stub.route_api_method;
        session.reasoning_effort = stub.reasoning_effort;
        session.exact_runtime_identity = stub.exact_runtime_identity;
        session.subagent_model = stub.subagent_model;
        session.improve_mode = stub.improve_mode;
        session.autoreview_enabled = stub.autoreview_enabled;
        session.autojudge_enabled = stub.autojudge_enabled;
        session.is_canary = stub.is_canary;
        session.testing_build = stub.testing_build;
        session.working_dir = stub.working_dir;
        session.short_name = stub.short_name;
        session.status = stub.status;
        session.last_pid = stub.last_pid;
        session.last_active_at = stub.last_active_at;
        session.is_debug = stub.is_debug;
        session.saved = stub.saved;
        session.save_label = stub.save_label;
        session.messages.clear();
        session.env_snapshots.clear();
        session.memory_injections.clear();
        session.replay_events.clear();
        session.rebuild_memory_profile_cache();
        session.reset_persist_state(true);
        session
    }

    pub(super) fn session_from_remote_startup_snapshot(
        snapshot: RemoteStartupSessionSnapshot,
    ) -> Self {
        let mut session = Self::create_with_id(snapshot.id, snapshot.parent_id, snapshot.title);
        session.custom_title = snapshot.custom_title;
        session.created_at = snapshot.created_at;
        session.updated_at = snapshot.updated_at;
        session.messages = snapshot.messages;
        session.archived_message_ids = snapshot.archived_message_ids;
        session.provider_image_char_budget = snapshot.provider_image_char_budget;
        session.provider_tool_use_suppressed_message_ids =
            snapshot.provider_tool_use_suppressed_message_ids;
        session.journal_sequence = snapshot.journal_sequence;
        session.journal_watermark = snapshot.journal_watermark;
        session.persistence_revision = snapshot.persistence_revision;
        session.context_nodes = snapshot.context_nodes;
        session.context_frontier = snapshot.context_frontier;
        session.compaction = snapshot.compaction;
        session.provider_session_id = snapshot.provider_session_id;
        session.provider_session_identity = snapshot.provider_session_identity;
        session.provider_key = snapshot.provider_key;
        session.model = snapshot.model;
        session.route_api_method = snapshot.route_api_method;
        session.reasoning_effort = snapshot.reasoning_effort;
        session.exact_runtime_identity = snapshot.exact_runtime_identity;
        session.subagent_model = snapshot.subagent_model;
        session.improve_mode = snapshot.improve_mode;
        session.autoreview_enabled = snapshot.autoreview_enabled;
        session.autojudge_enabled = snapshot.autojudge_enabled;
        session.is_canary = snapshot.is_canary;
        session.testing_build = snapshot.testing_build;
        session.working_dir = snapshot.working_dir;
        session.short_name = snapshot.short_name;
        session.status = snapshot.status;
        session.last_pid = snapshot.last_pid;
        session.last_active_at = snapshot.last_active_at;
        session.is_debug = snapshot.is_debug;
        session.saved = snapshot.saved;
        session.save_label = snapshot.save_label;
        session.replay_events.clear();
        session.env_snapshots.clear();
        session.memory_injections.clear();
        session.mark_memory_profile_dirty();
        session.reset_persist_state(true);
        session.reset_provider_messages_cache();
        session
    }

    pub fn debug_memory_profile(&self) -> serde_json::Value {
        let message_stats =
            summarize_message_content(self.messages.iter().map(|message| &message.content));

        let session_message_json_bytes: usize = self.messages.iter().map(estimate_json_bytes).sum();
        let provider_cache_stats = summarize_message_content(
            self.provider_messages_cache
                .iter()
                .map(|message| &message.content),
        );
        let provider_messages_cache_json_bytes: usize = self
            .provider_messages_cache
            .iter()
            .map(estimate_json_bytes)
            .sum();
        let env_snapshots_json_bytes: usize =
            self.env_snapshots.iter().map(estimate_json_bytes).sum();
        let memory_injections_json_bytes: usize =
            self.memory_injections.iter().map(estimate_json_bytes).sum();
        let replay_events_json_bytes: usize =
            self.replay_events.iter().map(estimate_json_bytes).sum();
        let compaction_json_bytes = self
            .compaction
            .as_ref()
            .map(estimate_json_bytes)
            .unwrap_or(0);
        let compaction_summary_bytes = self
            .compaction
            .as_ref()
            .map(|c| c.summary_text.len())
            .unwrap_or(0);
        let compaction_encrypted_bytes = self
            .compaction
            .as_ref()
            .and_then(|c| c.openai_encrypted_content.as_ref())
            .map(|text| text.len())
            .unwrap_or(0);

        serde_json::json!({
            "session_id": self.id,
            "messages": {
                "count": self.messages.len(),
                "json_bytes": session_message_json_bytes,
                "memory": message_stats.to_json(),
            },
            "compaction": {
                "present": self.compaction.is_some(),
                "covers_up_to_turn": self
                    .compaction
                    .as_ref()
                    .map(|c| c.covers_up_to_turn)
                    .unwrap_or(0),
                "original_turn_count": self
                    .compaction
                    .as_ref()
                    .map(|c| c.original_turn_count)
                    .unwrap_or(0),
                "compacted_count": self
                    .compaction
                    .as_ref()
                    .map(|c| c.compacted_count)
                    .unwrap_or(0),
                "json_bytes": compaction_json_bytes,
                "summary_text_bytes": compaction_summary_bytes,
                "encrypted_content_bytes": compaction_encrypted_bytes,
            },
            "env_snapshots": {
                "count": self.env_snapshots.len(),
                "json_bytes": env_snapshots_json_bytes,
            },
            "memory_injections": {
                "count": self.memory_injections.len(),
                "json_bytes": memory_injections_json_bytes,
            },
            "replay_events": {
                "count": self.replay_events.len(),
                "json_bytes": replay_events_json_bytes,
            },
            "provider_messages_cache": {
                "count": self.provider_messages_cache.len(),
                "source_len": self.provider_messages_cache_len,
                "mode": persist_vector_mode_label(self.provider_messages_cache_mode),
                "json_bytes": provider_messages_cache_json_bytes,
                "memory": provider_cache_stats.to_json(),
            },
            "totals": {
                "payload_text_bytes": message_stats.payload_text_bytes(),
                "json_bytes": session_message_json_bytes
                    + provider_messages_cache_json_bytes
                    + env_snapshots_json_bytes
                    + memory_injections_json_bytes
                    + replay_events_json_bytes
                    + compaction_json_bytes,
                "canonical_transcript_json_bytes": session_message_json_bytes,
                "provider_cache_json_bytes": provider_messages_cache_json_bytes,
                "canonical_tool_result_bytes": message_stats.tool_result_bytes,
                "provider_cache_tool_result_bytes": provider_cache_stats.tool_result_bytes,
                "canonical_large_blob_bytes": message_stats.large_block_bytes,
                "provider_cache_large_blob_bytes": provider_cache_stats.large_block_bytes,
            }
        })
    }

    pub(super) fn journal_meta(&self) -> SessionJournalMeta {
        SessionJournalMeta {
            parent_id: self.parent_id.clone(),
            title: self.title.clone(),
            custom_title: self.custom_title.clone(),
            updated_at: self.updated_at,
            archived_message_ids: self.archived_message_ids.clone(),
            compaction: self.compaction.clone(),
            provider_session_id: self.provider_session_id.clone(),
            provider_session_identity: self.provider_session_identity.clone(),
            provider_key: self.provider_key.clone(),
            route_api_method: self.route_api_method.clone(),
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            exact_runtime_identity: self.exact_runtime_identity.clone(),
            subagent_model: self.subagent_model.clone(),
            improve_mode: self.improve_mode,
            autoreview_enabled: self.autoreview_enabled,
            autojudge_enabled: self.autojudge_enabled,
            is_canary: self.is_canary,
            testing_build: self.testing_build.clone(),
            working_dir: self.working_dir.clone(),
            short_name: self.short_name.clone(),
            status: self.status.clone(),
            last_pid: self.last_pid,
            last_active_at: self.last_active_at,
            is_debug: self.is_debug,
            saved: self.saved,
            save_label: self.save_label.clone(),
            persistence_revision: Some(self.persistence_revision),
        }
    }

    pub(super) fn reset_persist_state(&mut self, snapshot_exists: bool) {
        self.persist_state = SessionPersistState {
            snapshot_exists,
            messages_len: self.messages.len(),
            env_snapshots_len: self.env_snapshots.len(),
            memory_injections_len: self.memory_injections.len(),
            replay_events_len: self.replay_events.len(),
            context_nodes_len: self.context_nodes.len(),
            context_generation: self
                .context_frontier
                .as_ref()
                .map_or(0, |frontier| frontier.generation),
            context_op_id: self.last_context_op_id.clone(),
            messages_mode: PersistVectorMode::Clean,
            env_snapshots_mode: PersistVectorMode::Clean,
            memory_injections_mode: PersistVectorMode::Clean,
            replay_events_mode: PersistVectorMode::Clean,
            pending_context_transaction: None,
            last_meta: Some(self.journal_meta()),
        };
    }
}
