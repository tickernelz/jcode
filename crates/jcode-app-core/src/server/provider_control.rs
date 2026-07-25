#![cfg_attr(test, allow(clippy::items_after_test_module))]

use crate::agent::Agent;
use crate::auth::lifecycle::{AuthActivationRequest, AuthActivationResult};
use crate::protocol::{AuthChanged, NotificationType, ServerEvent};
use crate::provider::{ModelCatalogRefreshSummary, ModelRoute, Provider, RouteSelection};
use jcode_provider_core::ModelCatalogSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Instant;
use tokio::sync::{Mutex, RwLock, mpsc};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;
static AUTH_REFRESH_GENERATIONS: OnceLock<StdMutex<HashMap<String, u64>>> = OnceLock::new();
static NEXT_AUTH_REFRESH_GENERATION: AtomicU64 = AtomicU64::new(1);
static ACCOUNT_RECONCILIATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
#[cfg(test)]
static FAIL_ACCOUNT_RECONCILIATION_SESSION: OnceLock<StdMutex<Option<String>>> = OnceLock::new();

const ACCOUNT_RECONCILIATION_SCHEMA_VERSION: u32 = 1;
const ACCOUNT_RECONCILIATION_FILE: &str = "provider-account-reconciliation.json";

struct AccountReconciliationFileLock {
    _inner: crate::session::AccountTransitionFileLock,
}

struct CancelAccountReconciliationWaitOnDrop(Arc<std::sync::atomic::AtomicBool>);

impl Drop for CancelAccountReconciliationWaitOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl AccountReconciliationFileLock {
    fn acquire() -> anyhow::Result<Self> {
        Ok(Self {
            _inner: crate::session::AccountTransitionFileLock::acquire_exclusive()?,
        })
    }

    fn try_acquire() -> anyhow::Result<Self> {
        let Some(inner) = crate::session::AccountTransitionFileLock::try_acquire_exclusive()?
        else {
            anyhow::bail!("another process is using or switching provider credentials");
        };
        Ok(Self { _inner: inner })
    }

    async fn acquire_async() -> anyhow::Result<Self> {
        const WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _cancel_on_drop = CancelAccountReconciliationWaitOnDrop(Arc::clone(&cancelled));
        let worker_cancelled = Arc::clone(&cancelled);
        let waiter = tokio::task::spawn_blocking(move || {
            crate::session::AccountTransitionFileLock::acquire_exclusive_cancellable(
                &worker_cancelled,
            )
        });
        match tokio::time::timeout(WAIT_LIMIT, waiter).await {
            Ok(joined) => Ok(Self { _inner: joined?? }),
            Err(_) => anyhow::bail!(
                "timed out waiting for active provider turns to release account credentials"
            ),
        }
    }

    #[cfg(test)]
    fn acquire_shared() -> anyhow::Result<Self> {
        Ok(Self {
            _inner: crate::session::AccountTransitionFileLock::acquire_shared()?,
        })
    }

    async fn acquire_shared_async() -> anyhow::Result<Self> {
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _cancel_on_drop = CancelAccountReconciliationWaitOnDrop(Arc::clone(&cancelled));
        let worker_cancelled = Arc::clone(&cancelled);
        Ok(Self {
            _inner: tokio::task::spawn_blocking(move || {
                crate::session::AccountTransitionFileLock::acquire_shared_cancellable(
                    &worker_cancelled,
                )
            })
            .await??,
        })
    }
}

/// Crash-safe intent for an explicit account switch. This deliberately contains
/// only locally generated account identity and session identifiers, never tokens.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PendingAccountReconciliation {
    schema_version: u32,
    runtime_key: jcode_provider_core::RuntimeKey,
    account_label: String,
    account_id: String,
    account_generation: u64,
    target_session_ids: Vec<String>,
}

/// Shared cross-process lease held for the complete lifetime of a provider
/// model turn. Explicit account switches take the corresponding exclusive
/// lease, so credentials and resume identity cannot change after admission.
pub struct AccountReconciliationAdmissionLock {
    _file_lock: AccountReconciliationFileLock,
}

