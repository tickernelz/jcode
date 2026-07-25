use super::*;

struct AbortTaskOnDrop {
    handle: tokio::task::AbortHandle,
    armed: bool,
}

impl AbortTaskOnDrop {
    fn new<T>(task: &tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: task.abort_handle(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.handle.abort();
        }
    }
}

fn server_event_for_compaction(event: &CompactionEvent) -> ServerEvent {
    ServerEvent::Compaction {
        trigger: event.trigger.clone(),
        engine: event.engine.clone(),
        ownership: event.ownership.clone(),
        configured_route: event.configured_route.clone(),
        effective_route: event.effective_route.clone(),
        fallback_reason: event.fallback_reason.clone(),
        leaf_count: event.leaf_count,
        parent_count: event.parent_count,
        frontier_size: event.frontier_size,
        max_node_level: event.max_node_level,
        graph_generation: event.graph_generation,
        pre_tokens: event.pre_tokens,
        post_tokens: event.post_tokens,
        tokens_saved: event.tokens_saved,
        duration_ms: event.duration_ms,
        messages_dropped: event.messages_dropped,
        messages_compacted: event.messages_compacted,
        summary_chars: event.summary_chars,
        active_messages: event.active_messages,
    }
}

/// Largest byte index `<= index` that is a UTF-8 char boundary in `text`.
/// Equivalent to the unstable `str::floor_char_boundary`, reimplemented so the
/// incremental marker scan can clamp its scan-window start onto a valid
/// boundary without re-scanning the whole accumulated response.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut boundary = index;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

/// The wrapped-tool-call markers emitted by some models inside plain text.
const WRAP_TOOL_MARKERS: [&str; 2] = ["to=functions.", "+#+#"];

/// Find the first wrapped-tool-call marker in `accumulated`, scanning only the
/// newly appended `delta` plus a short overlap from the previous tail (so a
/// marker straddling the append boundary is still found).
///
/// This avoids re-scanning the entire accumulated response on every streamed
/// delta, which was O(response) per token and O(response^2) over a full answer.
fn find_wrap_marker_incremental(accumulated: &str, appended_len: usize) -> Option<usize> {
    let max_marker_len = WRAP_TOOL_MARKERS
        .iter()
        .map(|marker| marker.len())
        .max()
        .unwrap_or(0);
    let scan_start = accumulated
        .len()
        .saturating_sub(appended_len + max_marker_len.saturating_sub(1));
    let scan_start = floor_char_boundary(accumulated, scan_start);
    let window = &accumulated[scan_start..];
    WRAP_TOOL_MARKERS
        .iter()
        .filter_map(|marker| window.find(marker))
        .min()
        .map(|rel_idx| scan_start + rel_idx)
}

fn reload_interrupted_tool_result(tc: &ToolCall, elapsed_secs: f64) -> (String, bool) {
    if tc.name == "selfdev" {
        return ("Reload initiated. Process restarting...".to_string(), false);
    }

    let action = tc
        .input
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let is_wait_like = (tc.name == "bg" && action == "wait")
        || (tc.name == "swarm" && matches!(action, "await_members" | "run_plan"));

    if is_wait_like {
        let input = serde_json::to_string(&tc.input).unwrap_or_else(|_| "{}".to_string());
        return (
            format!(
                "[Tool '{}' wait interrupted by server reload after {:.1}s. The underlying operation may still be running. Resume the wait by rerunning the same tool call with input: {}]",
                tc.name, elapsed_secs, input
            ),
            false,
        );
    }

    (
        format!(
            "[Tool '{}' interrupted by server reload after {:.1}s]",
            tc.name, elapsed_secs
        ),
        true,
    )
}

