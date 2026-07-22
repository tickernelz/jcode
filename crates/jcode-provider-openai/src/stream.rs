use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use bytes::Bytes;
use futures::Stream;
use jcode_message_types::{StreamEvent, sanitize_tool_id};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context as TaskContext, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

const WEBSOCKET_FALLBACK_NOTICE: &str = "falling back from websockets to https transport";
const STREAMED_SUMMARY_MARKER: &str = "\0jcode_streamed_summary";
static FALLBACK_TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(1);
static RECOVERED_TEXT_WRAPPED_TOOL_CALLS: AtomicU64 = AtomicU64::new(0);
static NORMALIZED_NULL_TOOL_ARGUMENTS: AtomicU64 = AtomicU64::new(0);

fn truncated_stream_payload_context(data: &str) -> String {
    jcode_core::util::truncate_str(&data.trim().replace("\n", "\\n"), 240).to_string()
}

fn is_structured_response_event(data: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    let Some(kind) = value.get("type").and_then(|kind| kind.as_str()) else {
        return false;
    };
    kind.starts_with("response.") || kind == "error"
}

fn is_websocket_fallback_notice(data: &str) -> bool {
    // The proxy injects the fallback notice as a plain-text control frame, not a
    // structured Responses API event. A legitimate `response.*`/`error` event can
    // contain this phrase in model output or tool-call arguments and must still be
    // parsed normally.
    if is_structured_response_event(data) {
        return false;
    }
    data.to_lowercase().contains(WEBSOCKET_FALLBACK_NOTICE)
}

fn extract_error_with_retry(
    response: &Option<Value>,
    top_level_error: &Option<Value>,
) -> (String, Option<u64>) {
    let error = response
        .as_ref()
        .and_then(|r| r.get("error"))
        .or(top_level_error.as_ref());

    let error = match error {
        Some(e) => e,
        None => {
            if let Some(resp) = response.as_ref()
                && let Some(msg) = resp
                    .get("status_message")
                    .or_else(|| resp.get("message"))
                    .and_then(|v| v.as_str())
            {
                return (msg.to_string(), None);
            }
            return (
                "OpenAI response stream error (no error details)".to_string(),
                None,
            );
        }
    };

    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("OpenAI response stream error (unknown)")
        .to_string();
    let error_type = error.get("type").and_then(|v| v.as_str());
    let code = error.get("code").and_then(|v| v.as_str());

    let message_lower = message.to_lowercase();
    let message = match (error_type, code) {
        (Some(error_type), Some(code))
            if !message_lower.contains(&error_type.to_lowercase())
                && !message_lower.contains(&code.to_lowercase()) =>
        {
            format!("{} ({}): {}", error_type, code, message)
        }
        (Some(error_type), _) if !message_lower.contains(&error_type.to_lowercase()) => {
            format!("{}: {}", error_type, message)
        }
        (_, Some(code)) if !message_lower.contains(&code.to_lowercase()) => {
            format!("{}: {}", code, message)
        }
        _ => message,
    };

    let retry_after = error
        .get("retry_after")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            response
                .as_ref()
                .and_then(|r| r.get("retry_after"))
                .and_then(|v| v.as_u64())
        });

    (message, retry_after)
}
pub fn parse_text_wrapped_tool_call(text: &str) -> Option<(String, String, String, String)> {
    let marker = "to=functions.";
    let marker_idx = text.find(marker)?;
    let after_marker = &text[marker_idx + marker.len()..];

    let mut tool_name_end = 0usize;
    for (idx, ch) in after_marker.char_indices() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            tool_name_end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    if tool_name_end == 0 {
        return None;
    }

    let tool_name = after_marker[..tool_name_end].to_string();
    let remaining = &after_marker[tool_name_end..];
    let mut fallback: Option<(String, String, String, String)> = None;
    for (brace_idx, ch) in remaining.char_indices() {
        if ch != '{' {
            continue;
        }
        let slice = &remaining[brace_idx..];
        let mut stream = serde_json::Deserializer::from_str(slice).into_iter::<Value>();
        let parsed = match stream.next() {
            Some(Ok(value)) => value,
            Some(Err(_)) => continue,
            None => continue,
        };
        let consumed = stream.byte_offset();
        if !parsed.is_object() {
            continue;
        }

        let prefix = text[..marker_idx].trim_end().to_string();
        let suffix = remaining[brace_idx + consumed..].trim().to_string();
        let args = serde_json::to_string(&parsed).ok()?;
        if suffix.is_empty() {
            return Some((prefix, tool_name.clone(), args, suffix));
        }
        if fallback.is_none() {
            fallback = Some((prefix, tool_name.clone(), args, suffix));
        }
    }

    fallback
}