pub async fn acquire_account_reconciliation_admission_lock()
-> anyhow::Result<AccountReconciliationAdmissionLock> {
    Ok(AccountReconciliationAdmissionLock {
        _file_lock: AccountReconciliationFileLock::acquire_shared_async().await?,
    })
}

struct AuthRefreshTargets {
    providers: Vec<Arc<dyn Provider>>,
    session_providers: Vec<Arc<dyn Provider>>,
    deferred_agents: Vec<Arc<Mutex<Agent>>>,
}

fn begin_auth_refresh(session_id: &str) -> u64 {
    let generation = NEXT_AUTH_REFRESH_GENERATION.fetch_add(1, Ordering::Relaxed);
    let mut generations = AUTH_REFRESH_GENERATIONS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    generations.insert(session_id.to_string(), generation);
    generation
}

fn auth_refresh_is_current(session_id: &str, generation: u64) -> bool {
    let generations = AUTH_REFRESH_GENERATIONS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    generations.get(session_id).copied() == Some(generation)
}

fn finish_auth_refresh(session_id: &str, generation: u64) {
    let mut generations = AUTH_REFRESH_GENERATIONS
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if generations.get(session_id).copied() == Some(generation) {
        generations.remove(session_id);
    }
}

fn available_models_snapshot_into_event(snapshot: ModelCatalogSnapshot) -> ServerEvent {
    ServerEvent::AvailableModelsUpdated {
        provider_name: snapshot.provider_name,
        provider_model: snapshot.provider_model,
        available_models: snapshot.available_models,
        available_model_routes: snapshot.model_routes,
    }
}

fn available_models_updated_event_from_agent(agent: &Agent) -> ServerEvent {
    available_models_snapshot_into_event(agent.model_catalog_snapshot())
}

async fn available_models_snapshot(agent: &Arc<Mutex<Agent>>) -> ModelCatalogSnapshot {
    let agent_guard = agent.lock().await;
    agent_guard.model_catalog_snapshot()
}

fn available_models_snapshot_from_provider(provider: &Arc<dyn Provider>) -> ModelCatalogSnapshot {
    ModelCatalogSnapshot::from_provider(provider.as_ref())
}

pub(super) async fn available_models_updated_event(agent: &Arc<Mutex<Agent>>) -> ServerEvent {
    let agent_guard = agent.lock().await;
    available_models_updated_event_from_agent(&agent_guard)
}

pub(super) fn try_available_models_updated_event(agent: &Arc<Mutex<Agent>>) -> Option<ServerEvent> {
    let agent_guard = agent.try_lock().ok()?;
    Some(available_models_updated_event_from_agent(&agent_guard))
}

fn format_auth_catalog_refresh_complete(
    provider_name: Option<&str>,
    provider_model: Option<&str>,
    summary: &ModelCatalogRefreshSummary,
    has_warning: bool,
) -> String {
    let provider_label = provider_name.unwrap_or("provider");
    let title = provider_model
        .map(|model| format!("**Model ready:** `{model}`"))
        .unwrap_or_else(|| "**Model access refreshed**".to_string());
    let changed = summary.models_added > 0
        || summary.models_removed > 0
        || summary.routes_added > 0
        || summary.routes_removed > 0
        || summary.routes_changed > 0;
    let catalog_status = if has_warning {
        if changed {
            format!("{provider_label} catalog changed; some routes missing. Use `/model`.")
        } else {
            format!("{provider_label} catalog unchanged; some routes missing. Use `/model`.")
        }
    } else if changed {
        format!(
            "{provider_label} catalog changed: models +{}/-{}, routes +{}/-{}/~{}. Use `/model`.",
            summary.models_added,
            summary.models_removed,
            summary.routes_added,
            summary.routes_removed,
            summary.routes_changed,
        )
    } else {
        format!(
            "{provider_label} catalog unchanged: {} models, {} routes. Use `/model`.",
            summary.model_count_after, summary.route_count_after,
        )
    };
    format!("{title}\n{catalog_status}")
}

