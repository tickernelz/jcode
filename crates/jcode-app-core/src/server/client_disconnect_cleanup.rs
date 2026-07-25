use super::{
    ClientConnectionInfo, ClientDebugState, FileTouchService, SessionInterruptQueues, SwarmEvent,
    SwarmEventType, SwarmMember, VersionedPlan, record_swarm_event, remove_background_tool_signal,
    remove_session_channel_subscriptions, remove_session_from_swarm,
    remove_session_interrupt_queue, unregister_session_event_sender, update_member_status,
};
use crate::agent::Agent;
use anyhow::Result;
use jcode_agent_runtime::InterruptSignal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;
type ChannelSubscriptions = Arc<RwLock<HashMap<String, HashMap<String, HashSet<String>>>>>;

const RELOAD_DISCONNECT_MARKER_MAX_AGE: Duration = Duration::from_secs(30);
const DISCONNECT_AGENT_LOCK_WARN_AFTER: Duration = Duration::from_secs(2);

async fn acquire_disconnect_cleanup_lock<'a, T>(
    lock: &'a Mutex<T>,
    session_id: &str,
    warn_after: Duration,
) -> tokio::sync::MutexGuard<'a, T> {
    match tokio::time::timeout(warn_after, lock.lock()).await {
        Ok(guard) => guard,
        Err(_) => {
            // The connection cleanup future remains the retry owner. Returning
            // here used to strand the Agent in the managed map forever after a
            // single two-second scheduling delay.
            crate::logging::warn(&format!(
                "Session {session_id} cleanup is still waiting for the agent lock; retaining cleanup ownership until it can publish durable lifecycle state"
            ));
            lock.lock().await
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisconnectDisposition {
    Closed,
    Crashed,
    Reloading,
}

fn disconnect_disposition(disconnected_while_processing: bool) -> DisconnectDisposition {
    if !disconnected_while_processing {
        return DisconnectDisposition::Closed;
    }

    if crate::server::reload_marker_active(RELOAD_DISCONNECT_MARKER_MAX_AGE) {
        DisconnectDisposition::Reloading
    } else {
        DisconnectDisposition::Crashed
    }
}

fn abort_disconnected_runtime_tasks(
    processing_task: &mut Option<tokio::task::JoinHandle<()>>,
    event_handle: tokio::task::JoinHandle<()>,
) {
    if let Some(handle) = processing_task.take() {
        handle.abort();
    }
    event_handle.abort();
}

async fn persist_terminal_disposition(
    agent: &mut Agent,
    session_id: &str,
    disposition: DisconnectDisposition,
) {
    let mut retry_delay = Duration::from_millis(25);
    loop {
        let result = match disposition {
            DisconnectDisposition::Closed => agent.try_mark_closed(),
            DisconnectDisposition::Reloading => {
                agent.try_mark_crashed(Some("Server reload interrupted processing".to_string()))
            }
            DisconnectDisposition::Crashed => {
                agent.try_mark_crashed(Some("Client disconnected while processing".to_string()))
            }
        };
        match result {
            Ok(()) => return,
            Err(error) => {
                crate::logging::warn(&format!(
                    "Session {session_id} terminal disconnect publication failed ({error}); retaining cleanup ownership and retrying"
                ));
                if let Err(refresh_error) = agent.refresh_session_for_terminal_retry() {
                    crate::logging::warn(&format!(
                        "Session {session_id} cleanup could not refresh the latest durable generation ({refresh_error}); retrying without relinquishing ownership"
                    ));
                }
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(Duration::from_secs(2));
            }
        }
    }
}

async fn session_has_live_successor(
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    session_id: &str,
) -> bool {
    client_connections
        .read()
        .await
        .values()
        .any(|info| info.session_id == session_id)
}

async fn claim_session_for_disconnect_cleanup(
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    session_id: &str,
    expected_agent: &Arc<Mutex<Agent>>,
) -> Option<Arc<Mutex<Agent>>> {
    let connections = client_connections.read().await;
    if connections
        .values()
        .any(|info| info.session_id == session_id)
    {
        return None;
    }

    // Attachment transitions take client_connections before sessions. Keep that
    // order so checking ownership and removing the attach target are atomic.
    // The pointer check is a second ownership fence for non-client map writers:
    // stale cleanup may never claim a replacement Agent by session ID alone.
    let removed = {
        let mut sessions = sessions.write().await;
        if sessions
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, expected_agent))
        {
            sessions.remove(session_id)
        } else {
            None
        }
    };
    if removed.is_some() {
        crate::storage::unregister_active_pid(session_id);
    }
    removed
}

