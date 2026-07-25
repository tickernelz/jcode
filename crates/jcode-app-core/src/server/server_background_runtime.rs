use super::*;

impl Server {
    pub(super) fn spawn_background_tasks(
        &self,
        server_start_time: Instant,
        temporary_server_policy: Option<lifecycle::TemporaryServerPolicy>,
    ) {
        // Preload the embedding model in background so warm startups get fast
        // memory recall. On a cold install, skip eager preload because the
        // first-time model download can make the first spawned client look hung
        // while the daemon finishes bootstrapping.
        if crate::embedding::is_model_available() {
            tokio::task::spawn_blocking(|| {
                let start = std::time::Instant::now();
                match crate::embedding::get_embedder() {
                    Ok(_) => {
                        crate::logging::info(&format!(
                            "Embedding model preloaded in {}ms",
                            start.elapsed().as_millis()
                        ));
                    }
                    Err(e) => {
                        crate::logging::info(&format!(
                            "Embedding model preload failed (non-fatal): {}",
                            e
                        ));
                    }
                }
            });
        } else {
            crate::logging::info(
                "Embedding model not installed yet; skipping eager preload during server startup",
            );
        }

        // Warm the lightweight session-search index after daemon startup. This
        // keeps the first agent `session_search` call from paying the cold
        // indexing cost while leaving exhaustive searches available on demand.
        crate::tool::spawn_recent_index_warmup();

        // Reconcile background-task status files orphaned by a previous
        // process image (crash or exec-based reload). Non-detached tasks die
        // with their owning process but their status files still say Running,
        // which leaves phantom entries in `bg list` and blocks `bg wait`
        // until timeout. Detached tasks are untouched (they survive reloads
        // and reconcile via their real pid).
        tokio::spawn(async move {
            let reconciled = crate::background::global().reconcile_orphaned_tasks().await;
            if reconciled > 0 {
                crate::logging::info(&format!(
                    "Marked {} orphaned background task(s) from a previous server process as failed",
                    reconciled
                ));
            }
        });

        // Spawn reload monitor (event-driven via in-process channel).
        // In the unified server design, self-dev sessions share the main server,
        // so the shared server must always listen for reload signals.
        let signal_sessions = Arc::clone(&self.sessions);
        let signal_swarm_members = Arc::clone(&self.swarm_state.members);
        let signal_shutdown_signals = Arc::clone(&self.shutdown_signals);
        let signal_swarm_event_tx = self.swarm_event_tx.clone();
        tokio::spawn(async move {
            await_reload_signal(
                signal_sessions,
                signal_swarm_members,
                signal_shutdown_signals,
                signal_swarm_event_tx,
            )
            .await;
        });

        // Log when we receive SIGTERM for debugging
        #[cfg(unix)]
        {
            let sigterm_server_name = self.identity.name.clone();
            tokio::spawn(async move {
                use tokio::signal::unix::{SignalKind, signal};
                if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
                    sigterm.recv().await;
                    crate::logging::info("Server received SIGTERM, shutting down gracefully");
                    let _ = crate::registry::unregister_server(&sigterm_server_name).await;
                    std::process::exit(0);
                }
            });
        }

        // Spawn the bus monitor for swarm coordination
        let monitor_file_touch = self.file_touch.clone();
        let monitor_swarm_members = Arc::clone(&self.swarm_state.members);
        let monitor_swarms_by_id = Arc::clone(&self.swarm_state.swarms_by_id);
        let monitor_swarm_plans = Arc::clone(&self.swarm_state.plans);
        let monitor_swarm_coordinators = Arc::clone(&self.swarm_state.coordinators);
        let monitor_shared_context = Arc::clone(&self.shared_context);
        let monitor_sessions = Arc::clone(&self.sessions);
        let monitor_soft_interrupt_queues = Arc::clone(&self.soft_interrupt_queues);
        let monitor_event_history = Arc::clone(&self.event_history);
        let monitor_event_counter = Arc::clone(&self.event_counter);
        let monitor_swarm_event_tx = self.swarm_event_tx.clone();
        tokio::spawn(async move {
            Self::monitor_bus(
                monitor_file_touch,
                monitor_swarm_members,
                monitor_swarms_by_id,
                monitor_swarm_plans,
                monitor_swarm_coordinators,
                monitor_shared_context,
                monitor_sessions,
                monitor_soft_interrupt_queues,
                monitor_event_history,
                monitor_event_counter,
                monitor_swarm_event_tx,
            )
            .await;
        });