fn log_provider_control_deferred(operation: &'static str, id: u64) -> Instant {
    let queued_at = Instant::now();
    crate::logging::event_warn(
        "SERVER_PROVIDER_CONTROL_DEFERRED",
        vec![
            ("phase", "queued".to_string()),
            ("operation", operation.to_string()),
            ("request_id", id.to_string()),
            ("reason", "agent_busy".to_string()),
        ],
    );
    queued_at
}

fn log_provider_control_lock_acquired(operation: &'static str, id: u64, queued_at: Instant) {
    crate::logging::event_info(
        "SERVER_PROVIDER_CONTROL_DEFERRED",
        vec![
            ("phase", "lock_acquired".to_string()),
            ("operation", operation.to_string()),
            ("request_id", id.to_string()),
            ("wait_ms", queued_at.elapsed().as_millis().to_string()),
        ],
    );
}

fn log_provider_control_completed(operation: &'static str, id: u64, queued_at: Instant) {
    crate::logging::event_info(
        "SERVER_PROVIDER_CONTROL_DEFERRED",
        vec![
            ("phase", "completed".to_string()),
            ("operation", operation.to_string()),
            ("request_id", id.to_string()),
            ("total_ms", queued_at.elapsed().as_millis().to_string()),
        ],
    );
}

fn spawn_deferred_agent_mutation<F>(
    operation: &'static str,
    id: u64,
    agent: Arc<Mutex<Agent>>,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
    apply: F,
) where
    F: FnOnce(&mut Agent, &mpsc::UnboundedSender<ServerEvent>) + Send + 'static,
{
    let queued_at = log_provider_control_deferred(operation, id);
    tokio::spawn(async move {
        let mut agent_guard = agent.lock().await;
        log_provider_control_lock_acquired(operation, id, queued_at);
        apply(&mut agent_guard, &client_event_tx);
        log_provider_control_completed(operation, id, queued_at);
    });
}

fn spawn_deferred_provider_operation<F>(
    operation: &'static str,
    id: u64,
    agent: Arc<Mutex<Agent>>,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
    apply: F,
) where
    F: FnOnce(Arc<dyn Provider>, &mpsc::UnboundedSender<ServerEvent>) + Send + 'static,
{
    let queued_at = log_provider_control_deferred(operation, id);
    tokio::spawn(async move {
        let provider = {
            let agent_guard = agent.lock().await;
            log_provider_control_lock_acquired(operation, id, queued_at);
            agent_guard.provider_handle()
        };
        apply(provider, &client_event_tx);
        log_provider_control_completed(operation, id, queued_at);
    });
}

async fn auth_refresh_targets(
    provider_template: &Arc<dyn Provider>,
    current_provider: &Arc<dyn Provider>,
    current_agent: &Arc<Mutex<Agent>>,
    sessions: &SessionAgents,
) -> AuthRefreshTargets {
    fn push_unique(handles: &mut Vec<Arc<dyn Provider>>, provider: Arc<dyn Provider>) {
        if !handles
            .iter()
            .any(|existing| Arc::ptr_eq(existing, &provider))
        {
            handles.push(provider);
        }
    }

    let mut handles = Vec::new();
    let mut session_handles = Vec::new();
    let mut deferred_agents = Vec::new();
    push_unique(&mut handles, Arc::clone(provider_template));
    push_unique(&mut handles, Arc::clone(current_provider));

    let agents: Vec<Arc<Mutex<Agent>>> = {
        let sessions_guard = sessions.read().await;
        sessions_guard.values().cloned().collect()
    };

    for agent in agents {
        // The requesting session's provider is already included explicitly,
        // even when that agent is busy and its lock cannot be inspected here.
        if Arc::ptr_eq(&agent, current_agent) {
            continue;
        }
        let Ok(agent_guard) = agent.try_lock() else {
            crate::logging::info(
                "Deferring busy session provider auth-change refresh until the session is idle",
            );
            deferred_agents.push(agent);
            continue;
        };
        let provider = agent_guard.provider_handle();
        if handles
            .iter()
            .any(|existing| Arc::ptr_eq(existing, &provider))
        {
            continue;
        }
        push_unique(&mut session_handles, provider);
    }

    AuthRefreshTargets {
        providers: handles,
        session_providers: session_handles,
        deferred_agents,
    }
}

