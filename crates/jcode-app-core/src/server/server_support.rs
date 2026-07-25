const SERVER_NAME_ENV: &str = "JCODE_SERVER_NAME";
const SERVER_DISPLAY_NAME_ENV: &str = "JCODE_SERVER_DISPLAY_NAME";
const MAX_CONFIGURED_SERVER_NAME_LEN: usize = 64;
const SWARM_TERMINAL_MEMBER_GC_BATCH_SIZE: usize = 64;

async fn prune_expired_terminal_swarm_members(
    sessions: &SessionAgents,
    swarm_state: &SwarmState,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
) -> usize {
    let retention = swarm::swarm_terminal_member_retention();
    let mut candidates = {
        let members = swarm_state.members.read().await;
        expired_terminal_member_ids(&members, retention)
    };
    candidates.truncate(SWARM_TERMINAL_MEMBER_GC_BATCH_SIZE);
    if candidates.is_empty() {
        return 0;
    }

    let live_sessions: HashSet<String> = sessions.read().await.keys().cloned().collect();
    let mut pruned = 0usize;
    for session_id in candidates {
        // A session can be resumed between candidate collection and removal.
        // Never collect a member that currently has a live agent runtime.
        if live_sessions.contains(&session_id) || sessions.read().await.contains_key(&session_id) {
            continue;
        }
        let removed_swarm_id = {
            let mut members = swarm_state.members.write().await;
            let still_expired = members.get(&session_id).is_some_and(|member| {
                swarm::member_status_is_terminal(&member.status)
                    && member.last_status_change.elapsed() >= retention
            });
            if still_expired {
                members
                    .remove(&session_id)
                    .and_then(|member| member.swarm_id)
            } else {
                None
            }
        };
        let Some(swarm_id) = removed_swarm_id else {
            continue;
        };

        remove_session_from_swarm(
            &session_id,
            &swarm_id,
            &swarm_state.members,
            &swarm_state.swarms_by_id,
            &swarm_state.coordinators,
            &swarm_state.plans,
        )
        .await;
        remove_session_channel_subscriptions(
            &session_id,
            channel_subscriptions,
            channel_subscriptions_by_session,
        )
        .await;
        pruned += 1;
    }

    if pruned > 0 {
        crate::logging::info(&format!(
            "Garbage-collected {pruned} expired terminal swarm member(s)"
        ));
    }
    pruned
}

/// Reap spawned swarm workers that finished their work and have sat idle past
/// the reap window: ask any attached client window to close, shut down the
/// server-side agent, and remove the member from swarm state.
///
/// This is the server-side backstop for coordinator `cleanup`: coordinators
/// that get interrupted, replaced, or never call cleanup used to leave every
/// spawned worker running forever (one ~80-150 MB client process each). Only
/// agent-spawned members (`report_back_to_session_id` set) are eligible;
/// user-created sessions are never touched. See
/// [`swarm::idle_spawned_worker_reap_candidates`] for the exact policy.
async fn reap_idle_spawned_workers(
    sessions: &SessionAgents,
    swarm_state: &SwarmState,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
    soft_interrupt_queues: &SessionInterruptQueues,
) -> usize {
    let Some(idle_after) = swarm::swarm_idle_worker_reap_after() else {
        return 0;
    };
    let candidates = {
        let members = swarm_state.members.read().await;
        swarm::idle_spawned_worker_reap_candidates(&members, idle_after)
    };
    if candidates.is_empty() {
        return 0;
    }

    let mut reaped = 0usize;
    for session_id in candidates {
        // Re-validate under the current map: status may have changed between
        // candidate collection and removal (a resumed/reassigned worker).
        let still_reapable = {
            let members = swarm_state.members.read().await;
            members.get(&session_id).is_some_and(|member| {
                member.report_back_to_session_id.is_some()
                    && member.role != "coordinator"
                    && (member.status == "ready"
                        || swarm::member_status_is_terminal(&member.status))
                    && member.last_status_change.elapsed() >= idle_after
            })
        };
        if !still_reapable {
            continue;
        }

        let agent_arc = { sessions.read().await.get(&session_id).cloned() };
        let Some(agent_arc) = agent_arc else {
            crate::logging::warn(&format!(
                "Cannot reap idle spawned swarm worker {session_id}: live agent entry is missing"
            ));
            continue;
        };
        let close_result = match agent_arc.try_lock() {
            Ok(mut agent) => agent.try_mark_closed(),
            Err(_) => {
                crate::logging::info(&format!(
                    "Deferring idle spawned swarm worker reap for {session_id}: agent is busy"
                ));
                continue;
            }
        };
        if let Err(error) = close_result {
            crate::logging::error(&format!(
                "Cannot reap idle spawned swarm worker {session_id}: durable close failed: {error}"
            ));
            continue;
        }

        // Ask any attached client (visible spawned window) to close only after
        // the canonical session state is durably closed.
        let _ = fanout_session_event(
            &swarm_state.members,
            &session_id,
            ServerEvent::SessionCloseRequested {
                reason: format!(
                    "Idle spawned worker reaped after {}s of inactivity",
                    idle_after.as_secs()
                ),
            },
        )
        .await;

        if let Some(agent_arc) = remove_session_entry(sessions, &session_id).await {
            remove_session_interrupt_queue(soft_interrupt_queues, &session_id).await;
            remove_background_tool_signal(&session_id);
            drop(agent_arc);
        }

        let removed_swarm_id = {
            let mut members = swarm_state.members.write().await;
            members
                .remove(&session_id)
                .and_then(|member| member.swarm_id)
        };
        if let Some(ref swarm_id) = removed_swarm_id {
            remove_session_from_swarm(
                &session_id,
                swarm_id,
                &swarm_state.members,
                &swarm_state.swarms_by_id,
                &swarm_state.coordinators,
                &swarm_state.plans,
            )
            .await;
        }
        remove_session_channel_subscriptions(
            &session_id,
            channel_subscriptions,
            channel_subscriptions_by_session,
        )
        .await;
        crate::logging::info(&format!(
            "Reaped idle spawned swarm worker {session_id} (idle > {}s)",
            idle_after.as_secs()
        ));
        reaped += 1;
    }
    reaped
}

