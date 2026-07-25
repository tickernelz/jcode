#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_comm_stop(
    id: u64,
    req_session_id: String,
    target_session: String,
    force: bool,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    sessions: &SessionAgents,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    channel_subscriptions: &ChannelSubscriptions,
    channel_subscriptions_by_session: &ChannelSubscriptions,
    event_history: &Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    event_counter: &Arc<std::sync::atomic::AtomicU64>,
    swarm_event_tx: &broadcast::Sender<SwarmEvent>,
    soft_interrupt_queues: &SessionInterruptQueues,
    swarm_mutation_runtime: &SwarmMutationRuntime,
) {
    // Stopping is authorized per-target by ownership (the requester is the
    // target's spawner or a transitive ancestor) rather than by the swarm-level
    // coordinator slot, so that any parent can stop agents in its own subtree.
    // We only require the requester to be a member of a swarm here; the concrete
    // permission check happens below via `stop_allowed`.
    let swarm_id = {
        let members = swarm_members.read().await;
        members
            .get(&req_session_id)
            .and_then(|member| member.swarm_id.clone())
    };
    let Some(swarm_id) = swarm_id else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "Not in a swarm.".to_string(),
            retry_after_secs: None,
        });
        return;
    };

    let target_session =
        match resolve_stop_target_session(&swarm_id, &target_session, swarm_members).await {
            Ok(target_session) => target_session,
            Err(message) => {
                let _ = client_event_tx.send(ServerEvent::Error {
                    id,
                    message,
                    retry_after_secs: None,
                });
                return;
            }
        };

    let stop_allowed = {
        let members = swarm_members.read().await;
        members
            .get(&target_session)
            .map(|member| {
                swarm_stop_allowed_by_owner(&req_session_id, member, force)
                    || (!force
                        && super::swarm_is_self_or_ancestor(
                            &members,
                            &req_session_id,
                            &target_session,
                        ))
            })
            .unwrap_or(false)
    };
    if !stop_allowed {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: format!(
                "Refusing to stop session '{target_session}' because it was not spawned by this coordinator. Pass force=true to stop a non-owned/user-created swarm session explicitly."
            ),
            retry_after_secs: None,
        });
        return;
    }

    let mutation_key = request_key(&req_session_id, "stop", &[swarm_id, target_session.clone()]);
    let Some(mutation_state) = begin_or_replay(
        swarm_mutation_runtime,
        &mutation_key,
        "stop",
        &req_session_id,
        id,
        client_event_tx,
    )
    .await
    else {
        return;
    };

    let removed_agent = super::remove_session_entry(sessions, &target_session).await;
    let removed_live_agent = removed_agent.is_some();
    if let Some(agent_arc) = removed_agent {
        let close_result = match agent_arc.try_lock() {
            Ok(mut agent) => agent.try_mark_closed().map_err(|error| error.to_string()),
            Err(_) => Err("session is busy; retry stop after the active turn exits".to_string()),
        };
        if let Err(message) = close_result {
            sessions
                .write()
                .await
                .insert(target_session.clone(), Arc::clone(&agent_arc));
            crate::storage::register_active_pid(&target_session, std::process::id());
            finish_request(
                swarm_mutation_runtime,
                &mutation_state,
                PersistedSwarmMutationResponse::Error {
                    message: format!(
                        "Failed to durably stop session '{target_session}': {message}"
                    ),
                    retry_after_secs: Some(1),
                },
            )
            .await;
            return;
        }
        if fanout_session_event(
            swarm_members,
            &target_session,
            ServerEvent::SessionCloseRequested {
                reason: format!("Stopped by coordinator {req_session_id}"),
            },
        )
        .await
            == 0
        {
            crate::logging::debug(&format!(
                "No live client received close request for {target_session}"
            ));
        }
        remove_session_interrupt_queue(soft_interrupt_queues, &target_session).await;
        remove_background_tool_signal(&target_session);
        if let Ok(agent) = agent_arc.try_lock() {
            let memory_enabled = agent.memory_enabled();
            let transcript = if memory_enabled {
                Some(agent.build_transcript_for_extraction())
            } else {
                None
            };
            let sid = target_session.clone();
            let working_dir = agent.working_dir().map(|dir| dir.to_string());
            drop(agent);
            if let Some(transcript) = transcript {
                crate::memory_agent::trigger_final_extraction_with_dir(
                    transcript,
                    sid,
                    working_dir,
                );
            }
        }
    }

    if !removed_live_agent {
        let durable_close = (|| -> anyhow::Result<()> {
            let mut session = crate::session::Session::load(&target_session)?;
            session.status = crate::session::SessionStatus::Closed;
            session.last_pid = None;
            session.last_active_at = Some(chrono::Utc::now());
            session.save()?;
            crate::storage::unregister_active_pid(&target_session);
            Ok(())
        })();
        if let Err(error) = durable_close {
            finish_request(
                swarm_mutation_runtime,
                &mutation_state,
                PersistedSwarmMutationResponse::Error {
                    message: format!(
                        "Failed to durably stop nonresident session '{target_session}': {error}"
                    ),
                    retry_after_secs: Some(1),
                },
            )
            .await;
            return;
        }
        if fanout_session_event(
            swarm_members,
            &target_session,
            ServerEvent::SessionCloseRequested {
                reason: format!("Stopped by coordinator {req_session_id}"),
            },
        )
        .await
            == 0
        {
            crate::logging::debug(&format!(
                "No live client received close request for nonresident {target_session}"
            ));
        }
    }

    let (removed_swarm_id, removed_name) = {
        let mut members = swarm_members.write().await;
        if let Some(member) = members.remove(&target_session) {
            (member.swarm_id, member.friendly_name)
        } else {
            (None, None)
        }
    };
    if let Some(ref swarm_id) = removed_swarm_id {
        record_swarm_event(
            event_history,
            event_counter,
            swarm_event_tx,
            target_session.clone(),
            removed_name.clone(),
            Some(swarm_id.clone()),
            SwarmEventType::MemberChange {
                action: "left".to_string(),
            },
        )
        .await;
        remove_session_from_swarm(
            &target_session,
            swarm_id,
            swarm_members,
            swarms_by_id,
            swarm_coordinators,
            swarm_plans,
        )
        .await;
    }
    remove_session_channel_subscriptions(
        &target_session,
        channel_subscriptions,
        channel_subscriptions_by_session,
    )
    .await;

    let response = if removed_live_agent || removed_swarm_id.is_some() {
        PersistedSwarmMutationResponse::Done
    } else {
        PersistedSwarmMutationResponse::Error {
            message: format!("Unknown session '{target_session}'"),
            retry_after_secs: None,
        }
    };
    finish_request(swarm_mutation_runtime, &mutation_state, response).await;
}

include!("comm_session_ownership.rs");
