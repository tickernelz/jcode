#[cfg(test)]
mod tests {
    use super::*;

    fn user_text(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: Some(1),
        }
    }

    #[test]
    fn messages_end_with_tool_result_detects_tool_continuation_context() {
        let messages = vec![
            user_text("tell me about the desktop application"),
            tool_result("functions.read:0", "desktop architecture docs"),
            tool_result("functions.agentgrep:4", "desktop source summary"),
        ];

        assert!(Agent::messages_end_with_tool_result(&messages));
    }

    #[test]
    fn messages_end_with_tool_result_allows_memory_after_tool_results() {
        let messages = vec![
            user_text("tell me about the desktop application"),
            tool_result("functions.read:0", "desktop architecture docs"),
            user_text("<system-reminder>Relevant memory</system-reminder>"),
        ];

        assert!(Agent::messages_end_with_tool_result(&messages));
    }

    #[test]
    fn messages_end_with_tool_result_ignores_plain_user_prompt() {
        let messages = vec![user_text("hello")];

        assert!(!Agent::messages_end_with_tool_result(&messages));
    }
}
