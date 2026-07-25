use super::*;

/// Outcome of [`CompactionManager::recover_within_budget`].
#[derive(Debug, Clone, Copy)]
pub struct EmergencyRecovery {
    /// Context usage fraction (1.0 == full budget) observed before recovery.
    pub pre_usage: f32,
    /// Messages dropped by the hard compact, or `None` if it could not run.
    pub dropped: Option<usize>,
    /// Number of oversized tool results that were truncated as a fallback.
    pub truncated: usize,
}

impl EmergencyRecovery {
    /// Whether any space-reclaiming action actually happened.
    pub fn did_anything(&self) -> bool {
        self.dropped.unwrap_or(0) > 0 || self.truncated > 0
    }

    /// A user-facing description of what recovery did, without a trailing
    /// call to action (callers append their own, e.g. "Retrying..." or
    /// "You can continue."). `trigger_usage` is the usage fraction that
    /// triggered recovery (rendered as a percentage).
    pub fn summary_line(&self, trigger_usage: f32) -> String {
        let pct = trigger_usage * 100.0;
        match (self.dropped, self.truncated) {
            (Some(dropped), 0) => format!(
                "⚡ Emergency compaction: dropped {dropped} old messages (context was at {pct:.0}%).",
            ),
            (Some(dropped), truncated) => format!(
                "⚡ Emergency compaction: dropped {dropped} old messages and truncated {truncated} tool result(s) (context was at {pct:.0}%).",
            ),
            (None, truncated) => format!(
                "⚡ Emergency truncation: shortened {truncated} large tool result(s) to fit context.",
            ),
        }
    }
}

impl Default for CompactionManager {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) static LCM_SAFE_PROMPT_CHARS: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static LCM_SCHEDULER: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
// Background hierarchy/leaf work may use at most three slots. Critical context
// recovery and user-waiting transfers can therefore always enter the shared
// FIFO scheduler without waiting behind a newly admitted background job.
static LCM_BACKGROUND_SCHEDULER: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(3)));
#[cfg(not(test))]
const LCM_PROVIDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
#[cfg(test)]
const LCM_PROVIDER_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(not(test))]
const LCM_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(test)]
const LCM_QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LcmJobPriority {
    Background,
    Critical,
}

impl LcmJobPriority {
    fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Critical => "critical",
        }
    }
}

pub(super) struct LcmSchedulerPermit {
    _shared: tokio::sync::OwnedSemaphorePermit,
    _background: Option<tokio::sync::OwnedSemaphorePermit>,
}

pub(super) async fn acquire_lcm_scheduler(priority: LcmJobPriority) -> Result<LcmSchedulerPermit> {
    acquire_lcm_scheduler_from(
        Arc::clone(&LCM_SCHEDULER),
        Arc::clone(&LCM_BACKGROUND_SCHEDULER),
        priority,
        LCM_QUEUE_TIMEOUT,
    )
    .await
}

pub(super) async fn acquire_lcm_scheduler_from(
    shared_scheduler: Arc<tokio::sync::Semaphore>,
    background_scheduler: Arc<tokio::sync::Semaphore>,
    priority: LcmJobPriority,
    queue_timeout: std::time::Duration,
) -> Result<LcmSchedulerPermit> {
    tokio::time::timeout(queue_timeout, async move {
        let background = if priority == LcmJobPriority::Background {
            Some(
                background_scheduler
                    .acquire_owned()
                    .await
                    .map_err(|_| anyhow::anyhow!("LCM background scheduler is closed"))?,
            )
        } else {
            None
        };
        let shared = shared_scheduler
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("LCM scheduler is closed"))?;
        Ok(LcmSchedulerPermit {
            _shared: shared,
            _background: background,
        })
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "LCM {} scheduler queue timed out after {} ms",
            priority.as_str(),
            queue_timeout.as_millis()
        )
    })?
}

pub(super) const LCM_SUMMARY_SYSTEM_PROMPT: &str = r#"You are the Jcode LCM context compactor.
Return at most 900 characters of stable Markdown with these sections: Objective and user intent; Explicit constraints and prohibited actions; Decisions and rationale; Repository state and exact paths/symbols/branches/commits; Changes actually completed; Commands and tests with actual outcomes; Failures, diagnosis, and unresolved blockers; Open questions and next steps; Retrieval anchors and source range. Retain at most one highest-value excerpt per section.
Separate observed facts from plans or assumptions. Preserve newer corrections and mark superseded decisions. Every non-placeholder content line must be `- [source] ` followed by one exact, contiguous excerpt copied verbatim from the observed source. Do not paraphrase or combine excerpts. Use `None observed.` when a section has no useful excerpt. Never claim an edit, commit, command, or test happened unless an exact source excerpt says it did. Do not include chain-of-thought, credentials, tokens, or raw tool blobs. Do not invent missing details."#;