include!("turn_streaming_mpsc/run.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TaskDropFlag(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for TaskDropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn nested_tool_task_is_aborted_when_turn_owner_drops() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        let mut task = tokio::spawn(async move {
            let _flag = TaskDropFlag(task_dropped);
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        let guard = AbortTaskOnDrop::new(&task);

        drop(guard);

        assert!(
            (&mut task)
                .await
                .expect_err("task must be aborted")
                .is_cancelled()
        );
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    fn tool_call(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "toolu_test".to_string(),
            name: name.to_string(),
            input,
            intent: None,
            thought_signature: None,
        }
    }

    #[test]
    fn reload_interrupted_bg_wait_is_non_error_and_resumable() {
        let tc = tool_call(
            "bg",
            json!({"action": "wait", "task_id": "bg-123", "max_wait_seconds": 300}),
        );

        let (message, is_error) = reload_interrupted_tool_result(&tc, 1.2);

        assert!(!is_error);
        assert!(message.contains("Resume the wait"));
        assert!(message.contains("\"task_id\":\"bg-123\""));
    }

    #[test]
    fn reload_interrupted_non_wait_tool_remains_error() {
        let tc = tool_call("bash", json!({"command": "sleep 10"}));

        let (message, is_error) = reload_interrupted_tool_result(&tc, 1.2);

        assert!(is_error);
        assert!(message.contains("interrupted by server reload"));
    }

    #[test]
    fn auto_recovery_compaction_event_preserves_lcm_details() {
        let event = CompactionEvent {
            trigger: "critical_local_emergency_chain".to_string(),
            engine: Some("lcm".to_string()),
            effective_route: Some("local:emergency".to_string()),
            fallback_reason: Some("selected>a>active>b>local".to_string()),
            graph_generation: Some(7),
            messages_compacted: Some(12),
            messages_dropped: Some(12),
            ..CompactionEvent::default()
        };
        let ServerEvent::Compaction {
            engine,
            effective_route,
            fallback_reason,
            graph_generation,
            messages_compacted,
            messages_dropped,
            ..
        } = server_event_for_compaction(&event)
        else {
            panic!("expected compaction event")
        };
        assert_eq!(engine.as_deref(), Some("lcm"));
        assert_eq!(effective_route.as_deref(), Some("local:emergency"));
        assert_eq!(
            fallback_reason.as_deref(),
            Some("selected>a>active>b>local")
        );
        assert_eq!(graph_generation, Some(7));
        assert_eq!(messages_compacted, Some(12));
        assert_eq!(messages_dropped, Some(12));
    }

    /// Reference O(n) full scan, preserving the original precedence: the
    /// `to=functions.` marker is checked before `+#+#`.
    fn find_wrap_marker_full(text: &str) -> Option<usize> {
        text.find("to=functions.").or_else(|| text.find("+#+#"))
    }

    /// Simulate streaming `full` in arbitrary deltas and assert the incremental
    /// scan finds the first marker position, matching a full rescan each step.
    fn assert_incremental_matches(full: &str, chunk: usize) {
        let mut acc = String::new();
        let mut incremental_hit: Option<usize> = None;
        let bytes = full.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let mut end = (i + chunk).min(bytes.len());
            while end < bytes.len() && !full.is_char_boundary(end) {
                end += 1;
            }
            let delta = &full[i..end];
            acc.push_str(delta);
            if incremental_hit.is_none() {
                incremental_hit = find_wrap_marker_incremental(&acc, delta.len());
            }
            i = end;
        }
        // The earliest of either marker in the full text.
        let fn_pos = full.find("to=functions.");
        let plus_pos = full.find("+#+#");
        let expected = match (fn_pos, plus_pos) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        assert_eq!(
            incremental_hit, expected,
            "incremental scan mismatch for {full:?} chunk={chunk}"
        );
    }

    #[test]
    fn wrap_marker_incremental_detects_markers_across_chunk_sizes() {
        let cases = [
            "plain answer with no marker at all",
            "answer then to=functions.foo({})",
            "answer then +#+# wrapped",
            "prefix +#+# and later to=functions.bar",
            "unicode 🔄 résumé then to=functions.baz",
            "",
            "to=functions.first",
            "+#+#",
        ];
        for case in cases {
            for chunk in [1usize, 2, 3, 5, 7, 100] {
                assert_incremental_matches(case, chunk);
            }
        }
    }

    #[test]
    fn wrap_marker_incremental_finds_marker_straddling_delta_boundary() {
        // Feed "to=functions." split right in the middle so the marker only
        // exists once both halves are appended; the overlap window must catch it.
        let mut acc = String::new();
        acc.push_str("answer to=fun");
        assert_eq!(
            find_wrap_marker_incremental(&acc, "answer to=fun".len()),
            None
        );
        acc.push_str("ctions.tool");
        let hit = find_wrap_marker_incremental(&acc, "ctions.tool".len());
        assert_eq!(hit, find_wrap_marker_full(&acc));
        assert_eq!(hit, Some("answer ".len()));
    }
}
