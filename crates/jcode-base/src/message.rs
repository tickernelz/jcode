use crate::logging;
use base64::Engine as _;
use jcode_background_types::{
    BackgroundTaskCompleted, BackgroundTaskProgressEvent, BackgroundTaskStatus,
};
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

pub use jcode_message_types::{
    CacheControl, ConnectionPhase, ContentBlock, InputShellResult, Message, Role, StreamEvent,
    TOOL_OUTPUT_MISSING_TEXT, ToolCall, ToolDefinition, cache_relevant_message_hashes,
    cache_relevant_message_value, cache_relevant_messages, ends_with_fresh_user_turn,
    extend_stable_hash, messages_with_dynamic_system_context, sanitize_tool_id,
    stable_message_hash,
};

mod notifications;

pub use notifications::{
    ParsedBackgroundTaskNotification, ParsedBackgroundTaskProgressNotification,
    background_task_display_label, background_task_status_notice,
    format_background_task_notification_markdown, format_background_task_progress_markdown,
    format_input_shell_result_markdown, format_model_refresh_progress_markdown,
    input_shell_status_notice, parse_background_task_notification_markdown,
    parse_background_task_progress_notification_markdown, strip_ansi_escape_sequences,
};

mod generated_images;
mod redaction;

pub use generated_images::{
    GENERATED_IMAGE_TOOL_NAME, generated_image_rendered_image, generated_image_summary,
    generated_image_tool_input, generated_image_visual_context_blocks, push_reasoning_blocks,
};
use redaction::compile_static_regex;

pub use redaction::{
    UNCERTAIN_SECRET_OMISSION, redact_secrets, redact_uncertain_json, redact_uncertain_secrets,
};

#[cfg(test)]
mod tests;
