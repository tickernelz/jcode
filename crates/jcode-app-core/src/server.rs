mod await_members_state;
mod background_tasks;
mod client_actions;
mod client_api;
mod client_comm;
mod client_comm_channels;
mod client_comm_context;
mod client_comm_message;
mod client_disconnect_cleanup;
mod client_lifecycle;
mod client_lifecycle_logging;
mod client_lightweight_control;
mod client_session;
mod client_session_lifecycle;
mod client_state;
mod client_writer;
mod comm_await;
mod comm_control;
mod comm_graph;
mod comm_plan;
mod comm_session;
mod comm_sync;
mod debug;
mod debug_ambient;
mod debug_command_exec;
mod debug_events;
mod debug_help;
mod debug_jobs;
mod debug_server_state;
mod debug_session_admin;
mod debug_swarm_read;
mod debug_swarm_write;
mod debug_testers;
mod durable_state;
mod headless;
mod jade_relay;
mod lifecycle;
mod live_turn;
pub(crate) mod provider_control;
pub use self::provider_control::{
    AccountReconciliationAdmissionLock, LocalAccountSwitchCompletion,
    acquire_account_reconciliation_admission_lock, ensure_no_pending_account_reconciliation,
    prepare_local_account_switch, reconcile_pending_account_transition_for_admission,
    reconcile_pending_account_transition_for_admission_async,
};
mod reload;
mod reload_recovery;
mod reload_state;
mod reload_trace;
mod runtime;
mod server_background_runtime;
mod server_bus_monitor;
mod server_headless_recovery;
mod socket;
mod swarm;
mod swarm_channels;
mod swarm_mutation_state;
mod swarm_persistence;
mod util;

pub(super) use self::await_members_state::AwaitMembersRuntime;
use self::background_tasks::{
    dispatch_background_task_completion, dispatch_background_task_progress,
    dispatch_swarm_await_completion, dispatch_swarm_batch_progress, dispatch_swarm_output_tail,
    dispatch_swarm_runtime_status, dispatch_swarm_todo_progress, dispatch_swarm_tool_activity,
    dispatch_ui_activity,
};
pub(crate) use self::client_session_lifecycle::{
    acquire_session_lifecycle_lease, acquire_session_lifecycle_pair,
};
use self::debug::{ClientConnectionInfo, ClientDebugState};
use self::debug_jobs::DebugJob;
use self::headless::create_headless_session;
use self::reload::await_reload_signal;
use self::runtime::ServerRuntime;
use self::swarm::{
    MAX_SWARM_MEMBERS, broadcast_swarm_plan, broadcast_swarm_plan_with_previous,
    broadcast_swarm_status, expired_terminal_member_ids, member_consumes_swarm_capacity,
    record_swarm_event, record_swarm_event_for_session, refresh_swarm_task_staleness,
    remove_plan_participant, remove_session_from_swarm, rename_plan_participant, run_swarm_message,
    send_swarm_plan_to_session, set_member_task_label, swarm_is_self_or_ancestor,
    update_member_status, update_member_status_with_report, update_member_status_with_report_tldr,
};
use self::swarm_channels::{
    remove_session_channel_subscriptions, subscribe_session_to_channel,
    unsubscribe_session_from_channel,
};
pub(super) use self::swarm_mutation_state::SwarmMutationRuntime;
use self::swarm_persistence::{
    LoadedSwarmRuntimeState, capture_swarm_state_version,
    load_runtime_state as load_persisted_swarm_runtime_state,
    persist_swarm_state as persist_swarm_state_snapshot, remove_swarm_state_if_version,
    swarm_operation_lock,
};
use self::util::get_shared_mcp_pool;
use crate::agent::Agent;
use crate::ambient_runner::AmbientRunnerHandle;
use crate::bus::{Bus, BusEvent};
use crate::protocol::{NotificationType, ServerEvent};
use crate::provider::Provider;
use crate::runtime_memory_log::{
    RuntimeMemoryLogController, RuntimeMemoryLogSampling, RuntimeMemoryLogTrigger,
    ServerRuntimeMemoryBackground, ServerRuntimeMemoryClients, ServerRuntimeMemoryEmbeddings,
    ServerRuntimeMemorySample, ServerRuntimeMemoryServer, ServerRuntimeMemorySessions,
    ServerRuntimeMemoryTopSession,
};
use crate::tool::selfdev::ReloadContext;
use crate::transport::Listener;
use anyhow::Result;
use jcode_agent_runtime::{InterruptSignal, SoftInterruptSource};
use jcode_swarm_core::{
    append_swarm_completion_report_instructions, format_structured_completion_report,
    summarize_plan_items, truncate_detail,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OnceCell, RwLock, broadcast, mpsc};