fn stream_text_or_recovered_tool_call(
    text: &str,
    pending: &mut VecDeque<StreamEvent>,
) -> Option<StreamEvent> {
    if text.is_empty() {
        return None;
    }

    if let Some((prefix, tool_name, arguments, suffix)) = parse_text_wrapped_tool_call(text) {
        let total = RECOVERED_TEXT_WRAPPED_TOOL_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
        jcode_logging::warn(&format!(
            "[openai] Recovered text-wrapped tool call for '{}' (total={})",
            tool_name, total
        ));
        let suffix = sanitize_recovered_tool_suffix(&suffix);
        if !prefix.is_empty() {
            pending.push_back(StreamEvent::TextDelta(prefix));
        }
        pending.push_back(StreamEvent::ToolUseStart {
            id: format!(
                "fallback_text_call_{}",
                FALLBACK_TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
            ),
            name: tool_name,
        });
        pending.push_back(StreamEvent::ToolInputDelta(arguments));
        pending.push_back(StreamEvent::ToolUseEnd);
        if !suffix.is_empty() {
            pending.push_back(StreamEvent::TextDelta(suffix));
        }
        return pending.pop_front();
    }

    Some(StreamEvent::TextDelta(text.to_string()))
}

fn sanitize_recovered_tool_suffix(suffix: &str) -> String {
    let trimmed = suffix.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let normalized = trimmed.trim_start_matches('"');

    if normalized.starts_with(",\"item_id\"")
        || normalized.starts_with(",\"output_index\"")
        || normalized.starts_with(",\"sequence_number\"")
        || normalized.starts_with(",\"call_id\"")
        || normalized.starts_with(",\"type\":\"response.")
        || (normalized.starts_with(',')
            && normalized.contains("\"item_id\"")
            && (normalized.contains("\"output_index\"")
                || normalized.contains("\"sequence_number\"")))
    {
        return String::new();
    }

    suffix.to_string()
}

#[derive(Deserialize, Debug)]
struct ResponseSseEvent {
    #[serde(rename = "type")]
    kind: String,
    item: Option<Value>,
    delta: Option<String>,
    item_id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
    response: Option<Value>,
    error: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct StreamingResponseItemState {
    call_id: Option<String>,
    name: Option<String>,
    arguments: String,
    summary_streamed: bool,
    summary_part_done: bool,
    thinking_started: bool,
    last_reasoning_item_id: Option<String>,
}

pub type StreamingToolCallState = StreamingResponseItemState;

fn normalize_openai_tool_arguments(raw_arguments: String) -> String {
    let trimmed = raw_arguments.trim();
    if trimmed.is_empty() || trimmed == "null" {
        let total = NORMALIZED_NULL_TOOL_ARGUMENTS.fetch_add(1, Ordering::Relaxed) + 1;
        jcode_logging::warn(&format!(
            "[openai] Normalized empty/null tool arguments to empty object (total={})",
            total
        ));
        "{}".to_string()
    } else {
        raw_arguments
    }
}

fn streaming_tool_item_id(item: &Value) -> Option<String> {
    item.get("id")
        .and_then(|v| v.as_str())
        .or_else(|| item.get("item_id").and_then(|v| v.as_str()))
        .map(|id| id.to_string())
}

fn stream_tool_call_from_state(
    item_id: Option<String>,
    mut state: StreamingResponseItemState,
    pending: &mut VecDeque<StreamEvent>,
) -> Option<StreamEvent> {
    let tool_name = state.name.take().filter(|name| !name.is_empty())?;
    let raw_call_id = state
        .call_id
        .take()
        .filter(|id| !id.is_empty())
        .or(item_id)
        .unwrap_or_else(|| {
            format!(
                "fallback_text_call_{}",
                FALLBACK_TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed)
            )
        });
    let call_id = sanitize_tool_id(&raw_call_id);
    let arguments = normalize_openai_tool_arguments(if state.arguments.is_empty() {
        "{}".to_string()
    } else {
        state.arguments
    });

    pending.push_back(StreamEvent::ToolUseStart {
        id: call_id,
        name: tool_name,
    });
    pending.push_back(StreamEvent::ToolInputDelta(arguments));
    pending.push_back(StreamEvent::ToolUseEnd);
    pending.pop_front()
}

