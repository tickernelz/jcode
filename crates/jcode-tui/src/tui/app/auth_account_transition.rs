use super::*;

impl App {
    pub(in super::super) fn prepare_local_account_switch(
        &mut self,
        runtime_key: jcode_provider_core::RuntimeKey,
        label: &str,
    ) -> anyhow::Result<()> {
        if self.is_processing {
            anyhow::bail!(
                "cannot switch accounts during an active model turn; retry after it finishes"
            );
        }
        crate::provider::ensure_no_account_transition()?;
        crate::server::ensure_no_pending_account_reconciliation()?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| anyhow::anyhow!("account switch requires the TUI runtime: {error}"))?;
        // Persist the current session unchanged before writing transition intent.
        // A CAS or storage failure therefore leaves both the active account and
        // its runtime/durable resume bindings untouched.
        self.session.save().map_err(|error| {
            anyhow::anyhow!("current session preflight persistence failed: {error}")
        })?;
        let (session, current_is_target, completion) = crate::server::prepare_local_account_switch(
            self.provider.clone(),
            runtime_key,
            label,
            &self.session.id,
        )?;

        if current_is_target {
            self.session = session;
            self.provider_session_id = None;
            self.reset_provider_view_after_identity_transition()?;
        }

        runtime.spawn(async move {
            if let Err(error) = completion.finish().await {
                crate::logging::error(&format!(
                    "Provider account switch remains blocked by durable reconciliation marker: {error}"
                ));
            }
        });
        Ok(())
    }

    pub(in super::super) fn reconcile_local_account_transition_for_admission(
        &mut self,
    ) -> anyhow::Result<()> {
        if self.is_remote || self.is_replay {
            return Ok(());
        }
        crate::provider::ensure_no_account_transition()?;
        crate::server::ensure_no_pending_account_reconciliation()?;
        if self.session.persistence_revision == 0 {
            return Ok(());
        }

        let mut durable = crate::session::Session::load(&self.session.id)?;
        if durable.persistence_revision <= self.session.persistence_revision {
            return Ok(());
        }
        let account_state_changed = durable.exact_runtime_identity
            != self.session.exact_runtime_identity
            || durable.provider_session_id != self.session.provider_session_id
            || durable.provider_session_identity != self.session.provider_session_identity
            || durable.compaction != self.session.compaction
            || durable.context_frontier != self.session.context_frontier
            || self.provider_session_id != durable.provider_session_id;
        if !account_state_changed {
            return Ok(());
        }

        if self.session_save_pending {
            let durable_len = durable.messages.len();
            let prefix_matches = durable_len <= self.session.messages.len()
                && serde_json::to_vec(&durable.messages)?
                    == serde_json::to_vec(&self.session.messages[..durable_len])?;
            if !prefix_matches {
                anyhow::bail!(
                    "durable account reconciliation diverged from unsaved canonical messages"
                );
            }
            durable
                .messages
                .extend_from_slice(&self.session.messages[durable_len..]);
            durable.save()?;
        }

        let previous_session = self.session.clone();
        let previous_runtime_resume = self.provider_session_id.clone();
        let previous_save_pending = self.session_save_pending;
        self.provider_session_id = durable.provider_session_id.clone();
        self.session = durable;
        self.session_save_pending = false;
        if let Err(error) = self.reset_provider_view_after_identity_transition() {
            // Keep the older revision installed so the next admission retries
            // adoption instead of treating a failed in-memory reset as current.
            self.session = previous_session;
            self.provider_session_id = previous_runtime_resume;
            self.session_save_pending = previous_save_pending;
            return Err(error);
        }
        Ok(())
    }

    pub(in super::super) async fn acquire_local_model_turn_admission(
        &mut self,
    ) -> anyhow::Result<crate::server::AccountReconciliationAdmissionLock> {
        crate::provider::ensure_no_account_transition()?;
        crate::server::reconcile_pending_account_transition_for_admission_async(Arc::clone(
            &self.provider,
        ))
        .await?;
        let admission = crate::server::acquire_account_reconciliation_admission_lock().await?;
        crate::session::with_account_transition_admission(async {

            // Re-adopt canonical state while the exclusive switch lease is
            // excluded. Any session save reuses this task-owned admission.
            self.reconcile_local_account_transition_for_admission()?;

        let expected = self.session.exact_runtime_identity.clone();
        let effective_before_refresh = self.provider.exact_runtime_identity();
        let uses_oauth = expected
            .as_ref()
            .or(effective_before_refresh.as_ref())
            .is_some_and(|identity| {
                matches!(
                    identity.route.runtime_key,
                    jcode_provider_core::RuntimeKey::ClaudeOAuth
                        | jcode_provider_core::RuntimeKey::OpenAIOAuth
                )
            });
        if uses_oauth {
            self.provider.ensure_credentials_current().await?;
        }

        let effective = self.provider.exact_runtime_identity();
        match (expected.as_ref(), effective.as_ref()) {
            (Some(_), Some(_)) if expected != effective => {
                self.reconcile_verified_local_provider_identity_transition()?;
            }
            (Some(expected), None) => {
                anyhow::bail!(
                    "provider cannot verify persisted exact runtime identity before model admission: {:?}",
                    expected
                );
            }
            (None, Some(effective)) => {
                let previous_session = self.session.clone();
                let previous_runtime_resume = self.provider_session_id.clone();
                let mut candidate = self.session.clone();
                candidate.provider_session_id = None;
                candidate.provider_session_identity = None;
                candidate.exact_runtime_identity = Some(effective.clone());
                candidate.reset_context_graph_for_identity_transition();
                candidate.save()?;
                self.session = candidate;
                self.provider_session_id = None;
                if let Err(error) = self.reset_provider_view_after_identity_transition() {
                    self.session = previous_session;
                    self.provider_session_id = previous_runtime_resume;
                    return Err(error);
                }
            }
            (None, None)
                if self.provider_session_id.is_some()
                    || self.session.provider_session_id.is_some()
                    || self.session.has_owned_native_lcm_projection() =>
            {
                anyhow::bail!(
                    "provider identity is opaque while resumable or identity-owned state is active"
                );
            }
            _ => {}
        }

        if self.session.provider_session_id.is_some()
            && (!effective
                .as_ref()
                .is_some_and(|identity| identity.has_verifiable_account_binding())
                || self.session.provider_session_identity != effective)
        {
            anyhow::bail!(
                "provider resume identity is missing, opaque, or stale before model admission"
            );
        }

            Ok::<(), anyhow::Error>(())
        })
        .await?;
        Ok(admission)
    }

    pub(in super::super) fn bind_local_provider_session_id(
        &mut self,
        session_id: String,
    ) -> anyhow::Result<()> {
        let effective = self.provider.exact_runtime_identity();
        let binding_is_safe = effective
            .as_ref()
            .is_some_and(|identity| identity.has_verifiable_account_binding())
            && effective == self.session.exact_runtime_identity;

        let mut candidate = self.session.clone();
        if !binding_is_safe {
            candidate.provider_session_id = None;
            candidate.provider_session_identity = None;
            candidate.save()?;
            self.session = candidate;
            self.provider_session_id = None;
            self.session_save_pending = false;
            anyhow::bail!(
                "ignored provider resume id because its exact account identity is opaque or stale"
            );
        }

        candidate.provider_session_id = Some(session_id.clone());
        candidate.provider_session_identity = effective;
        candidate.save()?;
        self.session = candidate;
        self.provider_session_id = Some(session_id);
        self.session_save_pending = false;
        Ok(())
    }

    pub(in super::super) fn ensure_local_provider_identity_matches_admission(
        &self,
    ) -> anyhow::Result<()> {
        let effective = self.provider.exact_runtime_identity();
        if effective != self.session.exact_runtime_identity {
            anyhow::bail!(
                "exact provider/account identity changed after model admission: admitted {:?}, effective {:?}",
                self.session.exact_runtime_identity,
                effective
            );
        }
        if self.provider_session_id.is_some()
            && (!effective
                .as_ref()
                .is_some_and(|identity| identity.has_verifiable_account_binding())
                || self.provider_session_id != self.session.provider_session_id
                || self.session.provider_session_identity != effective)
        {
            anyhow::bail!("provider resume id no longer matches its admitted exact identity");
        }
        Ok(())
    }

    pub(in super::super) fn reconcile_verified_local_provider_identity_transition(
        &mut self,
    ) -> anyhow::Result<()> {
        let effective = self.provider.exact_runtime_identity();
        if effective == self.session.exact_runtime_identity {
            return self.ensure_local_provider_identity_matches_admission();
        }

        let previous = self.session.exact_runtime_identity.as_ref();
        let Some(current) = effective.as_ref() else {
            anyhow::bail!("provider identity became unverifiable after request open");
        };
        let Some(previous) = previous else {
            anyhow::bail!(
                "provider identity appeared unexpectedly after request open: {:?}",
                current
            );
        };
        let generation_is_monotonic =
            match (previous.account_generation, current.account_generation) {
                (Some(previous), Some(current)) => current >= previous,
                (None, Some(_)) | (None, None) => true,
                (Some(_), None) => false,
            };
        let same_verified_account_route = previous.provider_key == current.provider_key
            && previous.route.runtime_key == current.route.runtime_key
            && previous.route.api_method == current.route.api_method
            && previous.route.provider_label == current.route.provider_label
            && previous.route.detail == current.route.detail
            && previous.reasoning_effort == current.reasoning_effort
            && previous.account_label == current.account_label
            && previous.account_id == current.account_id
            && generation_is_monotonic
            && current.has_verifiable_account_binding()
            && (previous.route.model != current.route.model
                || previous.account_generation != current.account_generation);
        if !same_verified_account_route {
            anyhow::bail!(
                "provider route/account identity changed unexpectedly after request open: admitted {:?}, effective {:?}",
                previous,
                current
            );
        }

        let previous = previous.clone();
        let serving_model_changed = previous.route.model != current.route.model;
        let current = current.clone();
        let apply_transition = |candidate: &mut crate::session::Session| {
            candidate.provider_session_id = None;
            candidate.provider_session_identity = None;
            if serving_model_changed {
                candidate.model = Some(crate::provider::persisted_session_model_for_route(
                    &current.route,
                    &current.route.model,
                ));
            }
            candidate.exact_runtime_identity = Some(current.clone());
            candidate.reset_context_graph_for_identity_transition();
        };
        let mut candidate = self.session.clone();
        apply_transition(&mut candidate);
        if let Err(first_error) = candidate.save() {
            let mut fresh = crate::session::Session::load(&self.session.id).map_err(|reload_error| {
                anyhow::anyhow!(
                    "automatic provider identity persistence failed: {first_error}; reload: {reload_error}"
                )
            })?;
            if fresh
                .exact_runtime_identity
                .as_ref()
                .is_some_and(|identity| identity != &previous && identity != &current)
            {
                anyhow::bail!(
                    "automatic provider identity reconciliation lost CAS to a newer or divergent durable identity"
                );
            }
            if self.session_save_pending {
                let durable_len = fresh.messages.len();
                let prefix_matches = durable_len <= self.session.messages.len()
                    && serde_json::to_vec(&fresh.messages)?
                        == serde_json::to_vec(&self.session.messages[..durable_len])?;
                if !prefix_matches {
                    anyhow::bail!(
                        "automatic provider identity reconciliation diverged from unsaved canonical messages"
                    );
                }
                fresh
                    .messages
                    .extend_from_slice(&self.session.messages[durable_len..]);
            }
            apply_transition(&mut fresh);
            fresh.save().map_err(|retry_error| {
                anyhow::anyhow!(
                    "automatic provider identity persistence failed: {first_error}; retry: {retry_error}"
                )
            })?;
            candidate = fresh;
        }

        self.session = candidate;
        self.provider_session_id = None;
        self.session_save_pending = false;
        self.invalidate_kv_cache_after_compaction();
        if let Err(error) = self.reset_provider_view_after_identity_transition() {
            self.should_quit = true;
            self.set_status_notice(
                "Unsafe in-memory context after automatic provider identity transition; shutting down",
            );
            return Err(error);
        }
        crate::logging::warn(&format!(
            "Accepted verified same-account provider identity transition for local session {}; stale resume and projection state were cleared",
            self.session.id
        ));
        Ok(())
    }
}