pub(super) type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;
pub(super) type ChannelSubscriptions =
    Arc<RwLock<HashMap<String, HashMap<String, HashSet<String>>>>>;

/// Remove a live server session and its process-presence marker as one
/// lifecycle operation. Server-owned sessions all share the long-running
/// server PID, so leaving the marker behind makes presence UIs count the
/// removed session forever.
pub(super) async fn remove_session_entry<T>(
    sessions: &Arc<RwLock<HashMap<String, T>>>,
    session_id: &str,
) -> Option<T> {
    let removed = sessions.write().await.remove(session_id);
    if removed.is_some() {
        crate::storage::unregister_active_pid(session_id);
    }
    removed
}

include!("server/server_support.rs");

pub struct Server {
    provider: Arc<dyn Provider>,
    socket_path: PathBuf,
    debug_socket_path: PathBuf,
    gateway_config_override: Option<crate::gateway::GatewayConfig>,
    /// Server identity for multi-server support
    identity: ServerIdentity,
    /// Broadcast channel for streaming events to all subscribers
    event_tx: broadcast::Sender<ServerEvent>,
    /// Active sessions (session_id -> Agent)
    sessions: Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>,
    /// Current processing state
    is_processing: Arc<RwLock<bool>>,
    /// Session ID for the default session
    session_id: Arc<RwLock<String>>,
    /// Number of connected clients
    client_count: Arc<RwLock<usize>>,
    /// Connected client mapping (client_id -> session_id)
    client_connections: Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    /// File-touch tracking service (forward path index + reverse session index)
    file_touch: FileTouchService,
    /// Shared ownership of core swarm coordination state.
    swarm_state: SwarmState,
    /// Shared context by swarm (swarm_id -> key -> SharedContext)
    shared_context: Arc<RwLock<HashMap<String, HashMap<String, SharedContext>>>>,
    /// Active and available TUI debug channels (request_id, command)
    client_debug_state: Arc<RwLock<ClientDebugState>>,
    /// Channel to receive client debug responses from TUI (request_id, response)
    client_debug_response_tx: broadcast::Sender<(u64, String)>,
    /// Background debug jobs (async debug commands)
    debug_jobs: Arc<RwLock<HashMap<String, DebugJob>>>,
    /// Channel subscriptions (swarm_id -> channel -> session_ids)
    channel_subscriptions: ChannelSubscriptions,
    /// Reverse index for channel subscriptions: session_id -> swarm_id -> channels
    channel_subscriptions_by_session: ChannelSubscriptions,
    /// Event history for real-time event subscription (ring buffer)
    event_history: Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    /// Counter for event IDs
    event_counter: Arc<std::sync::atomic::AtomicU64>,
    /// Broadcast channel for swarm event subscriptions (debug socket subscribers)
    swarm_event_tx: broadcast::Sender<SwarmEvent>,
    /// Ambient mode runner handle (None if ambient is disabled)
    ambient_runner: Option<AmbientRunnerHandle>,
    /// Shared MCP server pool (processes shared across sessions), initialized lazily.
    mcp_pool: Arc<OnceCell<Arc<crate::mcp::SharedMcpPool>>>,
    /// Graceful shutdown signals by session_id (stored outside agent mutex so they
    /// can be signaled without locking the agent during active tool execution)
    shutdown_signals: Arc<RwLock<HashMap<String, InterruptSignal>>>,
    /// Soft interrupt queues by session_id (stored outside agent mutex so swarm/debug
    /// notifications can be enqueued while an agent is actively processing)
    soft_interrupt_queues: SessionInterruptQueues,
    /// Persisted communicate await_members wait registry.
    await_members_runtime: AwaitMembersRuntime,
    /// Persisted dedupe registry for mutating swarm coordinator operations.
    swarm_mutation_runtime: SwarmMutationRuntime,
}