#[expect(
    clippy::too_many_arguments,
    reason = "disconnect cleanup updates sessions, swarms, files, channels, debug state, and shutdown signals together"
)]
pub(super) async fn cleanup_client_connection(
    sessions: &SessionAgents,
    expected_agent: &Arc<Mutex<Agent>>,
    client_session_id: &str,
    client_is_processing: bool,
    processing_task: &mut Option<tokio::task::JoinHandle<()>>,
    event_handle: tokio::task::JoinHandle<()>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    file_touch: &FileTouchService,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
    client_debug_state: &Arc<RwLock<ClientDebugState>>,
    client_debug_id: &str,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    client_connection_id: &str,
    shutdown_signals: &Arc<RwLock<HashMap<String, InterruptSignal>>>,
    soft_interrupt_queues: &SessionInterruptQueues,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
) -> Result<()> {
    // Resume takes the same per-session lease before reading durable state and
    // retains it until replacement Agent and connection ownership are both
    // visible. Keep this lease through the entire terminal/control-plane
    // teardown so a late successor either wins before cleanup or starts after
    // cleanup has published a complete terminal transition.
    let _session_lifecycle_lease =
        super::client_session_lifecycle::acquire_session_lifecycle_lease(client_session_id).await;

    let disconnected_while_processing = client_is_processing
        || processing_task
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false);
    let disposition = disconnect_disposition(disconnected_while_processing);

    {
        let mut debug_state = client_debug_state.write().await;
        debug_state.unregister(client_debug_id);
    }
    {
        let mut connections = client_connections.write().await;
        connections.remove(client_connection_id);
    }
    unregister_session_event_sender(swarm_members, client_session_id, client_connection_id).await;

    // Release stale live ownership before slower cleanup so a reconnecting TUI can
    // reclaim the same session without tripping duplicate-attach guards.
    tokio::task::yield_now().await;

    let successor_connected =
        session_has_live_successor(client_connections, client_session_id).await;
    if successor_connected {
        crate::logging::info(&format!(
            "Skipping destructive disconnect cleanup for {} because another client is still attached",
            client_session_id
        ));
        // The successor owns the durable session, but it does not own runtime
        // work spawned by the revoked connection. End that stale ownership
        // before returning while still preserving the shared Agent/session.
        abort_disconnected_runtime_tasks(processing_task, event_handle);
        return Ok(());
    }

    // Runtime ownership must end even if durable lifecycle publication below
    // fails. Returning early while these handles survive can leave an orphaned
    // turn/tool task running against a session whose disconnect was rejected.
    abort_disconnected_runtime_tasks(processing_task, event_handle);

    {
        let agent_arc = claim_session_for_disconnect_cleanup(
            sessions,
            client_connections,
            client_session_id,
            expected_agent,
        )
        .await;
        if let Some(agent_arc) = agent_arc {
            let mut agent = acquire_disconnect_cleanup_lock(
                agent_arc.as_ref(),
                client_session_id,
                DISCONNECT_AGENT_LOCK_WARN_AFTER,
            )
            .await;
            persist_terminal_disposition(&mut agent, client_session_id, disposition).await;

            let memory_enabled = agent.memory_enabled();
            let transcript = if memory_enabled {
                Some(agent.build_transcript_for_extraction())
            } else {
                None
            };
            let sid = client_session_id.to_string();
            let working_dir = agent.working_dir().map(|dir| dir.to_string());
            drop(agent);
            let event = match disposition {
                DisconnectDisposition::Closed => {
                    crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                        "session_closed",
                        "client_disconnected",
                    )
                }
                DisconnectDisposition::Crashed => {
                    crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                        "session_crashed",
                        "client_disconnected_while_processing",
                    )
                }
                DisconnectDisposition::Reloading => {
                    crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                        "session_reloading",
                        "server_reload_disconnect",
                    )
                }
            }
            .with_session_id(sid.clone())
            .force_attribution();
            crate::runtime_memory_log::emit_event(event);
            if let Some(transcript) = transcript {
                crate::memory_agent::trigger_final_extraction_with_dir(
                    transcript,
                    sid,
                    working_dir,
                );
            }
        } else if session_has_live_successor(client_connections, client_session_id).await
            || sessions.read().await.contains_key(client_session_id)
        {
            crate::logging::info(&format!(
                "Skipping destructive disconnect cleanup for {} because live ownership changed during cleanup",
                client_session_id
            ));
            return Ok(());
        }
    }

    {
        let (status, detail) = match disposition {
            DisconnectDisposition::Closed => ("stopped", Some("disconnected".to_string())),
            DisconnectDisposition::Crashed => {
                ("crashed", Some("disconnect while running".to_string()))
            }
            DisconnectDisposition::Reloading => {
                ("stopped", Some("server reload in progress".to_string()))
            }
        };
        update_member_status(
            client_session_id,
            status,
            detail,
            swarm_members,
            swarms_by_id,
            Some(event_history),
            Some(event_counter),
            Some(swarm_event_tx),
        )
        .await;

        let (swarm_id, removed_name) = {
            let mut members = swarm_members.write().await;
            if let Some(member) = members.remove(client_session_id) {
                (member.swarm_id, member.friendly_name)
            } else {
                (None, None)
            }
        };
        crate::session_metrics::forget(client_session_id);
        crate::session_effort::forget_session_effort(client_session_id);

        if let Some(ref swarm_id) = swarm_id {
            record_swarm_event(
                event_history,
                event_counter,
                swarm_event_tx,
                client_session_id.to_string(),
                removed_name.clone(),
                Some(swarm_id.clone()),
                SwarmEventType::MemberChange {
                    action: "left".to_string(),
                },
            )
            .await;
            remove_session_from_swarm(
                client_session_id,
                swarm_id,
                swarm_members,
                swarms_by_id,
                swarm_coordinators,
                swarm_plans,
            )
            .await;
        }
        remove_session_channel_subscriptions(
            client_session_id,
            channel_subscriptions,
            channel_subscriptions_by_session,
        )
        .await;
        file_touch.clear_session(client_session_id).await;
    }

    {
        let mut signals = shutdown_signals.write().await;
        signals.remove(client_session_id);
    }
    remove_background_tool_signal(client_session_id);
    remove_session_interrupt_queue(soft_interrupt_queues, client_session_id).await;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::ClientConnectionInfo;
    use super::{
        DisconnectDisposition, abort_disconnected_runtime_tasks, acquire_disconnect_cleanup_lock,
        claim_session_for_disconnect_cleanup, disconnect_disposition, persist_terminal_disposition,
    };
    use crate::agent::Agent;
    use crate::message::{ContentBlock, Message, Role, ToolDefinition};
    use crate::provider::{EventStream, Provider};
    use crate::tool::Registry;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct IdentityProvider(jcode_provider_core::ExactRuntimeIdentity);

    #[async_trait]
    impl Provider for IdentityProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> anyhow::Result<EventStream> {
            unreachable!("disconnect persistence tests do not call providers")
        }

        fn name(&self) -> &str {
            "openai"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(Self(self.0.clone()))
        }

        fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
            Some(self.0.clone())
        }
    }

    fn identity(account_id: &str) -> jcode_provider_core::ExactRuntimeIdentity {
        jcode_provider_core::ExactRuntimeIdentity {
            provider_key: "openai".to_string(),
            route: jcode_provider_core::RouteSelection {
                model: "test-model".to_string(),
                runtime_key: jcode_provider_core::RuntimeKey::OpenAIOAuth,
                api_method: "openai-responses".to_string(),
                provider_label: "openai".to_string(),
                detail: String::new(),
            },
            account_label: Some(account_id.to_string()),
            account_id: Some(account_id.to_string()),
            account_generation: Some(1),
            reasoning_effort: None,
        }
    }

    async fn agent_for_session(session: crate::session::Session) -> Agent {
        let provider: Arc<dyn Provider> = Arc::new(IdentityProvider(
            session.exact_runtime_identity.clone().expect("identity"),
        ));
        let registry = Registry::new(Arc::clone(&provider)).await;
        Agent::new_with_session(provider, registry, session, None)
    }

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }

    async fn pending_task(signal: tokio::sync::oneshot::Sender<()>) {
        let _drop_signal = DropSignal(Some(signal));
        std::future::pending::<()>().await;
    }

    #[tokio::test]
    async fn disconnect_aborts_processing_and_event_tasks_before_fallible_cleanup() {
        let (processing_tx, processing_rx) = tokio::sync::oneshot::channel();
        let (event_tx, event_rx) = tokio::sync::oneshot::channel();
        let mut processing_task = Some(tokio::spawn(pending_task(processing_tx)));
        let event_task = tokio::spawn(pending_task(event_tx));
        tokio::task::yield_now().await;

        abort_disconnected_runtime_tasks(&mut processing_task, event_task);

        assert!(processing_task.is_none());
        tokio::time::timeout(std::time::Duration::from_secs(1), processing_rx)
            .await
            .expect("processing task should abort")
            .expect("processing drop signal");
        tokio::time::timeout(std::time::Duration::from_secs(1), event_rx)
            .await
            .expect("event task should abort")
            .expect("event drop signal");
    }

    #[tokio::test]
    async fn successor_cleanup_aborts_stale_owner_tasks_without_terminal_publication() {
        let (processing_tx, processing_rx) = tokio::sync::oneshot::channel();
        let (event_tx, event_rx) = tokio::sync::oneshot::channel();
        let mut processing_task = Some(tokio::spawn(pending_task(processing_tx)));
        let event_task = tokio::spawn(pending_task(event_tx));
        tokio::task::yield_now().await;

        // This is the runtime-ownership portion of the successor path. Durable
        // Agent/session cleanup remains intentionally skipped for the successor.
        abort_disconnected_runtime_tasks(&mut processing_task, event_task);

        assert!(processing_task.is_none());
        tokio::time::timeout(std::time::Duration::from_secs(1), processing_rx)
            .await
            .expect("stale processing task should abort")
            .expect("stale processing drop signal");
        tokio::time::timeout(std::time::Duration::from_secs(1), event_rx)
            .await
            .expect("stale event task should abort")
            .expect("stale event drop signal");
    }

    #[tokio::test]
    async fn attach_after_successor_check_prevents_terminal_cleanup_claim() {
        let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let connections = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let session_id = "reattached-session";
        let mut session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        session.exact_runtime_identity = Some(identity("account-a"));
        let agent = Arc::new(tokio::sync::Mutex::new(agent_for_session(session).await));
        sessions
            .write()
            .await
            .insert(session_id.to_string(), Arc::clone(&agent));

        assert!(!super::session_has_live_successor(&connections, session_id).await);
        let now = std::time::Instant::now();
        let (disconnect_tx, _disconnect_rx) = tokio::sync::mpsc::unbounded_channel();
        connections.write().await.insert(
            "successor".to_string(),
            ClientConnectionInfo {
                client_id: "successor".to_string(),
                session_id: session_id.to_string(),
                client_instance_id: None,
                debug_client_id: None,
                connected_at: now,
                last_seen: now,
                is_processing: false,
                current_tool_name: None,
                terminal_env: Vec::new(),
                disconnect_tx,
            },
        );

        assert!(
            claim_session_for_disconnect_cleanup(&sessions, &connections, session_id, &agent)
                .await
                .is_none()
        );
        assert!(Arc::ptr_eq(&sessions.read().await[session_id], &agent));
    }

    #[tokio::test]
    async fn cleanup_claim_rejects_replacement_agent_without_connection_publication() {
        let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let connections = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let session_id = "replacement-before-connection-publication";
        let mut old_session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        old_session.exact_runtime_identity = Some(identity("account-a"));
        let old_agent = Arc::new(tokio::sync::Mutex::new(
            agent_for_session(old_session).await,
        ));
        let mut replacement_session =
            crate::session::Session::create_with_id(session_id.to_string(), None, None);
        replacement_session.exact_runtime_identity = Some(identity("account-a"));
        let replacement = Arc::new(tokio::sync::Mutex::new(
            agent_for_session(replacement_session).await,
        ));
        sessions
            .write()
            .await
            .insert(session_id.to_string(), Arc::clone(&replacement));

        assert!(
            claim_session_for_disconnect_cleanup(&sessions, &connections, session_id, &old_agent,)
                .await
                .is_none(),
            "stale cleanup must not claim a replacement before its connection is visible"
        );
        assert!(Arc::ptr_eq(
            &sessions.read().await[session_id],
            &replacement
        ));
    }

    #[tokio::test]
    async fn disconnect_lock_timeout_retains_retry_ownership_until_release() {
        let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let held = lock.lock().await;
        let retry_lock = lock.clone();
        let retry = tokio::spawn(async move {
            let _guard = acquire_disconnect_cleanup_lock(
                retry_lock.as_ref(),
                "retry-session",
                std::time::Duration::from_millis(10),
            )
            .await;
            true
        });

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!retry.is_finished(), "cleanup owner returned after timeout");
        drop(held);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), retry)
                .await
                .expect("retry owner should acquire released lock")
                .expect("retry owner task")
        );
    }

    #[tokio::test]
    async fn terminal_publication_reloads_stale_generation_and_retries_to_closed() {
        let _lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let previous_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());

        let mut initial = crate::session::Session::create_with_id(
            crate::id::new_id("terminal-retry-same-account"),
            None,
            None,
        );
        initial.exact_runtime_identity = Some(identity("account-a"));
        initial.save().expect("initial save");
        let stale = crate::session::Session::load(&initial.id).expect("stale load");
        let mut durable = crate::session::Session::load(&initial.id).expect("durable load");
        durable.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "durable concurrent message".to_string(),
                cache_control: None,
            }],
        );
        durable.save().expect("concurrent save");

        let mut agent = agent_for_session(stale).await;
        persist_terminal_disposition(&mut agent, &initial.id, DisconnectDisposition::Closed).await;

        let terminal = crate::session::Session::load(&initial.id).expect("terminal load");
        assert_eq!(terminal.status, crate::session::SessionStatus::Closed);
        assert_eq!(
            terminal
                .messages
                .iter()
                .filter(|message| message.content.iter().any(|block| matches!(
                    block,
                    ContentBlock::Text { text, .. } if text == "durable concurrent message"
                )))
                .count(),
            1,
            "reload/retry must retain the concurrent generation exactly once"
        );

        if let Some(value) = previous_home {
            crate::env::set_var("JCODE_HOME", value);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    #[tokio::test]
    async fn terminal_retry_rejects_cross_account_message_merge() {
        let _lock = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("home");
        let previous_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", home.path());

        let mut initial = crate::session::Session::create_with_id(
            crate::id::new_id("terminal-retry-cross-account"),
            None,
            None,
        );
        initial.exact_runtime_identity = Some(identity("account-a"));
        initial.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "account-a-only".to_string(),
                cache_control: None,
            }],
        );
        initial.save().expect("initial save");
        let stale = crate::session::Session::load(&initial.id).expect("stale load");
        let mut durable = crate::session::Session::load(&initial.id).expect("durable load");
        durable.exact_runtime_identity = Some(identity("account-b"));
        durable.replace_messages(Vec::new());
        durable.add_message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "account-b-only".to_string(),
                cache_control: None,
            }],
        );
        durable.save().expect("account transition save");

        let mut agent = agent_for_session(stale).await;
        persist_terminal_disposition(&mut agent, &initial.id, DisconnectDisposition::Closed).await;

        let terminal = crate::session::Session::load(&initial.id).expect("terminal load");
        assert_eq!(terminal.status, crate::session::SessionStatus::Closed);
        assert_eq!(terminal.exact_runtime_identity, Some(identity("account-b")));
        let text = terminal
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(text.contains(&"account-b-only"));
        assert!(!text.contains(&"account-a-only"));

        if let Some(value) = previous_home {
            crate::env::set_var("JCODE_HOME", value);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }

    #[test]
    fn idle_disconnect_is_closed() {
        assert_eq!(disconnect_disposition(false), DisconnectDisposition::Closed);
    }

    #[test]
    fn running_disconnect_without_reload_is_crash() {
        let _guard = crate::storage::lock_test_env();
        crate::server::clear_reload_marker();
        assert_eq!(disconnect_disposition(true), DisconnectDisposition::Crashed);
    }

    #[test]
    fn running_disconnect_during_reload_is_expected() {
        let _guard = crate::storage::lock_test_env();
        let runtime = tempfile::TempDir::new().expect("create runtime dir");
        crate::env::set_var("JCODE_RUNTIME_DIR", runtime.path());
        crate::server::clear_reload_marker();
        crate::server::write_reload_state(
            "test-request",
            "test-hash",
            crate::server::ReloadPhase::Starting,
            None,
        );
        assert_eq!(
            disconnect_disposition(true),
            DisconnectDisposition::Reloading
        );
        crate::server::clear_reload_marker();
        crate::env::remove_var("JCODE_RUNTIME_DIR");
    }

    #[test]
    fn running_disconnect_during_recent_socket_ready_reload_is_expected() {
        let _guard = crate::storage::lock_test_env();
        let runtime = tempfile::TempDir::new().expect("create runtime dir");
        crate::env::set_var("JCODE_RUNTIME_DIR", runtime.path());
        crate::server::clear_reload_marker();
        crate::server::write_reload_state(
            "test-request",
            "test-hash",
            crate::server::ReloadPhase::SocketReady,
            None,
        );
        assert_eq!(
            disconnect_disposition(true),
            DisconnectDisposition::Reloading
        );
        crate::server::clear_reload_marker();
        crate::env::remove_var("JCODE_RUNTIME_DIR");
    }
}
