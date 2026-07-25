impl Agent {
    pub(super) async fn run_turn_streaming_mpsc(
        &mut self,
        event_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> Result<()> {
        self.set_log_context();
        // Mark this session as actively streaming for presence UIs (e.g. the
        // macOS menu bar indicator). Cleared automatically on every exit path.
        let _streaming_guard = crate::session::StreamingGuard::new(self.session.id.clone());
        // Register this turn's cancel signal in the process-global registry so
        // a cancel routed through *any* control handle for this session (even a
        // stale one built for a different agent object, e.g. after a
        // reattach/reload) aborts this in-flight stream immediately (issue #428).
        let _turn_cancel_guard = crate::turn_cancel_registry::register_active_turn(
            &self.session.id,
            self.graceful_shutdown.clone(),
        );
        let trace = trace_enabled();
        let mut context_limit_retries = 0u32;
        let mut incomplete_continuations = 0u32;

        loop {
            let repaired = self.repair_missing_tool_outputs();
            if repaired > 0 {
                logging::warn(&format!(
                    "Recovered {} missing tool output(s) before API call",
                    repaired
                ));
            }
            let (messages, compaction_event) = self.messages_for_provider();
            if let Some(event) = compaction_event {
                // Reset cache tracker and tool lock on compaction since the message history changes
                self.cache_tracker.reset();
                self.locked_tools = None;
                logging::info(&format!(
                    "Context compacted ({}{})",
                    event.trigger,
                    event
                        .pre_tokens
                        .map(|t| format!(" {} tokens", t))
                        .unwrap_or_default()
                ));
                drop(event_tx.send(server_event_for_compaction(&event)));
            }

            let tools = self.tool_definitions().await;
            let messages: std::sync::Arc<[Message]> = messages.into();
            // Non-blocking memory: uses pending result from last turn, spawns check for next turn
            let memory_pending = self.build_memory_prompt_nonblocking_shared(
                std::sync::Arc::clone(&messages),
                Some(std::sync::Arc::new({
                    let event_tx = event_tx.clone();
                    move |event| {
                        let _ = event_tx.send(event);
                    }
                })),
            );
            // Use split prompt for better caching - static content cached, dynamic not
            let split_prompt = self.build_system_prompt_split(None);
            self.log_prompt_prefix_accounting(&split_prompt, &tools);

            // Check for client-side cache violations before memory injection.
            // Memory is an ephemeral suffix that changes each turn; tracking it would cause
            // false-positive violations every turn (prior turn's memory ≠ current history prefix).
            self.record_client_cache_request(&messages);

            // `messages` now owns the provider-facing request snapshot. Do not
            // retain the session's second, derived copy for the entire network
            // wait and response stream.
            self.session.release_provider_messages_cache();

            let mut cache_signature_messages =
                if crate::config::config().features.message_timestamps {
                    Message::with_timestamps(&messages)
                } else {
                    messages.iter().cloned().collect()
                };
            let mut ephemeral_signature_messages = Vec::new();

            // Inject memory as a user message at the end (preserves cache prefix)
            let mut messages_with_memory: Vec<Message> = messages.iter().cloned().collect();
            if let Some(memory) = memory_pending.as_ref() {
                let memory_count = memory.count.max(1);
                let computed_age_ms = memory.computed_at.elapsed().as_millis() as u64;
                crate::memory::record_injected_prompt(
                    &memory.prompt,
                    memory_count,
                    computed_age_ms,
                );
                self.record_memory_injection_in_session(memory);
                let _ = event_tx.send(ServerEvent::MemoryInjected {
                    count: memory_count,
                    prompt: memory.prompt.clone(),
                    display_prompt: memory.display_prompt.clone(),
                    prompt_chars: memory.prompt.chars().count(),
                    computed_age_ms,
                });
                let (memory_msg, persisted) = self.prepare_memory_injection_message(memory);
                if !persisted {
                    ephemeral_signature_messages.push(memory_msg.clone());
                } else {
                    cache_signature_messages.push(memory_msg.clone());
                }
                messages_with_memory.push(memory_msg);
            }

            logging::info(&format!(
                "API call starting: {} messages, {} tools",
                messages_with_memory.len(),
                tools.len()
            ));
            let api_start = Instant::now();

            let stamped = crate::config::config()
                .features
                .message_timestamps
                .then(|| Message::with_timestamps(&messages_with_memory));
            let send_messages = stamped.as_deref().unwrap_or(&messages_with_memory);
            let provider = Arc::clone(&self.provider);
            // Capture the model id the request was issued with. A provider may
            // transparently switch models mid-request (e.g. Anthropic's retired
            // `claude-fable-5` falls back to `claude-opus-4-8`). When that
            // happens the provider mutates its own model state, but the session
            // and clients still believe they are on the originally requested
            // model. Compare against this after the stream so we can emit a
            // `ModelChanged` and resync the UI/context-limit.
            let model_at_request_start = provider.model().to_string();
            let identity_at_request_start = provider.exact_runtime_identity();
            let resume_session_id = self.provider_session_id.clone();
            self.last_status_detail = None;
            let _ = event_tx.send(kv_cache_request_event(
                &cache_signature_messages,
                &tools,
                &split_prompt.static_part,
                &ephemeral_signature_messages,
            ));
            // These vectors are only needed to build the cache telemetry event.
            // Explicitly release their deeply cloned transcript strings before
            // waiting for the provider stream.
            drop(cache_signature_messages);
            drop(ephemeral_signature_messages);
            let mut keepalive = stream_keepalive_ticker();
            let mut stream = {
                let mut complete_future = std::pin::pin!(provider.complete_split(
                    send_messages,
                    &tools,
                    &split_prompt.static_part,
                    &split_prompt.dynamic_part,
                    resume_session_id.as_deref(),
                ));
                loop {
                    tokio::select! {
                        _ = keepalive.tick() => {
                            send_stream_keepalive_mpsc(&event_tx);
                        }
                        _ = self.graceful_shutdown.notified() => {
                            logging::info(
                                "Graceful shutdown/cancel before API stream opened - stopping turn",
                            );
                            return Ok(());
                        }
                        result = &mut complete_future => {
                            match result {
                                Ok(stream) => break stream,
                                Err(e) => {
                                    if let Some(compaction_event) =
                                        self.try_auto_compact_after_context_limit(&e.to_string())
                                    {
                                        context_limit_retries += 1;
                                        if context_limit_retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                                            logging::warn(
                                                "Context-limit compaction retry limit reached; giving up",
                                            );
                                            return Err(anyhow::anyhow!(
                                                "Context limit exceeded after {} compaction retries",
                                                Self::MAX_CONTEXT_LIMIT_RETRIES
                                            ));
                                        }
                                        let _ = event_tx
                                            .send(server_event_for_compaction(&compaction_event));
                                        continue;
                                    }
                                    return Err(e);
                                }
                            }
                        }
                    }
                }
            };
            self.reconcile_exact_runtime_identity_after_request_open(
                identity_at_request_start.as_ref(),
            )?;
            let identity_at_stream_open = self.provider.exact_runtime_identity();

            // `complete_split` has consumed the request and returned an owned
            // response stream. Keeping these full transcript snapshots alive
            // while tokens arrive needlessly multiplies active-session memory.
            drop(stamped);
            drop(messages_with_memory);
            drop(memory_pending);
            drop(messages);
            drop(split_prompt);

            // Successful API call - reset retry counter
            context_limit_retries = 0;

            logging::info(&format!(
                "API stream opened in {:.2}s",
                api_start.elapsed().as_secs_f64()
            ));
            log_agent_provider_stream_lifecycle(
                logging::LogLevel::Info,
                self,
                "stream_opened",
                api_start,
                vec![("mode", "mpsc".to_string())],
            );

            let mut text_content = String::new();
            let mut text_wrapped_detected = false;
            // Inline swarm worker output tap: publish a throttled tail of the
            // in-progress assistant text to the bus so a coordinator can render
            // a live inline gallery viewport.
            let inline_output_tap = self.inline_output_tap();
            let mut inline_tap_last = Instant::now()
                .checked_sub(std::time::Duration::from_millis(1000))
                .unwrap_or_else(Instant::now);
            // Throttled "this session is alive" marks while tokens stream, so
            // swarm status can distinguish a busy worker from a dead one
            // without paying a registry lock per token.
            let mut activity_mark_last = Instant::now()
                .checked_sub(std::time::Duration::from_secs(10))
                .unwrap_or_else(Instant::now);
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut current_tool: Option<ToolCall> = None;
            let mut current_tool_input = String::new();
            let mut generated_image_contexts: Vec<Vec<ContentBlock>> = Vec::new();
            let mut usage_input: Option<u64> = None;
            let mut usage_output: Option<u64> = None;
            let mut usage_cache_read: Option<u64> = None;
            let mut usage_cache_creation: Option<u64> = None;
            let mut saw_message_end = false;
            let mut stop_reason: Option<String> = None;
            let mut sdk_tool_results: std::collections::HashMap<String, (String, bool)> =
                std::collections::HashMap::new();
            let provider_name = self.provider.name().to_string();
            let store_reasoning_content =
                crate::provider::stores_reasoning_content_for_context(&provider_name);
            let mut reasoning_content = String::new();
            let mut reasoning_signature = String::new();
            // Whether a live reasoning region is currently streaming to the client.
            // Raw reasoning deltas are sent as `ReasoningDelta`; the client owns the
            // dim/italic styling and live partial-line rendering. We close the region
            // (via `ReasoningDone`) before real output or a tool call begins.
            let mut reasoning_open = false;
            // Last time hidden (non-displayed) reasoning activity was relayed
            // to clients as a keepalive; throttles issue #451 keepalives.
            let mut hidden_activity_last = Instant::now();
            let mut openai_reasoning_items: Vec<ContentBlock> = Vec::new();
            let mut openai_native_compaction: Option<(String, usize)> = None;
            let mut tool_id_to_name: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            let mut retry_after_compaction = false;
            let mut keepalive = stream_keepalive_ticker();
            loop {
                let next_event = std::pin::pin!(stream.next());
                let event = tokio::select! {
                    _ = keepalive.tick() => {
                        send_stream_keepalive_mpsc(&event_tx);
                        continue;
                    }
                    _ = self.graceful_shutdown.notified() => {
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Warn,
                            self,
                            "stream_cancelled",
                            api_start,
                            vec![
                                ("mode", "mpsc".to_string()),
                                ("reason", "graceful_shutdown".to_string()),
                            ],
                        );
                        logging::info(
                            "Graceful shutdown/cancel while waiting for API stream event - stopping stream",
                        );
                        break;
                    }
                    event = next_event => event,
                };

                if activity_mark_last.elapsed() >= std::time::Duration::from_secs(2) {
                    activity_mark_last = Instant::now();
                    crate::session_metrics::record_activity(&self.session.id);
                }
                let Some(event) = event else {
                    log_agent_provider_stream_lifecycle(
                        if saw_message_end {
                            logging::LogLevel::Info
                        } else {
                            logging::LogLevel::Warn
                        },
                        self,
                        "stream_eof",
                        api_start,
                        vec![
                            ("mode", "mpsc".to_string()),
                            ("saw_message_end", saw_message_end.to_string()),
                        ],
                    );
                    break;
                };
                let event = match event {
                    Ok(event) => event,
                    Err(e) => {
                        let err_str = e.to_string();
                        if let Some(compaction_event) =
                            self.try_auto_compact_after_context_limit(&err_str)
                        {
                            log_agent_provider_stream_lifecycle(
                                logging::LogLevel::Warn,
                                self,
                                "stream_error_retry_after_compaction",
                                api_start,
                                vec![
                                    ("mode", "mpsc".to_string()),
                                    ("error", err_str.clone()),
                                    (
                                        "context_limit_retries",
                                        (context_limit_retries + 1).to_string(),
                                    ),
                                ],
                            );
                            context_limit_retries += 1;
                            if context_limit_retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                                logging::warn(
                                    "Context-limit compaction retry limit reached; giving up",
                                );
                                return Err(anyhow::anyhow!(
                                    "Context limit exceeded after {} compaction retries",
                                    Self::MAX_CONTEXT_LIMIT_RETRIES
                                ));
                            }
                            retry_after_compaction = true;
                            let _ = event_tx.send(server_event_for_compaction(&compaction_event));
                            break;
                        }
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Error,
                            self,
                            "stream_error",
                            api_start,
                            vec![("mode", "mpsc".to_string()), ("error", err_str)],
                        );
                        return Err(e);
                    }
                };

                match event {
                    StreamEvent::ThinkingStart => {
                        // Reasoning tokens are counted in provider output usage even when
                        // `display.show_thinking` hides the text. Let remote clients start
                        // their TPS timer without forcing hidden reasoning into the transcript.
                        let _ = event_tx.send(ServerEvent::ConnectionPhase {
                            phase: crate::message::ConnectionPhase::Streaming.to_string(),
                        });
                    }
                    StreamEvent::ThinkingEnd => {}
                    StreamEvent::ThinkingSignatureDelta(signature) => {
                        if store_reasoning_content {
                            reasoning_signature.push_str(&signature);
                        }
                    }
                    StreamEvent::ThinkingDelta(thinking_text) => {
                        // Only send thinking content if enabled in config
                        if crate::config::config().display.show_thinking
                            && !thinking_text.is_empty()
                        {
                            reasoning_open = true;
                            let _ = event_tx.send(ServerEvent::ReasoningDelta {
                                text: thinking_text.clone(),
                            });
                        } else if hidden_activity_last.elapsed()
                            >= std::time::Duration::from_secs(5)
                        {
                            // Hidden reasoning is real provider activity, but it
                            // emits nothing over the client socket, so a long
                            // silent thinking phase looks identical to a dead
                            // connection and the client stall guard cancels a
                            // healthy stream (issue #451). Send a throttled
                            // non-rendered keepalive so clients track provider
                            // activity, not just displayable events.
                            hidden_activity_last = Instant::now();
                            send_stream_keepalive_mpsc(&event_tx);
                        }
                        // Always capture reasoning text so it can be persisted as a
                        // history-only trace, regardless of provider replay support.
                        reasoning_content.push_str(&thinking_text);
                    }
                    StreamEvent::ThinkingDone { duration_secs } => {
                        if reasoning_open {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone {
                                duration_secs: Some(duration_secs),
                            });
                        }
                    }
                    StreamEvent::TextDelta(text) => {
                        // Close any open reasoning region before real output so the
                        // answer renders as a normal paragraph rather than as reasoning.
                        if reasoning_open && !text.trim().is_empty() {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone {
                                duration_secs: None,
                            });
                        }
                        text_content.push_str(&text);
                        if inline_output_tap {
                            self.inline_tail.set_live(&text_content);
                            if inline_tap_last.elapsed() >= std::time::Duration::from_millis(200) {
                                inline_tap_last = Instant::now();
                                self.publish_inline_tail();
                            }
                        }
                        if !text_wrapped_detected {
                            // Scan only the new delta (plus a short overlap for
                            // markers straddling the boundary) instead of the
                            // whole accumulated response on every token.
                            if let Some(marker_idx) =
                                find_wrap_marker_incremental(&text_content, text.len())
                            {
                                text_wrapped_detected = true;
                                let clean_prefix =
                                    text_content[..marker_idx].trim_end().to_string();
                                let _ =
                                    event_tx.send(ServerEvent::TextReplace { text: clean_prefix });
                            } else {
                                let _ =
                                    event_tx.send(ServerEvent::TextDelta { text: text.clone() });
                            }
                        }
                        if self.is_graceful_shutdown() {
                            logging::info(
                                "Graceful shutdown during streaming - checkpointing partial response",
                            );
                            let _ = event_tx.send(ServerEvent::TextDelta {
                                text: "\n\n[generation interrupted - server reloading]".to_string(),
                            });
                            text_content
                                .push_str("\n\n[generation interrupted - server reloading]");
                            break;
                        }
                    }
                    StreamEvent::ToolUseStart { id, name } => {
                        if reasoning_open {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone {
                                duration_secs: None,
                            });
                        }
                        let _ = event_tx.send(ServerEvent::ToolStart {
                            id: id.clone(),
                            name: name.clone(),
                        });
                        tool_id_to_name.insert(id.clone(), name.clone());
                        current_tool = Some(ToolCall {
                            id,
                            name,
                            input: serde_json::Value::Null,
                            intent: None,
                            thought_signature: None,
                        });
                        current_tool_input.clear();
                    }
                    StreamEvent::ToolInputDelta(delta) => {
                        let _ = event_tx.send(ServerEvent::ToolInput {
                            delta: delta.clone(),
                        });
                        current_tool_input.push_str(&delta);
                    }
                    StreamEvent::ToolUseEnd => {
                        if let Some(mut tool) = current_tool.take() {
                            tool.input =
                                ToolCall::parse_streamed_input_to_object(&current_tool_input);
                            tool.refresh_intent_from_input();

                            let _ = event_tx.send(ServerEvent::ToolExec {
                                id: tool.id.clone(),
                                name: tool.name.clone(),
                            });

                            tool_calls.push(tool);
                            current_tool_input.clear();
                        }
                    }
                    StreamEvent::ToolUseSignature(signature) => {
                        // Attach Gemini 3 thought signature to the most recent
                        // tool call so it can be persisted and replayed.
                        if let Some(tool) = tool_calls.last_mut()
                            && !signature.is_empty()
                        {
                            tool.thought_signature = Some(signature);
                        }
                    }
                    StreamEvent::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let tool_name = tool_id_to_name
                            .get(&tool_use_id)
                            .cloned()
                            .unwrap_or_default();
                        let _ = event_tx.send(ServerEvent::ToolDone {
                            id: tool_use_id.clone(),
                            name: tool_name,
                            output: content.clone(),
                            error: if is_error {
                                Some("Tool error".to_string())
                            } else {
                                None
                            },
                        });
                        sdk_tool_results.insert(tool_use_id, (content, is_error));
                    }
                    StreamEvent::GeneratedImage {
                        id,
                        path,
                        metadata_path,
                        output_format,
                        revised_prompt,
                    } => {
                        let rendered_image = crate::message::generated_image_rendered_image(
                            &id,
                            &path,
                            &output_format,
                        );
                        if self.provider.supports_image_input() {
                            if let Some(blocks) =
                                crate::message::generated_image_visual_context_blocks(
                                    &path,
                                    metadata_path.as_deref(),
                                    &output_format,
                                    revised_prompt.as_deref(),
                                )
                            {
                                generated_image_contexts.push(blocks);
                            } else {
                                crate::logging::warn(&format!(
                                    "Generated image was not attached as visual context: {}",
                                    path
                                ));
                            }
                        }
                        let _ = event_tx.send(ServerEvent::GeneratedImage {
                            id,
                            path,
                            metadata_path,
                            output_format,
                            revised_prompt,
                        });
                        if let Some(image) = rendered_image {
                            let _ = event_tx.send(ServerEvent::SidePaneImages {
                                session_id: self.session.id.clone(),
                                images: vec![image],
                            });
                        }
                    }
                    StreamEvent::TokenUsage {
                        input_tokens,
                        output_tokens,
                        cache_read_input_tokens,
                        cache_creation_input_tokens,
                    } => {
                        if let Some(input) = input_tokens {
                            usage_input = Some(input);
                        }
                        if let Some(output) = output_tokens {
                            usage_output = Some(output);
                        }
                        if cache_read_input_tokens.is_some() {
                            usage_cache_read = cache_read_input_tokens;
                        }
                        if cache_creation_input_tokens.is_some() {
                            usage_cache_creation = cache_creation_input_tokens;
                        }
                        if let Some(input) = usage_input {
                            self.update_compaction_usage_from_stream(
                                input,
                                usage_cache_read,
                                usage_cache_creation,
                            );
                        }
                    }
                    StreamEvent::ConnectionType { connection } => {
                        crate::telemetry::record_connection_type(&connection);
                        self.last_connection_type = Some(connection.clone());
                        let _ = event_tx.send(ServerEvent::ConnectionType { connection });
                    }
                    StreamEvent::ConnectionPhase { phase } => {
                        let _ = event_tx.send(ServerEvent::ConnectionPhase {
                            phase: phase.to_string(),
                        });
                    }
                    StreamEvent::StatusDetail { detail } => {
                        self.last_status_detail = Some(detail.clone());
                        let _ = event_tx.send(ServerEvent::StatusDetail { detail });
                    }
                    StreamEvent::RetryRollback { attempt, max } => {
                        // A transient transport fault hit mid-stream after partial
                        // output was already emitted; the provider is replaying the
                        // request from the top. Discard everything accumulated for
                        // this attempt so the replay doesn't duplicate output, and
                        // tell the client to do the same.
                        logging::warn(&format!(
                            "Mid-stream retry rollback (attempt {}/{}): discarding partial output ({} text chars, {} tool calls)",
                            attempt,
                            max,
                            text_content.len(),
                            tool_calls.len(),
                        ));
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Warn,
                            self,
                            "retry_rollback",
                            api_start,
                            vec![
                                ("mode", "mpsc".to_string()),
                                ("attempt", attempt.to_string()),
                                ("max", max.to_string()),
                                ("text_chars", text_content.len().to_string()),
                                ("tool_calls", tool_calls.len().to_string()),
                            ],
                        );
                        text_content.clear();
                        if inline_output_tap {
                            // The provider replays from the top; drop the
                            // discarded partial from the live tail too.
                            self.inline_tail.clear_live();
                        }
                        text_wrapped_detected = false;
                        tool_calls.clear();
                        current_tool = None;
                        current_tool_input.clear();
                        tool_id_to_name.clear();
                        sdk_tool_results.clear();
                        generated_image_contexts.clear();
                        reasoning_content.clear();
                        reasoning_signature.clear();
                        reasoning_open = false;
                        openai_reasoning_items.clear();
                        openai_native_compaction = None;
                        saw_message_end = false;
                        stop_reason = None;
                        let _ = event_tx.send(ServerEvent::RetryRollback { attempt, max });
                        let _ = event_tx.send(ServerEvent::ConnectionPhase {
                            phase: crate::message::ConnectionPhase::Retrying { attempt, max }
                                .to_string(),
                        });
                    }
                    StreamEvent::MessageEnd {
                        stop_reason: reason,
                    } => {
                        saw_message_end = true;
                        if inline_output_tap {
                            // Fold the finished text into the rolling tail so
                            // it survives the next turn/continuation.
                            self.inline_tail.set_live(&text_content);
                            self.inline_tail.commit_live();
                        }
                        // Close any still-open reasoning region (e.g. a reasoning-only
                        // step) so the client flushes its live partial line.
                        if reasoning_open {
                            reasoning_open = false;
                            let _ = event_tx.send(ServerEvent::ReasoningDone {
                                duration_secs: None,
                            });
                        }
                        if reason.is_some() {
                            stop_reason = reason;
                        }
                        let _ = event_tx.send(ServerEvent::MessageEnd);
                    }
                    StreamEvent::SessionId(sid) => {
                        self.bind_provider_session_id_for_identity(
                            sid,
                            identity_at_stream_open.as_ref(),
                        );
                    }
                    StreamEvent::OpenAIReasoning {
                        id,
                        summary,
                        encrypted_content,
                        status,
                    } => {
                        if store_reasoning_content {
                            openai_reasoning_items.push(ContentBlock::OpenAIReasoning {
                                id,
                                summary,
                                encrypted_content,
                                status,
                            });
                        }
                    }
                    StreamEvent::Compaction {
                        openai_encrypted_content,
                        ..
                    } => {
                        if crate::config::config().compaction.engine
                            == crate::config::CompactionEngine::Lcm
                        {
                            logging::warn(
                                "Ignoring provider-native compaction event because LCM owns context",
                            );
                            continue;
                        }
                        if let Some(encrypted_content) = openai_encrypted_content {
                            openai_native_compaction
                                .get_or_insert((encrypted_content, self.session.messages.len()));
                        }
                    }
                    StreamEvent::NativeToolCall {
                        request_id,
                        tool_name,
                        input,
                    } => {
                        // Execute native tool and send result back to SDK bridge
                        let ctx = ToolContext {
                            session_id: self.session.id.clone(),
                            message_id: self.session.id.clone(),
                            tool_call_id: request_id.clone(),
                            working_dir: self.working_dir().map(PathBuf::from),
                            stdin_request_tx: self.stdin_request_tx.clone(),
                            graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
                            execution_mode: ToolExecutionMode::AgentTurn,
                        };
                        crate::telemetry::record_tool_call();
                        let tool_result = self
                            .registry
                            .execute(&tool_name, ToolCall::normalize_input_to_object(input), ctx)
                            .await;
                        if tool_result.is_err() {
                            crate::telemetry::record_tool_failure();
                        }
                        let native_result = match tool_result {
                            Ok(output) => NativeToolResult::success(request_id, output.output),
                            Err(e) => NativeToolResult::error(request_id, e.to_string()),
                        };
                        if let Some(sender) = self.provider.native_result_sender() {
                            let _ = sender.send(native_result).await;
                        }
                    }
                    StreamEvent::UpstreamProvider { provider } => {
                        self.last_upstream_provider = Some(provider.clone());
                        let _ = event_tx.send(ServerEvent::UpstreamProvider { provider });
                    }
                    StreamEvent::Error {
                        message,
                        retry_after_secs,
                    } => {
                        if let Some(compaction_event) =
                            self.try_auto_compact_after_context_limit(&message)
                        {
                            log_agent_provider_stream_lifecycle(
                                logging::LogLevel::Warn,
                                self,
                                "stream_event_retry_after_compaction",
                                api_start,
                                vec![
                                    ("mode", "mpsc".to_string()),
                                    ("error", message.clone()),
                                    (
                                        "context_limit_retries",
                                        (context_limit_retries + 1).to_string(),
                                    ),
                                ],
                            );
                            context_limit_retries += 1;
                            if context_limit_retries > Self::MAX_CONTEXT_LIMIT_RETRIES {
                                logging::warn(
                                    "Context-limit compaction retry limit reached; giving up",
                                );
                                return Err(anyhow::anyhow!(
                                    "Context limit exceeded after {} compaction retries",
                                    Self::MAX_CONTEXT_LIMIT_RETRIES
                                ));
                            }
                            retry_after_compaction = true;
                            let _ = event_tx.send(server_event_for_compaction(&compaction_event));
                            break;
                        }
                        log_agent_provider_stream_lifecycle(
                            logging::LogLevel::Error,
                            self,
                            "stream_event_error",
                            api_start,
                            vec![
                                ("mode", "mpsc".to_string()),
                                ("error", message.clone()),
                                (
                                    "retry_after_secs",
                                    retry_after_secs
                                        .map(|seconds| seconds.to_string())
                                        .unwrap_or_else(|| "none".to_string()),
                                ),
                            ],
                        );
                        return Err(StreamError::new(message, retry_after_secs).into());
                    }
                }
            }

            // An owned response stream may refresh credentials after the request
            // opened. Reconcile again before any tool execution or continuation.
            self.reconcile_exact_runtime_identity_after_request_open(
                identity_at_stream_open.as_ref(),
            )?;

            if retry_after_compaction {
                log_agent_provider_stream_lifecycle(
                    logging::LogLevel::Info,
                    self,
                    "retry_after_compaction",
                    api_start,
                    vec![("mode", "mpsc".to_string())],
                );
                continue;
            }

            let api_elapsed = api_start.elapsed();
            logging::info(&format!(
                "API call complete in {:.2}s (input={} output={} cache_read={} cache_write={})",
                api_elapsed.as_secs_f64(),
                usage_input.unwrap_or(0),
                usage_output.unwrap_or(0),
                usage_cache_read.unwrap_or(0),
                usage_cache_creation.unwrap_or(0),
            ));
            log_agent_provider_stream_lifecycle(
                logging::LogLevel::Info,
                self,
                "stream_complete",
                api_start,
                vec![
                    ("mode", "mpsc".to_string()),
                    ("saw_message_end", saw_message_end.to_string()),
                    (
                        "stop_reason",
                        stop_reason.clone().unwrap_or_else(|| "none".to_string()),
                    ),
                    ("input_tokens", usage_input.unwrap_or(0).to_string()),
                    ("output_tokens", usage_output.unwrap_or(0).to_string()),
                    ("cache_read", usage_cache_read.unwrap_or(0).to_string()),
                    ("cache_write", usage_cache_creation.unwrap_or(0).to_string()),
                ],
            );

            if usage_input.is_some()
                || usage_output.is_some()
                || usage_cache_read.is_some()
                || usage_cache_creation.is_some()
            {
                crate::telemetry::record_token_usage(
                    usage_input.unwrap_or(0),
                    usage_output.unwrap_or(0),
                    usage_cache_read,
                    usage_cache_creation,
                );

                let input = usage_input.unwrap_or(0);
                let output = usage_output.unwrap_or(0);
                let total = input
                    .saturating_add(output)
                    .saturating_add(usage_cache_read.unwrap_or(0))
                    .saturating_add(usage_cache_creation.unwrap_or(0));
                crate::session_metrics::record_token_usage(&self.session.id, total, output);
            }

            if usage_input.is_some()
                || usage_output.is_some()
                || usage_cache_read.is_some()
                || usage_cache_creation.is_some()
            {
                let _ = event_tx.send(ServerEvent::TokenUsage {
                    input: usage_input.unwrap_or(0),
                    output: usage_output.unwrap_or(0),
                    cache_read_input: usage_cache_read,
                    cache_creation_input: usage_cache_creation,
                });
            }

            // Store usage for debug queries
            self.last_usage = TokenUsage {
                input_tokens: usage_input.unwrap_or(0),
                output_tokens: usage_output.unwrap_or(0),
                cache_read_input_tokens: usage_cache_read,
                cache_creation_input_tokens: usage_cache_creation,
            };

            // Detect a transparent mid-request model switch (e.g. Anthropic's
            // retired `claude-fable-5` falling back to `claude-opus-4-8`). The
            // provider mutates its own model state during the stream, so the
            // session and clients would otherwise keep showing the originally
            // requested model with a stale context-limit. Resync the session and
            // notify clients with a `ModelChanged` so the header, picker, and
            // context budget all reflect the model that actually served.
            let model_after_stream = self.provider.model();
            if model_after_stream != model_at_request_start {
                let provider_name = self.provider.display_name();
                logging::warn(&format!(
                    "Provider switched model mid-request: '{}' -> '{}' (resyncing session/UI)",
                    model_at_request_start, model_after_stream
                ));
                self.session.model = Some(self.persisted_serving_model(&model_after_stream));
                self.provider_runtime_state.apply(
                    crate::provider::ProviderStateEvent::RuntimeModelObserved {
                        model: model_after_stream.clone(),
                    },
                );
                self.persist_session_best_effort("model fallback");
                let _ = event_tx.send(ServerEvent::ModelChanged {
                    id: 0,
                    model: model_after_stream,
                    provider_name: Some(provider_name),
                    error: None,
                });
            }

            let had_tool_calls_before = !tool_calls.is_empty();
            self.recover_text_wrapped_tool_call(&mut text_content, &mut tool_calls);

            if !had_tool_calls_before
                && !tool_calls.is_empty()
                && let Some(tc) = tool_calls.last()
                && tc.id.starts_with("fallback_text_call_")
            {
                let _ = event_tx.send(ServerEvent::TextReplace {
                    text: text_content.clone(),
                });
                let _ = event_tx.send(ServerEvent::ToolStart {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                });
                tool_id_to_name.insert(tc.id.clone(), tc.name.clone());
                let _ = event_tx.send(ServerEvent::ToolInput {
                    delta: tc.input.to_string(),
                });
                let _ = event_tx.send(ServerEvent::ToolExec {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                });
            }

            // Add assistant message to history
            let mut content_blocks = Vec::new();
            if !text_content.is_empty() {
                content_blocks.push(ContentBlock::Text {
                    text: text_content.clone(),
                    cache_control: None,
                });
            }
            crate::message::push_reasoning_blocks(
                &mut content_blocks,
                &provider_name,
                &reasoning_content,
                Some(&reasoning_signature),
                store_reasoning_content,
            );
            if store_reasoning_content {
                content_blocks.extend(openai_reasoning_items.iter().cloned());
            }
            for tc in &tool_calls {
                content_blocks.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.input.clone(),
                    thought_signature: None,
                });
            }

            let assistant_message_id = if !content_blocks.is_empty() {
                crate::telemetry::record_assistant_response();
                let token_usage = Some(crate::session::StoredTokenUsage {
                    input_tokens: self.last_usage.input_tokens,
                    output_tokens: self.last_usage.output_tokens,
                    cache_read_input_tokens: self.last_usage.cache_read_input_tokens,
                    cache_creation_input_tokens: self.last_usage.cache_creation_input_tokens,
                });
                let message_id =
                    self.add_message_ext(Role::Assistant, content_blocks, None, token_usage);
                self.push_embedding_snapshot_if_semantic(&text_content);
                self.session.save()?;
                Some(message_id)
            } else {
                None
            };

            if let Some((encrypted_content, compacted_count)) = openai_native_compaction.take() {
                self.apply_openai_native_compaction(encrypted_content, compacted_count)?;
                // Native OpenAI compaction is applied after the provider stream,
                // so `messages_for_provider()` did not have an event to emit at
                // the top of this iteration. Notify clients now, before any
                // tool-driven continuation can enqueue its next KvCacheRequest.
                // The FIFO event ordering lets the TUI invalidate its old
                // append-only baseline before seeing the compacted signature.
                let _ = event_tx.send(ServerEvent::Compaction {
                    trigger: "openai_native".to_string(),
                    engine: Some("rolling".to_string()),
                    ownership: Some("provider_native".to_string()),
                    configured_route: None,
                    effective_route: Some(format!(
                        "{}:{}",
                        self.provider.name(),
                        self.provider.model()
                    )),
                    fallback_reason: None,
                    leaf_count: None,
                    parent_count: None,
                    frontier_size: None,
                    max_node_level: None,
                    graph_generation: None,
                    pre_tokens: usage_input,
                    post_tokens: None,
                    tokens_saved: None,
                    duration_ms: None,
                    messages_dropped: None,
                    messages_compacted: Some(compacted_count),
                    summary_chars: None,
                    active_messages: None,
                });
            }

            // If stop_reason indicates truncation (e.g. max_tokens), discard tool calls
            // with null/empty inputs since they were likely truncated mid-generation.
            self.filter_truncated_tool_calls(
                stop_reason.as_deref(),
                &mut tool_calls,
                assistant_message_id.as_ref(),
            );

            if tool_calls.is_empty() && !generated_image_contexts.is_empty() {
                for blocks in generated_image_contexts.drain(..) {
                    self.add_message(Role::User, blocks);
                }
                self.session.save()?;
                logging::info(
                    "Continuing turn so model can inspect generated image visual context",
                );
                continue;
            }

            // If no tool calls, check for soft interrupt or exit
            // NOTE: We only inject here (Point B) when there are no tools.
            // Injecting before tool_results would break the API requirement that
            // tool_use must be immediately followed by tool_result.
            if tool_calls.is_empty() {
                match self.handle_streaming_no_tool_calls(
                    stop_reason.as_deref(),
                    &mut incomplete_continuations,
                )? {
                    NoToolCallOutcome::Break => {
                        // Surface silent guardrail/refusal stops: the provider
                        // ended the turn with no visible output (e.g. Anthropic
                        // stop_reason "refusal", or a reasoning-only response).
                        // Only when the provider actually finished the message
                        // (saw_message_end) and the user did not cancel, so
                        // interrupted turns never show a spurious notice.
                        if saw_message_end
                            && !self.is_graceful_shutdown()
                            && let Some(notice) = Self::provider_guardrail_notice(
                                stop_reason.as_deref(),
                                text_content.trim().is_empty(),
                                !reasoning_content.trim().is_empty(),
                            )
                        {
                            logging::warn(&format!(
                                "PROVIDER_GUARDRAIL: turn ended with no visible output (stop_reason={:?}, reasoning_chars={})",
                                stop_reason,
                                reasoning_content.len()
                            ));
                            let _ = event_tx.send(ServerEvent::ProviderGuardrail {
                                stop_reason: stop_reason.clone(),
                                message: notice,
                            });
                        }
                        break;
                    }
                    NoToolCallOutcome::ContinueWithoutEvent => continue,
                    NoToolCallOutcome::ContinueWithSoftInterrupt { injected, point } => {
                        for event in Self::build_soft_interrupt_events(injected, point, None) {
                            let _ = event_tx.send(event);
                        }
                        continue;
                    }
                }
            }

            // If graceful shutdown was signaled during streaming and we have tool calls,
            // we need to provide tool results for them (API requires tool_use -> tool_result)
            // then exit cleanly
            if self.is_graceful_shutdown() {
                logging::info(&format!(
                    "Graceful shutdown - skipping {} tool call(s)",
                    tool_calls.len()
                ));
                for tc in &tool_calls {
                    self.add_message(
                        Role::User,
                        vec![ContentBlock::ToolResult {
                            tool_use_id: tc.id.clone(),
                            content: "[Skipped - server reloading]".to_string(),
                            is_error: Some(true),
                        }],
                    );
                }
                self.session.save()?;
                break;
            }

            logging::info(&format!(
                "Turn has {} tool calls to execute",
                tool_calls.len()
            ));

            if self.provider.handles_tools_internally() {
                tool_calls.retain(|tc| JCODE_NATIVE_TOOLS.contains(&tc.name.as_str()));
                if tool_calls.is_empty() {
                    // === INJECTION POINT D: After provider-handled tools, before next API call ===
                    let injected = self.inject_soft_interrupts();
                    if !injected.is_empty() {
                        for event in Self::build_soft_interrupt_events(injected, "D", None) {
                            let _ = event_tx.send(event);
                        }
                        // Don't break - continue loop to process injected message
                        continue;
                    }
                    break;
                }
            }

            let mut tool_results_dirty = false;
            include!("tool_execution.rs");

            if tool_results_dirty {
                self.session.save()?;
            }

            if !generated_image_contexts.is_empty() {
                for blocks in generated_image_contexts.drain(..) {
                    self.add_message(Role::User, blocks);
                }
                self.session.save()?;
            }

            // === INJECTION POINT D: All tools done, before next API call ===
            // This is the safest point for non-urgent injection since all tool_results
            // have been added and the conversation is in a valid state.
            if let PostToolInterruptOutcome::SoftInterrupt { injected, point } =
                self.take_post_tool_soft_interrupt()
            {
                for event in Self::build_soft_interrupt_events(injected, point, None) {
                    let _ = event_tx.send(event);
                }
            }
        }

        Ok(())
    }
}
