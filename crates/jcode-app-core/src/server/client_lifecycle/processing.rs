use super::*;

pub(super) async fn start_processing_message(
    message: ProcessingMessage,
    client_session_id: &str,
    state: &mut ProcessingState<'_>,
    agent: &Arc<Mutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    processing_done_tx: &mpsc::UnboundedSender<(u64, Result<()>, Option<String>)>,
    swarm: &SwarmStatusRefs<'_>,
) {
    let ProcessingMessage {
        id,
        content,
        images,
        system_reminder,
    } = message;
    if server_reload_starting() {
        crate::logging::info(&format!(
            "Rejecting new message for session {} because server reload is starting",
            client_session_id
        ));
        let _ = client_event_tx.send(ServerEvent::Reloading { new_socket: None });
        return;
    }

    if *state.client_is_processing {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "Already processing a message".to_string(),
            retry_after_secs: None,
        });
        return;
    }

    *state.client_is_processing = true;
    *state.message_id = Some(id);
    *state.session_id = Some(client_session_id.to_string());

    if let Some(reminder) = system_reminder.as_deref()
        && let Err(error) = super::super::reload_recovery::mark_delivered_if_matching_continuation(
            client_session_id,
            reminder,
            "client_message_accepted",
        )
    {
        crate::logging::warn(&format!(
            "Failed to mark reload recovery intent delivered for accepted message session={} id={}: {}",
            client_session_id, id, error
        ));
    }

    update_member_status(
        client_session_id,
        "running",
        Some(truncate_detail(&content, 120)),
        swarm.members,
        swarm.swarms_by_id,
        Some(swarm.event_history),
        Some(swarm.event_counter),
        Some(swarm.event_tx),
    )
    .await;

    let start_message_index = {
        let agent_guard = agent.lock().await;
        agent_guard.message_count()
    };
    let agent = Arc::clone(agent);
    let report_agent = Arc::clone(&agent);
    let tx = super::super::state::session_event_fanout_sender_with_fallback(
        client_session_id.to_string(),
        Arc::clone(swarm.members),
        client_event_tx.clone(),
    );
    let done_tx = processing_done_tx.clone();
    crate::logging::info(&format!("Processing message id={} spawning task", id));
    *state.task = Some(tokio::spawn(async move {
        let event_tx = tx.clone();
        let result = match std::panic::AssertUnwindSafe(process_message_streaming_mpsc(
            agent,
            &content,
            images,
            system_reminder,
            event_tx,
        ))
        .catch_unwind()
        .await
        {
            Ok(result) => result,
            Err(panic_payload) => {
                let msg = if let Some(text) = panic_payload.downcast_ref::<&str>() {
                    text.to_string()
                } else if let Some(text) = panic_payload.downcast_ref::<String>() {
                    text.clone()
                } else {
                    "unknown panic".to_string()
                };
                crate::logging::error(&format!(
                    "Processing task PANICKED for message id={}: {}",
                    id, msg
                ));
                Err(anyhow::anyhow!("Processing task panicked: {}", msg))
            }
        };
        match &result {
            Ok(()) => crate::logging::info(&format!(
                "Processing task completed OK for message id={}",
                id
            )),
            Err(error) => crate::logging::warn(&format!(
                "Processing task completed with error for message id={}: {}",
                id, error
            )),
        }
        let completion_report = if result.is_ok() {
            let agent = report_agent.lock().await;
            agent.latest_assistant_text_after(start_message_index)
        } else {
            None
        };
        // Keep the terminal event on the same ordered fanout channel as the
        // stream. Sending it later from the owning client's event loop could
        // race ahead of the final MessageEnd for newly attached clients.
        let terminal_event = match &result {
            Ok(()) => ServerEvent::Done { id },
            Err(error) => ServerEvent::Error {
                id,
                message: crate::util::format_error_chain(error),
                retry_after_secs: error
                    .downcast_ref::<StreamError>()
                    .and_then(|stream_error| stream_error.retry_after_secs),
            },
        };
        let _ = tx.send(terminal_event);
        let _ = done_tx.send((id, result, completion_report));
    }));
}