pub fn parse_openai_response_event(
    data: &str,
    saw_text_delta: &mut bool,
    streaming_items: &mut HashMap<String, StreamingResponseItemState>,
    completed_tool_items: &mut HashSet<String>,
    pending: &mut VecDeque<StreamEvent>,
) -> Option<StreamEvent> {
    if data == "[DONE]" {
        streaming_items.clear();
        completed_tool_items.clear();
        return Some(StreamEvent::MessageEnd { stop_reason: None });
    }

    if is_websocket_fallback_notice(data) {
        jcode_logging::warn(&format!("OpenAI stream transport notice: {}", data.trim()));
        return None;
    }

    if data
        .to_lowercase()
        .contains("stream disconnected before completion")
        && !is_structured_response_event(data)
    {
        return Some(StreamEvent::Error {
            message: data.to_string(),
            retry_after_secs: None,
        });
    }

    let event: ResponseSseEvent = match serde_json::from_str(data) {
        Ok(parsed) => parsed,
        Err(error) => {
            jcode_logging::warn(&format!(
                "OpenAI SSE JSON parse failed: {} payload={}",
                error,
                truncated_stream_payload_context(data)
            ));
            return None;
        }
    };

    match event.kind.as_str() {
        "response.output_text.delta" => {
            if let Some(delta) = event.delta {
                *saw_text_delta = true;
                return stream_text_or_recovered_tool_call(&delta, pending);
            }
        }
        "response.reasoning.delta" | "response.reasoning_summary_text.delta" => {
            if let Some(mut delta) = event.delta {
                if let Some(item_id) = event.item_id
                    && !delta.is_empty()
                {
                    let separate_items = streaming_items
                        .get(STREAMED_SUMMARY_MARKER)
                        .and_then(|state| state.last_reasoning_item_id.as_deref())
                        .is_some_and(|last_item_id| last_item_id != item_id);
                    let state = streaming_items.entry(item_id.clone()).or_default();
                    if separate_items || (state.summary_streamed && state.summary_part_done) {
                        delta.insert_str(0, "\n\n");
                    }
                    state.summary_streamed = true;
                    state.summary_part_done = false;

                    let stream_state = streaming_items
                        .entry(STREAMED_SUMMARY_MARKER.to_string())
                        .or_default();
                    stream_state.last_reasoning_item_id = Some(item_id);
                }
                return Some(StreamEvent::ThinkingDelta(delta));
            }
        }
        "response.reasoning_summary_part.done" => {
            if let Some(item_id) = event.item_id
                && let Some(state) = streaming_items.get_mut(&item_id)
                && state.summary_streamed
            {
                state.summary_part_done = true;
            }
        }
        "response.reasoning.done" | "response.output_item.added" => {
            if let Some(item) = &event.item {
                if item.get("type").and_then(|v| v.as_str()) == Some("reasoning") {
                    if let Some(item_id) = streaming_tool_item_id(item) {
                        let state = streaming_items.entry(item_id).or_default();
                        state.thinking_started = true;
                    }

                    let stream_state = streaming_items
                        .entry(STREAMED_SUMMARY_MARKER.to_string())
                        .or_default();
                    if stream_state.thinking_started {
                        return None;
                    }
                    stream_state.thinking_started = true;
                    return Some(StreamEvent::ThinkingStart);
                }
                if matches!(
                    item.get("type").and_then(|v| v.as_str()),
                    Some("function_call") | Some("custom_tool_call")
                ) && let Some(item_id) = streaming_tool_item_id(item)
                {
                    let state = streaming_items.entry(item_id).or_default();
                    state.call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| state.call_id.clone());
                    state.name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| state.name.clone());
                    if let Some(arguments) = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("input").and_then(|v| v.as_str()))
                    {
                        state.arguments = arguments.to_string();
                    } else if let Some(input) = item.get("input")
                        && (input.is_object() || input.is_array())
                    {
                        state.arguments = input.to_string();
                    }
                }
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(item_id) = event.item_id {
                let state = streaming_items.entry(item_id).or_default();
                if let Some(call_id) = event.call_id {
                    state.call_id = Some(call_id);
                }
                if let Some(name) = event.name {
                    state.name = Some(name);
                }
                if let Some(delta) = event.delta {
                    state.arguments.push_str(&delta);
                }
            }
        }
        "response.function_call_arguments.done" => {
            if let Some(item_id) = event.item_id {
                let mut state = streaming_items.remove(&item_id).unwrap_or_default();
                if let Some(call_id) = event.call_id {
                    state.call_id = Some(call_id);
                }
                if let Some(name) = event.name {
                    state.name = Some(name);
                }
                if let Some(arguments) = event.arguments {
                    state.arguments = arguments;
                }
                if let Some(tool_event) =
                    stream_tool_call_from_state(Some(item_id.clone()), state.clone(), pending)
                {
                    completed_tool_items.insert(item_id);
                    return Some(tool_event);
                }
                streaming_items.insert(item_id, state);
            }
        }
        "response.output_item.done" => {
            if let Some(item) = event.item {
                let item_id = streaming_tool_item_id(&item);
                if let Some(item_id) = item_id.as_ref()
                    && completed_tool_items.contains(item_id)
                    && matches!(
                        item.get("type").and_then(|v| v.as_str()),
                        Some("function_call") | Some("custom_tool_call")
                    )
                {
                    completed_tool_items.remove(item_id);
                    return None;
                }
                let state = item_id
                    .as_ref()
                    .and_then(|item_id| streaming_items.remove(item_id))
                    .unwrap_or_default();
                let is_reasoning_item =
                    item.get("type").and_then(|value| value.as_str()) == Some("reasoning");
                let has_final_summary = is_reasoning_item
                    && item
                        .get("summary")
                        .and_then(|value| value.as_array())
                        .is_some_and(|summary| {
                            summary.iter().any(|part| {
                                part.get("type").and_then(|value| value.as_str())
                                    == Some("summary_text")
                                    && part
                                        .get("text")
                                        .and_then(|value| value.as_str())
                                        .is_some_and(|text| !text.is_empty())
                            })
                        });
                let stream_thinking_started = streaming_items
                    .get(STREAMED_SUMMARY_MARKER)
                    .is_some_and(|state| state.thinking_started);
                let separate_summary = !state.summary_streamed
                    && has_final_summary
                    && streaming_items
                        .get(STREAMED_SUMMARY_MARKER)
                        .and_then(|state| state.last_reasoning_item_id.as_deref())
                        .is_some_and(|last_item_id| item_id.as_deref() != Some(last_item_id));
                let other_reasoning_item_active = is_reasoning_item
                    && streaming_items.iter().any(|(item_id, state)| {
                        item_id != STREAMED_SUMMARY_MARKER
                            && (state.thinking_started || state.summary_streamed)
                    });
                let end_thinking = is_reasoning_item
                    && (stream_thinking_started || (has_final_summary && !state.summary_streamed))
                    && !other_reasoning_item_active;

                if is_reasoning_item {
                    let stream_state = streaming_items
                        .entry(STREAMED_SUMMARY_MARKER.to_string())
                        .or_default();
                    if has_final_summary && !state.summary_streamed && item_id.is_some() {
                        stream_state.last_reasoning_item_id = item_id.clone();
                    }
                    if end_thinking {
                        stream_state.thinking_started = false;
                    }
                }
                if streaming_items
                    .get(STREAMED_SUMMARY_MARKER)
                    .is_some_and(|state| {
                        !state.thinking_started && state.last_reasoning_item_id.is_none()
                    })
                {
                    streaming_items.remove(STREAMED_SUMMARY_MARKER);
                }
                let output_event = handle_openai_output_item_with_summary_state(
                    item,
                    saw_text_delta,
                    pending,
                    state.summary_streamed,
                    stream_thinking_started,
                    end_thinking,
                    separate_summary,
                );
                if let Some(event) = output_event {
                    return Some(event);
                }
            }
        }
        "response.incomplete" => {
            streaming_items.clear();
            completed_tool_items.clear();
            let stop_reason = event
                .response
                .as_ref()
                .and_then(extract_stop_reason_from_response)
                .or_else(|| Some("incomplete".to_string()));
            if let Some(response) = event.response
                && let Some(usage_event) = extract_usage_from_response(&response)
            {
                pending.push_back(usage_event);
            }
            pending.push_back(StreamEvent::MessageEnd { stop_reason });
            return pending.pop_front();
        }
        "response.completed" => {
            streaming_items.clear();
            completed_tool_items.clear();
            let stop_reason = event
                .response
                .as_ref()
                .and_then(extract_stop_reason_from_response);
            if let Some(response) = event.response
                && let Some(usage_event) = extract_usage_from_response(&response)
            {
                pending.push_back(usage_event);
            }
            pending.push_back(StreamEvent::MessageEnd { stop_reason });
            return pending.pop_front();
        }
        "response.failed" | "response.error" | "error" => {
            streaming_items.clear();
            completed_tool_items.clear();
            jcode_logging::warn(&format!(
                "OpenAI stream error event (type={}): response={:?}, error={:?}",
                event.kind, event.response, event.error
            ));
            let (message, retry_after_secs) =
                extract_error_with_retry(&event.response, &event.error);
            return Some(StreamEvent::Error {
                message,
                retry_after_secs,
            });
        }
        _ => {}
    }

    None
}

