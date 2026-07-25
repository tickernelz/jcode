fn swarm_stop_allowed_by_owner(
    req_session_id: &str,
    target_member: &SwarmMember,
    force: bool,
) -> bool {
    force || target_member.report_back_to_session_id.as_deref() == Some(req_session_id)
}

async fn resolve_stop_target_session(
    swarm_id: &str,
    target: &str,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> std::result::Result<String, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("target_session is required.".to_string());
    }

    let members = swarm_members.read().await;
    if members
        .get(target)
        .is_some_and(|member| member.swarm_id.as_deref() == Some(swarm_id))
    {
        return Ok(target.to_string());
    }

    let mut matches = members
        .iter()
        .filter(|(_, member)| member.swarm_id.as_deref() == Some(swarm_id))
        .filter(|(session_id, member)| {
            member.friendly_name.as_deref() == Some(target)
                || session_id.starts_with(target)
                || session_id.ends_with(target)
        })
        .map(|(session_id, member)| {
            (
                session_id.clone(),
                member
                    .friendly_name
                    .as_deref()
                    .unwrap_or(session_id)
                    .to_string(),
            )
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| a.0.cmp(&b.0));

    match matches.len() {
        0 => Err(format!(
            "Unknown swarm session '{target}'. Use an exact session ID, unique friendly name, or unique session ID prefix/suffix."
        )),
        1 => Ok(matches.remove(0).0),
        _ => Err(format!(
            "Ambiguous swarm session '{target}' matched: {}. Use an exact session ID.",
            matches
                .iter()
                .map(|(session_id, friendly)| format!("{friendly} [{session_id}]"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn swarm_member_status_is_stale_for_coordination(status: &str) -> bool {
    matches!(
        status,
        "crashed" | "failed" | "stopped" | "closed" | "disconnected"
    )
}

#[allow(clippy::too_many_arguments)]
async fn ensure_spawn_coordinator_swarm(
    id: u64,
    req_session_id: &str,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
    swarm_coordinators: &Arc<RwLock<HashMap<String, String>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    configured_live_agent_limit: usize,
) -> Option<String> {
    let (
        swarm_id,
        from_name,
        is_root,
        root_session_id,
        coordinator_id,
        coordinator_is_stale,
        live_member_count,
        live_spawned_agent_count,
    ) = {
        let members = swarm_members.read().await;
        let swarm_id = members
            .get(req_session_id)
            .and_then(|member| member.swarm_id.clone());
        let from_name = members
            .get(req_session_id)
            .and_then(|member| member.friendly_name.clone());
        // A session is a "root" when it has no spawner/owner above it.
        let is_root = members
            .get(req_session_id)
            .and_then(|member| member.report_back_to_session_id.clone())
            .is_none();
        let root_session_id = super::swarm::swarm_ancestors(&members, req_session_id)
            .last()
            .cloned()
            .unwrap_or_else(|| req_session_id.to_string());
        // Count both all live members for the absolute hard cap and live spawned
        // agents for the configurable RAM-safety cap. User-created roots do not
        // consume worker slots; every recursively spawned descendant does.
        let (live_member_count, live_spawned_agent_count) = swarm_id
            .as_ref()
            .map(|swarm_id| {
                members
                    .values()
                    .filter(|member| member.swarm_id.as_deref() == Some(swarm_id.as_str()))
                    .filter(|member| super::member_consumes_swarm_capacity(member))
                    .fold((0usize, 0usize), |(members, spawned), member| {
                        (
                            members + 1,
                            spawned + usize::from(member.report_back_to_session_id.is_some()),
                        )
                    })
            })
            .unwrap_or_default();
        let coordinator_id = if let Some(ref swarm_id) = swarm_id {
            let coordinators = swarm_coordinators.read().await;
            coordinators.get(swarm_id).cloned()
        } else {
            None
        };
        let coordinator_is_stale = coordinator_id.as_ref().is_some_and(|coordinator| {
            !members.get(coordinator).is_some_and(|member| {
                // A coordinator is stale for slot-reclaim purposes when it left
                // the swarm, reached a terminal status, or can no longer be
                // reached at all (every event channel closed). The last case
                // catches a wedged coordinator whose client died without a
                // clean status transition; without it the slot stays blocked
                // until the status sweep happens to notice.
                let unreachable = member.event_tx.is_closed()
                    && member.event_txs.values().all(|tx| tx.is_closed());
                member.swarm_id.as_deref() == swarm_id.as_deref()
                    && !swarm_member_status_is_stale_for_coordination(&member.status)
                    && !unreachable
            })
        });
        (
            swarm_id,
            from_name,
            is_root,
            root_session_id,
            coordinator_id,
            coordinator_is_stale,
            live_member_count,
            live_spawned_agent_count,
        )
    };

    let Some(swarm_id) = swarm_id else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "Not in a swarm.".to_string(),
            retry_after_secs: None,
        });
        return None;
    };

    // Light and ad hoc swarms are deliberately one-level fan-out: only the root
    // session may create workers. Recursive spawning is an explicit deep-swarm
    // capability, keyed from the root's effort rather than the requesting
    // child's effort so a worker cannot opt itself into unbounded growth.
    if !is_root {
        let root_is_deep = crate::session_effort::session_effort(&root_session_id)
            .as_deref()
            .is_some_and(crate::prompt::is_deep_swarm_effort);
        if !root_is_deep {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!(
                    "Recursive swarm spawning is disabled for light and ad hoc swarms. Only the root session ({root_session_id}) may spawn agents unless that root is running in swarm-deep mode."
                ),
                retry_after_secs: None,
            });
            return None;
        }
    }

    // Keep an absolute hard ceiling even when the configurable limit is disabled.
    if live_member_count >= super::MAX_SWARM_MEMBERS {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: format!(
                "Swarm member limit reached (hard max {}). This swarm already has {live_member_count} live members; it cannot spawn more. Let existing agents finish and free up capacity, or narrow the task decomposition before spawning further.",
                super::MAX_SWARM_MEMBERS
            ),
            retry_after_secs: None,
        });
        return None;
    }

    // `swarm_max_concurrent_agents` is the machine-safety budget shared by
    // run_plan and deep recursive spawning. Previously only run_plan obeyed it,
    // so nested agents could grow to the 1000-member hard cap and exhaust RAM.
    let live_agent_limit = (configured_live_agent_limit > 0)
        .then(|| configured_live_agent_limit.min(super::MAX_SWARM_MEMBERS));
    if live_agent_limit.is_some_and(|limit| live_spawned_agent_count >= limit) {
        let limit = live_agent_limit.unwrap_or(super::MAX_SWARM_MEMBERS);
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: format!(
                "Swarm live-agent limit reached (max {limit}, configured by agents.swarm_max_concurrent_agents). This swarm already has {live_spawned_agent_count} active spawned agents. Let existing agents finish or stop them before spawning more."
            ),
            retry_after_secs: None,
        });
        return None;
    }

    // Coordinator-slot election is now only about the swarm-level coordinator used
    // for shared plan operations (propose/approve/assign). Only a root session
    // (depth 0, no spawner) claims it, and only when the slot is empty or stale.
    // Authorized deep-swarm descendants coordinate their own subtree via
    // report-back ownership and never disturb the swarm-level coordinator slot.
    if is_root && coordinator_id.as_deref() != Some(req_session_id) {
        let should_claim = coordinator_id.is_none() || coordinator_is_stale;
        if should_claim {
            let promoted = {
                let mut coordinators = swarm_coordinators.write().await;
                match coordinators.get(&swarm_id) {
                    Some(existing) if existing == req_session_id => false,
                    Some(_) if !coordinator_is_stale => false,
                    _ => {
                        coordinators.insert(swarm_id.clone(), req_session_id.to_string());
                        true
                    }
                }
            };

            if promoted {
                {
                    let mut members = swarm_members.write().await;
                    if let Some(member) = members.get_mut(req_session_id) {
                        member.role = "coordinator".to_string();
                    }
                }
                let swarm_state = SwarmState {
                    members: Arc::clone(swarm_members),
                    swarms_by_id: Arc::clone(swarms_by_id),
                    plans: Arc::clone(swarm_plans),
                    coordinators: Arc::clone(swarm_coordinators),
                };
                persist_swarm_state_for(&swarm_id, &swarm_state).await;
                broadcast_swarm_status(&swarm_id, swarm_members, swarms_by_id).await;
                let _ = client_event_tx.send(ServerEvent::Notification {
                    from_session: req_session_id.to_string(),
                    from_name,
                    notification_type: NotificationType::Message {
                        scope: Some("swarm".to_string()),
                        channel: None,
                        tldr: None,
                    },
                    message: "You are the coordinator for this swarm.".to_string(),
                });
            }
        }
    }

    Some(swarm_id)
}

#[cfg(test)]
#[path = "comm_session_tests.rs"]
mod comm_session_tests;