fn spawn_deferred_auth_refreshes(agents: Vec<Arc<Mutex<Agent>>>) {
    for agent in agents {
        tokio::spawn(async move {
            let provider = {
                let agent_guard = agent.lock().await;
                agent_guard.provider_handle()
            };
            provider.on_auth_changed_preserve_current_provider();
            crate::bus::Bus::global().publish_models_updated();
        });
    }
}

async fn apply_auth_runtime_model_to_agent(
    activation: &AuthActivationResult,
    model: Option<&str>,
    agent: &Arc<Mutex<Agent>>,
    unless_user_selected_after: Option<u64>,
) {
    let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
        return;
    };

    let provider = activation.provider_id.as_deref().unwrap_or("auth");
    let result = {
        let mut agent_guard = agent.lock().await;
        if unless_user_selected_after
            .is_some_and(|generation| agent_guard.user_selected_provider_model_after(generation))
        {
            crate::logging::auth_event(
                "auth_changed_auto_model_skipped_after_manual_switch",
                provider,
                &[("reason", "user_selected_provider_model_during_refresh")],
            );
            return;
        }
        let provider_name = agent_guard.provider_handle().name().to_string();
        let model_request = activation.model_switch_request(&provider_name, model);
        let result = agent_guard.set_model_from_auth(&model_request);
        result.map(|_| agent_guard.provider_model())
    };

    match result {
        Ok(resolved_model) => crate::logging::auth_event(
            "auth_changed_runtime_model_applied",
            provider,
            &[
                ("requested_model", model),
                ("resolved_model", resolved_model.as_str()),
                ("provider_session", "reset"),
            ],
        ),
        Err(error) => {
            let message = error.to_string();
            crate::logging::auth_event(
                "auth_changed_runtime_model_failed",
                provider,
                &[("requested_model", model), ("reason", message.as_str())],
            );
        }
    }
}

async fn apply_auth_route_to_agent(
    route: &ModelRoute,
    agent: &Arc<Mutex<Agent>>,
    unless_user_selected_after: Option<u64>,
) {
    let selection = RouteSelection::from_model_route(route);
    let requested_model = selection.routed_model_spec();
    let result = {
        let mut agent_guard = agent.lock().await;
        if unless_user_selected_after
            .is_some_and(|generation| agent_guard.user_selected_provider_model_after(generation))
        {
            crate::logging::auth_event(
                "auth_changed_auto_model_skipped_after_manual_switch",
                &route.provider,
                &[("reason", "user_selected_provider_model_during_refresh")],
            );
            return;
        }
        let result = agent_guard.set_route_selection_from_auth(&selection);
        result.map(|_| agent_guard.provider_model())
    };

    match result {
        Ok(resolved_model) => crate::logging::auth_event(
            "auth_changed_global_route_applied",
            &route.provider,
            &[
                ("requested_model", requested_model.as_str()),
                ("resolved_model", resolved_model.as_str()),
                ("api_method", route.api_method.as_str()),
                ("provider_session", "reset"),
            ],
        ),
        Err(error) => {
            let message = error.to_string();
            crate::logging::auth_event(
                "auth_changed_global_route_failed",
                &route.provider,
                &[
                    ("requested_model", requested_model.as_str()),
                    ("api_method", route.api_method.as_str()),
                    ("reason", message.as_str()),
                ],
            );
        }
    }
}

fn model_switching_unavailable_current(agent: &Agent) -> Option<String> {
    if agent.available_models_for_switching().is_empty() {
        Some(agent.provider_model())
    } else {
        None
    }
}