pub(super) const LCM_REQUIRED_SECTIONS: [&str; 9] = [
    "Objective and user intent",
    "Explicit constraints and prohibited actions",
    "Decisions and rationale",
    "Repository state and exact paths/symbols/branches/commits",
    "Changes actually completed",
    "Commands and tests with actual outcomes",
    "Failures, diagnosis, and unresolved blockers",
    "Open questions and next steps",
    "Retrieval anchors and source range",
];

pub(super) fn compactor_safe_messages(messages: &[Message]) -> Vec<Message> {
    let mut safe_messages = messages.to_vec();
    for message in &mut safe_messages {
        for block in &mut message.content {
            match block {
                ContentBlock::ToolUse { input, .. } => {
                    *input = serde_json::json!({
                        "payload": "omitted from compactor context; use conversation_search for canonical details"
                    });
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let status = match is_error {
                        Some(true) => "error",
                        Some(false) => "success",
                        None => "unknown",
                    };
                    *content = format!(
                        "[tool result payload omitted from compactor context; status={status}; use conversation_search for canonical details]"
                    );
                }
                _ => {}
            }
        }
    }
    safe_messages
}

/// Apply a stricter trust-boundary policy than the general transcript redactor:
/// omit the entire line when it labels sensitive material or contains an opaque,
/// mixed-class value. Tool payloads are removed before this classifier runs.
#[cfg(test)]
pub(super) const LCM_OMITTED_SOURCE: &str = crate::message::UNCERTAIN_SECRET_OMISSION;

pub(super) fn lcm_redact_uncertain_secrets(text: &str) -> String {
    crate::message::redact_uncertain_secrets(text)
}

pub(super) fn lcm_safe_messages(messages: &[Message]) -> Vec<Message> {
    let mut safe_messages = compactor_safe_messages(messages);
    for message in &mut safe_messages {
        for block in &mut message.content {
            if let ContentBlock::Text { text, .. } = block {
                *text = lcm_redact_uncertain_secrets(text);
            }
        }
    }
    safe_messages
}

pub(super) fn lcm_safe_source_text(
    messages: &[Message],
    existing_summary: Option<&Summary>,
) -> String {
    let safe_messages = lcm_safe_messages(messages);
    lcm_redact_uncertain_secrets(&build_compaction_conversation_text(
        &safe_messages,
        existing_summary,
    ))
}

pub(super) fn build_lcm_compaction_prompt(
    messages: &[Message],
    existing_summary: Option<&Summary>,
    max_prompt_chars: usize,
) -> String {
    let mut source = lcm_safe_source_text(messages, existing_summary);
    const INSTRUCTION: &str = "Select only exact source excerpts according to the section schema in the system instruction. Prefix every excerpt with `- [source] ` and use every required heading even when its value is `None observed.`.";
    const OMISSION: &str =
        "\n\n... [middle source omitted from this chunk; use canonical retrieval anchors] ...\n\n";
    let overhead = INSTRUCTION.len() + OMISSION.len() + 9;
    if source.len().saturating_add(overhead) > max_prompt_chars && max_prompt_chars > overhead {
        let budget = max_prompt_chars - overhead;
        let head_budget = budget / 2;
        let tail_budget = budget.saturating_sub(head_budget);
        let head = jcode_compaction_core::truncate_str_boundary(&source, head_budget);
        let mut tail_start = source.len().saturating_sub(tail_budget);
        while tail_start < source.len() && !source.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        source = format!("{head}{OMISSION}{}", &source[tail_start..]);
    }
    format!("{source}\n\n---\n\n{INSTRUCTION}")
}

pub(super) fn lcm_output_has_required_sections(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    LCM_REQUIRED_SECTIONS
        .iter()
        .all(|section| lower.contains(&format!("# {}", section.to_ascii_lowercase())))
}

