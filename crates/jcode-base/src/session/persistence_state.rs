use super::*;

impl Session {
    pub(super) fn apply_context_transaction_inner(
        &mut self,
        transaction: &ContextGraphTransaction,
        persist: bool,
    ) -> anyhow::Result<bool> {
        let transaction_sha256 = context_transaction_sha256(transaction)?;
        if self.last_context_op_id.as_deref() == Some(transaction.op_id.as_str()) {
            if self.last_context_op_sha256.as_deref() == Some(transaction_sha256.as_str()) {
                return Ok(false);
            }
            anyhow::bail!(
                "context op id {} was reused with a different transaction",
                transaction.op_id
            );
        }
        let current_generation = self
            .context_frontier
            .as_ref()
            .map_or(0, |frontier| frontier.generation);
        if transaction.base_generation != current_generation
            || transaction.generation != current_generation + 1
            || transaction.frontier.generation != transaction.generation
        {
            anyhow::bail!(
                "context generation mismatch: current {}, base {}, transaction {}, frontier {}",
                current_generation,
                transaction.base_generation,
                transaction.generation,
                transaction.frontier.generation
            );
        }
        if transaction.schema_version != 1
            || transaction.input_proof.schema_version != 1
            || transaction.op_id.is_empty()
            || transaction.input_proof.source_session_id != self.id
            || !is_sha256_hex(&transaction.input_proof.source_sha256)
            || transaction.append_context_nodes.iter().any(|node| {
                node.source_session_id != self.id || !is_sha256_hex(&node.source_sha256)
            })
            || !is_sha256_hex(&transaction.frontier.source_prefix_sha256)
        {
            anyhow::bail!("invalid context transaction identity or SHA-256 proof");
        }
        let active_messages = self.active_stored_messages();
        if transaction
            .append_context_nodes
            .iter()
            .all(|node| node.level == 0)
        {
            let covered = transaction.frontier.covered_message_count;
            if covered == 0 || covered > active_messages.len() {
                anyhow::bail!("context transaction covers an invalid message prefix");
            }
            let source_start = self
                .context_frontier
                .as_ref()
                .map_or(0, |frontier| frontier.covered_message_count)
                .min(covered);
            let source = &active_messages[source_start..covered];
            let source_prefix = &active_messages[..covered];
            if source.is_empty() {
                anyhow::bail!("context transaction has an empty leaf source");
            }
            let source_ids: Vec<String> = source.iter().map(|message| message.id.clone()).collect();
            let source_sha256 = stored_messages_sha256(source)?;
            let source_prefix_sha256 = stored_messages_sha256(source_prefix)?;
            if transaction.input_proof.source_message_ids != source_ids
                || transaction.input_proof.source_sha256 != source_sha256
                || transaction.frontier.source_prefix_sha256 != source_prefix_sha256
                || transaction.frontier.covered_through_message_id.as_deref()
                    != source_prefix.last().map(|message| message.id.as_str())
                || transaction.append_context_nodes.iter().any(|node| {
                    node.source_message_ids != source_ids || node.source_sha256 != source_sha256
                })
            {
                anyhow::bail!("context transaction source proof does not match canonical history");
            }
        } else if transaction.append_context_nodes.len() >= 2
            && transaction.append_context_nodes[0].level == 0
            && transaction.append_context_nodes[1..]
                .iter()
                .all(|node| node.level > 0)
        {
            const FANOUT: usize = 4;
            let current_frontier = self.context_frontier.as_ref().ok_or_else(|| {
                anyhow::anyhow!("atomic hierarchy carry requires an existing frontier")
            })?;
            let leaf = &transaction.append_context_nodes[0];
            let covered = transaction.frontier.covered_message_count;
            let source_start = current_frontier.covered_message_count;
            if covered <= source_start || covered > active_messages.len() {
                anyhow::bail!("atomic leaf-parent transaction covers an invalid message delta");
            }
            let source = &active_messages[source_start..covered];
            let source_prefix = &active_messages[..covered];
            let source_ids = source
                .iter()
                .map(|message| message.id.clone())
                .collect::<Vec<_>>();
            let source_sha256 = stored_messages_sha256(source)?;
            if transaction.input_proof.source_message_ids != source_ids
                || transaction.input_proof.source_sha256 != source_sha256
                || leaf.source_message_ids != source_ids
                || leaf.source_sha256 != source_sha256
                || transaction.frontier.source_prefix_sha256
                    != stored_messages_sha256(source_prefix)?
                || transaction.frontier.covered_through_message_id.as_deref()
                    != source_prefix.last().map(|message| message.id.as_str())
            {
                anyhow::bail!("atomic leaf-parent canonical source proof is invalid");
            }
            let mut expected_frontier = current_frontier.active_node_ids.clone();
            let mut carried_id = leaf.id.clone();
            for (parent_offset, parent) in transaction.append_context_nodes[1..].iter().enumerate()
            {
                if expected_frontier.len() < FANOUT - 1 {
                    anyhow::bail!(
                        "atomic hierarchy carry lacks three active prior level-{} children",
                        parent.level.saturating_sub(1)
                    );
                }
                let retained = expected_frontier.len() - (FANOUT - 1);
                let mut expected_child_ids = expected_frontier[retained..].to_vec();
                expected_child_ids.push(carried_id.clone());
                if parent.child_node_ids != expected_child_ids {
                    anyhow::bail!(
                        "atomic hierarchy parent children are not the adjacent frontier suffix"
                    );
                }
                let available_new_nodes = &transaction.append_context_nodes[..=parent_offset];
                let children = expected_child_ids
                    .iter()
                    .map(|child_id| {
                        self.context_nodes
                            .iter()
                            .chain(available_new_nodes.iter())
                            .find(|node| node.id == *child_id)
                            .ok_or_else(|| {
                                anyhow::anyhow!("context child node {child_id} is missing")
                            })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                if children
                    .iter()
                    .any(|child| child.level != children[0].level)
                    || parent.level != children[0].level.saturating_add(1)
                {
                    anyhow::bail!("atomic parent must directly exceed four equal-level children");
                }
                let children_sha256 =
                    format!("{:x}", Sha256::digest(serde_json::to_vec(&children)?));
                let mut seen = std::collections::HashSet::new();
                let expected_source_ids = children
                    .iter()
                    .flat_map(|child| child.source_message_ids.iter().cloned())
                    .filter(|id| seen.insert(id.clone()))
                    .collect::<Vec<_>>();
                if parent.source_sha256 != children_sha256
                    || parent.source_message_ids != expected_source_ids
                {
                    anyhow::bail!("atomic parent source proof does not match its children");
                }
                expected_frontier.truncate(retained);
                carried_id = parent.id.clone();
            }
            expected_frontier.push(carried_id);
            if transaction.frontier.active_node_ids != expected_frontier {
                anyhow::bail!("atomic hierarchy root was not installed as the frontier suffix");
            }
        } else {
            let current_frontier = self.context_frontier.as_ref().ok_or_else(|| {
                anyhow::anyhow!("hierarchical transaction requires an existing frontier")
            })?;
            if transaction.frontier.covered_message_count != current_frontier.covered_message_count
                || transaction.frontier.covered_through_message_id
                    != current_frontier.covered_through_message_id
                || transaction.frontier.source_prefix_sha256
                    != current_frontier.source_prefix_sha256
            {
                anyhow::bail!("hierarchical transaction cannot change canonical source coverage");
            }
            if transaction.append_context_nodes.len() != 1 {
                anyhow::bail!(
                    "hierarchical context transactions must append exactly one parent node"
                );
            }
            let parent = &transaction.append_context_nodes[0];
            if parent.level == 0 || parent.child_node_ids.is_empty() {
                anyhow::bail!("hierarchical context node must reference child nodes");
            }
            let children = parent
                .child_node_ids
                .iter()
                .map(|child_id| {
                    self.context_nodes
                        .iter()
                        .find(|node| node.id == *child_id)
                        .ok_or_else(|| anyhow::anyhow!("context child node {child_id} is missing"))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if children.iter().any(|child| child.level >= parent.level) {
                anyhow::bail!("context parent level must exceed every child level");
            }
            if transaction.input_proof.source_message_ids != parent.child_node_ids {
                anyhow::bail!("hierarchical input proof does not match child node order");
            }
            let children_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&children)?));
            if transaction.input_proof.source_sha256 != children_sha256
                || parent.source_sha256 != children_sha256
            {
                anyhow::bail!("hierarchical input proof hash does not match child nodes");
            }
            let mut seen = std::collections::HashSet::new();
            let expected_source_ids = children
                .iter()
                .flat_map(|child| child.source_message_ids.iter().cloned())
                .filter(|id| seen.insert(id.clone()))
                .collect::<Vec<_>>();
            if parent.source_message_ids != expected_source_ids {
                anyhow::bail!("context parent raw source coverage does not match children");
            }
        }
        if persist && self.persist_state.pending_context_transaction.is_some() {
            anyhow::bail!("a context transaction is already pending persistence");
        }

        let mut candidate = self.context_nodes.clone();
        for node in &transaction.append_context_nodes {
            match candidate.iter().find(|existing| existing.id == node.id) {
                Some(existing) if existing == node => {}
                Some(_) => anyhow::bail!("conflicting duplicate context node id {}", node.id),
                None => candidate.push(node.clone()),
            }
        }
        validate_context_graph(&candidate, Some(&transaction.frontier))
            .map_err(anyhow::Error::msg)?;
        validate_context_node_source_proofs(
            &self.id,
            self.exact_runtime_identity.as_ref(),
            &active_messages,
            &candidate,
            Some(&transaction.frontier),
        )
        .map_err(anyhow::Error::msg)?;
        validate_context_frontier_coverage(&active_messages, &candidate, &transaction.frontier)
            .map_err(anyhow::Error::msg)?;

        drop(active_messages);
        self.context_nodes = candidate;
        self.context_frontier = Some(transaction.frontier.clone());
        self.last_context_op_id = Some(transaction.op_id.clone());
        self.last_context_op_sha256 = Some(transaction_sha256);
        if persist {
            self.persist_state.pending_context_transaction = Some(transaction.clone());
        }
        Ok(true)
    }

    /// Stage one immutable graph delta for journal and recovery tests. Runtime
    /// callers must use `commit_context_graph_transaction`, which persists a
    /// cloned candidate before publishing it to this live session.
    #[cfg(test)]
    pub(crate) fn apply_context_graph_transaction(
        &mut self,
        transaction: ContextGraphTransaction,
    ) -> anyhow::Result<bool> {
        self.apply_context_transaction_inner(&transaction, true)
    }

    pub(super) fn discard_invalid_context_graph(&mut self) {
        let canonical_ids = self
            .messages
            .iter()
            .map(|message| message.id.as_str())
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        let invalid_archive = self
            .archived_message_ids
            .iter()
            .any(|id| !canonical_ids.contains(id.as_str()) || !seen.insert(id.as_str()));
        drop(seen);
        drop(canonical_ids);
        if invalid_archive {
            crate::logging::warn(&format!(
                "Archived branch metadata is invalid for session {}; failing closed with an empty active branch",
                self.id
            ));
            self.archived_message_ids = self
                .messages
                .iter()
                .map(|message| message.id.clone())
                .collect();
            self.reset_provider_messages_cache();
        }
        let active_messages = self.active_stored_messages();
        let has_native_graph = !self.context_nodes.is_empty() || self.context_frontier.is_some();
        let graph_validation =
            validate_context_graph(&self.context_nodes, self.context_frontier.as_ref()).and_then(
                |()| {
                    if has_native_graph {
                        validate_context_node_source_proofs(
                            &self.id,
                            self.exact_runtime_identity.as_ref(),
                            &active_messages,
                            &self.context_nodes,
                            self.context_frontier.as_ref(),
                        )?;
                        validate_imported_context_ancestor_proofs(self)
                    } else {
                        Ok(())
                    }
                },
            );
        if let Err(err) = graph_validation {
            let imported_provenance_failure = err.contains("imported context node");
            crate::logging::warn(&format!(
                "{} derived context graph for session {}: {}",
                if imported_provenance_failure {
                    "Deactivating unverifiable imported"
                } else {
                    "Discarding corrupt"
                },
                self.id,
                err
            ));
            drop(active_messages);
            if imported_provenance_failure {
                self.deactivate_context_graph_state();
            } else {
                // Structurally or cryptographically invalid bytes are not valid
                // committed nodes and retaining them would poison every future
                // graph transaction. This differs from imported ancestor proof
                // failure, which preserves otherwise-valid immutable bytes for
                // forensics and deactivates only the projection/frontier.
                self.compaction = None;
                self.clear_context_graph_state();
            }
            return;
        }
        if let Some(frontier) = self.context_frontier.as_ref()
            && let Err(err) =
                validate_context_frontier_coverage(&active_messages, &self.context_nodes, frontier)
        {
            crate::logging::warn(&format!(
                "Deactivating context graph with invalid transcript coverage for session {}: {}",
                self.id, err
            ));
            drop(active_messages);
            self.deactivate_context_graph_state();
            return;
        }
        drop(active_messages);
        if self.context_frontier.is_some()
            && self.compaction.is_some()
            && !self.has_owned_native_lcm_projection()
        {
            crate::logging::warn(&format!(
                "Deactivating unauthenticated or inconsistent derived context projection for session {}",
                self.id
            ));
            self.deactivate_context_graph_state();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_validate_context_graph_state(&self) -> Result<(), String> {
        let active_messages = self.active_stored_messages();
        validate_context_graph(&self.context_nodes, self.context_frontier.as_ref())?;
        validate_context_node_source_proofs(
            &self.id,
            self.exact_runtime_identity.as_ref(),
            &active_messages,
            &self.context_nodes,
            self.context_frontier.as_ref(),
        )?;
        validate_imported_context_ancestor_proofs(self)?;
        if let Some(frontier) = self.context_frontier.as_ref() {
            validate_context_frontier_coverage(&active_messages, &self.context_nodes, frontier)?;
        }
        Ok(())
    }

    pub(super) fn reset_provider_messages_cache(&mut self) {
        self.provider_messages_cache.clear();
        self.provider_message_prefix_hashes_cache.clear();
        self.provider_messages_cache_len = 0;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
        self.memory_profile_cache.provider_cache_count = 0;
        self.memory_profile_cache.provider_cache_json_bytes = 0;
        self.memory_profile_cache.provider_cache_stats = ContentBlockMemoryStats::default();
    }

    /// Drop the derived provider-facing transcript once the current request has
    /// copied the messages it needs. The canonical [`StoredMessage`] history is
    /// still retained, so this cache can be rebuilt on the next provider call.
    ///
    /// Long-running server sessions otherwise keep two fully owned transcript
    /// copies while waiting on the network. Tool results and reasoning payloads
    /// can make that duplicate tens of MiB per active session.
    pub fn release_provider_messages_cache(&mut self) {
        self.provider_messages_cache = Vec::new();
        self.provider_message_prefix_hashes_cache = Vec::new();
        self.provider_messages_cache_len = 0;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
        self.memory_profile_cache.provider_cache_count = 0;
        self.memory_profile_cache.provider_cache_json_bytes = 0;
        self.memory_profile_cache.provider_cache_stats = ContentBlockMemoryStats::default();
    }

    pub(super) fn push_provider_message_cache_entry(&mut self, message: Message) {
        let message_hash = crate::message::stable_message_hash(&message);
        let prefix_hash = self
            .provider_message_prefix_hashes_cache
            .last()
            .copied()
            .map(|prev| crate::message::extend_stable_hash(prev, message_hash))
            .unwrap_or(message_hash);
        self.memory_profile_cache.provider_cache_count += 1;
        self.memory_profile_cache.provider_cache_json_bytes += estimate_json_bytes(&message);
        self.memory_profile_cache
            .provider_cache_stats
            .merge_from(&summarize_blocks(&message.content));
        self.provider_messages_cache.push(message);
        self.provider_message_prefix_hashes_cache.push(prefix_hash);
    }

    pub(super) fn mark_memory_profile_dirty(&mut self) {
        self.memory_profile_dirty = true;
    }

    pub(super) fn rebuild_memory_profile_cache(&mut self) {
        let message_stats =
            summarize_message_content(self.messages.iter().map(|message| &message.content));
        let provider_cache_stats = summarize_message_content(
            self.provider_messages_cache
                .iter()
                .map(|message| &message.content),
        );

        self.memory_profile_cache = SessionMemoryProfileCache {
            messages_count: self.messages.len(),
            messages_json_bytes: self.messages.iter().map(estimate_json_bytes).sum(),
            message_stats,
            env_snapshots_count: self.env_snapshots.len(),
            env_snapshots_json_bytes: self.env_snapshots.iter().map(estimate_json_bytes).sum(),
            memory_injections_count: self.memory_injections.len(),
            memory_injections_json_bytes: self
                .memory_injections
                .iter()
                .map(estimate_json_bytes)
                .sum(),
            replay_events_count: self.replay_events.len(),
            replay_events_json_bytes: self.replay_events.iter().map(estimate_json_bytes).sum(),
            provider_cache_count: self.provider_messages_cache.len(),
            provider_cache_json_bytes: self
                .provider_messages_cache
                .iter()
                .map(estimate_json_bytes)
                .sum(),
            provider_cache_stats,
        };
        self.memory_profile_dirty = false;
    }

    pub(super) fn ensure_memory_profile_cache(&mut self) {
        if self.memory_profile_dirty {
            self.rebuild_memory_profile_cache();
        }
    }

    pub fn memory_profile_snapshot(&mut self) -> SessionMemoryProfileSnapshot {
        self.ensure_memory_profile_cache();
        let compaction_json_bytes = self
            .compaction
            .as_ref()
            .map(estimate_json_bytes)
            .unwrap_or(0);

        SessionMemoryProfileSnapshot {
            message_count: self.memory_profile_cache.messages_count,
            provider_cache_message_count: self.memory_profile_cache.provider_cache_count,
            env_snapshot_count: self.memory_profile_cache.env_snapshots_count,
            memory_injection_count: self.memory_profile_cache.memory_injections_count,
            replay_event_count: self.memory_profile_cache.replay_events_count,
            payload_text_bytes: self.memory_profile_cache.message_stats.payload_text_bytes(),
            total_json_bytes: self.memory_profile_cache.messages_json_bytes
                + self.memory_profile_cache.provider_cache_json_bytes
                + self.memory_profile_cache.env_snapshots_json_bytes
                + self.memory_profile_cache.memory_injections_json_bytes
                + self.memory_profile_cache.replay_events_json_bytes
                + compaction_json_bytes,
            provider_cache_json_bytes: self.memory_profile_cache.provider_cache_json_bytes,
            canonical_tool_result_bytes: self.memory_profile_cache.message_stats.tool_result_bytes,
            provider_cache_tool_result_bytes: self
                .memory_profile_cache
                .provider_cache_stats
                .tool_result_bytes,
            canonical_large_blob_bytes: self.memory_profile_cache.message_stats.large_block_bytes,
            provider_cache_large_blob_bytes: self
                .memory_profile_cache
                .provider_cache_stats
                .large_block_bytes,
        }
    }

    pub(super) fn mark_messages_append_dirty(&mut self) {
        if self.persist_state.messages_mode != PersistVectorMode::Full {
            self.persist_state.messages_mode = PersistVectorMode::Append;
        }
        // Image suppression is a whole-transcript character budget. Appending
        // another image can require suppressing an older cached image, so the
        // provider projection must be rebuilt globally rather than extending
        // the old projected cache with one raw message.
        if self.provider_image_char_budget.is_some() {
            self.provider_messages_cache_mode = PersistVectorMode::Full;
        } else if self.provider_messages_cache_mode != PersistVectorMode::Full {
            self.provider_messages_cache_mode = PersistVectorMode::Append;
        }
    }

    pub(super) fn mark_messages_full_dirty(&mut self) {
        self.persist_state.messages_mode = PersistVectorMode::Full;
        self.provider_messages_cache_mode = PersistVectorMode::Full;
    }

    pub(super) fn mark_env_snapshots_append_dirty(&mut self) {
        if self.persist_state.env_snapshots_mode != PersistVectorMode::Full {
            self.persist_state.env_snapshots_mode = PersistVectorMode::Append;
        }
    }

    pub(super) fn mark_env_snapshots_full_dirty(&mut self) {
        self.persist_state.env_snapshots_mode = PersistVectorMode::Full;
    }

    pub(super) fn mark_memory_injections_append_dirty(&mut self) {
        if self.persist_state.memory_injections_mode != PersistVectorMode::Full {
            self.persist_state.memory_injections_mode = PersistVectorMode::Append;
        }
    }

    pub(super) fn mark_replay_events_append_dirty(&mut self) {
        if self.persist_state.replay_events_mode != PersistVectorMode::Full {
            self.persist_state.replay_events_mode = PersistVectorMode::Append;
        }
    }

    pub(super) fn apply_journal_meta(&mut self, meta: SessionJournalMeta) {
        self.parent_id = meta.parent_id;
        self.title = meta.title;
        self.custom_title = meta.custom_title;
        self.updated_at = meta.updated_at;
        self.archived_message_ids = meta.archived_message_ids;
        self.compaction = meta.compaction;
        self.provider_session_id = meta.provider_session_id;
        self.provider_session_identity = meta.provider_session_identity;
        self.provider_key = meta.provider_key;
        self.route_api_method = meta.route_api_method;
        self.model = meta.model;
        self.reasoning_effort = meta.reasoning_effort;
        self.exact_runtime_identity = meta.exact_runtime_identity;
        self.subagent_model = meta.subagent_model;
        self.improve_mode = meta.improve_mode;
        self.autoreview_enabled = meta.autoreview_enabled;
        self.autojudge_enabled = meta.autojudge_enabled;
        self.is_canary = meta.is_canary;
        self.testing_build = meta.testing_build;
        self.working_dir = meta.working_dir;
        self.short_name = meta.short_name;
        self.status = meta.status;
        self.last_pid = meta.last_pid;
        self.last_active_at = meta.last_active_at;
        self.is_debug = meta.is_debug;
        self.saved = meta.saved;
        self.save_label = meta.save_label;
        let next_revision = self.persistence_revision.saturating_add(1);
        self.persistence_revision = meta
            .persistence_revision
            .map_or(next_revision, |revision| next_revision.max(revision));
        self.mark_memory_profile_dirty();
    }
}