fn send_model_changed_result(
    id: u64,
    result: anyhow::Result<(String, String)>,
    fallback_model: String,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    match result {
        Ok((updated, provider_name)) => {
            crate::telemetry::record_model_switch();
            crate::logging::event_info(
                "server_model_changed",
                vec![
                    ("id", id.to_string()),
                    ("model", updated.clone()),
                    ("provider", provider_name.clone()),
                ],
            );
            let _ = client_event_tx.send(ServerEvent::ModelChanged {
                id,
                model: updated,
                provider_name: Some(provider_name),
                error: None,
            });
        }
        Err(error) => {
            crate::logging::event_error(
                "server_model_change_failed",
                vec![
                    ("id", id.to_string()),
                    ("fallback_model", fallback_model.clone()),
                    ("error", error.to_string()),
                ],
            );
            let _ = client_event_tx.send(ServerEvent::ModelChanged {
                id,
                model: fallback_model,
                provider_name: None,
                error: Some(error.to_string()),
            });
        }
    }
}

fn apply_cycle_model(
    id: u64,
    direction: i8,
    agent: &mut Agent,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let models = agent.available_models_for_switching();
    if models.is_empty() {
        let _ = client_event_tx.send(ServerEvent::ModelChanged {
            id,
            model: agent.provider_model(),
            provider_name: None,
            error: Some("Model switching is not available for this provider.".to_string()),
        });
        return;
    }

    let current = agent.provider_model();
    let current_index = models.iter().position(|m| *m == current).unwrap_or(0);
    let len = models.len();
    let next_index = if direction >= 0 {
        (current_index + 1) % len
    } else {
        (current_index + len - 1) % len
    };
    let next_model = models[next_index].clone();
    crate::logging::event_info(
        "server_cycle_model_request",
        vec![
            ("id", id.to_string()),
            ("direction", (direction as i64).to_string()),
            ("current_model", current.clone()),
            ("next_model", next_model.clone()),
            ("available_models", len.to_string()),
        ],
    );
    let result = {
        let result = agent.set_model(&next_model);
        result.map(|_| (agent.provider_model(), agent.provider_name()))
    };
    send_model_changed_result(id, result, current, client_event_tx);
}

pub(super) async fn handle_cycle_model(
    id: u64,
    direction: i8,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if let Ok(mut agent_guard) = agent.try_lock() {
        apply_cycle_model(id, direction, &mut agent_guard, client_event_tx);
    } else {
        spawn_deferred_agent_mutation(
            "cycle_model",
            id,
            Arc::clone(agent),
            client_event_tx.clone(),
            move |agent_guard, client_event_tx| {
                apply_cycle_model(id, direction, agent_guard, client_event_tx);
            },
        );
    }
}

fn premium_mode_label(mode: crate::provider::copilot::PremiumMode) -> &'static str {
    use crate::provider::copilot::PremiumMode;
    match mode {
        PremiumMode::Zero => "zero premium requests",
        PremiumMode::OnePerSession => "one premium per session",
        PremiumMode::Normal => "normal",
    }
}

fn apply_set_premium_mode(
    id: u64,
    mode: u8,
    premium_mode: crate::provider::copilot::PremiumMode,
    agent: &Agent,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    agent.set_premium_mode(premium_mode);
    crate::logging::info(&format!(
        "Server: premium mode set to {} ({})",
        mode,
        premium_mode_label(premium_mode)
    ));
    let _ = client_event_tx.send(ServerEvent::Ack { id });
}

pub(super) async fn handle_set_premium_mode(
    id: u64,
    mode: u8,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    use crate::provider::copilot::PremiumMode;

    let premium_mode = match mode {
        2 => PremiumMode::Zero,
        1 => PremiumMode::OnePerSession,
        _ => PremiumMode::Normal,
    };
    if let Ok(agent_guard) = agent.try_lock() {
        apply_set_premium_mode(id, mode, premium_mode, &agent_guard, client_event_tx);
    } else {
        spawn_deferred_agent_mutation(
            "set_premium_mode",
            id,
            Arc::clone(agent),
            client_event_tx.clone(),
            move |agent_guard, client_event_tx| {
                apply_set_premium_mode(id, mode, premium_mode, agent_guard, client_event_tx);
            },
        );
    }
}

