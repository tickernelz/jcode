use super::*;

impl Agent {
    pub(crate) fn persisted_serving_model(&self, resolved_model: &str) -> String {
        self.session
            .exact_runtime_identity
            .as_ref()
            .filter(|identity| identity.route.model == resolved_model)
            .map(|identity| {
                crate::provider::persisted_session_model_for_route(&identity.route, resolved_model)
            })
            .unwrap_or_else(|| resolved_model.to_string())
    }

    fn acquire_compaction_identity_transition(
        &self,
    ) -> Result<tokio::sync::OwnedRwLockWriteGuard<crate::compaction::CompactionManager>> {
        self.registry.compaction().try_write_owned().map_err(|_| {
            anyhow::anyhow!(
                "Cannot reset compaction safely after identity transition because compaction is busy"
            )
        })
    }

    fn reset_compaction_after_identity_transition(
        &mut self,
        manager: &mut crate::compaction::CompactionManager,
    ) {
        let active_messages = self.session.active_stored_messages().into_owned();
        manager.reset();
        manager.set_budget(self.provider.context_window());
        manager.seed_restored_stored_messages_with(&active_messages);
    }

    pub(crate) fn ensure_tool_identity_ready(&self) -> Result<()> {
        crate::server::provider_control::reconcile_pending_account_transition_for_admission(
            &self.provider,
        )?;
        let persisted = self.session.exact_runtime_identity.as_ref();
        let effective = self.provider.exact_runtime_identity();
        if !persisted.is_some_and(|identity| identity.has_verifiable_account_binding())
            || !effective
                .as_ref()
                .is_some_and(|identity| identity.has_verifiable_account_binding())
            || persisted != effective.as_ref()
        {
            anyhow::bail!(
                "Tool execution is disabled because exact provider/account identity is opaque, incomplete, or stale"
            );
        }
        Ok(())
    }

    pub(crate) fn reconcile_exact_runtime_identity_after_request_open(
        &mut self,
        previous: Option<&jcode_provider_core::ExactRuntimeIdentity>,
    ) -> Result<()> {
        let current = self.provider.exact_runtime_identity();
        if current.as_ref() == previous {
            return Ok(());
        }

        // A provider-side retry may have changed credentials before returning
        // the stream. Never retain a resume ID across that transition, even if
        // this provider does not emit a replacement session ID.
        self.provider_session_id = None;
        self.session.provider_session_id = None;
        self.session.provider_session_identity = None;

        let Some(current) = current else {
            let error = "Provider identity became unverifiable while opening the request";
            self.provider_identity_error = Some(error.to_string());
            self.session.save()?;
            anyhow::bail!(error);
        };
        let Some(previous) = previous else {
            let error = format!(
                "Provider identity changed unexpectedly while opening the request: {:?}",
                current
            );
            self.provider_identity_error = Some(error.clone());
            self.session.save()?;
            anyhow::bail!(error);
        };

        let serving_model_changed = previous.route.model != current.route.model;
        let same_runtime_route = previous.provider_key == current.provider_key
            && previous.route.runtime_key == current.route.runtime_key
            && previous.route.api_method == current.route.api_method
            && previous.route.provider_label == current.route.provider_label
            && previous.route.detail == current.route.detail
            && previous.reasoning_effort == current.reasoning_effort
            && (previous.route.model != current.route.model
                || previous.account_label != current.account_label
                || previous.account_id != current.account_id
                || previous.account_generation != current.account_generation);
        if !same_runtime_route || !current.has_verifiable_account_binding() {
            let error = format!(
                "Provider route identity changed unexpectedly while opening the request: expected {:?}, effective {:?}",
                previous, current
            );
            self.provider_identity_error = Some(error.clone());
            anyhow::bail!(error);
        }

        crate::logging::warn(&format!(
            "Accepted verified same-runtime serving identity transition for session {}; provider resume state was cleared",
            self.session.id
        ));
        let mut compaction = self.acquire_compaction_identity_transition()?;
        let previous_session = self.session.clone();
        if serving_model_changed {
            self.session.model = Some(crate::provider::persisted_session_model_for_route(
                &current.route,
                &current.route.model,
            ));
        }
        self.session.exact_runtime_identity = Some(current.clone());
        self.session.reset_context_graph_for_identity_transition();
        if let Err(first_error) = self.session.save() {
            // Another process may have reconciled the durable session while this
            // Agent still held the pre-transition revision. Reload that canonical
            // state, reapply the exact transition, and retry under its fresh CAS.
            self.session = match crate::session::Session::load(&previous_session.id) {
                Ok(mut fresh) => {
                    if fresh
                        .exact_runtime_identity
                        .as_ref()
                        .is_some_and(|identity| identity != previous && identity != &current)
                    {
                        let error = anyhow::anyhow!(
                            "automatic account transition lost CAS to a newer or divergent durable identity"
                        );
                        self.provider_identity_error = Some(error.to_string());
                        return Err(error);
                    }
                    if serving_model_changed {
                        fresh.model = Some(crate::provider::persisted_session_model_for_route(
                            &current.route,
                            &current.route.model,
                        ));
                    }
                    fresh.exact_runtime_identity = Some(current.clone());
                    fresh.provider_session_id = None;
                    fresh.provider_session_identity = None;
                    fresh.reset_context_graph_for_identity_transition();
                    if let Err(retry_error) = fresh.save() {
                        self.provider_identity_error = Some(format!(
                            "Failed to persist automatic account transition: {first_error}; retry: {retry_error}"
                        ));
                        return Err(retry_error);
                    }
                    fresh
                }
                Err(reload_error) => {
                    self.session = previous_session;
                    self.provider_identity_error = Some(format!(
                        "Failed to persist automatic account transition: {first_error}; reload: {reload_error}"
                    ));
                    return Err(reload_error);
                }
            };
        }
        self.reset_compaction_after_identity_transition(&mut compaction);
        if serving_model_changed {
            self.provider_runtime_state.apply(
                crate::provider::ProviderStateEvent::RuntimeModelObserved {
                    model: self.provider.model(),
                },
            );
        }
        self.provider_identity_error = None;
        Ok(())
    }