impl Server {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self::new_with_name(provider, None)
    }

    pub fn new_with_name(provider: Arc<dyn Provider>, server_name: Option<String>) -> Self {
        use crate::id::{new_id, new_memorable_server_id, server_icon};

        // A previous explicit account switch may have crashed after mutating only
        // some peer sessions. Reconcile its durable target set before this server
        // can accept model or tool turns. Failures intentionally leave the marker
        // in place, and the turn gates remain closed until a later retry succeeds.
        if let Err(error) =
            provider_control::reconcile_pending_account_transition_on_startup_sync(&provider)
        {
            crate::logging::error(&format!(
                "Pending provider account reconciliation remains blocked at startup: {error}"
            ));
        }

        // Register the live provider so background helpers (the memory sidecar)
        // can make cheap model calls on whatever provider the user is running.
        // Without this, the sidecar only works on OpenAI/Claude OAuth and
        // silently degrades (rerank -> hybrid order, no relevance/extraction) on
        // Copilot, Antigravity, Gemini, Cursor, Bedrock, and OpenRouter.
        crate::provider::set_active_provider(Arc::clone(&provider));

        let (event_tx, _) = broadcast::channel(1024);
        let (client_debug_response_tx, _) = broadcast::channel(64);

        // Generate a memorable server name unless the operator configured a
        // stable one for long-lived remote runtimes.
        let (id, name) = match configured_server_name(server_name) {
            Some(name) => (new_id(&format!("server_{name}")), name),
            None => new_memorable_server_id(),
        };
        let icon = server_icon(&name).to_string();
        let identity = ServerIdentity {
            id,
            name,
            icon,
            git_hash: jcode_build_meta::git_hash().to_string(),
            version: jcode_build_meta::version().to_string(),
        };
        crate::process_title::set_server_title(&identity.name);

        // Initialize the background runner even when ambient mode is disabled so
        // session-targeted scheduled tasks still have a live delivery loop.
        let ambient_runner = {
            let safety = Arc::new(crate::safety::SafetySystem::new());
            let handle = AmbientRunnerHandle::new(safety);
            crate::tool::ambient::init_schedule_runner(handle.clone());
            Some(handle)
        };

        let LoadedSwarmRuntimeState {
            plans: restored_swarm_plans,
            coordinators: restored_swarm_coordinators,
            members: restored_swarm_members,
            swarms_by_id: restored_swarms_by_id,
        } = load_persisted_swarm_runtime_state();

        Self {
            provider,
            socket_path: socket_path(),
            debug_socket_path: debug_socket_path(),
            gateway_config_override: None,
            identity,
            event_tx,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            is_processing: Arc::new(RwLock::new(false)),
            session_id: Arc::new(RwLock::new(String::new())),
            client_count: Arc::new(RwLock::new(0)),
            client_connections: Arc::new(RwLock::new(HashMap::new())),
            file_touch: FileTouchService::new(),
            swarm_state: SwarmState::new(
                restored_swarm_members,
                restored_swarms_by_id,
                restored_swarm_plans,
                restored_swarm_coordinators,
            ),
            shared_context: Arc::new(RwLock::new(HashMap::new())),
            client_debug_state: Arc::new(RwLock::new(ClientDebugState::default())),
            client_debug_response_tx,
            debug_jobs: Arc::new(RwLock::new(HashMap::new())),
            channel_subscriptions: Arc::new(RwLock::new(HashMap::new())),
            channel_subscriptions_by_session: Arc::new(RwLock::new(HashMap::new())),
            event_history: Arc::new(RwLock::new(std::collections::VecDeque::new())),
            event_counter: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            swarm_event_tx: broadcast::channel(256).0,
            ambient_runner,
            mcp_pool: Arc::new(OnceCell::new()),
            shutdown_signals: Arc::new(RwLock::new(HashMap::new())),
            soft_interrupt_queues: Arc::new(RwLock::new(HashMap::new())),
            await_members_runtime: AwaitMembersRuntime::default(),
            swarm_mutation_runtime: SwarmMutationRuntime::default(),
        }
    }

    pub fn new_with_paths(
        provider: Arc<dyn Provider>,
        socket_path: PathBuf,
        debug_socket_path: PathBuf,
    ) -> Self {
        let mut server = Self::new(provider);
        server.socket_path = socket_path;
        server.debug_socket_path = debug_socket_path;
        server
    }

    pub fn with_gateway_config(mut self, gateway_config: crate::gateway::GatewayConfig) -> Self {
        self.gateway_config_override = Some(gateway_config);
        self
    }

    /// Get the server identity
    pub fn identity(&self) -> &ServerIdentity {
        &self.identity
    }

    fn runtime(&self) -> ServerRuntime {
        ServerRuntime::from_server(self)
    }

    fn build_registry_info(&self) -> crate::registry::ServerInfo {
        crate::registry::ServerInfo {
            id: self.identity.id.clone(),
            name: self.identity.name.clone(),
            icon: self.identity.icon.clone(),
            socket: self.socket_path.clone(),
            debug_socket: self.debug_socket_path.clone(),
            git_hash: self.identity.git_hash.clone(),
            version: self.identity.version.clone(),
            pid: std::process::id(),
            started_at: chrono::Utc::now().to_rfc3339(),
            sessions: Vec::new(),
        }
    }

    fn spawn_registry_prewarm(&self) {
        let registry_warm_provider = Arc::clone(&self.provider);
        tokio::spawn(async move {
            let start = Instant::now();
            let provider = registry_warm_provider.fork();
            let _ = crate::tool::Registry::new(provider).await;
            crate::logging::info(&format!(
                "Registry prewarm completed in {}ms",
                start.elapsed().as_millis()
            ));
        });
    }

    async fn finish_startup_after_bind(
        &self,
        main_listener: Listener,
        debug_listener: Listener,
        server_start_time: Instant,
    ) -> (
        ServerRuntime,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
        self.spawn_registry_prewarm();
        let registry_info = self.build_registry_info();

        let runtime = self.runtime();
        let main_handle = runtime.spawn_main_accept_loop(main_listener);
        let debug_handle = runtime.spawn_debug_accept_loop(debug_listener, server_start_time);

        crate::logging::info("Accept loop tasks spawned");

        // Signal readiness to the spawning client only after the accept loops
        // are live, so a "ready" server can immediately handle requests.
        publish_reload_socket_ready();
        signal_ready_fd();

        // Persist auxiliary discovery metadata after the server is already live.
        self.spawn_registry_metadata_publisher(registry_info);

        // Spawn WebSocket gateway for iOS/web clients (if enabled)
        self.spawn_gateway(runtime.clone()).await;

        // Startup recovery can be expensive in multi-session reloads. Run it
        // only after the replacement daemon is already accepting reconnects.
        self.recover_headless_sessions_on_startup().await;

        (runtime, main_handle, debug_handle)
    }

    fn spawn_registry_metadata_publisher(&self, registry_info: crate::registry::ServerInfo) {
        let registry_identity = self.identity.display_name();
        tokio::spawn(async move {
            let hash_path = format!("{}.hash", registry_info.socket.display());
            let _ = std::fs::write(&hash_path, jcode_build_meta::git_hash());

            let mut registry = crate::registry::ServerRegistry::load()
                .await
                .unwrap_or_default();
            registry.register(registry_info);
            let _ = registry.save().await;
            crate::logging::info(&format!(
                "Registered as {} in server registry",
                registry_identity,
            ));

            if let Ok(mut registry) = crate::registry::ServerRegistry::load().await {
                let _ = registry.cleanup_stale().await;
                let _ = registry.save().await;
            }
        });
    }

    /// Spawn the background loop that keeps the machine awake while any session
    /// is actively streaming/processing.
    ///
    /// The shared daemon owns every session, so a single inhibitor here covers
    /// all of them. We poll the swarm-member map (the authoritative "running"
    /// signal that also drives Waybar's "N streaming" indicator) on a short
    /// interval and reconcile a best-effort OS power inhibitor against it. The
    /// inhibitor blocks automatic system sleep; Linux also blocks lid-switch
    /// handling. Windows still honors explicit lid/power-button actions from the
    /// active power plan. The display can turn off. When no session is running,
    /// the guard is released so normal power management resumes immediately.
    fn spawn_power_inhibitor(swarm_members: Arc<RwLock<HashMap<String, SwarmMember>>>) {
        // Reconcile interval. Short enough that the inhibitor engages promptly
        // when a turn starts and releases promptly when work finishes, but cheap
        // (a read lock + a scan) so it adds no meaningful load.
        const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

        let mut inhibitor = crate::power_inhibit::PowerInhibitor::new();
        if !inhibitor.is_available() {
            // Disabled via the legacy env escape hatch, or unsupported platform.
            crate::logging::info(
                "power_inhibit: unavailable (unsupported platform or JCODE_DISABLE_POWER_INHIBIT set); not monitoring",
            );
            return;
        }

        crate::logging::info(
            "power_inhibit: monitoring active sessions to prevent sleep while streaming",
        );

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_active: Option<bool> = None;
            loop {
                interval.tick().await;

                // Re-evaluate the config each tick so toggling it at runtime
                // takes effect without restarting the daemon.
                let enabled = crate::config::config().power.prevent_sleep_while_streaming;

                let active = enabled && Self::any_session_streaming(&swarm_members).await;
                if last_active != Some(active) {
                    crate::logging::info(&format!(
                        "power_inhibit: {} (streaming sessions {})",
                        if active { "engaging" } else { "releasing" },
                        if active { "present" } else { "absent" },
                    ));
                    last_active = Some(active);
                }
                inhibitor.set_active(active);
            }
        });
    }

    /// Whether at least one session is currently in the "running" state, i.e.
    /// actively streaming/processing a turn. This is the same signal that drives
    /// the Waybar "N streaming" indicator.
    async fn any_session_streaming(
        swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    ) -> bool {
        let members = swarm_members.read().await;
        members.values().any(|member| member.status == "running")
    }

    pub async fn run(&self) -> Result<()> {
        // Ensure socket directory exists (for named sockets like /run/user/1000/jcode/)
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        #[cfg(unix)]
        let _daemon_lock = acquire_daemon_lock()?;

        if socket_has_live_listener(&self.socket_path).await {
            anyhow::bail!(
                "Refusing to replace active server socket at {}",
                self.socket_path.display()
            );
        }

        // Remove existing sockets (uses transport abstraction for cross-platform cleanup)
        crate::transport::remove_socket(&self.socket_path);
        crate::transport::remove_socket(&self.debug_socket_path);

        let main_listener = Listener::bind(&self.socket_path)?;
        let debug_listener = Listener::bind(&self.debug_socket_path)?;

        #[cfg(unix)]
        {
            // Server reload uses exec. Force the published listener fds to close
            // across exec so the replacement daemon can safely rebind them.
            mark_close_on_exec(&main_listener);
            mark_close_on_exec(&debug_listener);
        }

        // Preserve an in-flight reload marker for exec-based reloads owned by this
        // process, but clear stale markers from unrelated/stale processes.
        clear_reload_marker_if_stale_for_pid(std::process::id());

        match reload_recovery::collect_garbage() {
            Ok(stats) if stats.removed > 0 || stats.errors > 0 => {
                crate::logging::info(&format!(
                    "Reload recovery GC: removed={}, retained={}, errors={}",
                    stats.removed, stats.retained, stats.errors
                ));
            }
            Ok(_) => {}
            Err(error) => crate::logging::warn(&format!(
                "Reload recovery GC failed during startup: {error}"
            )),
        }

        // Restrict socket files to owner-only so other local users cannot connect.
        let _ = crate::platform::set_permissions_owner_only(&self.socket_path);
        let _ = crate::platform::set_permissions_owner_only(&self.debug_socket_path);

        // Set logging context for this server
        crate::logging::set_server(&self.identity.name);

        // Log server identity
        crate::logging::info(&format!(
            "Server {} starting ({})",
            self.identity.display_name(),
            self.identity.version
        ));
        crate::logging::info(&format!("Server listening on {:?}", self.socket_path));
        crate::logging::info(&format!("Debug socket on {:?}", self.debug_socket_path));

        let temporary_server_policy = lifecycle::temporary_server_policy_from_env();
        if let Some(policy) = temporary_server_policy.as_ref() {
            crate::logging::info(&format!(
                "Temporary server lifecycle enabled: owner_pid={:?}, idle_timeout_secs={}",
                policy.owner_pid, policy.idle_timeout_secs
            ));
            let _ = lifecycle::write_temporary_metadata(
                &self.socket_path,
                &self.debug_socket_path,
                policy,
            );
        }

        let server_start_time = Instant::now();

        self.spawn_background_tasks(server_start_time, temporary_server_policy);
        let (runtime, main_handle, debug_handle) = self
            .finish_startup_after_bind(main_listener, debug_listener, server_start_time)
            .await;

        // If either listener exits unexpectedly, stop accepting work and wait
        // for every owned connection task before returning. The normal daemon
        // path runs until process shutdown or exec-based reload.
        let mut main_handle = main_handle;
        let mut debug_handle = debug_handle;
        tokio::select! {
            result = &mut main_handle => {
                if let Err(error) = result {
                    crate::logging::error(&format!("Main accept loop failed: {error}"));
                }
                runtime.shutdown().await;
                let _ = debug_handle.await;
            }
            result = &mut debug_handle => {
                if let Err(error) = result {
                    crate::logging::error(&format!("Debug accept loop failed: {error}"));
                }
                runtime.shutdown().await;
                let _ = main_handle.await;
            }
        }
        Ok(())
    }

    /// Spawn the WebSocket gateway if enabled in config.
    /// The runtime task scope owns both the listener and client accept loop so
    /// server shutdown can cancel and join them with the other connection work.
    async fn spawn_gateway(&self, runtime: ServerRuntime) {
        let config = if let Some(override_config) = &self.gateway_config_override {
            override_config.clone()
        } else {
            let gw_config = &crate::config::config().gateway;
            crate::gateway::GatewayConfig {
                port: gw_config.port,
                bind_addr: gw_config.bind_addr.clone(),
                enabled: gw_config.enabled,
            }
        };

        if !config.enabled {
            return;
        }

        let (client_tx, client_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::gateway::GatewayClient>();

        let listener_runtime = runtime.clone();
        let listener_spawned = runtime
            .spawn_background_task(async move {
                if let Err(e) = crate::gateway::run_gateway(config, client_tx).await {
                    crate::logging::error(&format!("Gateway error: {}", e));
                }
            })
            .await;
        if listener_spawned {
            let _ = listener_runtime.spawn_gateway_accept_loop(client_rx).await;
        }
    }
}

pub use self::client_api::Client;

#[cfg(test)]
mod tests;