fn extract_last_assistant_message_phase(response: &Value) -> Option<String> {
    let output = response.get("output")?.as_array()?;
    output.iter().rev().find_map(|item| {
        if item.get("type").and_then(|v| v.as_str()) != Some("message") {
            return None;
        }
        if item.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            return None;
        }
        item.get("phase")
            .and_then(|v| v.as_str())
            .map(|phase| phase.to_string())
    })
}

fn extract_stop_reason_from_response(response: &Value) -> Option<String> {
    let status = response.get("status").and_then(|v| v.as_str());
    if status == Some("completed") {
        if extract_last_assistant_message_phase(response).as_deref() == Some("commentary") {
            return Some("commentary".to_string());
        }
        return None;
    }

    let incomplete_reason = response
        .get("incomplete_details")
        .and_then(|v| v.get("reason"))
        .and_then(|v| v.as_str());

    if let Some(reason) = incomplete_reason {
        return Some(reason.to_string());
    }

    status
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

pub fn handle_openai_output_item(
    item: Value,
    saw_text_delta: &mut bool,
    pending: &mut VecDeque<StreamEvent>,
) -> Option<StreamEvent> {
    handle_openai_output_item_with_summary_state(
        item,
        saw_text_delta,
        pending,
        false,
        false,
        true,
        false,
    )
}

fn handle_openai_output_item_with_summary_state(
    item: Value,
    saw_text_delta: &mut bool,
    pending: &mut VecDeque<StreamEvent>,
    summary_streamed: bool,
    thinking_started: bool,
    end_thinking: bool,
    separate_summary: bool,
) -> Option<StreamEvent> {
    let item_type = item.get("type")?.as_str()?;
    match item_type {
        "compaction" => {
            let encrypted_content = item
                .get("encrypted_content")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string())?;
            return Some(StreamEvent::Compaction {
                trigger: "openai_native_auto".to_string(),
                pre_tokens: None,
                openai_encrypted_content: Some(encrypted_content),
            });
        }
        "function_call" | "custom_tool_call" => {
            let call_id = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let raw_arguments = item
                .get("arguments")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .or_else(|| {
                    item.get("input").and_then(|v| {
                        if v.is_object() || v.is_array() {
                            Some(v.to_string())
                        } else {
                            v.as_str().map(|s| s.to_string())
                        }
                    })
                })
                .unwrap_or_else(|| "{}".to_string());
            let arguments = normalize_openai_tool_arguments(raw_arguments);

            pending.push_back(StreamEvent::ToolUseStart {
                id: call_id.clone(),
                name,
            });
            pending.push_back(StreamEvent::ToolInputDelta(arguments));
            pending.push_back(StreamEvent::ToolUseEnd);
            return pending.pop_front();
        }
        "image_generation_call" => {
            if let Some(event) = handle_openai_image_generation_item(&item, pending) {
                return Some(event);
            }
        }
        "message" => {
            if *saw_text_delta {
                return None;
            }
            let mut text = String::new();
            if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                for entry in content {
                    let entry_type = entry.get("type").and_then(|v| v.as_str());
                    if matches!(entry_type, Some("output_text") | Some("text"))
                        && let Some(t) = entry.get("text").and_then(|v| v.as_str())
                    {
                        text.push_str(t);
                    }
                }
            }
            return stream_text_or_recovered_tool_call(&text, pending);
        }
        "reasoning" => {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let mut summary = Vec::new();
            if let Some(summary_arr) = item.get("summary").and_then(|v| v.as_array()) {
                for summary_item in summary_arr {
                    if summary_item.get("type").and_then(|v| v.as_str()) == Some("summary_text")
                        && let Some(text) = summary_item.get("text").and_then(|v| v.as_str())
                    {
                        summary.push(text.to_string());
                    }
                }
            }
            let encrypted_content = item
                .get("encrypted_content")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string());
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .map(|value| value.to_string());

            if !id.is_empty() && (encrypted_content.is_some() || !summary.is_empty()) {
                pending.push_back(StreamEvent::OpenAIReasoning {
                    id,
                    summary: summary.clone(),
                    encrypted_content,
                    status,
                });
            }

            if summary_streamed {
                if end_thinking {
                    pending.push_back(StreamEvent::ThinkingEnd);
                }
                return pending.pop_front();
            }

            if !summary.is_empty() {
                if !thinking_started {
                    pending.push_back(StreamEvent::ThinkingStart);
                }
                let mut rendered_summary = summary.join("\n\n");
                if separate_summary {
                    rendered_summary.insert_str(0, "\n\n");
                }
                pending.push_back(StreamEvent::ThinkingDelta(rendered_summary));
                if end_thinking {
                    pending.push_back(StreamEvent::ThinkingEnd);
                }
                return pending.pop_front();
            }
            if thinking_started && end_thinking {
                pending.push_back(StreamEvent::ThinkingEnd);
            }
            return pending.pop_front();
        }
        _ => {}
    }

    None
}