        // Resume any background `swarm await_members` watchers that were active
        // before this (re)start. Their results are delivered via notify/wake, so
        // they can pick up transparently without the agent rerunning the wait.
        {
            let resume_swarm_members = Arc::clone(&self.swarm_state.members);
            let resume_swarms_by_id = Arc::clone(&self.swarm_state.swarms_by_id);
            let resume_swarm_event_tx = self.swarm_event_tx.clone();
            let resume_await_runtime = self.await_members_runtime.clone();
            tokio::spawn(async move {
                comm_await::resume_background_awaits(
                    &resume_swarm_members,
                    &resume_swarms_by_id,
                    &resume_swarm_event_tx,
                    &resume_await_runtime,
                )
                .await;
            });
        }

        let stale_swarm_members = Arc::clone(&self.swarm_state.members);
        let stale_swarms_by_id = Arc::clone(&self.swarm_state.swarms_by_id);
        let stale_swarm_plans = Arc::clone(&self.swarm_state.plans);
        let stale_swarm_coordinators = Arc::clone(&self.swarm_state.coordinators);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(crate::server::swarm::swarm_task_sweep_interval());
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                refresh_swarm_task_staleness(
                    &stale_swarm_members,
                    &stale_swarms_by_id,
                    &stale_swarm_plans,
                    &stale_swarm_coordinators,
                )
                .await;
            }
        });

        let gc_sessions = Arc::clone(&self.sessions);
        let gc_swarm_state = self.swarm_state.clone();
        let gc_channel_subscriptions = Arc::clone(&self.channel_subscriptions);
        let gc_channel_subscriptions_by_session =
            Arc::clone(&self.channel_subscriptions_by_session);
        let gc_soft_interrupt_queues = Arc::clone(&self.soft_interrupt_queues);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(swarm::swarm_terminal_member_gc_interval());
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                prune_expired_terminal_swarm_members(
                    &gc_sessions,
                    &gc_swarm_state,
                    &gc_channel_subscriptions,
                    &gc_channel_subscriptions_by_session,
                )
                .await;
                // Backstop for coordinator cleanup: close finished spawned
                // workers that have been idle past the reap window so they do
                // not accumulate one leaked client process each.
                reap_idle_spawned_workers(
                    &gc_sessions,
                    &gc_swarm_state,
                    &gc_channel_subscriptions,
                    &gc_channel_subscriptions_by_session,
                    &gc_soft_interrupt_queues,
                )
                .await;
            }
        });

        // Keep the machine awake while any session is actively streaming/processing.
        // This watches the same "running" member signal Waybar surfaces as
        // "N streaming" and toggles a best-effort OS power inhibitor accordingly.
        Self::spawn_power_inhibitor(Arc::clone(&self.swarm_state.members));

        // Initialize the memory agent early so it's ready for all sessions
        if crate::config::config().features.memory {
            tokio::spawn(async {
                let _ = crate::memory_agent::init().await;
            });
        }

        // Spawn the background ambient/schedule loop.
        if let Some(ref runner) = self.ambient_runner {
            let ambient_handle = runner.clone();
            let ambient_provider = Arc::clone(&self.provider);
            crate::logging::info("Starting ambient/schedule background loop");
            tokio::spawn(async move {
                ambient_handle.run_loop(ambient_provider).await;
            });
        }

        // Spawn the Jade cloud relay listener independently of ambient mode. The
        // worker is strictly opt-in and requires an explicit API base, token,
        // session id, and reply-enabled flag before it makes any outbound calls.
        jade_relay::spawn_if_configured(
            &crate::config::config().safety,
            Arc::clone(&self.sessions),
            Arc::clone(&self.soft_interrupt_queues),
            Arc::clone(&self.shutdown_signals),
            Arc::clone(&self.swarm_state.members),
        );

        // Spawn embedding idle monitor so the model can be unloaded when this
        // server has been quiet for a while.
        let embedding_idle_secs = embedding_idle_unload_secs();
        tokio::spawn(async move {
            let idle_for = std::time::Duration::from_secs(embedding_idle_secs);
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(EMBEDDING_IDLE_CHECK_SECS));
            loop {
                interval.tick().await;
                let unloaded = crate::embedding::maybe_unload_if_idle(idle_for);
                if unloaded {
                    let stats = crate::embedding::stats();
                    crate::logging::info(&format!(
                        "Embedding idle monitor: model unloaded (loads={}, unloads={}, calls={}, avg_ms={})",
                        stats.load_count,
                        stats.unload_count,
                        stats.embed_calls,
                        stats
                            .avg_embed_ms
                            .map(|v| format!("{:.1}", v))
                            .unwrap_or_else(|| "n/a".to_string())
                    ));
                }
            }
        });

        // Spawn the retained-heap watchdog: glibc/jemalloc keep freed pages
        // inside arenas, and the event-driven trim hooks (turn completion,
        // history load) rarely fire on a server hosting mostly-idle sessions.
        // Periodically check the allocator's freed-but-retained byte count and
        // trim when it crosses the threshold, returning the pages to the OS.
        let retention_threshold = crate::process_memory::retention_trim_threshold_bytes();
        if retention_threshold != u64::MAX {
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                    HEAP_RETENTION_CHECK_SECS,
                ));
                loop {
                    interval.tick().await;
                    crate::process_memory::release_retained_heap_if_excessive(
                        "server_retention_watchdog",
                        retention_threshold,
                        std::time::Duration::from_secs(60),
                    );
                }
            });
        }

        if crate::runtime_memory_log::server_logging_enabled() {
            let log_identity = self.identity.clone();
            let log_sessions = Arc::clone(&self.sessions);
            let log_client_count = Arc::clone(&self.client_count);
            let (memory_event_tx, mut memory_event_rx) = mpsc::unbounded_channel();
            crate::runtime_memory_log::install_event_sink(memory_event_tx);
            tokio::spawn(async move {
                match crate::runtime_memory_log::prune_old_server_logs() {
                    Ok(removed) if removed > 0 => {
                        crate::logging::info(&format!(
                            "Runtime memory logging pruned {} old log files",
                            removed
                        ));
                    }
                    Ok(_) => {}
                    Err(err) => {
                        crate::logging::info(&format!(
                            "Runtime memory logging could not prune old logs: {}",
                            err
                        ));
                    }
                }

                let log_config = crate::runtime_memory_log::server_logging_config();
                match crate::runtime_memory_log::current_server_log_path() {
                    Ok(path) => crate::logging::info(&format!(
                        "Runtime memory logging enabled: process={}s attribution={}s -> {}",
                        log_config.process_interval.as_secs(),
                        log_config.attribution_interval.as_secs(),
                        path.display()
                    )),
                    Err(err) => crate::logging::info(&format!(
                        "Runtime memory logging enabled: process={}s attribution={}s (path unavailable: {})",
                        log_config.process_interval.as_secs(),
                        log_config.attribution_interval.as_secs(),
                        err
                    )),
                }

                let mut controller = RuntimeMemoryLogController::new(log_config);
                let startup_now = Instant::now();
                let mut startup_sample = capture_runtime_memory_attribution_sample(
                    &log_identity,
                    &log_sessions,
                    &log_client_count,
                    server_start_time,
                    "attribution:startup",
                    RuntimeMemoryLogTrigger {
                        category: "startup".to_string(),
                        reason: "server_start".to_string(),
                        session_id: None,
                        detail: None,
                    },
                    RuntimeMemoryLogSampling {
                        forced: true,
                        threshold_reasons: vec!["initial_attribution".to_string()],
                        pending_event_count: 0,
                        pending_categories: Vec::new(),
                    },
                )
                .await;
                controller.record_process_sample(startup_now);
                controller.finalize_attribution_sample(startup_now, &mut startup_sample);
                if let Err(err) = crate::runtime_memory_log::append_server_sample(&startup_sample) {
                    crate::logging::info(&format!(
                        "Runtime memory logging startup sample failed: {}",
                        err
                    ));
                }

                let mut process_interval =
                    tokio::time::interval(controller.config().process_interval);
                let mut attribution_interval =
                    tokio::time::interval(controller.config().attribution_interval);
                process_interval.tick().await;
                attribution_interval.tick().await;
                loop {
                    tokio::select! {
                        _ = process_interval.tick() => {
                            let now = Instant::now();
                            let process_sample = capture_runtime_memory_process_sample(
                                &log_identity,
                                &log_client_count,
                                server_start_time,
                                "process:heartbeat",
                                RuntimeMemoryLogTrigger {
                                    category: "process_heartbeat".to_string(),
                                    reason: "periodic".to_string(),
                                    session_id: None,
                                    detail: None,
                                },
                                controller.build_sampling_for_process(None),
                            )
                            .await;
                            controller.record_process_sample(now);
                            if let Err(err) = crate::runtime_memory_log::append_server_sample(&process_sample) {
                                crate::logging::info(&format!(
                                    "Runtime memory logging process heartbeat sample failed: {}",
                                    err
                                ));
                            }

                            if let Some(sampling) = controller.build_sampling_for_attribution(
                                now,
                                &process_sample.process,
                                None,
                                None,
                            ) {
                                let mut attribution_sample = capture_runtime_memory_attribution_sample(
                                    &log_identity,
                                    &log_sessions,
                                    &log_client_count,
                                    server_start_time,
                                    "attribution:process-heartbeat",
                                    RuntimeMemoryLogTrigger {
                                        category: "process_heartbeat".to_string(),
                                        reason: "threshold_flush".to_string(),
                                        session_id: None,
                                        detail: None,
                                    },
                                    sampling,
                                )
                                .await;
                                controller.finalize_attribution_sample(now, &mut attribution_sample);
                                if let Err(err) = crate::runtime_memory_log::append_server_sample(&attribution_sample) {
                                    crate::logging::info(&format!(
                                        "Runtime memory logging attribution flush failed: {}",
                                        err
                                    ));
                                }
                            }
                        }
                        _ = attribution_interval.tick() => {
                            let now = Instant::now();
                            let preflight = capture_runtime_memory_process_sample(
                                &log_identity,
                                &log_client_count,
                                server_start_time,
                                "process:attribution-preflight",
                                RuntimeMemoryLogTrigger {
                                    category: "attribution_heartbeat".to_string(),
                                    reason: "preflight".to_string(),
                                    session_id: None,
                                    detail: None,
                                },
                                RuntimeMemoryLogSampling::default(),
                            )
                            .await;
                            if let Some(sampling) = controller.build_sampling_for_attribution(
                                now,
                                &preflight.process,
                                None,
                                Some("attribution_heartbeat"),
                            ) {
                                let mut attribution_sample = capture_runtime_memory_attribution_sample(
                                    &log_identity,
                                    &log_sessions,
                                    &log_client_count,
                                    server_start_time,
                                    "attribution:heartbeat",
                                    RuntimeMemoryLogTrigger {
                                        category: "attribution_heartbeat".to_string(),
                                        reason: "periodic".to_string(),
                                        session_id: None,
                                        detail: None,
                                    },
                                    sampling,
                                )
                                .await;
                                controller.finalize_attribution_sample(now, &mut attribution_sample);
                                if let Err(err) = crate::runtime_memory_log::append_server_sample(&attribution_sample) {
                                    crate::logging::info(&format!(
                                        "Runtime memory logging attribution heartbeat failed: {}",
                                        err
                                    ));
                                }
                            } else {
                                controller.mark_attribution_heartbeat_pending();
                            }
                        }
                        maybe_event = memory_event_rx.recv() => {
                            let Some(event) = maybe_event else {
                                break;
                            };
                            let now = Instant::now();
                            let should_write_process = controller.should_write_process_for_event(now, &event);
                            let process_sample = if should_write_process {
                                Some(
                                    capture_runtime_memory_process_sample(
                                        &log_identity,
                                        &log_client_count,
                                        server_start_time,
                                        &format!("process:event:{}", event.category),
                                        RuntimeMemoryLogTrigger {
                                            category: event.category.clone(),
                                            reason: event.reason.clone(),
                                            session_id: event.session_id.clone(),
                                            detail: event.detail.clone(),
                                        },
                                        controller.build_sampling_for_process(Some(&event)),
                                    )
                                    .await,
                                )
                            } else {
                                None
                            };

                            if let Some(process_sample) = process_sample.as_ref() {
                                controller.record_process_sample(now);
                                if let Err(err) = crate::runtime_memory_log::append_server_sample(process_sample) {
                                    crate::logging::info(&format!(
                                        "Runtime memory logging event process sample failed: {}",
                                        err
                                    ));
                                }
                            }

                            let mut wrote_attribution = false;
                            let preflight_sample = if process_sample.is_none() && controller.can_write_attribution(now) {
                                Some(
                                    capture_runtime_memory_process_sample(
                                        &log_identity,
                                        &log_client_count,
                                        server_start_time,
                                        &format!("process:event-preflight:{}", event.category),
                                        RuntimeMemoryLogTrigger {
                                            category: event.category.clone(),
                                            reason: "preflight".to_string(),
                                            session_id: event.session_id.clone(),
                                            detail: event.detail.clone(),
                                        },
                                        RuntimeMemoryLogSampling::default(),
                                    )
                                    .await,
                                )
                            } else {
                                None
                            };
                            let preflight = process_sample.as_ref().or(preflight_sample.as_ref());
                            if let Some(preflight) = preflight
                                && let Some(sampling) = controller.build_sampling_for_attribution(
                                    now,
                                    &preflight.process,
                                    Some(&event),
                                    None,
                                )
                            {
                                    let mut attribution_sample = capture_runtime_memory_attribution_sample(
                                        &log_identity,
                                        &log_sessions,
                                        &log_client_count,
                                        server_start_time,
                                        &format!("attribution:event:{}", event.category),
                                        RuntimeMemoryLogTrigger {
                                            category: event.category.clone(),
                                            reason: event.reason.clone(),
                                            session_id: event.session_id.clone(),
                                            detail: event.detail.clone(),
                                        },
                                        sampling,
                                    )
                                    .await;
                                    controller.finalize_attribution_sample(now, &mut attribution_sample);
                                    wrote_attribution = true;
                                    if let Err(err) = crate::runtime_memory_log::append_server_sample(&attribution_sample) {
                                        crate::logging::info(&format!(
                                            "Runtime memory logging event attribution sample failed: {}",
                                            err
                                        ));
                                    }
                                }

                            if !wrote_attribution {
                                controller.defer_event(event);
                            }
                        }
                    }
                }
            });
        }

        if let Some(policy) = temporary_server_policy {
            lifecycle::spawn_temporary_lifecycle_monitor(
                Arc::clone(&self.client_count),
                self.socket_path.clone(),
                self.debug_socket_path.clone(),
                self.identity.name.clone(),
                policy,
            );
        } else if debug_control_allowed() {
            crate::logging::info("Debug control enabled; idle timeout monitor disabled.");
        } else {
            let idle_client_count = Arc::clone(&self.client_count);
            let idle_server_name = self.identity.name.clone();
            tokio::spawn(async move {
                let mut idle_since: Option<std::time::Instant> = None;
                let mut check_interval = tokio::time::interval(std::time::Duration::from_secs(10));

                loop {
                    check_interval.tick().await;

                    let count = *idle_client_count.read().await;

                    if count == 0 {
                        // No clients connected
                        if idle_since.is_none() {
                            idle_since = Some(std::time::Instant::now());
                            crate::logging::info(&format!(
                                "No clients connected. Server will exit after {} minutes of idle.",
                                IDLE_TIMEOUT_SECS / 60
                            ));
                        }

                        if let Some(since) = idle_since {
                            let idle_duration = since.elapsed().as_secs();
                            if idle_duration >= IDLE_TIMEOUT_SECS {
                                crate::logging::info(&format!(
                                    "Server idle for {} minutes with no clients. Shutting down.",
                                    idle_duration / 60
                                ));
                                let _ = crate::registry::unregister_server(&idle_server_name).await;
                                std::process::exit(EXIT_IDLE_TIMEOUT);
                            }
                        }
                    } else {
                        // Clients connected - reset idle timer
                        if idle_since.is_some() {
                            crate::logging::info("Client connected. Idle timer cancelled.");
                        }
                        idle_since = None;
                    }
                }
            });
        }
    }
}