fn apply_set_model(
    id: u64,
    model: String,
    agent: &mut Agent,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    crate::logging::event_info(
        "server_set_model_request",
        vec![
            ("id", id.to_string()),
            ("requested_model", model.clone()),
            ("current_model", agent.provider_model()),
            ("current_provider", agent.provider_name()),
        ],
    );

    if let Some(current) = model_switching_unavailable_current(agent) {
        crate::logging::event_warn(
            "server_set_model_unavailable",
            vec![
                ("id", id.to_string()),
                ("requested_model", model.clone()),
                ("current_model", current.clone()),
            ],
        );
        let _ = client_event_tx.send(ServerEvent::ModelChanged {
            id,
            model: current,
            provider_name: None,
            error: Some("Model switching is not available for this provider.".to_string()),
        });
        return;
    }

    let current = agent.provider_model();
    let result = {
        let result = agent.set_model(&model);
        result.map(|_| (agent.provider_model(), agent.provider_name()))
    };
    send_model_changed_result(id, result, current, client_event_tx);
}

fn apply_set_route(
    id: u64,
    selection: crate::provider::RouteSelection,
    agent: &mut Agent,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    crate::logging::event_info(
        "server_set_route_request",
        vec![
            ("id", id.to_string()),
            ("requested_model", selection.model.clone()),
            ("requested_provider", selection.provider_label.clone()),
            ("requested_api_method", selection.api_method.clone()),
            ("current_model", agent.provider_model()),
            ("current_provider", agent.provider_name()),
        ],
    );

    if let Some(current) = model_switching_unavailable_current(agent) {
        crate::logging::event_warn(
            "server_set_route_unavailable",
            vec![
                ("id", id.to_string()),
                ("requested_model", selection.model.clone()),
                ("requested_provider", selection.provider_label.clone()),
                ("current_model", current.clone()),
            ],
        );
        let _ = client_event_tx.send(ServerEvent::ModelChanged {
            id,
            model: current,
            provider_name: None,
            error: Some("Model switching is not available for this provider.".to_string()),
        });
        return;
    }

    let current = agent.provider_model();
    let result = {
        let result = agent.set_route_selection(&selection);
        result.map(|_| (agent.provider_model(), agent.provider_name()))
    };
    send_model_changed_result(id, result, current, client_event_tx);
}

pub(super) async fn handle_set_model(
    id: u64,
    model: String,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if let Ok(mut agent_guard) = agent.try_lock() {
        apply_set_model(id, model, &mut agent_guard, client_event_tx);
    } else {
        spawn_deferred_agent_mutation(
            "set_model",
            id,
            Arc::clone(agent),
            client_event_tx.clone(),
            move |agent_guard, client_event_tx| {
                apply_set_model(id, model, agent_guard, client_event_tx);
            },
        );
    }
}

pub(super) async fn handle_set_route(
    id: u64,
    selection: crate::provider::RouteSelection,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if let Ok(mut agent_guard) = agent.try_lock() {
        apply_set_route(id, selection, &mut agent_guard, client_event_tx);
    } else {
        spawn_deferred_agent_mutation(
            "set_route",
            id,
            Arc::clone(agent),
            client_event_tx.clone(),
            move |agent_guard, client_event_tx| {
                apply_set_route(id, selection, agent_guard, client_event_tx);
            },
        );
    }
}