pub(super) fn lcm_output_is_grounded(summary: &str, source: &str) -> bool {
    summary.lines().all(|line| {
        let line = line.trim();
        if line.is_empty() {
            return true;
        }
        if line.starts_with('#') {
            let heading = line.trim_start_matches('#').trim();
            return LCM_REQUIRED_SECTIONS
                .iter()
                .any(|required| heading.eq_ignore_ascii_case(required));
        }
        let content = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line)
            .trim();
        if content.eq_ignore_ascii_case("none observed.")
            || content.eq_ignore_ascii_case("none observed")
        {
            return true;
        }
        content.strip_prefix("[source] ").is_some_and(|excerpt| {
            let excerpt = excerpt.trim();
            !excerpt.is_empty()
                && source
                    .lines()
                    .any(|source_line| source_line.trim() == excerpt)
        })
    })
}

pub(super) fn repair_lcm_output_grounding(
    summary: &str,
    source: &str,
    max_chars: usize,
) -> Option<String> {
    if !lcm_output_has_required_sections(summary) {
        return None;
    }
    let mut excerpts = vec![Vec::<String>::new(); LCM_REQUIRED_SECTIONS.len()];
    let mut current_section = None;

    for line in summary.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            let heading = line.trim_start_matches('#').trim();
            current_section = LCM_REQUIRED_SECTIONS
                .iter()
                .position(|required| heading.eq_ignore_ascii_case(required));
            continue;
        }
        let Some(section) = current_section else {
            continue;
        };
        let content = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line)
            .trim();
        let Some(excerpt) = content.strip_prefix("[source] ").map(str::trim) else {
            continue;
        };
        let canonical_line = source
            .lines()
            .map(str::trim)
            .find(|source_line| !excerpt.is_empty() && source_line.contains(excerpt));
        if let Some(canonical_line) = canonical_line
            && !excerpts[section].iter().any(|kept| kept == canonical_line)
        {
            excerpts[section].push(canonical_line.to_string());
        }
    }

    let render = |excerpts: &[Vec<String>]| {
        LCM_REQUIRED_SECTIONS
            .iter()
            .zip(excerpts)
            .map(|(heading, excerpts)| {
                let content = if excerpts.is_empty() {
                    "None observed.".to_string()
                } else {
                    excerpts
                        .iter()
                        .map(|excerpt| format!("- [source] {excerpt}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                format!("# {heading}\n{content}")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let mut repaired = render(&excerpts);
    while repaired.len() > max_chars {
        let Some(section) = excerpts.iter().rposition(|section| !section.is_empty()) else {
            break;
        };
        excerpts[section].pop();
        repaired = render(&excerpts);
    }
    Some(repaired)
}

pub(super) fn reduce_validated_lcm_summaries(
    summaries: &[String],
    max_chars: usize,
) -> Option<String> {
    if summaries.is_empty()
        || summaries
            .iter()
            .any(|summary| !lcm_output_has_required_sections(summary))
    {
        return None;
    }
    let candidate = summaries.join("\n\n");
    let exact_excerpts = summaries
        .iter()
        .flat_map(|summary| summary.lines())
        .filter_map(|line| line.trim().strip_prefix("- [source] ").map(str::trim))
        .filter(|excerpt| !excerpt.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    repair_lcm_output_grounding(&candidate, &exact_excerpts, max_chars)
}

pub(super) fn summarize_small_lcm_source(
    messages: &[Message],
    existing_summary: Option<&Summary>,
    output_budget_chars: usize,
) -> Option<String> {
    let source = lcm_safe_source_text(messages, existing_summary);
    let excerpts = source
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with("**")
                && !line.eq_ignore_ascii_case("none observed.")
        })
        .map(|line| format!("- [source] {line}"))
        .collect::<Vec<_>>();
    let candidate = LCM_REQUIRED_SECTIONS
        .iter()
        .map(|heading| {
            if *heading == "Retrieval anchors and source range" && !excerpts.is_empty() {
                format!("# {heading}\n{}", excerpts.join("\n"))
            } else {
                format!("# {heading}\nNone observed.")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    repair_lcm_output_grounding(
        &candidate,
        &source,
        output_budget_chars.min(LCM_INLINE_SOURCE_CHARS),
    )
}

pub(super) fn is_lcm_context_limit_error(error: &anyhow::Error) -> bool {
    let lower = error.to_string().to_ascii_lowercase();
    (lower.contains("context")
        && (lower.contains("limit")
            || lower.contains("length")
            || lower.contains("window")
            || lower.contains("too long")))
        || lower.contains("too many tokens")
        || lower.contains("prompt is too long")
        || lower.contains("prompt too long")
        || lower.contains("input is too long")
        || lower.contains("maximum token")
        || lower.contains("max token")
}

pub(super) fn lcm_prompt_budget(
    provider: &dyn Provider,
    route_identity: Option<&str>,
) -> (String, usize, usize) {
    let context_tokens = provider.context_window().max(4_096);
    let output_reserve = (context_tokens / 8).clamp(1_024, 8_192);
    let safety_reserve = (context_tokens / 10).max(1_024);
    let prompt_chars = context_tokens
        .saturating_sub(output_reserve)
        .saturating_sub(safety_reserve)
        .max(1_024)
        .saturating_mul(CHARS_PER_TOKEN);
    let route_key = route_identity
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}:{}", provider.name(), provider.model()));
    let cached = LCM_SAFE_PROMPT_CHARS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&route_key)
        .copied();
    (
        route_key,
        cached.map_or(prompt_chars, |safe| safe.min(prompt_chars)),
        output_reserve.saturating_mul(CHARS_PER_TOKEN),
    )
}

pub(super) fn lcm_output_is_concise(
    summary: &str,
    source_chars: usize,
    output_budget_chars: usize,
) -> bool {
    let length = summary.trim().len();
    length > 0 && length <= output_budget_chars && (length < source_chars || length <= 1_024)
}

pub(super) fn lcm_message_chunks(messages: Vec<Message>, max_chars: usize) -> Vec<Vec<Message>> {
    // A boundary after message `n - 1` is unsafe when a tool call is on its
    // left and the corresponding result is on its right. This preserves exact
    // IDs and ordering for parallel calls without forcing unrelated turns into
    // the same chunk. Oversized individual transactions remain intact and rely
    // on the bounded prompt renderer's per-result truncation.
    let mut tool_calls = HashMap::new();
    let mut resolved_tool_calls = std::collections::HashSet::new();
    let mut unsafe_boundaries = vec![false; messages.len() + 1];
    for (index, message) in messages.iter().enumerate() {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    tool_calls.entry(id.clone()).or_insert(index);
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    if let Some(call_index) = tool_calls.get(tool_use_id).copied() {
                        resolved_tool_calls.insert(tool_use_id.clone());
                        let transaction_end = if messages
                            .get(index + 1)
                            .is_some_and(|next| next.role == Role::Assistant)
                        {
                            index + 1
                        } else {
                            index
                        };
                        unsafe_boundaries[call_index.saturating_add(1)..=transaction_end]
                            .fill(true);
                    }
                }
                _ => {}
            }
        }
    }
    for (tool_use_id, call_index) in &tool_calls {
        if !resolved_tool_calls.contains(tool_use_id) && call_index + 1 < messages.len() {
            unsafe_boundaries[call_index + 1] = true;
        }
    }

    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut current_chars: usize = 0;
    for (index, message) in messages.into_iter().enumerate() {
        let chars = message_char_count(&message);
        if !current.is_empty()
            && current_chars.saturating_add(chars) > max_chars
            && !unsafe_boundaries[index]
        {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
        current_chars = current_chars.saturating_add(chars);
        current.push(message);
        if current_chars >= max_chars && !unsafe_boundaries[index + 1] {
            chunks.push(std::mem::take(&mut current));
            current_chars = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub(super) async fn complete_lcm_bounded(
    provider: &Arc<dyn Provider>,
    messages: &[Message],
    existing_summary: Option<&Summary>,
    max_prompt_chars: usize,
    output_budget_chars: usize,
) -> Result<String> {
    let source_chars = messages.iter().map(message_char_count).sum::<usize>()
        + existing_summary
            .map(summary_payload_char_count)
            .unwrap_or(0);
    let canonical_source = lcm_safe_source_text(messages, existing_summary);
    let prompt = build_lcm_compaction_prompt(messages, existing_summary, max_prompt_chars);
    let completion_start = Instant::now();
    let summary = tokio::time::timeout(
        LCM_PROVIDER_TIMEOUT,
        provider.complete_simple(&prompt, LCM_SUMMARY_SYSTEM_PROMPT),
    )
    .await
    .map_err(|_| anyhow::anyhow!("LCM compactor provider call timed out"))??;
    let sanitized_summary = lcm_redact_uncertain_secrets(&summary);
    if sanitized_summary != summary {
        anyhow::bail!("LCM compactor output contained secret-shaped content");
    }
    let summary = sanitized_summary;
    let concise = lcm_output_is_concise(&summary, source_chars, output_budget_chars);
    let structured = lcm_output_has_required_sections(&summary);
    let grounded = lcm_output_is_grounded(&summary, &canonical_source);
    crate::logging::event_info(
        "LCM_PROVIDER_STAGE",
        vec![
            ("phase", "initial_completion".to_string()),
            (
                "elapsed_ms",
                completion_start.elapsed().as_millis().to_string(),
            ),
            ("source_chars", source_chars.to_string()),
            ("prompt_chars", prompt.len().to_string()),
            ("output_chars", summary.len().to_string()),
            ("concise", concise.to_string()),
            ("structured", structured.to_string()),
            ("grounded", grounded.to_string()),
        ],
    );
    if concise && structured && grounded {
        return Ok(summary.trim().to_string());
    }
    let deterministic_repair_budget =
        output_budget_chars.min(source_chars.saturating_sub(1).max(1_024));
    if structured
        && let Some(repaired) =
            repair_lcm_output_grounding(&summary, &canonical_source, deterministic_repair_budget)
    {
        let concise = lcm_output_is_concise(&repaired, source_chars, output_budget_chars);
        let grounded = lcm_output_is_grounded(&repaired, &canonical_source);
        let retained_source = repaired.contains("- [source] ");
        crate::logging::event_info(
            "LCM_PROVIDER_STAGE",
            vec![
                ("phase", "deterministic_repair".to_string()),
                ("source_chars", source_chars.to_string()),
                ("input_chars", summary.len().to_string()),
                ("output_chars", repaired.len().to_string()),
                ("concise", concise.to_string()),
                ("grounded", grounded.to_string()),
                ("retained_source", retained_source.to_string()),
            ],
        );
        if concise && grounded && retained_source {
            return Ok(repaired);
        }
    }
    let target_chars = output_budget_chars
        .min(source_chars.saturating_sub(1))
        .max(128);
    let rewrite_prompt = format!(
        "Rewrite the following candidate into at most {target_chars} characters. Use all nine exact required Markdown headings from the system instruction. Every retained content line must be `- [source] ` plus one exact contiguous excerpt from the original observed source; otherwise replace it with `None observed.`. Remove unsupported completion claims and secrets. Return only the rewrite.\n\n{summary}"
    );
    let rewrite_start = Instant::now();
    let rewritten = tokio::time::timeout(
        LCM_PROVIDER_TIMEOUT,
        provider.complete_simple(&rewrite_prompt, LCM_SUMMARY_SYSTEM_PROMPT),
    )
    .await
    .map_err(|_| anyhow::anyhow!("LCM compactor rewrite timed out"))??;
    let sanitized_rewritten = lcm_redact_uncertain_secrets(&rewritten);
    if sanitized_rewritten != rewritten {
        anyhow::bail!("LCM compactor rewrite contained secret-shaped content");
    }
    let rewritten = sanitized_rewritten;
    let concise = lcm_output_is_concise(&rewritten, source_chars, output_budget_chars);
    let structured = lcm_output_has_required_sections(&rewritten);
    let grounded = lcm_output_is_grounded(&rewritten, &canonical_source);
    crate::logging::event_info(
        "LCM_PROVIDER_STAGE",
        vec![
            ("phase", "rewrite_completion".to_string()),
            (
                "elapsed_ms",
                rewrite_start.elapsed().as_millis().to_string(),
            ),
            ("source_chars", source_chars.to_string()),
            ("prompt_chars", rewrite_prompt.len().to_string()),
            ("output_chars", rewritten.len().to_string()),
            ("concise", concise.to_string()),
            ("structured", structured.to_string()),
            ("grounded", grounded.to_string()),
        ],
    );
    if !concise || !structured || !grounded {
        anyhow::bail!(
            "LCM compactor output remained invalid, unsupported, or oversized after one rewrite"
        );
    }
    Ok(rewritten.trim().to_string())
}

pub(super) async fn summarize_lcm_source_with_budget(
    provider: &Arc<dyn Provider>,
    messages: Vec<Message>,
    existing_summary: Option<Summary>,
    max_prompt_chars: usize,
    output_budget_chars: usize,
) -> Result<String> {
    let chunk_chars = (max_prompt_chars * 3 / 4).max(1_024);
    let source_chars = messages.iter().map(message_char_count).sum::<usize>()
        + existing_summary
            .as_ref()
            .map(summary_payload_char_count)
            .unwrap_or(0);
    if source_chars <= chunk_chars {
        return complete_lcm_bounded(
            provider,
            &messages,
            existing_summary.as_ref(),
            max_prompt_chars,
            output_budget_chars,
        )
        .await;
    }

    let chunks = lcm_message_chunks(messages, chunk_chars);
    crate::logging::event_info(
        "LCM_PROVIDER_STAGE",
        vec![
            ("phase", "source_chunks".to_string()),
            ("chunks", chunks.len().to_string()),
            ("source_chars", source_chars.to_string()),
            ("max_concurrency", LCM_CHUNK_CONCURRENCY.to_string()),
        ],
    );
    let existing_summary_ref = existing_summary.as_ref();
    let mut indexed_summaries = futures::stream::iter(chunks.into_iter().enumerate().map(
        |(index, chunk)| async move {
            let prior_summary = (index == 0).then_some(existing_summary_ref).flatten();
            let chunk_source_chars = chunk.iter().map(message_char_count).sum::<usize>()
                + prior_summary.map(summary_payload_char_count).unwrap_or(0);
            if chunk_source_chars <= LCM_INLINE_SOURCE_CHARS {
                let summary =
                    summarize_small_lcm_source(&chunk, prior_summary, output_budget_chars)
                        .ok_or_else(|| {
                            anyhow::anyhow!("LCM small source could not be reduced safely")
                        })?;
                crate::logging::event_info(
                    "LCM_PROVIDER_STAGE",
                    vec![
                        ("phase", "deterministic_small_source".to_string()),
                        ("source_chars", chunk_source_chars.to_string()),
                        ("output_chars", summary.len().to_string()),
                    ],
                );
                return Ok::<_, anyhow::Error>((index, summary));
            }
            let summary = complete_lcm_bounded(
                provider,
                &chunk,
                prior_summary,
                max_prompt_chars,
                output_budget_chars,
            )
            .await?;
            Ok::<_, anyhow::Error>((index, summary))
        },
    ))
    .buffer_unordered(LCM_CHUNK_CONCURRENCY)
    .try_collect::<Vec<_>>()
    .await?;
    indexed_summaries.sort_unstable_by_key(|(index, _)| *index);
    let mut summaries = indexed_summaries
        .into_iter()
        .map(|(_, summary)| summary)
        .collect::<Vec<_>>();
    if summaries.is_empty()
        && let Some(existing) = existing_summary
    {
        summaries.push(existing.text);
    }

    if summaries.len() > 1 {
        let input_chars = summaries.iter().map(String::len).sum::<usize>();
        let reduction_budget = output_budget_chars.min(input_chars.saturating_sub(1).max(1_024));
        let reduced = reduce_validated_lcm_summaries(&summaries, reduction_budget)
            .ok_or_else(|| anyhow::anyhow!("LCM chunk summaries could not be reduced safely"))?;
        crate::logging::event_info(
            "LCM_PROVIDER_STAGE",
            vec![
                ("phase", "deterministic_reduction".to_string()),
                ("chunks", summaries.len().to_string()),
                ("input_chars", input_chars.to_string()),
                ("output_chars", reduced.len().to_string()),
            ],
        );
        summaries = vec![reduced];
    }
    summaries
        .pop()
        .ok_or_else(|| anyhow::anyhow!("LCM source did not contain summarizable context"))
}

/// Generate summary using the provider
pub(super) async fn generate_compaction_artifact(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    mut existing_summary: Option<Summary>,
) -> Result<CompactionResult> {
    let start = Instant::now();
    if let Some(summary) = existing_summary.as_mut()
        && let Some(encrypted_content) = summary.openai_encrypted_content.as_ref()
        && !openai_encrypted_content_is_sendable(encrypted_content)
    {
        let encrypted_content_len = encrypted_content.len();
        crate::logging::warn(&format!(
            "[compaction] Existing OpenAI native compaction payload is oversized ({} chars); falling back to text summary",
            encrypted_content_len,
        ));
        summary.openai_encrypted_content = None;
        let fallback = openai_encrypted_content_fallback_summary(encrypted_content_len);
        if summary.text.trim().is_empty() {
            summary.text = fallback;
        } else if !summary
            .text
            .contains("OpenAI native compaction state was discarded")
        {
            summary.text.push_str("\n\n");
            summary.text.push_str(&fallback);
        }
    }
    if let Some(summary) = existing_summary.as_mut() {
        summary.text = crate::message::redact_uncertain_secrets(&summary.text);
    }
    let safe_messages = lcm_safe_messages(&messages);

    if let Ok(native) = provider
        .native_compact(
            &safe_messages,
            existing_summary
                .as_ref()
                .map(|summary| summary.text.as_str()),
            existing_summary
                .as_ref()
                .and_then(|summary| summary.openai_encrypted_content.as_deref()),
        )
        .await
    {
        if let Some(encrypted_content) = native.openai_encrypted_content.as_ref()
            && !openai_encrypted_content_is_sendable(encrypted_content)
        {
            crate::logging::warn(&format!(
                "[compaction] OpenAI native compaction returned oversized encrypted_content ({} chars); falling back to text summary",
                encrypted_content.len(),
            ));
        } else {
            return Ok(CompactionResult {
                summary_text: crate::message::redact_uncertain_secrets(
                    native.summary_text.as_deref().unwrap_or(""),
                ),
                atomic_parent_summaries: Vec::new(),
                openai_encrypted_content: native.openai_encrypted_content,
                covers_up_to_turn: messages.len(),
                duration_ms: start.elapsed().as_millis() as u64,
                summarized_messages: messages.len(),
            });
        }
    }

    let max_prompt_chars = provider.context_window().saturating_sub(4000) * CHARS_PER_TOKEN;
    let prompt = crate::message::redact_uncertain_secrets(&build_compaction_prompt(
        &safe_messages,
        existing_summary.as_ref(),
        max_prompt_chars,
    ));

    // Generate summary using simple completion
    let summary = provider
        .complete_simple(
            &prompt,
            "You are a helpful assistant that summarizes conversations.",
        )
        .await?;
    let summary = crate::message::redact_uncertain_secrets(&summary);

    Ok(CompactionResult {
        summary_text: summary,
        atomic_parent_summaries: Vec::new(),
        openai_encrypted_content: None,
        covers_up_to_turn: messages.len(),
        duration_ms: start.elapsed().as_millis() as u64,
        summarized_messages: messages.len(),
    })
}

/// Generate a portable LCM leaf. Provider-native encrypted compaction is
/// intentionally not consulted because graph nodes must remain replayable on
/// every provider route.
#[cfg(test)]
pub(super) async fn generate_lcm_compaction_artifact(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    existing_summary: Option<Summary>,
    route_identity: Option<String>,
    priority: LcmJobPriority,
) -> Result<CompactionResult> {
    let source_messages = messages.len();
    let pre_tokens = messages
        .iter()
        .map(message_char_count)
        .sum::<usize>()
        .div_ceil(CHARS_PER_TOKEN) as u64;
    let effective_route = route_identity
        .clone()
        .unwrap_or_else(|| format!("{}:{}", provider.name(), provider.model()));
    generate_lcm_compaction_artifact_with_attempt(
        provider,
        messages,
        existing_summary,
        route_identity,
        priority,
        LcmAttemptContext {
            attempt_id: crate::id::new_id("lcm_attempt"),
            session_id: "unscoped".to_string(),
            trigger: priority.as_str().to_string(),
            stage: "leaf".to_string(),
            configured_route: None,
            effective_route,
            source_messages,
            pre_tokens,
            terminal_emitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    )
    .await
}

pub(super) async fn generate_lcm_compaction_artifact_with_attempt(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    existing_summary: Option<Summary>,
    route_identity: Option<String>,
    priority: LcmJobPriority,
    attempt: LcmAttemptContext,
) -> Result<CompactionResult> {
    let start = Instant::now();
    let queued_at = Instant::now();
    emit_lcm_attempt(
        &attempt,
        LcmAttemptOutcome::Queued,
        LcmAttemptReason::SchedulerWait,
        None,
        None,
        None,
        0,
    );
    let permit = match acquire_lcm_scheduler(priority).await {
        Ok(permit) => permit,
        Err(error) => {
            emit_lcm_attempt(
                &attempt,
                LcmAttemptOutcome::Failed,
                lcm_attempt_failure_reason(&error),
                Some(queued_at.elapsed().as_millis() as u64),
                None,
                None,
                0,
            );
            return Err(error);
        }
    };
    let queue_ms = queued_at.elapsed().as_millis() as u64;
    let execution_start = Instant::now();
    emit_lcm_attempt(
        &attempt,
        LcmAttemptOutcome::Running,
        LcmAttemptReason::SchedulerAdmitted,
        Some(queue_ms),
        None,
        None,
        0,
    );
    crate::logging::event_info(
        "LCM_SCHEDULER",
        vec![
            ("provider", provider.name().to_string()),
            ("model", provider.model()),
            ("priority", priority.as_str().to_string()),
            ("queue_ms", queue_ms.to_string()),
        ],
    );
    let (route_key, mut max_prompt_chars, output_budget_chars) =
        lcm_prompt_budget(provider.as_ref(), route_identity.as_deref());
    if priority == LcmJobPriority::Critical {
        max_prompt_chars = max_prompt_chars.min(LCM_CRITICAL_PROMPT_CHARS);
    }
    let mut attempts = 0;
    let summary = loop {
        match summarize_lcm_source_with_budget(
            &provider,
            messages.clone(),
            existing_summary.clone(),
            max_prompt_chars,
            output_budget_chars,
        )
        .await
        {
            Ok(summary) => break summary,
            Err(error) if attempts < 2 && is_lcm_context_limit_error(&error) => {
                attempts += 1;
                max_prompt_chars = (max_prompt_chars / 2).max(1_024);
                LCM_SAFE_PROMPT_CHARS
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(route_key.clone(), max_prompt_chars);
                crate::logging::warn(&format!(
                    "LCM compactor route {route_key} exceeded its declared context; retrying with {max_prompt_chars} prompt chars"
                ));
                emit_lcm_attempt(
                    &attempt,
                    LcmAttemptOutcome::Retrying,
                    LcmAttemptReason::ContextLimitAdaptiveRetry,
                    Some(queue_ms),
                    Some(execution_start.elapsed().as_millis() as u64),
                    None,
                    attempts,
                );
            }
            Err(error) => {
                emit_lcm_attempt(
                    &attempt,
                    LcmAttemptOutcome::Failed,
                    lcm_attempt_failure_reason(&error),
                    Some(queue_ms),
                    Some(execution_start.elapsed().as_millis() as u64),
                    None,
                    attempts,
                );
                return Err(error);
            }
        }
    };
    drop(permit);
    emit_lcm_attempt(
        &attempt,
        LcmAttemptOutcome::Generated,
        LcmAttemptReason::CandidateReady,
        Some(queue_ms),
        Some(execution_start.elapsed().as_millis() as u64),
        None,
        attempts,
    );
    Ok(CompactionResult {
        summary_text: summary.trim().to_string(),
        atomic_parent_summaries: Vec::new(),
        openai_encrypted_content: None,
        covers_up_to_turn: messages.len(),
        duration_ms: start.elapsed().as_millis() as u64,
        summarized_messages: messages.len(),
    })
}

pub async fn build_transfer_compaction_state(
    provider: Arc<dyn Provider>,
    messages: Vec<Message>,
    existing_state: Option<crate::session::StoredCompactionState>,
    engine: crate::config::CompactionEngine,
) -> Result<Option<crate::session::StoredCompactionState>> {
    let existing_summary = existing_state.as_ref().map(|state| Summary {
        text: state.summary_text.clone(),
        openai_encrypted_content: state.openai_encrypted_content.clone(),
        covers_up_to_turn: state.covers_up_to_turn,
        original_turn_count: state.original_turn_count,
    });

    if messages.is_empty() {
        return Ok(existing_state.map(|mut state| {
            state.compacted_count = 0;
            state
        }));
    }

    let prior_turns = existing_state
        .as_ref()
        .map(|state| state.original_turn_count.max(state.covers_up_to_turn))
        .unwrap_or(0);
    let result = if engine == crate::config::CompactionEngine::Lcm {
        generate_lcm_compaction_artifact_with_attempt(
            provider,
            messages.clone(),
            existing_summary,
            None,
            LcmJobPriority::Critical,
            LcmAttemptContext {
                attempt_id: crate::id::new_id("lcm_attempt"),
                session_id: "transfer".to_string(),
                trigger: "transfer".to_string(),
                stage: "transfer".to_string(),
                configured_route: None,
                effective_route: "transfer-provider".to_string(),
                source_messages: messages.len(),
                pre_tokens: messages
                    .iter()
                    .map(message_char_count)
                    .sum::<usize>()
                    .div_ceil(CHARS_PER_TOKEN) as u64,
                terminal_emitted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        )
        .await?
    } else {
        generate_compaction_artifact(provider, messages.clone(), existing_summary).await?
    };
    let total_turns = prior_turns + messages.len();

    Ok(Some(crate::session::StoredCompactionState {
        summary_text: result.summary_text,
        openai_encrypted_content: result.openai_encrypted_content,
        covers_up_to_turn: total_turns,
        original_turn_count: total_turns,
        compacted_count: 0,
    }))
}