pub(super) async fn cancel_processing_message(
    state: &mut ProcessingState<'_>,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
    swarm: &SwarmStatusRefs<'_>,
    request_id: Option<u64>,
    request_decoded_at: Option<Instant>,
) {
    let cancel_start = Instant::now();
    let session_label = state
        .session_id
        .as_deref()
        .unwrap_or(session_control.session_id.as_str())
        .to_string();
    crate::logging::info(&format!(
        "SERVER_INTERRUPT_CANCEL_RECEIVED request_id={:?} session={} control_session={} client_processing={} message_id={:?} has_task={} decoded_age_ms={:?}",
        request_id,
        session_label,
        session_control.session_id,
        *state.client_is_processing,
        *state.message_id,
        state.task.is_some(),
        request_decoded_at.map(|instant| instant.elapsed().as_millis())
    ));
    if let Some(mut handle) = state.task.take() {
        if handle.is_finished() {
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_IGNORED_FINISHED request_id={:?} session={} message_id={:?} total_ms={}",
                request_id,
                session_label,
                *state.message_id,
                cancel_start.elapsed().as_millis()
            ));
            *state.task = Some(handle);
            return;
        }
        let cancel_epoch = session_control.request_cancel();
        crate::logging::info(&format!(
            "SERVER_INTERRUPT_CANCEL_SIGNALLED request_id={:?} session={} message_id={:?} wait_ms=500",
            request_id, session_label, *state.message_id
        ));
        match tokio::time::timeout(std::time::Duration::from_millis(500), &mut handle).await {
            Ok(_) => {
                crate::logging::info(&format!(
                    "SERVER_INTERRUPT_CANCEL_COOPERATIVE_DONE request_id={:?} session={} message_id={:?} elapsed_ms={}",
                    request_id,
                    session_label,
                    *state.message_id,
                    cancel_start.elapsed().as_millis()
                ));
            }
            Err(_) => {
                crate::logging::warn(&format!(
                    "SERVER_INTERRUPT_CANCEL_COOPERATIVE_TIMEOUT request_id={:?} session={} message_id={:?} elapsed_ms={} action=abort_task",
                    request_id,
                    session_label,
                    *state.message_id,
                    cancel_start.elapsed().as_millis()
                ));
                handle.abort();
                match tokio::time::timeout(std::time::Duration::from_millis(2000), handle).await {
                    Ok(_) => crate::logging::info(&format!(
                        "SERVER_INTERRUPT_CANCEL_ABORT_RELEASED request_id={:?} session={} elapsed_ms={}",
                        request_id,
                        session_label,
                        cancel_start.elapsed().as_millis()
                    )),
                    Err(_) => crate::logging::warn(&format!(
                        "SERVER_INTERRUPT_CANCEL_ABORT_RELEASE_TIMEOUT request_id={:?} session={} elapsed_ms={} wait_ms=2000",
                        request_id,
                        session_label,
                        cancel_start.elapsed().as_millis()
                    )),
                }
            }
        }
        // Only clear the cancel we fired: a newer cancel (repeated Esc, jade
        // relay, another connection) must not be erased before its target
        // observes it (issue #428).
        session_control.reset_cancel_if_epoch(cancel_epoch);
        *state.task = None;
        *state.client_is_processing = false;
        if let Some(session_id) = state.session_id.take() {
            update_member_status(
                &session_id,
                "stopped",
                Some("cancelled".to_string()),
                swarm.members,
                swarm.swarms_by_id,
                Some(swarm.event_history),
                Some(swarm.event_counter),
                Some(swarm.event_tx),
            )
            .await;
        }
        if let Some(message_id) = state.message_id.take() {
            let _ = client_event_tx.send(ServerEvent::Interrupted);
            let _ = client_event_tx.send(ServerEvent::Done { id: message_id });
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_EVENTS_EMITTED request_id={:?} session={} interrupted=true done_id={} total_ms={}",
                request_id,
                session_label,
                message_id,
                cancel_start.elapsed().as_millis()
            ));
        }
    } else {
        crate::logging::warn(&format!(
            "SERVER_INTERRUPT_CANCEL_NO_LOCAL_TASK request_id={:?} session={} control_session={} client_processing={} message_id={:?}; signalling session cancel handle anyway",
            request_id,
            session_label,
            session_control.session_id,
            *state.client_is_processing,
            *state.message_id
        ));
        let cancel_epoch = session_control.request_cancel();
        let reset_control = session_control.clone();
        tokio::spawn(async move {
            // The running turn is not owned by this connection (post-reload
            // recovery, server-initiated turn, or attach), so we cannot await
            // it. Clear the flag later so the *next* turn is not aborted by a
            // stale cancel, but only if no newer cancel fired in the meantime:
            // an unconditional reset here used to erase rapid repeated Esc
            // cancels before the busy turn observed them (issue #428).
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            reset_control.reset_cancel_if_epoch(cancel_epoch);
        });
        *state.client_is_processing = false;
        let status_session_id = state
            .session_id
            .take()
            .unwrap_or_else(|| session_control.session_id.clone());
        update_member_status(
            &status_session_id,
            "stopped",
            Some("cancelled".to_string()),
            swarm.members,
            swarm.swarms_by_id,
            Some(swarm.event_history),
            Some(swarm.event_counter),
            Some(swarm.event_tx),
        )
        .await;
        let _ = client_event_tx.send(ServerEvent::Interrupted);
        if let Some(message_id) = state.message_id.take() {
            let _ = client_event_tx.send(ServerEvent::Done { id: message_id });
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_EVENTS_EMITTED request_id={:?} session={} interrupted=true done_id={} total_ms={}",
                request_id,
                session_label,
                message_id,
                cancel_start.elapsed().as_millis()
            ));
        } else {
            crate::logging::info(&format!(
                "SERVER_INTERRUPT_CANCEL_EVENTS_EMITTED request_id={:?} session={} interrupted=true done_id=None total_ms={}",
                request_id,
                session_label,
                cancel_start.elapsed().as_millis()
            ));
        }
    }
}

