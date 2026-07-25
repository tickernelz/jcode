use super::*;

impl App {
    /// Process all queued messages (combined into a single request)
    /// Loops until queue is empty (in case more messages are queued during processing)
    pub(in super::super) async fn process_queued_messages(
        &mut self,
        terminal: &mut DefaultTerminal,
        event_stream: &mut EventStream,
    ) {
        while !self.queued_messages.is_empty() || !self.hidden_queued_system_messages.is_empty() {
            if let Err(error) = self.reconcile_local_account_transition_for_admission() {
                self.push_display_message(DisplayMessage::error(format!(
                    "Cannot start queued model turn while account identity is reconciling: {error}"
                )));
                self.set_status_notice("Account identity reconciliation required");
                return;
            }
            // Combine all currently queued messages into one, treating [SYSTEM: ...]
            // startup continuations as system reminders rather than user turns.
            let queued_messages = std::mem::take(&mut self.queued_messages);
            let hidden_reminders = std::mem::take(&mut self.hidden_queued_system_messages);
            let (messages, reminder, display_system_messages) =
                super::super::helpers::partition_queued_messages(queued_messages, hidden_reminders);
            let combined = messages.join("\n\n");
            let has_combined = !combined.is_empty();
            let preserve_visible_turn =
                super::super::commands::queued_messages_are_only_pokes(&messages);

            self.commit_pending_streaming_assistant_message();

            for msg in display_system_messages {
                self.push_display_message(DisplayMessage::system(msg));
            }

            for msg in &messages {
                if !super::super::commands::is_poke_message(msg) {
                    self.push_display_message(DisplayMessage::user(msg.clone()));
                }
            }

            self.current_turn_system_reminder =
                merge_turn_reminders(reminder, mission_turn_reminder(&self.session.id));

            if has_combined {
                self.add_provider_message(Message::user(&combined));
                self.session.add_message(
                    Role::User,
                    vec![ContentBlock::Text {
                        text: combined.clone(),
                        cache_control: None,
                    }],
                );
            }
            self.session_save_pending = true;
            self.clear_streaming_render_state();
            self.stream_buffer.clear();
            self.thought_line_inserted = false;
            self.thinking_prefix_emitted = false;
            self.thinking_buffer.clear();
            self.streaming_tool_calls.clear();
            self.streaming.streaming_input_tokens = 0;
            self.streaming.streaming_output_tokens = 0;
            self.streaming.streaming_cache_read_tokens = None;
            self.streaming.streaming_cache_creation_tokens = None;
            self.kv_cache.current_api_usage_recorded = false;
            self.upstream_provider = None;
            self.status_detail = None;
            self.streaming.streaming_tps_start = None;
            self.streaming.streaming_tps_elapsed = Duration::ZERO;
            self.streaming.streaming_tps_collect_output = false;
            self.streaming.streaming_total_output_tokens = 0;
            self.streaming.streaming_tps_observed_output_tokens = 0;
            self.streaming.streaming_tps_observed_elapsed = Duration::ZERO;
            self.processing_started = Some(Instant::now());
            if has_combined {
                if preserve_visible_turn {
                    self.visible_turn_started.get_or_insert_with(Instant::now);
                } else {
                    self.visible_turn_started = Some(Instant::now());
                }
            }
            self.is_processing = true;
            self.status = ProcessingStatus::Sending;

            match self
                .run_turn_interactive(terminal, event_stream, None)
                .await
            {
                Ok(()) => {
                    self.last_stream_error = None;
                    self.last_submitted_input = None;
                }
                Err(e) => {
                    let err_str = crate::util::format_error_chain(&e);
                    if is_request_payload_too_large_error(&err_str) {
                        if !self
                            .try_recover_payload_too_large_and_retry(terminal, event_stream)
                            .await
                        {
                            self.handle_turn_error(err_str);
                        }
                    } else if is_context_limit_error(&err_str) {
                        if self
                            .try_auto_compact_and_retry(terminal, event_stream)
                            .await
                        {
                            // Successfully recovered
                        } else {
                            self.handle_turn_error(err_str);
                        }
                    } else {
                        self.handle_turn_error(err_str);
                    }
                }
            }
            self.current_turn_system_reminder = None;
            // Loop will check if more messages were queued during this turn
        }
    }
}