fn handle_openai_image_generation_item(
    item: &Value,
    pending: &mut VecDeque<StreamEvent>,
) -> Option<StreamEvent> {
    let result_b64 = item.get("result")?.as_str()?;
    if result_b64.is_empty() {
        return None;
    }

    let image_bytes = match BASE64_STANDARD.decode(result_b64) {
        Ok(bytes) => bytes,
        Err(err) => {
            jcode_logging::warn(&format!(
                "OpenAI image_generation_call returned invalid base64: {}",
                err
            ));
            return Some(StreamEvent::TextDelta(
                "\n[Generated image received, but Jcode could not decode it.]\n".to_string(),
            ));
        }
    };

    let output_format = item
        .get("output_format")
        .and_then(|v| v.as_str())
        .unwrap_or("png");
    let extension = match output_format {
        "jpeg" | "jpg" => "jpg",
        "webp" => "webp",
        _ => "png",
    };
    let item_id = item.get("id").and_then(|v| v.as_str()).unwrap_or("image");
    let safe_id: String = item_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .take(80)
        .collect();
    let safe_id = if safe_id.is_empty() {
        "image".to_string()
    } else {
        safe_id
    };
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let dir = std::env::current_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(".jcode")
        .join("generated-images");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        jcode_logging::warn(&format!(
            "Failed to create OpenAI generated image directory: {}",
            err
        ));
        return Some(StreamEvent::TextDelta(format!(
            "\n[Generated image received ({} bytes), but Jcode could not save it.]\n",
            image_bytes.len()
        )));
    }

    let filename = format!("{}-{}.{}", timestamp_ms, safe_id, extension);
    let path = dir.join(filename);
    if let Err(err) = std::fs::write(&path, image_bytes) {
        jcode_logging::warn(&format!("Failed to save OpenAI generated image: {}", err));
        return Some(StreamEvent::TextDelta(
            "\n[Generated image received, but Jcode could not save it.]\n".to_string(),
        ));
    }

    let metadata_path = path.with_extension("json");
    let mut response_item = item.clone();
    if let Some(object) = response_item.as_object_mut() {
        object.remove("result");
    }
    let revised_prompt = item
        .get("revised_prompt")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let metadata = serde_json::json!({
        "schema_version": 1,
        "provider": "openai",
        "native_tool": "image_generation",
        "id": item_id,
        "status": item.get("status").and_then(|v| v.as_str()),
        "created_at_unix_ms": timestamp_ms,
        "image_path": path.display().to_string(),
        "output_format": output_format,
        "byte_count": std::fs::metadata(&path).map(|m| m.len()).unwrap_or_default(),
        "revised_prompt": revised_prompt,
        "response_item": response_item,
    });
    let metadata_path_string = match serde_json::to_vec_pretty(&metadata).ok().and_then(|bytes| {
        std::fs::write(&metadata_path, bytes)
            .ok()
            .map(|_| metadata_path.clone())
    }) {
        Some(path) => Some(path.display().to_string()),
        None => {
            jcode_logging::warn("Failed to save OpenAI generated image metadata");
            None
        }
    };

    let mut markdown = format!(
        "\n![Generated image]({})\n\nGenerated image saved to `{}`.",
        path.display(),
        path.display()
    );
    if let Some(metadata_path) = metadata_path_string.as_deref() {
        markdown.push_str(&format!("\nMetadata saved to `{}`.", metadata_path));
    }
    markdown.push('\n');

    pending.push_back(StreamEvent::TextDelta(markdown));

    Some(StreamEvent::GeneratedImage {
        id: item_id.to_string(),
        path: path.display().to_string(),
        metadata_path: metadata_path_string,
        output_format: output_format.to_string(),
        revised_prompt,
    })
}