    pub(crate) fn sync_exact_runtime_identity_from_provider(&mut self) {
        let current = self.provider.exact_runtime_identity();
        match (
            self.session.exact_runtime_identity.as_ref(),
            current.as_ref(),
        ) {
            (Some(expected), Some(actual)) if expected != actual => {
                self.provider_identity_error = Some(format!(
                    "Exact runtime identity mismatch for session {}: expected {:?}, effective {:?}",
                    self.session.id, expected, actual
                ));
            }
            (Some(_), None) => {
                self.provider_identity_error = Some(format!(
                    "Provider {} cannot verify the exact persisted runtime identity for session {}",
                    self.provider.name(),
                    self.session.id
                ));
            }
            (None, Some(actual)) => {
                self.provider_session_id = None;
                self.session.provider_session_id = None;
                self.session.provider_session_identity = None;
                self.session.reset_context_graph_for_identity_transition();
                self.session.exact_runtime_identity = Some(actual.clone());
            }
            _ => {}
        }

        if self.session.provider_session_id.is_some()
            && (!current
                .as_ref()
                .is_some_and(|identity| identity.has_verifiable_account_binding())
                || self.session.provider_session_identity != current)
        {
            crate::logging::warn(&format!(
                "Discarding provider session id for {} because its exact runtime binding is missing or stale",
                self.session.id
            ));
            self.session.provider_session_id = None;
            self.session.provider_session_identity = None;
            self.provider_session_id = None;
        } else {
            self.provider_session_id = self.session.provider_session_id.clone();
        }
    }

    #[cfg(test)]
    pub(crate) fn bind_provider_session_id(&mut self, session_id: String) -> bool {
        let identity = self.provider.exact_runtime_identity();
        self.bind_provider_session_id_for_identity(session_id, identity.as_ref())
    }