pub(super) fn try_available_models_snapshot(agent: &Arc<Mutex<Agent>>) -> Option<String> {
    let event = try_available_models_updated_event(agent)?;
    Some(crate::protocol::encode_event(&event))
}

/// Build a names-only copy of an `AvailableModelsUpdated` event by dropping the
/// per-model route expansion. Used when the fully-routed frame exceeds the live
/// update size cap so clients still receive fresh model names.
pub(super) fn names_only_available_models_event(event: &ServerEvent) -> Option<ServerEvent> {
    let ServerEvent::AvailableModelsUpdated {
        provider_name,
        provider_model,
        available_models,
        ..
    } = event
    else {
        return None;
    };
    Some(ServerEvent::AvailableModelsUpdated {
        provider_name: provider_name.clone(),
        provider_model: provider_model.clone(),
        available_models: available_models.clone(),
        available_model_routes: Vec::new(),
    })
}

pub(super) fn queue_soft_interrupt(
    id: u64,
    content: String,
    urgent: bool,
    source: SoftInterruptSource,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let content_bytes = content.len();
    let content_chars = content.chars().count();
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_QUEUE_REQUEST id={} session={} source={:?} urgent={} content_bytes={} content_chars={}",
        id, session_control.session_id, source, urgent, content_bytes, content_chars
    ));
    let queued = session_control.queue_soft_interrupt(content, urgent, source);
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_QUEUE_RESULT id={} session={} queued={} ack_queued={}",
        id, session_control.session_id, queued, ack_queued
    ));
}

pub(super) fn clear_soft_interrupts(
    id: u64,
    session_id: &str,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_CLEAR_REQUEST id={} session={} control_session={}",
        id, session_id, session_control.session_id
    ));
    session_control.clear_soft_interrupts();
    let persisted_clear = match crate::soft_interrupt_store::clear(session_id) {
        Ok(()) => true,
        Err(err) => {
            crate::logging::warn(&format!(
                "SERVER_SOFT_INTERRUPT_CLEAR_PERSISTED_FAILED id={} session={} error={}",
                id, session_id, err
            ));
            false
        }
    };
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    crate::logging::info(&format!(
        "SERVER_SOFT_INTERRUPT_CLEAR_RESULT id={} session={} persisted_clear={} ack_queued={}",
        id, session_id, persisted_clear, ack_queued
    ));
}

pub(super) fn move_tool_to_background(
    id: u64,
    session_control: &SessionControlHandle,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TOOL_REQUEST id={} session={}",
        id, session_control.session_id
    ));
    let signalled = session_control.request_background_current_tool();
    let ack_queued = client_event_tx.send(ServerEvent::Ack { id }).is_ok();
    crate::logging::info(&format!(
        "SERVER_BACKGROUND_TOOL_RESULT id={} session={} signalled={} ack_queued={}",
        id, session_control.session_id, signalled, ack_queued
    ));
}

/// Process a message and stream events (mpsc channel - per-client)
pub(in crate::server) async fn process_message_streaming_mpsc(
    agent: Arc<Mutex<Agent>>,
    content: &str,
    images: Vec<(String, String)>,
    system_reminder: Option<String>,
    event_tx: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
) -> Result<()> {
    let mut agent = agent.lock().await;
    let session_id = agent.session_id().to_string();
    let result = agent
        .run_once_streaming_mpsc(content, images, system_reminder, event_tx)
        .await;
    if result.is_ok() {
        crate::runtime_memory_log::emit_event(
            crate::runtime_memory_log::RuntimeMemoryLogEvent::new(
                "turn_completed",
                "message_turn_finished",
            )
            .with_session_id(session_id)
            .force_attribution(),
        );
        crate::process_memory::release_retained_heap_debounced(
            "server_turn_completed",
            std::time::Duration::from_secs(30),
        );
    }
    result
}
