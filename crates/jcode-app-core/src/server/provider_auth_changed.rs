#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_notify_auth_changed(
    id: u64,
    provider_hint: Option<String>,
    auth: Option<AuthChanged>,
    prefer_strongest: bool,
    provider: &Arc<dyn Provider>,
    provider_template: &Arc<dyn Provider>,
    sessions: &SessionAgents,
    client_session_id: &str,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let refresh_started = Instant::now();
    crate::auth::AuthStatus::invalidate_cache();
    let (session_id, before_snapshot) = if let Ok(agent_guard) = agent.try_lock() {
        (
            agent_guard.session_id().to_string(),
            agent_guard.model_catalog_snapshot(),
        )
    } else {
        crate::logging::event_warn(
            "SERVER_PROVIDER_CONTROL_DEFERRED",
            vec![
                ("phase", "fallback_snapshot".to_string()),
                ("operation", "notify_auth_changed".to_string()),
                ("request_id", id.to_string()),
                ("session_id", client_session_id.to_string()),
                ("reason", "agent_busy".to_string()),
            ],
        );
        (
            client_session_id.to_string(),
            available_models_snapshot_from_provider(provider),
        )
    };
    let auth_refresh_generation = begin_auth_refresh(&session_id);
    let activation_request = AuthActivationRequest::new(provider_hint, auth);
    crate::bus::Bus::global().publish(crate::bus::BusEvent::UiActivity(
        crate::bus::UiActivity::auth(
            Some(session_id.clone()),
            "",
            Some("Auth: refreshing providers..."),
        ),
    ));
    let targets = auth_refresh_targets(provider_template, provider, agent, sessions).await;
    let client_event_tx_clone = client_event_tx.clone();
    let agent_clone = agent.clone();
    tokio::spawn(async move {
        if !auth_refresh_is_current(&session_id, auth_refresh_generation) {
            return;
        }
        let activation = crate::auth::lifecycle::activate_auth_change(&activation_request);
        // Snapshot which providers jcode now believes are configured right after
        // an auth change activates. This is the cornerstone for diagnosing
        // "logged in but model picker still empty / only OpenAI+Anthropic" and
        // "paste key silently returns to menu" reports (#312, #292, #304): if a
        // provider the user just configured is not Available here, the failure is
        // upstream of the picker.
        crate::auth::AuthStatus::check_fast().log_snapshot("auth_changed");
        let mut bus_rx = crate::bus::Bus::global().subscribe();
        let AuthRefreshTargets {
            providers,
            session_providers,
            deferred_agents,
        } = targets;
        let mut refresh_providers = providers.clone();
        for candidate in &session_providers {
            if !refresh_providers
                .iter()
                .any(|existing| Arc::ptr_eq(existing, candidate))
            {
                refresh_providers.push(Arc::clone(candidate));
            }
        }
        for provider in providers {
            provider.on_auth_changed();
        }
        for provider in session_providers {
            provider.on_auth_changed_preserve_current_provider();
        }

        // Auth refresh is global so every live session learns about newly
        // configured credentials, but the automatic post-login model switch is
        // session-local. A user logging Groq/Cerebras into one workspace should
        // not silently move unrelated sessions off their chosen provider/model.
        if auth_refresh_is_current(&session_id, auth_refresh_generation) {
            apply_auth_runtime_model_to_agent(
                &activation,
                activation.activated_model.as_deref(),
                &agent_clone,
                None,
            )
            .await;
        }
        let auth_selection_generation = {
            let agent_guard = agent_clone.lock().await;
            agent_guard.provider_model_selection_generation()
        };

        crate::bus::Bus::global().publish_models_updated();
        crate::bus::Bus::global().publish(crate::bus::BusEvent::UiActivity(
            crate::bus::UiActivity::catalog(
                Some(session_id.clone()),
                "",
                Some("Auth: model routes updating..."),
            ),
        ));

        spawn_deferred_auth_refreshes(deferred_agents);

        // Hot-initializing providers is synchronous, while dynamic catalogs may
        // continue refreshing in the background. Push an immediate snapshot so
        // the model picker/header stop looking stale right after login, then
        // push another snapshot when the background refresh announces itself.
        let mut latest_snapshot = available_models_snapshot(&agent_clone).await;
        let _ = client_event_tx_clone.send(available_models_snapshot_into_event(
            latest_snapshot.clone(),
        ));

        // Wait for the catalog work that providers actually launched. The old
        // implementation waited for two stacked 750 ms debounce windows even
        // when every provider had already finished. Tracking real work removes
        // that fixed tax while retaining the 10 s safety ceiling.
        let settle_started = Instant::now();
        let max_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut model_update_events = 0_u64;
        while refresh_providers
            .iter()
            .any(|provider| provider.auth_model_refresh_pending())
            && tokio::time::Instant::now() < max_deadline
        {
            tokio::select! {
                event = bus_rx.recv() => {
                    if matches!(event, Ok(crate::bus::BusEvent::ModelsUpdated)) {
                        model_update_events = model_update_events.saturating_add(1);
                        latest_snapshot = available_models_snapshot(&agent_clone).await;
                        let _ = client_event_tx_clone.send(available_models_snapshot_into_event(latest_snapshot.clone()));
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
            }
        }
        let refresh_timed_out = refresh_providers
            .iter()
            .any(|provider| provider.auth_model_refresh_pending());
        latest_snapshot = available_models_snapshot(&agent_clone).await;
        let _ = client_event_tx_clone.send(available_models_snapshot_into_event(
            latest_snapshot.clone(),
        ));
        let settle_ms = settle_started.elapsed().as_millis();

        if !auth_refresh_is_current(&session_id, auth_refresh_generation) {
            crate::logging::event_info(
                "SERVER_AUTH_MODEL_REFRESH_SUPERSEDED",
                vec![
                    ("session_id", session_id.clone()),
                    ("generation", auth_refresh_generation.to_string()),
                    (
                        "total_ms",
                        refresh_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            finish_auth_refresh(&session_id, auth_refresh_generation);
            return;
        }

        let manual_model_selected_during_auth_refresh = {
            let agent_guard = agent_clone.lock().await;
            agent_guard.user_selected_provider_model_after(auth_selection_generation)
        };
        if manual_model_selected_during_auth_refresh {
            crate::logging::auth_event(
                "auth_changed_auto_model_skipped_after_manual_switch",
                activation.provider_id.as_deref().unwrap_or("auth"),
                &[("reason", "user_selected_provider_model_during_refresh")],
            );
            latest_snapshot = available_models_snapshot(&agent_clone).await;
            let _ = client_event_tx_clone.send(available_models_snapshot_into_event(
                latest_snapshot.clone(),
            ));
        } else {
            if prefer_strongest {
                if let Some(route) = crate::auth::lifecycle::globally_preferred_default_route(
                    &latest_snapshot.model_routes,
                ) {
                    apply_auth_route_to_agent(
                        &route,
                        &agent_clone,
                        Some(auth_selection_generation),
                    )
                    .await;
                }
            } else if let Some(model_to_select) =
                crate::auth::lifecycle::provider_model_to_select_after_auth(
                    &activation,
                    latest_snapshot.provider_model.as_deref(),
                    &latest_snapshot.model_routes,
                )
            {
                apply_auth_runtime_model_to_agent(
                    &activation,
                    Some(&model_to_select),
                    &agent_clone,
                    Some(auth_selection_generation),
                )
                .await;
            }
            latest_snapshot = available_models_snapshot(&agent_clone).await;
            let _ = client_event_tx_clone.send(available_models_snapshot_into_event(
                latest_snapshot.clone(),
            ));
        }

        let summary = crate::provider::summarize_model_catalog_refresh(
            before_snapshot.available_models,
            latest_snapshot.available_models.clone(),
            before_snapshot.model_routes,
            latest_snapshot.model_routes.clone(),
        );
        let catalog_invariants = crate::auth::lifecycle::validate_catalog_invariants(
            &activation,
            latest_snapshot.provider_model.as_deref(),
            &latest_snapshot.model_routes,
        );
        let catalog_warning = catalog_invariants.warning_message();
        let catalog_message = format_auth_catalog_refresh_complete(
            activation
                .provider_label
                .as_deref()
                .or(latest_snapshot.provider_name.as_deref()),
            latest_snapshot.provider_model.as_deref(),
            &summary,
            catalog_warning.is_some(),
        );
        if let Some(warning) = catalog_warning.as_deref() {
            crate::logging::warn(&format!("Auth catalog invariant warning: {warning}"));
        }
        crate::logging::event_info(
            "SERVER_AUTH_MODEL_REFRESH_COMPLETED",
            vec![
                (
                    "total_ms",
                    refresh_started.elapsed().as_millis().to_string(),
                ),
                ("settle_ms", settle_ms.to_string()),
                ("models_before", summary.model_count_before.to_string()),
                ("models_after", summary.model_count_after.to_string()),
                ("routes_before", summary.route_count_before.to_string()),
                ("routes_after", summary.route_count_after.to_string()),
                ("model_update_events", model_update_events.to_string()),
                ("timed_out", refresh_timed_out.to_string()),
            ],
        );
        send_catalog_activity(&client_event_tx_clone, &catalog_message);
        finish_auth_refresh(&session_id, auth_refresh_generation);
    });
    let _ = client_event_tx.send(ServerEvent::Done { id });
}

#[cfg(test)]
#[path = "provider_control_tests.rs"]
mod provider_control_tests;