    pub(crate) fn bind_provider_session_id_for_identity(
        &mut self,
        session_id: String,
        stream_identity: Option<&jcode_provider_core::ExactRuntimeIdentity>,
    ) -> bool {
        let Some(identity) = stream_identity else {
            crate::logging::warn(
                "Ignoring provider session id because the provider cannot expose exact runtime identity",
            );
            self.provider_session_id = None;
            self.session.provider_session_id = None;
            self.session.provider_session_identity = None;
            return false;
        };
        if !identity.has_verifiable_account_binding() {
            crate::logging::warn(
                "Ignoring provider session id because the active credential has no verifiable account identity",
            );
            self.provider_session_id = None;
            self.session.provider_session_id = None;
            self.session.provider_session_identity = None;
            return false;
        }
        if self.session.exact_runtime_identity.as_ref() != Some(identity) {
            crate::logging::warn(
                "Ignoring provider session id because the response stream identity does not match the admitted session identity",
            );
            self.provider_session_id = None;
            self.session.provider_session_id = None;
            self.session.provider_session_identity = None;
            return false;
        }
        self.session.provider_session_identity = Some(identity.clone());
        self.provider_session_id = Some(session_id.clone());
        self.session.provider_session_id = Some(session_id);
        true
    }

    pub fn set_premium_mode(&self, mode: crate::provider::copilot::PremiumMode) {
        self.provider.set_premium_mode(mode);
    }

    pub fn premium_mode(&self) -> crate::provider::copilot::PremiumMode {
        self.provider.premium_mode()
    }

    pub fn provider_fork(&self) -> Arc<dyn Provider> {
        self.provider.fork()
    }

    pub fn provider_handle(&self) -> Arc<dyn Provider> {
        Arc::clone(&self.provider)
    }

    pub fn provider_session_id(&self) -> Option<&str> {
        self.provider_session_id.as_deref()
    }