pub(super) async fn persist_swarm_state_for(swarm_id: &str, swarm_state: &SwarmState) {
    // Never call this while holding any SwarmState map guard. The operation
    // lock deliberately spans the independent map reads and atomic file write.
    let operation_lock = swarm_operation_lock(swarm_id);
    let _operation_guard = operation_lock.lock().await;
    let runtime = swarm_state.load_runtime(swarm_id).await;
    persist_swarm_state_snapshot(
        swarm_id,
        runtime.plan.as_ref(),
        runtime.coordinator_session_id.as_deref(),
        &runtime.members,
    );
}

pub(super) async fn remove_persisted_swarm_state_for(swarm_id: &str, swarm_state: &SwarmState) {
    // Persist and remove share one per-swarm ordering domain. The file version
    // is an extra CAS guard against direct/recovery writers outside this path.
    let operation_lock = swarm_operation_lock(swarm_id);
    let _operation_guard = operation_lock.lock().await;
    let file_version = capture_swarm_state_version(swarm_id);
    let runtime = swarm_state.load_runtime(swarm_id).await;
    if runtime.has_any_state() {
        return;
    }
    let _ = remove_swarm_state_if_version(swarm_id, &file_version);
}

fn headless_member_should_restore(status: &str, is_headless: bool) -> bool {
    is_headless
        && !matches!(
            status,
            "ready" | "completed" | "done" | "failed" | "stopped"
        )
}

fn headless_reload_continuation_message(reload_ctx: Option<ReloadContext>) -> Option<String> {
    ReloadContext::recovery_directive(reload_ctx.as_ref(), true, "", None)
        .map(|directive| directive.continuation_message)
}

fn configured_server_name(cli_name: Option<String>) -> Option<String> {
    cli_name
        .as_deref()
        .and_then(normalize_configured_server_name)
        .or_else(configured_server_name_from_env)
}

fn configured_server_name_from_env() -> Option<String> {
    [SERVER_NAME_ENV, SERVER_DISPLAY_NAME_ENV]
        .into_iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .and_then(|value| normalize_configured_server_name(&value))
        })
}

fn normalize_configured_server_name(raw: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut previous_dash = false;

    for ch in raw.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if ch == '.' || ch == '-' {
            ch
        } else {
            '-'
        };

        if mapped == '-' {
            if previous_dash {
                continue;
            }
            previous_dash = true;
        } else {
            previous_dash = false;
        }
        normalized.push(mapped);
        if normalized.len() >= MAX_CONFIGURED_SERVER_NAME_LEN {
            break;
        }
    }

    let trimmed = normalized.trim_matches(|ch| matches!(ch, '-' | '.'));
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[derive(Default)]
struct HeadlessRecoveryStats {
    candidates: usize,
    resumed: usize,
    skipped: usize,
    failed_to_load: usize,
}