pub(super) async fn handle_refresh_models(
    id: u64,
    provider: &Arc<dyn Provider>,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let provider_clone = provider.clone();
    let agent_clone = agent.clone();
    let client_event_tx_clone = client_event_tx.clone();
    tokio::spawn(async move {
        send_catalog_activity(
            &client_event_tx_clone,
            &crate::message::format_model_refresh_progress_markdown(
                "Starting provider model catalog refresh",
                Some(5),
            ),
        );

        let refresh_started = Instant::now();
        let refresh_future = provider_clone.refresh_model_catalog();
        tokio::pin!(refresh_future);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(2));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let result = loop {
            tokio::select! {
                result = &mut refresh_future => break result,
                _ = heartbeat.tick() => {
                    let elapsed_secs = refresh_started.elapsed().as_secs();
                    if elapsed_secs > 0 {
                        send_catalog_activity(
                            &client_event_tx_clone,
                            &crate::message::format_model_refresh_progress_markdown(
                                &format!("Waiting on provider APIs ({elapsed_secs}s elapsed)"),
                                None,
                            ),
                        );
                    }
                }
            }
        };
        match result {
            Ok(_) => {
                send_catalog_activity(
                    &client_event_tx_clone,
                    &crate::message::format_model_refresh_progress_markdown(
                        "Updating model picker",
                        Some(95),
                    ),
                );
                crate::bus::Bus::global().publish_models_updated();
                let event = available_models_updated_event(&agent_clone).await;
                let _ = client_event_tx_clone.send(event);
                send_catalog_activity(
                    &client_event_tx_clone,
                    &crate::message::format_model_refresh_progress_markdown(
                        "Model list refresh complete",
                        Some(100),
                    ),
                );
            }
            Err(err) => {
                send_catalog_activity(
                    &client_event_tx_clone,
                    &crate::message::format_model_refresh_progress_markdown(
                        "Model list refresh failed",
                        None,
                    ),
                );
                let _ = client_event_tx_clone.send(ServerEvent::Error {
                    id,
                    message: format!("Failed to refresh models: {}", err),
                    retry_after_secs: None,
                });
            }
        }
    });
    let _ = client_event_tx.send(ServerEvent::Done { id });
}

fn send_catalog_activity(client_event_tx: &mpsc::UnboundedSender<ServerEvent>, message: &str) {
    let _ = client_event_tx.send(ServerEvent::Notification {
        from_session: "jcode".to_string(),
        from_name: Some("Jcode".to_string()),
        notification_type: NotificationType::Message {
            scope: Some("catalog_activity".to_string()),
            channel: None,
            tldr: None,
        },
        message: message.to_string(),
    });
}

pub(super) async fn handle_set_reasoning_effort(
    id: u64,
    effort: String,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = if let Ok(mut agent_guard) = agent.try_lock() {
        agent_guard.set_reasoning_effort(&effort)
    } else {
        spawn_deferred_reasoning_effort_change(
            id,
            effort,
            Arc::clone(agent),
            client_event_tx.clone(),
        );
        return;
    };

    send_reasoning_effort_result(id, result, client_event_tx);
}

fn send_reasoning_effort_result(
    id: u64,
    result: anyhow::Result<Option<String>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    match result {
        Ok(effort) => {
            let _ = client_event_tx.send(ServerEvent::ReasoningEffortChanged {
                id,
                effort,
                error: None,
            });
        }
        Err(e) => {
            let _ = client_event_tx.send(ServerEvent::ReasoningEffortChanged {
                id,
                effort: None,
                error: Some(e.to_string()),
            });
        }
    }
}

fn spawn_deferred_reasoning_effort_change(
    id: u64,
    effort: String,
    agent: Arc<Mutex<Agent>>,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
) {
    let queued_at = log_provider_control_deferred("set_reasoning_effort", id);
    tokio::spawn(async move {
        let mut agent_guard = agent.lock().await;
        log_provider_control_lock_acquired("set_reasoning_effort", id, queued_at);
        let result = agent_guard.set_reasoning_effort(&effort);
        crate::logging::info(&format!(
            "Deferred reasoning effort change completed request_id={} requested={} success={}",
            id,
            effort,
            result.is_ok()
        ));
        send_reasoning_effort_result(id, result, &client_event_tx);
        log_provider_control_completed("set_reasoning_effort", id, queued_at);
    });
}