    pub fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        self.provider.exact_runtime_identity()
    }

    pub(crate) fn account_transition_applies_to(
        &self,
        runtime_key: &jcode_provider_core::RuntimeKey,
    ) -> bool {
        let current = self.provider.exact_runtime_identity();
        current
            .as_ref()
            .is_some_and(|identity| &identity.route.runtime_key == runtime_key)
            || self
                .session
                .exact_runtime_identity
                .as_ref()
                .is_some_and(|identity| &identity.route.runtime_key == runtime_key)
    }

    pub(crate) fn reconcile_pending_account_transition(
        &mut self,
        runtime_key: &jcode_provider_core::RuntimeKey,
        identity_matches_marker: impl Fn(&jcode_provider_core::ExactRuntimeIdentity) -> bool,
    ) -> Result<()> {
        if !self.account_transition_applies_to(runtime_key) {
            return Ok(());
        }
        let current = self.provider.exact_runtime_identity();
        let Some(identity) = current else {
            let error = format!(
                "Provider {} cannot verify the new account identity after an explicit account transition",
                self.provider.name()
            );
            self.provider_identity_error = Some(error.clone());
            anyhow::bail!(error);
        };
        if !identity.has_verifiable_account_binding() {
            let error = format!(
                "Provider {} returned an incomplete account identity after an explicit account transition",
                self.provider.name()
            );
            self.provider_identity_error = Some(error.clone());
            anyhow::bail!(error);
        }
        if !identity_matches_marker(&identity) {
            let error = format!(
                "Provider {} identity does not match the pending explicit account transition",
                self.provider.name()
            );
            self.provider_identity_error = Some(error.clone());
            anyhow::bail!(error);
        }
        let mut compaction = self.acquire_compaction_identity_transition()?;

        let already_durable = self.session.exact_runtime_identity.as_ref() == Some(&identity)
            && self.session.provider_session_id.is_none()
            && self.session.provider_session_identity.is_none()
            && self.session.compaction.is_none()
            && self
                .session
                .context_frontier
                .as_ref()
                .is_none_or(|frontier| {
                    frontier.active_node_ids.is_empty() && frontier.covered_message_count == 0
                });
        if already_durable {
            self.provider_session_id = None;
            self.reset_compaction_after_identity_transition(&mut compaction);
            self.provider_identity_error = None;
            return Ok(());
        }

        let previous = self.session.clone();
        self.session.exact_runtime_identity = Some(identity.clone());
        self.session.provider_session_id = None;
        self.session.provider_session_identity = None;
        self.provider_session_id = None;
        self.session.reset_context_graph_for_identity_transition();
        if let Err(first_error) = self.session.save_during_account_transition() {
            // A peer may have advanced the durable CAS revision after this Agent
            // loaded. While holding the Agent lock, merge from that fresh durable
            // snapshot and retry so reconciliation can actually converge.
            self.session = match crate::session::Session::load(&previous.id) {
                Ok(mut fresh) => {
                    fresh.exact_runtime_identity = Some(identity);
                    fresh.provider_session_id = None;
                    fresh.provider_session_identity = None;
                    fresh.reset_context_graph_for_identity_transition();
                    if let Err(retry_error) = fresh.save_during_account_transition() {
                        self.provider_identity_error = Some(format!(
                            "Failed to persist the new exact account identity: {first_error}; retry: {retry_error}"
                        ));
                        return Err(retry_error);
                    }
                    fresh
                }
                Err(load_error) => {
                    self.session = previous;
                    self.provider_identity_error = Some(format!(
                        "Failed to persist the new exact account identity: {first_error}; reload: {load_error}"
                    ));
                    return Err(load_error);
                }
            };
        }
        self.reset_compaction_after_identity_transition(&mut compaction);
        self.provider_identity_error = None;
        Ok(())
    }

    pub fn durable_provider_session_id(&self) -> Option<&str> {
        self.session.provider_session_id.as_deref()
    }

    pub fn available_models(&self) -> Vec<&'static str> {
        self.provider.available_models()
    }

    pub fn available_models_for_switching(&self) -> Vec<String> {
        self.provider.available_models_for_switching()
    }

    pub fn available_models_display(&self) -> Vec<String> {
        self.provider.available_models_display()
    }

    pub fn model_routes(&self) -> Vec<crate::provider::ModelRoute> {
        self.provider.model_routes()
    }

    pub fn model_catalog_snapshot(&self) -> jcode_provider_core::ModelCatalogSnapshot {
        jcode_provider_core::ModelCatalogSnapshot::new(
            Some(self.provider_name()),
            Some(self.provider_model()),
            self.available_models_display(),
            self.model_routes(),
        )
    }

    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }

    pub async fn compaction_mode(&self) -> crate::config::CompactionMode {
        self.registry.compaction().read().await.mode()
    }

    pub async fn set_compaction_mode(&self, mode: crate::config::CompactionMode) -> Result<()> {
        let compaction = self.registry.compaction();
        let mut manager = compaction.write().await;
        manager.set_mode(mode);
        Ok(())
    }

    pub fn provider_messages(&mut self) -> Vec<Message> {
        self.session.messages_for_provider()
    }

    pub fn set_model(&mut self, model: &str) -> Result<()> {
        self.set_model_from_provider_state_event(
            model,
            crate::provider::ProviderModelSelectionSource::User,
        )
    }

    pub fn set_route_selection(
        &mut self,
        selection: &crate::provider::RouteSelection,
    ) -> Result<()> {
        self.set_route_selection_from_provider_state_event(
            selection,
            crate::provider::ProviderModelSelectionSource::User,
        )
    }

    pub(crate) fn set_route_selection_from_auth(
        &mut self,
        selection: &crate::provider::RouteSelection,
    ) -> Result<()> {
        self.set_route_selection_from_provider_state_event(
            selection,
            crate::provider::ProviderModelSelectionSource::Auth,
        )
    }

    fn set_route_selection_from_provider_state_event(
        &mut self,
        selection: &crate::provider::RouteSelection,
        source: crate::provider::ProviderModelSelectionSource,
    ) -> Result<()> {
        let mut compaction = self.acquire_compaction_identity_transition()?;
        let previous_session = self.session.clone();
        let previous_provider_model = self.provider.model();
        self.provider.set_route_selection(selection)?;
        let resolved_model = self.provider.model();
        self.session.provider_key = Some(selection.runtime_key.stable_id());
        self.session.route_api_method = Some(selection.api_method.clone());
        self.session.model = Some(crate::provider::persisted_session_model_for_route(
            selection,
            &resolved_model,
        ));
        self.session.provider_session_id = None;
        self.session.provider_session_identity = None;
        self.session.exact_runtime_identity = self.provider.exact_runtime_identity();
        self.session.reset_context_graph_for_identity_transition();
        if let Err(error) = self.session.save() {
            let rollback_request =
                crate::provider::MultiProvider::model_switch_request_for_session_route(
                    previous_session
                        .model
                        .as_deref()
                        .unwrap_or(&previous_provider_model),
                    previous_session.provider_key.as_deref(),
                    previous_session.route_api_method.as_deref(),
                );
            self.session = previous_session;
            if let Err(rollback) = crate::provider::set_model_with_auth_refresh(
                self.provider.as_ref(),
                &rollback_request,
            ) {
                self.provider_identity_error = Some(rollback.to_string());
                return Err(anyhow::anyhow!(
                    "{error}; failed to roll back exact provider route: {rollback}"
                ));
            }
            return Err(error);
        }
        self.reset_compaction_after_identity_transition(&mut compaction);
        self.provider_session_id = None;
        self.provider_identity_error = None;
        let event = crate::provider::ProviderStateEvent::selected_model(source, resolved_model);
        self.provider_runtime_state.apply(event);
        self.log_env_snapshot("set_route_selection");
        Ok(())
    }

    pub(crate) fn set_model_from_auth(&mut self, model: &str) -> Result<()> {
        self.set_model_from_provider_state_event(
            model,
            crate::provider::ProviderModelSelectionSource::Auth,
        )
    }

    fn set_model_from_provider_state_event(
        &mut self,
        model: &str,
        source: crate::provider::ProviderModelSelectionSource,
    ) -> Result<()> {
        let mut compaction = self.acquire_compaction_identity_transition()?;
        let previous_session = self.session.clone();
        let previous_model = self.provider.model();
        let previous_route_request =
            crate::provider::MultiProvider::model_switch_request_for_session_route(
                previous_session.model.as_deref().unwrap_or(&previous_model),
                previous_session.provider_key.as_deref(),
                previous_session.route_api_method.as_deref(),
            );
        crate::provider::set_model_with_auth_refresh(self.provider.as_ref(), model)?;
        let resolved_model = self.provider.model();
        self.session.provider_key =
            crate::provider::MultiProvider::session_provider_key_after_model_switch(
                model,
                self.provider.name(),
                self.session.provider_key.as_deref(),
            );
        self.session.route_api_method =
            crate::provider::MultiProvider::route_api_method_after_model_switch(
                model,
                self.session.route_api_method.as_deref(),
            );
        self.session.model = Some(resolved_model.clone());
        self.session.provider_session_id = None;
        self.session.provider_session_identity = None;
        self.session.exact_runtime_identity = self.provider.exact_runtime_identity();
        self.session.reset_context_graph_for_identity_transition();
        if let Err(error) = self.session.save() {
            self.session = previous_session;
            if let Err(rollback) = crate::provider::set_model_with_auth_refresh(
                self.provider.as_ref(),
                &previous_route_request,
            ) {
                self.provider_identity_error = Some(rollback.to_string());
                return Err(anyhow::anyhow!(
                    "{error}; failed to roll back exact provider route: {rollback}"
                ));
            }
            return Err(error);
        }
        self.reset_compaction_after_identity_transition(&mut compaction);
        self.provider_session_id = None;
        self.provider_identity_error = None;
        let event = crate::provider::ProviderStateEvent::selected_model(source, resolved_model);
        self.provider_runtime_state.apply(event);
        self.log_env_snapshot("set_model");
        Ok(())
    }

    pub(crate) fn provider_model_selection_generation(&self) -> u64 {
        self.provider_runtime_state.selection_generation()
    }

    pub(crate) fn user_selected_provider_model_after(&self, generation: u64) -> bool {
        self.provider_runtime_state.user_selected_after(generation)
    }

    pub fn restore_reasoning_effort_from_session(&mut self) {
        if let Some(effort) = self.session.reasoning_effort.clone() {
            if let Err(e) = self.provider.set_reasoning_effort(&effort) {
                let error =
                    format!("Failed to restore exact session reasoning effort '{effort}': {e}");
                crate::logging::error(&error);
                self.provider_identity_error = Some(error);
            }
        } else {
            self.session.reasoning_effort = self.provider.reasoning_effort();
        }
        // Mirror the effort into the deadlock-free side-table so server handlers
        // (e.g. the swarm seed handler) can learn this session's effort without
        // taking the agent lock.
        crate::session_effort::record_session_effort(
            &self.session.id,
            self.session.reasoning_effort.as_deref(),
        );
    }

    pub fn set_reasoning_effort(&mut self, effort: &str) -> Result<Option<String>> {
        let mut compaction = self.acquire_compaction_identity_transition()?;
        let previous = self.provider.reasoning_effort();
        self.provider.set_reasoning_effort(effort)?;
        let current = self.provider.reasoning_effort();
        let mut candidate = self.session.clone();
        candidate.reasoning_effort = current.clone();
        candidate.exact_runtime_identity = self.provider.exact_runtime_identity();
        candidate.provider_session_id = None;
        candidate.provider_session_identity = None;
        candidate.reset_context_graph_for_identity_transition();
        if let Err(error) = candidate.save() {
            if let Err(rollback) = self
                .provider
                .set_reasoning_effort(previous.as_deref().unwrap_or(""))
            {
                self.provider_identity_error = Some(rollback.to_string());
                return Err(anyhow::anyhow!(
                    "{error}; failed to roll back reasoning effort: {rollback}"
                ));
            }
            return Err(error);
        }
        self.session = candidate;
        self.reset_compaction_after_identity_transition(&mut compaction);
        self.provider_session_id = None;
        // Keep the side-table in sync (see `restore_reasoning_effort_from_session`).
        crate::session_effort::record_session_effort(&self.session.id, current.as_deref());
        self.log_env_snapshot("set_reasoning_effort");
        Ok(current)
    }

    pub fn subagent_model(&self) -> Option<String> {
        self.session.subagent_model.clone()
    }

    pub fn set_subagent_model(&mut self, model: Option<String>) -> Result<()> {
        self.session.subagent_model = model;
        self.log_env_snapshot("set_subagent_model");
        self.session.save()?;
        Ok(())
    }

    pub fn session_provider_key(&self) -> Option<String> {
        self.session.provider_key.clone()
    }

    /// API method/runtime route used to select the active model (e.g.
    /// "openai-api", "claude-oauth", "openai-compatible:nvidia-nim"). Spawned
    /// swarm agents inherit this so they reconstruct the coordinator's exact
    /// auth route instead of falling back to the config default.
    pub fn session_route_api_method(&self) -> Option<String> {
        self.session.route_api_method.clone()
    }

    /// The credential the active provider will use for the next request, when
    /// the provider distinguishes OAuth (subscription) from API key (cost).
    /// Resolved authoritatively here so remote clients can render billing/usage
    /// without re-deriving it from the provider name.
    pub fn active_resolved_credential(&self) -> Option<jcode_provider_core::ResolvedCredential> {
        self.provider.active_resolved_credential()
    }

    pub fn set_session_provider_key(&mut self, provider_key: Option<String>) {
        self.session.provider_key = provider_key;
    }

    pub fn rename_session_title(&mut self, title: Option<String>) -> Result<String> {
        self.session.rename_title(title);
        self.log_env_snapshot("rename_session");
        self.session.save()?;
        Ok(self.session.display_title_or_name().to_string())
    }

    pub fn autoreview_enabled(&self) -> Option<bool> {
        self.session.autoreview_enabled
    }

    pub fn set_autoreview_enabled(&mut self, enabled: bool) -> Result<()> {
        self.session.autoreview_enabled = Some(enabled);
        self.log_env_snapshot("set_autoreview_enabled");
        self.session.save()?;
        Ok(())
    }

    pub fn autojudge_enabled(&self) -> Option<bool> {
        self.session.autojudge_enabled
    }

    pub fn set_autojudge_enabled(&mut self, enabled: bool) -> Result<()> {
        self.session.autojudge_enabled = Some(enabled);
        self.log_env_snapshot("set_autojudge_enabled");
        self.session.save()?;
        Ok(())
    }

    /// Set the working directory for this session
    pub fn set_working_dir(&mut self, dir: &str) {
        if self.session.working_dir.as_deref() == Some(dir) {
            return;
        }
        self.session.working_dir = Some(dir.to_string());
        self.session.refresh_initial_session_context_message();
        self.log_env_snapshot("working_dir");
    }

    /// Get the working directory for this session
    pub fn working_dir(&self) -> Option<&str> {
        self.session.working_dir.as_deref()
    }

    /// Get the stored messages (for transcript export)
    pub fn messages(&self) -> &[StoredMessage] {
        &self.session.messages
    }
}
