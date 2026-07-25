{
            // Execute tools and add results
            let tool_count = tool_calls.len();
            for tool_index in 0..tool_count {
                // === INJECTION POINT C (before): Check for urgent abort before each tool (except first) ===
                if tool_index > 0 && self.has_urgent_interrupt() {
                    crate::telemetry::record_user_cancelled();
                    // Add tool_results for all remaining skipped tools to maintain valid history
                    for skipped_tc in &tool_calls[tool_index..] {
                        self.add_message(
                            Role::User,
                            vec![ContentBlock::ToolResult {
                                tool_use_id: skipped_tc.id.clone(),
                                content: "[Skipped: user interrupted]".to_string(),
                                is_error: Some(true),
                            }],
                        );
                    }
                    let tools_remaining = tool_count - tool_index;
                    let injected = self.inject_soft_interrupts();
                    if !injected.is_empty() {
                        for event in
                            Self::build_soft_interrupt_events(injected, "C", Some(tools_remaining))
                        {
                            let _ = event_tx.send(event);
                        }
                        // Add note about skipped tools for the AI
                        self.add_message(
                            Role::User,
                            vec![ContentBlock::Text {
                                text: format!(
                                    "[User interrupted: {} remaining tool(s) skipped]",
                                    tools_remaining
                                ),
                                cache_control: None,
                            }],
                        );
                    }
                    self.persist_session_best_effort("streamed tool output");
                    break; // Skip remaining tools
                }
                let tc = &tool_calls[tool_index];

                let message_id = assistant_message_id
                    .clone()
                    .unwrap_or_else(|| self.session.id.clone());

                if let Some(error_msg) = tc.validation_error() {
                    logging::warn(&error_msg);
                    let _ = event_tx.send(ServerEvent::ToolDone {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: error_msg.clone(),
                        error: Some(error_msg.clone()),
                    });
                    self.add_message(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: error_msg,
                            is_error: Some(true),
                        }],
                    );
                    tool_results_dirty = true;
                    continue;
                }

                self.validate_tool_allowed(&tc.name)?;

                let is_native_tool = JCODE_NATIVE_TOOLS.contains(&tc.name.as_str());

                if let Some((sdk_content, sdk_is_error)) = sdk_tool_results.remove(&tc.id) {
                    // For native tools, ignore SDK errors and execute locally
                    if !(is_native_tool && sdk_is_error) {
                        let sdk_content = cap_sdk_tool_content_for_history(&tc.name, sdk_content);
                        self.add_message(
                            Role::User,
                            vec![ContentBlock::ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: sdk_content,
                                is_error: if sdk_is_error { Some(true) } else { None },
                            }],
                        );
                        tool_results_dirty = true;

                        // NOTE: No injection here - wait for Point D after all tools

                        continue;
                    }
                    // Fall through to local execution for native tools with SDK errors
                }

                let ctx = ToolContext {
                    session_id: self.session.id.clone(),
                    message_id: message_id.clone(),
                    tool_call_id: tc.id.clone(),
                    working_dir: self.working_dir().map(PathBuf::from),
                    stdin_request_tx: self.stdin_request_tx.clone(),
                    graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
                    execution_mode: ToolExecutionMode::AgentTurn,
                };

                if trace {
                    eprintln!("[trace] tool_exec_start name={} id={}", tc.name, tc.id);
                }

                logging::info(&format!("Tool starting: {}", tc.name));
                crate::session_metrics::record_activity(&self.session.id);
                if inline_output_tap {
                    // Surface the tool execution on the coordinator's inline
                    // viewport immediately: workers spend most wall-clock time
                    // here, where no assistant text streams.
                    self.inline_tail.start_tool(&tc.name, &tc.input);
                    self.publish_inline_tail();
                }
                let tool_start = Instant::now();

                // Spawn tool in its own task so we can detach it to background on Alt+B
                self.ensure_tool_identity_ready()?;
                let registry_clone = self.registry.clone();
                let tool_name_for_spawn = tc.name.clone();
                let tool_input_for_spawn = tc.input.clone();
                let account_admission =
                    crate::session::capture_account_transition_admission();
                let tool_handle = tokio::spawn(
                    crate::session::with_inherited_account_transition_admission(
                        account_admission,
                        async move {
                            registry_clone
                                .execute(&tool_name_for_spawn, tool_input_for_spawn, ctx)
                                .await
                        },
                    ),
                );
                let mut abort_tool_on_turn_drop = AbortTaskOnDrop::new(&tool_handle);

                // Reset background signal before waiting
                self.background_tool_signal.reset();

                // Wait for tool completion OR background signal from user (Alt+B)
                // OR graceful shutdown signal from server reload
                let bg_signal = self.background_tool_signal.clone();
                let shutdown_signal = self.graceful_shutdown.clone();
                let allow_reload_handoff = tc.name == "bash";
                let tool_result;
                let mut tool_handle = tool_handle;
                tokio::select! {
                    biased;
                    res = &mut tool_handle => {
                        abort_tool_on_turn_drop.disarm();
                        tool_result = Some(match res {
                            Ok(r) => r,
                            Err(e) => Err(anyhow::anyhow!("Tool task panicked: {}", e)),
                        });
                    }
                    _ = async {
                        tokio::select! {
                            _ = bg_signal.notified() => {}
                            _ = shutdown_signal.notified() => {}
                        }
                    } => {
                        if self.is_graceful_shutdown() && allow_reload_handoff {
                            tool_result = match tokio::time::timeout(
                                Duration::from_millis(750),
                                &mut tool_handle,
                            )
                            .await
                            {
                                Ok(res) => Some(match res {
                                    // The nested task reached a terminal outcome.
                                    Ok(r) => r,
                                    Err(e) => Err(anyhow::anyhow!("Tool task panicked: {}", e)),
                                }),
                                Err(_) => None,
                            };
                            if tool_result.is_some() {
                                abort_tool_on_turn_drop.disarm();
                            }
                        } else {
                            tool_result = None;
                        }
                    }
                };

                self.unlock_tools_if_needed(&tc.name);
                let tool_elapsed = tool_start.elapsed();
                crate::session_metrics::record_activity(&self.session.id);

                if let Some(result) = tool_result {
                    // Normal tool completion
                    logging::info(&format!(
                        "Tool finished: {} in {:.2}s",
                        tc.name,
                        tool_elapsed.as_secs_f64()
                    ));
                    if inline_output_tap {
                        // Update the tool marker in place with duration/error.
                        self.inline_tail
                            .finish_tool(tool_elapsed.as_secs_f64(), result.is_err());
                        self.publish_inline_tail();
                    }

                    match result {
                        Ok(output) => {
                            let output = cap_tool_output_for_history(&tc.name, output);
                            let _ = event_tx.send(ServerEvent::ToolDone {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                output: output.output.clone(),
                                error: None,
                            });

                            let side_pane_images =
                                tool_output_side_pane_images(&tc.id, &tc.name, &tc.input, &output);
                            if !side_pane_images.is_empty() {
                                logging::info(&format!(
                                    "SidePaneImages: emitting {} image(s) from tool '{}' (session={})",
                                    side_pane_images.len(),
                                    tc.name,
                                    self.session.id
                                ));
                                let _ = event_tx.send(ServerEvent::SidePaneImages {
                                    session_id: self.session.id.clone(),
                                    images: side_pane_images,
                                });
                            }

                            let blocks = tool_output_to_content_blocks(tc.id.clone(), output);
                            self.add_message_with_duration(
                                Role::User,
                                blocks,
                                Some(tool_elapsed.as_millis() as u64),
                            );
                            tool_results_dirty = true;
                        }
                        Err(e) => {
                            let error_msg = format!("Error: {}", e);
                            let _ = event_tx.send(ServerEvent::ToolDone {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                output: error_msg.clone(),
                                error: Some(error_msg.clone()),
                            });

                            self.add_message_with_duration(
                                Role::User,
                                vec![ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: error_msg,
                                    is_error: Some(true),
                                }],
                                Some(tool_elapsed.as_millis() as u64),
                            );
                            tool_results_dirty = true;
                        }
                    }
                } else if self.is_graceful_shutdown() {
                    // Server reload - abort tool and save interrupted result
                    logging::info(&format!(
                        "Tool '{}' interrupted by server reload after {:.1}s",
                        tc.name,
                        tool_elapsed.as_secs_f64()
                    ));
                    tool_handle.abort();
                    let _ = (&mut tool_handle).await;
                    abort_tool_on_turn_drop.disarm();

                    // For selfdev reload and wait-like tools, the interruption is expected:
                    // selfdev initiated the restart, while wait-like tools should be resumed
                    // after reload rather than treated as failed work.
                    let (interrupted_msg, is_error) =
                        reload_interrupted_tool_result(tc, tool_elapsed.as_secs_f64());

                    let _ = event_tx.send(ServerEvent::ToolDone {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: interrupted_msg.clone(),
                        error: if is_error {
                            Some("interrupted by reload".to_string())
                        } else {
                            None
                        },
                    });

                    self.add_message_with_duration(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: interrupted_msg,
                            is_error: Some(is_error),
                        }],
                        Some(tool_elapsed.as_millis() as u64),
                    );
                    self.session.save()?;

                    // Add results for any remaining tools too
                    for remaining_tc in &tool_calls[(tool_index + 1)..] {
                        self.add_message(
                            Role::User,
                            vec![ContentBlock::ToolResult {
                                tool_use_id: remaining_tc.id.clone(),
                                content: "[Skipped - server reloading]".to_string(),
                                is_error: Some(true),
                            }],
                        );
                    }
                    self.session.save()?;
                    return Ok(());
                } else {
                    // User pressed Alt+B — move tool to background
                    logging::info(&format!(
                        "Tool '{}' moved to background after {:.1}s",
                        tc.name,
                        tool_elapsed.as_secs_f64()
                    ));

                    let bg_info = crate::background::global()
                        .adopt(&tc.name, &self.session.id, tool_handle)
                        .await?;
                    // Disarm only after the background manager has durably
                    // published ownership of the nested task.
                    abort_tool_on_turn_drop.disarm();

                    let bg_msg = format!(
                        "Tool '{}' was moved to background by the user (task_id: {}). \
                         Use the `bg` tool with action 'wait' to wait for completion/checkpoints, \
                         or action 'status'/'output' to inspect it.",
                        tc.name, bg_info.task_id
                    );

                    let _ = event_tx.send(ServerEvent::ToolDone {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        output: bg_msg.clone(),
                        error: None,
                    });

                    self.add_message_with_duration(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: bg_msg,
                            is_error: None,
                        }],
                        Some(tool_elapsed.as_millis() as u64),
                    );
                    self.session.save()?;

                    self.background_tool_signal.reset();
                }

                // NOTE: We do NOT inject between tools (non-urgent) because that would
                // place user text between tool_results, which may violate API constraints.
                // All non-urgent injection happens at Point D after all tools are done.
            }
}