async fn capture_runtime_memory_common_sample(
    identity: &ServerIdentity,
    client_count: &Arc<RwLock<usize>>,
    server_start_time: Instant,
    kind: &str,
    source: &str,
    trigger: RuntimeMemoryLogTrigger,
    sampling: RuntimeMemoryLogSampling,
) -> ServerRuntimeMemorySample {
    let now = chrono::Utc::now();
    let process =
        crate::process_memory::snapshot_with_source(format!("server:runtime-log:{source}"));
    let connected_count = *client_count.read().await;
    let background_task_count = crate::background::global().list().await.len();
    let embedder_stats = crate::embedding::stats();
    let embedding_model_available = crate::embedding::is_model_available();

    ServerRuntimeMemorySample {
        schema_version: 2,
        kind: kind.to_string(),
        timestamp: now.to_rfc3339(),
        timestamp_ms: now.timestamp_millis(),
        source: source.to_string(),
        trigger,
        sampling,
        server: ServerRuntimeMemoryServer {
            id: identity.id.clone(),
            name: identity.name.clone(),
            icon: identity.icon.clone(),
            version: identity.version.clone(),
            git_hash: identity.git_hash.clone(),
            uptime_secs: server_start_time.elapsed().as_secs(),
        },
        process_diagnostics: crate::runtime_memory_log::build_process_diagnostics(&process),
        process,
        clients: ServerRuntimeMemoryClients { connected_count },
        sessions: None,
        background: ServerRuntimeMemoryBackground {
            task_count: background_task_count,
        },
        embeddings: ServerRuntimeMemoryEmbeddings {
            model_available: embedding_model_available,
            stats: embedder_stats,
        },
    }
}

async fn capture_runtime_memory_process_sample(
    identity: &ServerIdentity,
    client_count: &Arc<RwLock<usize>>,
    server_start_time: Instant,
    source: &str,
    trigger: RuntimeMemoryLogTrigger,
    sampling: RuntimeMemoryLogSampling,
) -> ServerRuntimeMemorySample {
    capture_runtime_memory_common_sample(
        identity,
        client_count,
        server_start_time,
        "process",
        source,
        trigger,
        sampling,
    )
    .await
}

async fn capture_runtime_memory_attribution_sample(
    identity: &ServerIdentity,
    sessions: &SessionAgents,
    client_count: &Arc<RwLock<usize>>,
    server_start_time: Instant,
    source: &str,
    trigger: RuntimeMemoryLogTrigger,
    sampling: RuntimeMemoryLogSampling,
) -> ServerRuntimeMemorySample {
    let mut sample = capture_runtime_memory_common_sample(
        identity,
        client_count,
        server_start_time,
        "attribution",
        source,
        trigger,
        sampling,
    )
    .await;

    let sessions_guard = sessions.read().await;
    let live_count = sessions_guard.len();
    let mut sampled_count = 0usize;
    let mut contended_count = 0usize;
    let mut memory_enabled_session_count = 0usize;
    let mut total_message_count = 0u64;
    let mut total_provider_cache_message_count = 0u64;
    let mut total_json_bytes = 0u64;
    let mut total_payload_text_bytes = 0u64;
    let mut total_provider_cache_json_bytes = 0u64;
    let mut total_tool_result_bytes = 0u64;
    let mut total_provider_cache_tool_result_bytes = 0u64;
    let mut total_large_blob_bytes = 0u64;
    let mut total_provider_cache_large_blob_bytes = 0u64;
    let mut top_sessions: Vec<ServerRuntimeMemoryTopSession> = Vec::new();

    for (session_id, agent_arc) in sessions_guard.iter() {
        let Ok(mut agent) = agent_arc.try_lock() else {
            contended_count += 1;
            continue;
        };

        sampled_count += 1;
        let profile = agent.session_memory_profile_snapshot();
        let memory_enabled = agent.memory_enabled();
        if memory_enabled {
            memory_enabled_session_count += 1;
        }

        let message_count = profile.message_count as u64;
        let provider_cache_message_count = profile.provider_cache_message_count as u64;
        let json_bytes = profile.total_json_bytes as u64;
        let payload_text_bytes = profile.payload_text_bytes as u64;
        let provider_cache_json_bytes = profile.provider_cache_json_bytes as u64;
        let tool_result_bytes = profile.canonical_tool_result_bytes as u64;
        let provider_cache_tool_result_bytes = profile.provider_cache_tool_result_bytes as u64;
        let large_blob_bytes = profile.canonical_large_blob_bytes as u64;
        let provider_cache_large_blob_bytes = profile.provider_cache_large_blob_bytes as u64;

        total_message_count += message_count;
        total_provider_cache_message_count += provider_cache_message_count;
        total_json_bytes += json_bytes;
        total_payload_text_bytes += payload_text_bytes;
        total_provider_cache_json_bytes += provider_cache_json_bytes;
        total_tool_result_bytes += tool_result_bytes;
        total_provider_cache_tool_result_bytes += provider_cache_tool_result_bytes;
        total_large_blob_bytes += large_blob_bytes;
        total_provider_cache_large_blob_bytes += provider_cache_large_blob_bytes;

        top_sessions.push(ServerRuntimeMemoryTopSession {
            session_id: session_id.clone(),
            provider: agent.provider_name(),
            model: agent.provider_model(),
            memory_enabled,
            message_count,
            provider_cache_message_count,
            json_bytes,
            payload_text_bytes,
            provider_cache_json_bytes,
            tool_result_bytes,
            provider_cache_tool_result_bytes,
            large_blob_bytes,
            provider_cache_large_blob_bytes,
        });
    }
    drop(sessions_guard);

    top_sessions.sort_by(|left, right| right.json_bytes.cmp(&left.json_bytes));
    top_sessions.truncate(5);

    sample.sessions = Some(ServerRuntimeMemorySessions {
        live_count,
        sampled_count,
        contended_count,
        memory_enabled_session_count,
        total_message_count,
        total_provider_cache_message_count,
        total_json_bytes,
        total_payload_text_bytes,
        total_provider_cache_json_bytes,
        total_tool_result_bytes,
        total_provider_cache_tool_result_bytes,
        total_large_blob_bytes,
        total_provider_cache_large_blob_bytes,
        top_by_json_bytes: top_sessions,
    });
    sample
}