pub struct OpenAIResponsesStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buffer: String,
    pending: VecDeque<StreamEvent>,
    saw_text_delta: bool,
    streaming_items: HashMap<String, StreamingResponseItemState>,
    completed_tool_items: HashSet<String>,
}

impl OpenAIResponsesStream {
    pub fn new(stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static) -> Self {
        Self {
            inner: Box::pin(stream),
            buffer: String::new(),
            pending: VecDeque::new(),
            saw_text_delta: false,
            streaming_items: HashMap::new(),
            completed_tool_items: HashSet::new(),
        }
    }

    fn parse_next_event(&mut self) -> Option<StreamEvent> {
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }

        while let Some(pos) = self.buffer.find("\n\n") {
            let event_str = self.buffer[..pos].to_string();
            self.buffer = self.buffer[pos + 2..].to_string();

            let mut data_lines = Vec::new();
            for line in event_str.lines() {
                if let Some(data) = jcode_core::util::sse_data_line(line) {
                    data_lines.push(data);
                }
            }

            if data_lines.is_empty() {
                continue;
            }

            let data = data_lines.join("\n");
            if let Some(event) = parse_openai_response_event(
                &data,
                &mut self.saw_text_delta,
                &mut self.streaming_items,
                &mut self.completed_tool_items,
                &mut self.pending,
            ) {
                return Some(event);
            }
        }

        None
    }
}

fn extract_cached_input_tokens(usage: &Value) -> Option<u64> {
    usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(|v| v.as_u64())
}

fn extract_usage_from_response(response: &Value) -> Option<StreamEvent> {
    let usage = response.get("usage")?;
    let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64());
    let output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64());
    let cache_read_input_tokens = extract_cached_input_tokens(usage);
    if input_tokens.is_some() || output_tokens.is_some() || cache_read_input_tokens.is_some() {
        Some(StreamEvent::TokenUsage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens: None,
        })
    } else {
        None
    }
}