pub(super) async fn handle_set_service_tier(
    id: u64,
    service_tier: String,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let apply = move |provider: Arc<dyn Provider>,
                      client_event_tx: &mpsc::UnboundedSender<ServerEvent>| {
        match provider.set_service_tier(&service_tier) {
            Ok(()) => {
                let _ = client_event_tx.send(ServerEvent::ServiceTierChanged {
                    id,
                    service_tier: provider.service_tier(),
                    error: None,
                });
            }
            Err(e) => {
                let _ = client_event_tx.send(ServerEvent::ServiceTierChanged {
                    id,
                    service_tier: None,
                    error: Some(e.to_string()),
                });
            }
        }
    };

    if let Ok(agent_guard) = agent.try_lock() {
        apply(agent_guard.provider_handle(), client_event_tx);
    } else {
        spawn_deferred_provider_operation(
            "set_service_tier",
            id,
            Arc::clone(agent),
            client_event_tx.clone(),
            apply,
        );
    }
}

pub(super) async fn handle_set_transport(
    id: u64,
    transport: String,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let apply = move |provider: Arc<dyn Provider>,
                      client_event_tx: &mpsc::UnboundedSender<ServerEvent>| {
        match provider.set_transport(&transport) {
            Ok(()) => {
                let _ = client_event_tx.send(ServerEvent::TransportChanged {
                    id,
                    transport: provider.transport(),
                    error: None,
                });
            }
            Err(e) => {
                let _ = client_event_tx.send(ServerEvent::TransportChanged {
                    id,
                    transport: None,
                    error: Some(e.to_string()),
                });
            }
        }
    };

    if let Ok(agent_guard) = agent.try_lock() {
        apply(agent_guard.provider_handle(), client_event_tx);
    } else {
        spawn_deferred_provider_operation(
            "set_transport",
            id,
            Arc::clone(agent),
            client_event_tx.clone(),
            apply,
        );
    }
}

pub(super) async fn handle_set_compaction_mode(
    id: u64,
    mode: crate::config::CompactionMode,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if let Ok(agent_guard) = agent.try_lock() {
        let registry = agent_guard.registry();
        drop(agent_guard);
        apply_set_compaction_mode(id, mode, registry, client_event_tx).await;
    } else {
        spawn_deferred_set_compaction_mode(id, mode, Arc::clone(agent), client_event_tx.clone());
    }
}

async fn apply_set_compaction_mode(
    id: u64,
    mode: crate::config::CompactionMode,
    registry: crate::tool::Registry,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let result = {
        let compaction = registry.compaction();
        let mut manager = compaction.write().await;
        manager.set_mode(mode);
        Ok::<(), anyhow::Error>(())
    };

    match result {
        Ok(()) => {
            let updated_mode = registry.compaction().read().await.mode();
            let _ = client_event_tx.send(ServerEvent::CompactionModeChanged {
                id,
                mode: updated_mode,
                error: None,
            });
        }
        Err(e) => {
            let fallback_mode = registry.compaction().read().await.mode();
            let _ = client_event_tx.send(ServerEvent::CompactionModeChanged {
                id,
                mode: fallback_mode,
                error: Some(e.to_string()),
            });
        }
    }
}

fn spawn_deferred_set_compaction_mode(
    id: u64,
    mode: crate::config::CompactionMode,
    agent: Arc<Mutex<Agent>>,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
) {
    let queued_at = log_provider_control_deferred("set_compaction_mode", id);
    tokio::spawn(async move {
        let registry = {
            let agent_guard = agent.lock().await;
            log_provider_control_lock_acquired("set_compaction_mode", id, queued_at);
            agent_guard.registry()
        };
        apply_set_compaction_mode(id, mode, registry, &client_event_tx).await;
        log_provider_control_completed("set_compaction_mode", id, queued_at);
    });
}

include!("provider_auth_changed.rs");

include!("provider_account_reconciliation.rs");

include!("provider_account_switch.rs");

#[cfg(test)]
#[path = "provider_control_unit_tests.rs"]
mod tests;