mod state;

use self::state::latest_peer_touches;
pub use self::state::{
    FileAccess, SessionControlHandle, SharedContext, SwarmEvent, SwarmEventType, SwarmMember,
    SwarmState,
};
use self::state::{
    SessionInterruptQueues, fanout_live_client_event, fanout_session_event,
    queue_soft_interrupt_for_session, register_background_tool_signal,
    register_session_event_sender, register_session_interrupt_queue, remove_background_tool_signal,
    remove_session_interrupt_queue, session_event_fanout_sender,
    unregister_session_event_sender,
};
pub use crate::plan::{SwarmTaskProgress, VersionedPlan};

pub use self::await_members_state::pending_await_members_for_session;
use self::reload_state::clear_reload_marker_if_stale_for_pid;
#[cfg(test)]
pub(crate) use self::reload_state::subscribe_reload_signal_for_tests;
pub use self::reload_state::{
    ReloadAck, ReloadPhase, ReloadSignal, ReloadState, ReloadWaitStatus, acknowledge_reload_signal,
    await_reload_handoff, clear_reload_marker, inspect_reload_wait_status,
    publish_reload_socket_ready, recent_reload_state, reload_marker_active, reload_marker_exists,
    reload_marker_path, reload_process_alive, reload_state_summary, send_reload_signal,
    wait_for_reload_ack, wait_for_reload_handoff_event, write_reload_marker, write_reload_state,
};

pub use self::lifecycle::configure_temporary_server;
#[cfg(unix)]
pub use self::socket::spawn_server_notify;
#[cfg(unix)]
use self::socket::{acquire_daemon_lock, mark_close_on_exec};
pub use self::socket::{
    cleanup_socket_pair, connect_socket, debug_socket_path, has_live_listener, is_server_ready,
    reap_stale_socket_if_dead, set_socket_path, socket_path, wait_for_server_ready,
};
use self::socket::{signal_ready_fd, socket_has_live_listener};

pub use self::util::ServerIdentity;
pub(crate) use self::util::server_has_newer_binary;
use self::util::{
    debug_control_allowed, embedding_idle_unload_secs, git_common_dir_for, reload_exec_target,
    startup_headless_recovery_test_delay, swarm_id_for_dir,
};

mod file_activity;
use self::file_activity::file_activity_scope_label;

mod file_touch_service;
pub(crate) use self::file_touch_service::FileTouchService;

#[cfg(test)]
mod socket_tests;

#[cfg(test)]
mod startup_tests;

#[cfg(test)]
mod queue_tests;

#[cfg(test)]
mod file_activity_tests;

/// Idle timeout for the shared server when no clients are connected (5 minutes)
const IDLE_TIMEOUT_SECS: u64 = 300;

/// How often to check whether the embedding model can be unloaded. Keep this
/// comfortably below the default idle threshold so reclamation is prompt and
/// predictable rather than delayed by another full sampling interval.
const EMBEDDING_IDLE_CHECK_SECS: u64 = 10;

/// How often the retained-heap watchdog samples allocator retention.
const HEAP_RETENTION_CHECK_SECS: u64 = 120;

/// Exit code when server shuts down due to idle timeout
pub const EXIT_IDLE_TIMEOUT: i32 = 44;