impl Stream for OpenAIResponsesStream {
    type Item = Result<StreamEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(event) = self.parse_next_event() {
                return Poll::Ready(Some(Ok(event)));
            }

            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        self.buffer.push_str(text);
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(anyhow::anyhow!("Stream error: {}", e))));
                }
                Poll::Ready(None) => {
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_events(
        data: &[&str],
    ) -> (
        Vec<StreamEvent>,
        HashMap<String, StreamingResponseItemState>,
    ) {
        let mut saw_text_delta = false;
        let mut streaming_items = HashMap::new();
        let mut completed_tool_items = HashSet::new();
        let mut pending = VecDeque::new();
        let mut events = Vec::new();

        for data in data {
            if let Some(event) = parse_openai_response_event(
                data,
                &mut saw_text_delta,
                &mut streaming_items,
                &mut completed_tool_items,
                &mut pending,
            ) {
                events.push(event);
            }
            events.extend(pending.drain(..));
        }

        (events, streaming_items)
    }

    #[test]
    fn streamed_reasoning_summary_is_not_repeated_by_output_item_done() {
        let (events, streaming_items) = parse_events(&[
            r#"{"type":"response.output_item.added","item":{"id":"rs_test","type":"reasoning"}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_test","delta":"Checked constraints."}"#,
            r#"{"type":"response.reasoning_summary_part.done","item_id":"rs_test","part":{"type":"summary_text","text":"Checked constraints."}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_test","delta":"Selected fix."}"#,
            r#"{"type":"response.output_item.done","item":{"id":"rs_test","type":"reasoning","status":"completed","encrypted_content":"enc_test","summary":[{"type":"summary_text","text":"Checked constraints."},{"type":"summary_text","text":"Selected fix."}]}}"#,
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
        ]);

        assert!(matches!(events.first(), Some(StreamEvent::ThinkingStart)));
        let rendered: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(rendered, "Checked constraints.\n\nSelected fix.");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ThinkingStart))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ThinkingEnd))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::OpenAIReasoning {
                id,
                summary,
                encrypted_content: Some(encrypted_content),
                status: Some(status),
            } if id == "rs_test"
                && summary == &["Checked constraints.".to_string(), "Selected fix.".to_string()]
                && encrypted_content == "enc_test"
                && status == "completed"
        )));
        assert!(streaming_items.is_empty());
    }

    #[test]
    fn consecutive_reasoning_items_have_a_readable_separator() {
        let (events, streaming_items) = parse_events(&[
            r#"{"type":"response.output_item.added","item":{"id":"rs_one","type":"reasoning"}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_one","delta":"First item."}"#,
            r#"{"type":"response.output_item.done","item":{"id":"rs_one","type":"reasoning","encrypted_content":"enc_one","summary":[{"type":"summary_text","text":"First item."}]}}"#,
            r#"{"type":"response.output_item.added","item":{"id":"rs_two","type":"reasoning"}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_two","delta":"Second item."}"#,
            r#"{"type":"response.output_item.done","item":{"id":"rs_two","type":"reasoning","encrypted_content":"enc_two","summary":[{"type":"summary_text","text":"Second item."}]}}"#,
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
        ]);

        let rendered: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(rendered, "First item.\n\nSecond item.");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::OpenAIReasoning { .. }))
                .count(),
            2
        );
        assert!(streaming_items.is_empty());
    }

    #[test]
    fn interleaved_reasoning_items_share_one_lifecycle_and_keep_boundaries() {
        let (events, streaming_items) = parse_events(&[
            r#"{"type":"response.output_item.added","item":{"id":"rs_a","type":"reasoning"}}"#,
            r#"{"type":"response.output_item.added","item":{"id":"rs_b","type":"reasoning"}}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_a","delta":"A1"}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_b","delta":"B1"}"#,
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs_a","delta":"A2"}"#,
            r#"{"type":"response.output_item.done","item":{"id":"rs_a","type":"reasoning","summary":[{"type":"summary_text","text":"A1A2"}]}}"#,
            r#"{"type":"response.output_item.done","item":{"id":"rs_b","type":"reasoning","summary":[{"type":"summary_text","text":"B1"}]}}"#,
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
        ]);

        let rendered: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(rendered, "A1\n\nB1\n\nA2");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ThinkingStart))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ThinkingEnd))
                .count(),
            1
        );
        assert!(streaming_items.is_empty());
    }

    #[test]
    fn final_item_only_reasoning_renders_summary_fallback() {
        let (events, streaming_items) = parse_events(&[
            r#"{"type":"response.output_item.done","item":{"id":"rs_final","type":"reasoning","encrypted_content":"enc_final","summary":[{"type":"summary_text","text":"Final summary."}]}}"#,
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
        ]);

        assert!(events.iter().any(
            |event| matches!(event, StreamEvent::ThinkingDelta(text) if text == "Final summary.")
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::OpenAIReasoning {
                id,
                summary,
                encrypted_content: Some(encrypted_content),
                ..
            } if id == "rs_final"
                && summary == &["Final summary.".to_string()]
                && encrypted_content == "enc_final"
        )));
        assert!(streaming_items.is_empty());
    }

    #[test]
    fn encrypted_only_reasoning_closes_and_cleans_up() {
        let (events, streaming_items) = parse_events(&[
            r#"{"type":"response.output_item.added","item":{"id":"rs_encrypted","type":"reasoning"}}"#,
            r#"{"type":"response.output_item.done","item":{"id":"rs_encrypted","type":"reasoning","encrypted_content":"enc_only","summary":[]}}"#,
        ]);

        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::OpenAIReasoning {
                id,
                summary,
                encrypted_content: Some(encrypted_content),
                ..
            } if id == "rs_encrypted" && summary.is_empty() && encrypted_content == "enc_only"
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, StreamEvent::ThinkingEnd))
                .count(),
            1
        );
        assert!(streaming_items.is_empty());
    }

    #[test]
    fn final_encrypted_only_reasoning_without_added_cleans_up() {
        let (events, streaming_items) = parse_events(&[
            r#"{"type":"response.output_item.done","item":{"id":"rs_final_encrypted","type":"reasoning","encrypted_content":"enc_only","summary":[]}}"#,
        ]);

        assert!(events.iter().any(|event| matches!(
            event,
            StreamEvent::OpenAIReasoning {
                id,
                summary,
                encrypted_content: Some(encrypted_content),
                ..
            } if id == "rs_final_encrypted" && summary.is_empty() && encrypted_content == "enc_only"
        )));
        assert!(streaming_items.is_empty());
    }

    #[test]
    fn response_reasoning_delta_behavior_is_unchanged() {
        assert!(matches!(
            parse_events(&[r#"{"type":"response.reasoning.delta","item_id":"rs_test","delta":"legacy reasoning"}"#]).0.as_slice(),
            [StreamEvent::ThinkingDelta(text)] if text == "legacy reasoning"
        ));
    }

    #[test]
    fn streamed_reasoning_delta_is_not_repeated_by_final_summary() {
        let (events, streaming_items) = parse_events(&[
            r#"{"type":"response.output_item.added","item":{"id":"rs_legacy","type":"reasoning"}}"#,
            r#"{"type":"response.reasoning.delta","item_id":"rs_legacy","delta":"Same summary"}"#,
            r#"{"type":"response.output_item.done","item":{"id":"rs_legacy","type":"reasoning","summary":[{"type":"summary_text","text":"Same summary"}]}}"#,
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
        ]);

        let rendered: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ThinkingDelta(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(rendered, "Same summary");
        assert!(streaming_items.is_empty());
    }

    #[test]
    fn every_terminal_event_clears_all_streaming_state() {
        for terminal in [
            "[DONE]",
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
            r#"{"type":"response.incomplete","response":{"status":"incomplete"}}"#,
            r#"{"type":"response.failed","error":{"message":"failed"}}"#,
        ] {
            let mut saw_text_delta = false;
            let mut streaming_items =
                HashMap::from([("item".to_string(), StreamingResponseItemState::default())]);
            let mut completed_tool_items = HashSet::from(["tool".to_string()]);
            let mut pending = VecDeque::new();

            let _ = parse_openai_response_event(
                terminal,
                &mut saw_text_delta,
                &mut streaming_items,
                &mut completed_tool_items,
                &mut pending,
            );

            assert!(streaming_items.is_empty(), "terminal event: {terminal}");
            assert!(
                completed_tool_items.is_empty(),
                "terminal event: {terminal}"
            );
        }
    }

    #[test]
    fn parse_text_wrapped_tool_call_rejects_non_object_json() {
        let text = "prefix to=functions.read [1,2,3]";
        let parsed = parse_text_wrapped_tool_call(text);
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_openai_response_event_ignores_malformed_json_chunks() {
        let mut saw_text_delta = false;
        let mut streaming_tool_calls = HashMap::new();
        let mut completed_tool_items = HashSet::new();
        let mut pending = VecDeque::new();

        let event = parse_openai_response_event(
            "{not-json}",
            &mut saw_text_delta,
            &mut streaming_tool_calls,
            &mut completed_tool_items,
            &mut pending,
        );

        assert!(event.is_none());
        assert!(!saw_text_delta);
        assert!(streaming_tool_calls.is_empty());
        assert!(completed_tool_items.is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn response_completed_emits_message_end_even_when_payload_mentions_fallback() {
        // Regression: when the model edits source that mentions the websocket
        // fallback phrase, that text rides along inside structured events. A
        // `response.completed` frame containing the phrase must still produce a
        // MessageEnd, otherwise the stream "ends before the completion marker".
        let mut saw_text_delta = false;
        let mut streaming_tool_calls = HashMap::new();
        let mut completed_tool_items = HashSet::new();
        let mut pending = VecDeque::new();

        let payload = serde_json::json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "falling back from websockets to https transport"
                    }]
                }]
            }
        })
        .to_string();

        let event = parse_openai_response_event(
            &payload,
            &mut saw_text_delta,
            &mut streaming_tool_calls,
            &mut completed_tool_items,
            &mut pending,
        );

        assert!(
            matches!(event, Some(StreamEvent::MessageEnd { .. })),
            "expected MessageEnd, got {event:?}"
        );
    }

    #[test]
    fn function_call_arguments_with_fallback_phrase_still_emit_tool_call() {
        let mut saw_text_delta = false;
        let mut streaming_tool_calls = HashMap::new();
        let mut completed_tool_items = HashSet::new();
        let mut pending = VecDeque::new();

        let payload = serde_json::json!({
            "type": "response.function_call_arguments.done",
            "item_id": "fc_1",
            "call_id": "call_1",
            "name": "bash",
            "arguments": "{\"command\":\"echo falling back from websockets to https transport\"}"
        })
        .to_string();

        let event = parse_openai_response_event(
            &payload,
            &mut saw_text_delta,
            &mut streaming_tool_calls,
            &mut completed_tool_items,
            &mut pending,
        );

        assert!(
            matches!(event, Some(StreamEvent::ToolUseStart { .. })),
            "expected ToolUseStart, got {event:?}"
        );
    }

    #[test]
    fn plain_text_fallback_notice_is_still_dropped() {
        let mut saw_text_delta = false;
        let mut streaming_tool_calls = HashMap::new();
        let mut completed_tool_items = HashSet::new();
        let mut pending = VecDeque::new();

        let event = parse_openai_response_event(
            "falling back from websockets to https transport",
            &mut saw_text_delta,
            &mut streaming_tool_calls,
            &mut completed_tool_items,
            &mut pending,
        );

        assert!(event.is_none());
    }
}
