pub(super) async fn handle_switch_anthropic_account(
    id: u64,
    label: String,
    sessions: &SessionAgents,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let _reconciliation_lock = ACCOUNT_RECONCILIATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .await;
    let _account_transition = crate::provider::begin_account_transition();
    let _file_lock = match AccountReconciliationFileLock::acquire_async().await {
        Ok(lock) => lock,
        Err(error) => {
            drop(client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("Failed to lock Anthropic account transition: {error}"),
                retry_after_secs: None,
            }));
            return;
        }
    };
    if let Err(error) = reconcile_pending_account_marker(sessions, agent).await {
        drop(client_event_tx.send(ServerEvent::Error {
            id,
            message: format!("Pending Anthropic account reconciliation failed: {error}"),
            retry_after_secs: None,
        }));
        return;
    }
    let target_session_ids = managed_target_session_ids(
        sessions,
        agent,
        &jcode_provider_core::RuntimeKey::ClaudeOAuth,
    )
    .await;
    let switch = target_session_ids
        .and_then(|target_session_ids| {
            marker_for_target_account(
                jcode_provider_core::RuntimeKey::ClaudeOAuth,
                &label,
                target_session_ids,
            )
        })
        .and_then(|marker| persist_pending_account_reconciliation(&marker))
        .and_then(|()| crate::auth::claude::set_active_account(&label));
    match switch {
        Ok(()) => {
            if let Err(error) = reconcile_pending_account_marker(sessions, agent).await {
                drop(client_event_tx.send(ServerEvent::Error {
                    id,
                    message: format!(
                        "Anthropic account switched, but exact identity persistence failed: {error}"
                    ),
                    retry_after_secs: None,
                }));
                return;
            }
            crate::auth::AuthStatus::invalidate_cache();
            spawn_account_switch_refresh(
                id,
                "anthropic",
                Arc::clone(agent),
                client_event_tx.clone(),
            );
        }
        Err(e) => {
            drop(client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("Failed to switch Anthropic account: {}", e),
                retry_after_secs: None,
            }));
        }
    }
}

pub(super) async fn handle_switch_openai_account(
    id: u64,
    label: String,
    sessions: &SessionAgents,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let _reconciliation_lock = ACCOUNT_RECONCILIATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .await;
    let _account_transition = crate::provider::begin_account_transition();
    let _file_lock = match AccountReconciliationFileLock::acquire_async().await {
        Ok(lock) => lock,
        Err(error) => {
            drop(client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("Failed to lock OpenAI account transition: {error}"),
                retry_after_secs: None,
            }));
            return;
        }
    };
    if let Err(error) = reconcile_pending_account_marker(sessions, agent).await {
        drop(client_event_tx.send(ServerEvent::Error {
            id,
            message: format!("Pending OpenAI account reconciliation failed: {error}"),
            retry_after_secs: None,
        }));
        return;
    }
    let target_session_ids = managed_target_session_ids(
        sessions,
        agent,
        &jcode_provider_core::RuntimeKey::OpenAIOAuth,
    )
    .await;
    let switch = target_session_ids
        .and_then(|target_session_ids| {
            marker_for_target_account(
                jcode_provider_core::RuntimeKey::OpenAIOAuth,
                &label,
                target_session_ids,
            )
        })
        .and_then(|marker| persist_pending_account_reconciliation(&marker))
        .and_then(|()| crate::auth::codex::set_active_account(&label));
    match switch {
        Ok(()) => {
            if let Err(error) = reconcile_pending_account_marker(sessions, agent).await {
                drop(client_event_tx.send(ServerEvent::Error {
                    id,
                    message: format!(
                        "OpenAI account switched, but exact identity persistence failed: {error}"
                    ),
                    retry_after_secs: None,
                }));
                return;
            }
            crate::auth::AuthStatus::invalidate_cache();
            spawn_account_switch_refresh(id, "openai", Arc::clone(agent), client_event_tx.clone());
        }
        Err(e) => {
            drop(client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("Failed to switch OpenAI account: {}", e),
                retry_after_secs: None,
            }));
        }
    }
}

fn spawn_account_switch_refresh(
    id: u64,
    provider_kind: &'static str,
    agent: Arc<Mutex<Agent>>,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
) {
    tokio::spawn(async move {
        let started = Instant::now();
        crate::logging::event_info(
            "SERVER_PROVIDER_CONTROL_ACCOUNT_SWITCH",
            vec![
                ("phase", "refresh_start".to_string()),
                ("provider", provider_kind.to_string()),
                ("request_id", id.to_string()),
            ],
        );
        crate::provider::clear_all_provider_unavailability_for_account();
        crate::provider::clear_all_model_unavailability_for_account();

        match provider_kind {
            "anthropic" => {
                tokio::spawn(async {
                    let _ = crate::usage::get().await;
                });
            }
            "openai" => {
                tokio::spawn(async {
                    let _ = crate::usage::get_openai_usage().await;
                });
            }
            _ => {}
        }

        crate::bus::Bus::global().publish_models_updated();
        let event = available_models_updated_event(&agent).await;
        let _ = client_event_tx.send(event);
        let _ = client_event_tx.send(ServerEvent::Done { id });
        crate::logging::event_info(
            "SERVER_PROVIDER_CONTROL_ACCOUNT_SWITCH",
            vec![
                ("phase", "refresh_done".to_string()),
                ("provider", provider_kind.to_string()),
                ("request_id", id.to_string()),
                ("elapsed_ms", started.elapsed().as_millis().to_string()),
            ],
        );
    });
}
