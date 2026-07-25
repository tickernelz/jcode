use super::*;
#[derive(Debug, Clone)]
pub struct SessionSearchQueryProfile {
    pub normalized: String,
    pub terms: Vec<String>,
    pub min_term_matches: usize,
}

impl SessionSearchQueryProfile {
    pub fn new(query: &str) -> Self {
        let normalized = query.trim().to_lowercase();
        let terms = tokenize_session_search_query(&normalized);
        let min_term_matches = minimum_session_search_term_matches(terms.len());
        Self {
            normalized,
            terms,
            min_term_matches,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.normalized.is_empty()
    }

    pub fn is_actionable(&self) -> bool {
        !self.normalized.is_empty() && !self.terms.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct SessionSearchMatchScore {
    pub snippet: String,
    pub score: f64,
    pub matched_terms: Vec<String>,
    pub exact_match: bool,
}

pub fn score_session_search_text_match(
    text: &str,
    query: &SessionSearchQueryProfile,
) -> Option<SessionSearchMatchScore> {
    if !query.is_actionable() {
        return None;
    }

    let text_lower = text.to_lowercase();
    let exact_pos = (!query.normalized.is_empty())
        .then(|| text_lower.find(&query.normalized))
        .flatten();

    let mut matched_terms = Vec::new();
    let mut total_term_hits = 0usize;
    let mut first_term_pos = None;

    for term in &query.terms {
        if let Some(pos) = text_lower.find(term) {
            matched_terms.push(term.clone());
            total_term_hits += text_lower.matches(term).count();
            first_term_pos = Some(first_term_pos.map_or(pos, |current: usize| current.min(pos)));
        }
    }

    if exact_pos.is_none() && matched_terms.len() < query.min_term_matches {
        return None;
    }

    let anchor = exact_pos.or(first_term_pos);
    let snippet = extract_session_search_snippet(text, anchor, query, 280);
    let coverage = matched_terms.len() as f64 / query.terms.len() as f64;
    let score = if exact_pos.is_some() { 4.0 } else { 0.0 }
        + coverage * 3.0
        + matched_terms.len() as f64 * 0.25
        + (total_term_hits as f64 / (text.len() as f64 + 1.0)) * 200.0;

    Some(SessionSearchMatchScore {
        snippet,
        score,
        matched_terms,
        exact_match: exact_pos.is_some(),
    })
}

pub fn session_search_raw_matches_query(raw: &[u8], query: &SessionSearchQueryProfile) -> bool {
    if !query.is_actionable() {
        return false;
    }

    if query.normalized.is_ascii() {
        if contains_case_insensitive_bytes(raw, query.normalized.as_bytes()) {
            return true;
        }
        let matched_terms = query
            .terms
            .iter()
            .filter(|term| contains_case_insensitive_bytes(raw, term.as_bytes()))
            .count();
        return matched_terms >= query.min_term_matches;
    }

    let Ok(raw_text) = std::str::from_utf8(raw) else {
        return false;
    };
    normalized_session_search_text_matches(&raw_text.to_lowercase(), query)
}

pub fn session_search_path_matches_query(
    path_text: &str,
    query: &SessionSearchQueryProfile,
) -> bool {
    normalized_session_search_text_matches(&path_text.to_lowercase(), query)
}

pub fn normalized_session_search_text_matches(
    text_lower: &str,
    query: &SessionSearchQueryProfile,
) -> bool {
    if !query.is_actionable() {
        return false;
    }
    if text_lower.contains(&query.normalized) {
        return true;
    }
    query
        .terms
        .iter()
        .filter(|term| text_lower.contains(term.as_str()))
        .count()
        >= query.min_term_matches
}

pub fn tokenize_session_search_query(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = HashSet::new();

    for token in query.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }

        let token = token.to_lowercase();
        if is_session_search_stop_word(&token) {
            continue;
        }

        let keep = token.chars().count() >= 2 || token.chars().all(|c| c.is_ascii_digit());
        if keep && seen.insert(token.clone()) {
            terms.push(token);
        }
    }

    terms
}

pub fn is_session_search_stop_word(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "but"
            | "by"
            | "for"
            | "from"
            | "how"
            | "i"
            | "in"
            | "into"
            | "is"
            | "it"
            | "my"
            | "of"
            | "on"
            | "or"
            | "our"
            | "that"
            | "the"
            | "their"
            | "this"
            | "to"
            | "we"
            | "what"
            | "when"
            | "where"
            | "which"
            | "with"
            | "you"
            | "your"
    )
}

pub fn minimum_session_search_term_matches(term_count: usize) -> usize {
    match term_count {
        0 => 0,
        1 => 1,
        2 => 2,
        3..=5 => 2,
        _ => 3,
    }
}

/// Fast case-insensitive byte search. Avoids allocating a lowercase copy of the
/// entire file for the common ASCII-query case.
///
/// Uses memchr's SIMD-accelerated scan to find candidate positions for the
/// first needle byte (both cases when it is an ASCII letter), then verifies the
/// remainder case-insensitively. This is orders of magnitude faster than a
/// naive byte-by-byte scan over multi-megabyte session files.
pub fn contains_case_insensitive_bytes(haystack: &[u8], needle_lower: &[u8]) -> bool {
    let Some((&first_lower, rest)) = needle_lower.split_first() else {
        return true;
    };
    if haystack.len() < needle_lower.len() {
        return false;
    }

    let search_end = haystack.len() - needle_lower.len();
    let first_upper = first_lower.to_ascii_uppercase();
    let mut offset = 0;
    while offset <= search_end {
        let window = &haystack[offset..];
        let found = if first_upper == first_lower {
            memchr::memchr(first_lower, window)
        } else {
            memchr::memchr2(first_lower, first_upper, window)
        };
        let Some(pos) = found else {
            return false;
        };
        let candidate = offset + pos;
        if candidate > search_end {
            return false;
        }
        if haystack[candidate + 1..candidate + needle_lower.len()]
            .iter()
            .zip(rest)
            .all(|(&hb, &nb)| hb.to_ascii_lowercase() == nb)
        {
            return true;
        }
        offset = candidate + 1;
    }
    false
}

pub fn session_search_working_dir_matches(session_wd: &str, filter: &str) -> bool {
    let session_norm = normalize_path_for_session_search_match(session_wd);
    let filter_norm = normalize_path_for_session_search_match(filter);
    if filter_norm.is_empty() {
        return true;
    }

    if session_norm == filter_norm {
        return true;
    }

    let filter_with_sep = format!("{filter_norm}/");
    if session_norm.starts_with(&filter_with_sep) {
        return true;
    }

    // If the user supplied only a project name or path fragment, keep substring
    // matching as a fallback. This preserves the previous loose behavior while
    // making absolute path filters deterministic above.
    !filter_norm.contains('/') && session_norm.contains(&filter_norm)
}

pub fn session_search_truncate_title_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        format!(
            "{}…",
            trimmed
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub fn session_search_field_filter_matches(value: Option<&str>, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    value
        .map(|value| value.to_ascii_lowercase().contains(filter))
        .unwrap_or(false)
}

pub fn session_search_datetime_matches(
    value: chrono::DateTime<chrono::Utc>,
    after: Option<chrono::DateTime<chrono::Utc>>,
    before: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    if after.is_some_and(|after| value < after) {
        return false;
    }
    if before.is_some_and(|before| value > before) {
        return false;
    }
    true
}

pub fn session_search_format_matched_terms(terms: &[String]) -> String {
    if terms.is_empty() {
        return "matched exact phrase".to_string();
    }
    let rendered = terms
        .iter()
        .take(8)
        .map(|term| format!("`{term}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if terms.len() > 8 {
        format!("matched terms {rendered}, ...")
    } else {
        format!("matched terms {rendered}")
    }
}

pub fn session_search_markdown_code_block(text: &str) -> String {
    let longest_backtick_run = longest_repeated_char_run(text, '`');
    let fence_len = if longest_backtick_run >= 3 {
        longest_backtick_run + 1
    } else {
        3
    };
    let fence = "`".repeat(fence_len);
    format!("{fence}text\n{text}\n{fence}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSearchResultKind {
    Metadata,
    Message,
}

impl SessionSearchResultKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Message => "message",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionSearchContextLine {
    pub message_index: usize,
    pub role: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct SessionSearchResult {
    pub source: String,
    pub session_id: String,
    pub short_name: Option<String>,
    pub title: Option<String>,
    pub working_dir: Option<String>,
    pub provider_key: Option<String>,
    pub model: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub kind: SessionSearchResultKind,
    pub role: String,
    pub message_index: Option<usize>,
    pub message_id: Option<String>,
    pub message_timestamp: Option<DateTime<Utc>>,
    pub snippet: String,
    pub score: f64,
    pub matched_terms: Vec<String>,
    pub exact_match: bool,
    pub context: Vec<SessionSearchContextLine>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionSearchReport {
    pub results: Vec<SessionSearchResult>,
    pub scanned_jcode_sessions: usize,
    pub candidate_jcode_sessions: usize,
    pub scanned_external_sessions: usize,
    pub external_sources: Vec<&'static str>,
    pub read_errors: usize,
    pub parse_errors: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionSearchRenderOptions {
    pub include_current: bool,
    pub include_external: bool,
    pub include_tools: bool,
    pub include_system: bool,
    pub max_per_session: usize,
    pub has_working_dir_filter: bool,
}

pub fn format_session_search_results(
    query: &str,
    report: &SessionSearchReport,
    options: &SessionSearchRenderOptions,
) -> String {
    let results = &report.results;
    let mut output = format!(
        "## Found {} results for '{}'\n\n",
        results.len(),
        query.trim()
    );

    output.push_str(&format!(
        "_Defaults: current session {}, external sources {}, tool calls/results {}, system reminders {}. Max per session: {}._\n\n",
        if options.include_current { "included" } else { "excluded" },
        if options.include_external { "included" } else { "hidden" },
        if options.include_tools { "included" } else { "hidden" },
        if options.include_system { "included" } else { "hidden" },
        options.max_per_session,
    ));

    output.push_str(&format!(
        "_Scanned: {} Jcode sessions ({} candidates), {} external sessions{}{}._\n\n",
        report.scanned_jcode_sessions,
        report.candidate_jcode_sessions,
        report.scanned_external_sessions,
        if report.external_sources.is_empty() {
            String::new()
        } else {
            format!(" from {}", report.external_sources.join(", "))
        },
        if report.truncated {
            "; scan truncated"
        } else {
            ""
        },
    ));

    for (i, result) in results.iter().enumerate() {
        let session_name = result
            .short_name
            .as_deref()
            .or(result.title.as_deref())
            .unwrap_or(&result.session_id);
        output.push_str(&format!("### Result {} - {}\n", i + 1, session_name));
        output.push_str(&format!("- Source: `{}`\n", result.source));
        output.push_str(&format!("- Session ID: `{}`\n", result.session_id));
        if let Some(title) = &result.title {
            output.push_str(&format!("- Title: {}\n", title));
        }
        if let Some(dir) = &result.working_dir {
            output.push_str(&format!("- Working dir: `{}`\n", dir));
        }
        if let Some(provider_key) = &result.provider_key {
            output.push_str(&format!("- Provider: `{}`\n", provider_key));
        }
        if let Some(model) = &result.model {
            output.push_str(&format!("- Model: `{}`\n", model));
        }
        output.push_str(&format!(
            "- Updated: {}\n- Match: {}",
            session_search_format_datetime(result.updated_at),
            result.kind.label(),
        ));
        if let Some(index) = result.message_index {
            output.push_str(&format!(" #{}", index + 1));
        }
        output.push_str(&format!(" ({})", result.role));
        if let Some(message_id) = &result.message_id {
            output.push_str(&format!(", id `{}`", message_id));
        }
        if let Some(timestamp) = result.message_timestamp {
            output.push_str(&format!(
                ", at {}",
                session_search_format_datetime(timestamp)
            ));
        }
        output.push('\n');
        output.push_str(&format!(
            "- Why: {}{}\n",
            if result.exact_match {
                "exact phrase; "
            } else {
                ""
            },
            session_search_format_matched_terms(&result.matched_terms),
        ));
        output.push('\n');
        output.push_str(&session_search_markdown_code_block(&result.snippet));
        if !result.context.is_empty() {
            output.push_str("\n\nContext:\n");
            for context in &result.context {
                output.push_str(&format!(
                    "- #{} {}{}\n",
                    context.message_index + 1,
                    context.role,
                    context
                        .timestamp
                        .map(|ts| format!(" at {}", session_search_format_datetime(ts)))
                        .unwrap_or_default()
                ));
                output.push_str(&session_search_markdown_code_block(&context.text));
                output.push('\n');
            }
        }
        output.push_str("\n\n");
    }

    output
}

pub fn format_session_search_no_results(
    query: &str,
    options: &SessionSearchRenderOptions,
) -> String {
    let mut output = format!("No results found for '{}' in past sessions.", query.trim());
    let mut hints = Vec::new();
    if !options.include_current {
        hints.push(
            "current session is excluded by default; retry with include_current=true if needed",
        );
    }
    if !options.include_tools {
        hints.push(
            "tool calls/results are hidden by default; retry with include_tools=true for raw logs",
        );
    }
    if !options.include_system {
        hints.push("system reminders are hidden by default; retry with include_system=true for internal context");
    }
    if options.has_working_dir_filter {
        hints.push("the working_dir filter may be too narrow");
    }
    if !hints.is_empty() {
        output.push_str("\n\nSearch notes:\n");
        for hint in hints {
            output.push_str("- ");
            output.push_str(hint);
            output.push('\n');
        }
    }
    output
}

pub fn session_search_format_datetime(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn longest_repeated_char_run(text: &str, needle: char) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for ch in text.chars() {
        if ch == needle {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

pub fn normalize_path_for_session_search_match(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// Extract a snippet around the first match.
pub fn extract_session_search_snippet(
    text: &str,
    anchor: Option<usize>,
    query: &SessionSearchQueryProfile,
    max_len: usize,
) -> String {
    if let Some(pos) = anchor {
        let focus_len = if !query.normalized.is_empty() {
            query.normalized.len()
        } else {
            query.terms.first().map(|term| term.len()).unwrap_or(0)
        };
        let start = pos.saturating_sub(max_len / 2);
        let end = (pos + focus_len + max_len / 2).min(text.len());

        let start = floor_char_boundary(text, start);
        let end = ceil_char_boundary(text, end);

        let start = text[..start]
            .rfind(char::is_whitespace)
            .map(|p| p + 1)
            .unwrap_or(start);
        let end = text[end..]
            .find(char::is_whitespace)
            .map(|p| end + p)
            .unwrap_or(end);

        let mut snippet = text[start..end].to_string();
        if start > 0 {
            snippet = format!("...{}", snippet);
        }
        if end < text.len() {
            snippet = format!("{}...", snippet);
        }
        snippet
    } else {
        text.chars().take(max_len).collect()
    }
}

fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut idx = i;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut idx = i;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx.min(s.len())
}

#[cfg(test)]
mod session_search_tests {
    use super::*;

    #[test]
    fn query_profile_filters_stop_words_and_requires_actionable_terms() {
        let empty = SessionSearchQueryProfile::new("the and of");
        assert!(!empty.is_actionable());

        let query = SessionSearchQueryProfile::new("AirPods reconnect bluetooth bluetooth");
        assert_eq!(query.terms, vec!["airpods", "reconnect", "bluetooth"]);
        assert_eq!(query.min_term_matches, 2);
        assert!(query.is_actionable());
    }

    #[test]
    fn score_text_match_handles_token_overlap_without_exact_phrase() {
        let query = SessionSearchQueryProfile::new("airpods reconnect bluetooth");
        let score = score_session_search_text_match(
            "Try reconnecting your AirPods after the Bluetooth audio drops.",
            &query,
        )
        .expect("token overlap should match");

        assert!(!score.exact_match);
        assert!(score.matched_terms.contains(&"airpods".to_string()));
        assert!(score.snippet.to_lowercase().contains("airpods"));
    }

    #[test]
    fn raw_and_path_matching_are_case_insensitive() {
        let query = SessionSearchQueryProfile::new("Project Needle");
        assert!(session_search_raw_matches_query(
            b"logs mention project needle here",
            &query
        ));
        assert!(session_search_path_matches_query(
            "/TMP/PROJECT/NEEDLE.json",
            &query
        ));
    }

    #[test]
    fn contains_case_insensitive_bytes_matches_naive_reference() {
        fn naive(haystack: &[u8], needle_lower: &[u8]) -> bool {
            if needle_lower.is_empty() {
                return true;
            }
            haystack
                .windows(needle_lower.len())
                .any(|window| window.eq_ignore_ascii_case(needle_lower))
        }

        let haystacks: &[&[u8]] = &[
            b"",
            b"a",
            b"A",
            b"needle",
            b"NEEDLE",
            b"NeEdLe",
            b"xxneedle",
            b"needlexx",
            b"xxNEEDLExx",
            b"nee",
            b"neenee needle",
            b"nnnnnnnnnn",
            b"9needle9",
            b"\xff\xfeNEEDLE\xff",
            b"the ne edle is split",
            b"ends with nee",
            b"NEEDLNEEDLE",
        ];
        let needles: &[&[u8]] = &[b"", b"n", b"needle", b"9", b"\xff", b"ee", b"edle"];

        for haystack in haystacks {
            for needle in needles {
                assert_eq!(
                    contains_case_insensitive_bytes(haystack, needle),
                    naive(haystack, needle),
                    "mismatch for haystack={haystack:?} needle={needle:?}"
                );
            }
        }
    }

    #[test]
    fn working_dir_match_is_case_insensitive_and_prefix_based() {
        assert!(session_search_working_dir_matches(
            "/tmp/Project/Subdir",
            "/TMP/project"
        ));
        assert!(session_search_working_dir_matches(
            "/workspace/jcode",
            "jcode"
        ));
        assert!(!session_search_working_dir_matches(
            "/workspace/jcode",
            "/workspace/other"
        ));
    }

    #[test]
    fn snippet_respects_utf8_boundaries() {
        let query = SessionSearchQueryProfile::new("needle");
        let text = "αβγ before needle after δεζ";
        let snippet = extract_session_search_snippet(text, text.find("needle"), &query, 12);
        assert!(snippet.contains("needle"));
    }

    #[test]
    fn formatting_helpers_are_stable() {
        assert_eq!(session_search_truncate_title_text("  abcdef  ", 4), "abc…");
        assert!(session_search_field_filter_matches(
            Some("Claude Sonnet"),
            Some("sonnet")
        ));
        assert!(!session_search_field_filter_matches(None, Some("sonnet")));
        assert_eq!(
            session_search_format_matched_terms(&["alpha".to_string(), "beta".to_string()]),
            "matched terms `alpha`, `beta`"
        );

        let fenced = session_search_markdown_code_block("contains ``` fence");
        assert!(fenced.starts_with("````text\n"));
        assert!(fenced.ends_with("\n````"));
    }
}

#[cfg(test)]
mod context_graph_tests {
    use super::*;

    fn node(id: &str, children: &[&str]) -> StoredContextNode {
        StoredContextNode {
            id: id.to_string(),
            schema_version: 2,
            level: u32::from(!children.is_empty()),
            source_session_id: "session".to_string(),
            source_message_ids: vec![format!("message-{id}")],
            source_sha256: "a".repeat(64),
            source_runtime_identity_sha256: "b".repeat(64),
            child_node_ids: children.iter().map(|id| (*id).to_string()).collect(),
            summary_text: id.to_string(),
            summary_sha256: Some("d".repeat(64)),
            estimated_tokens: 1,
            summarizer_model: "model".to_string(),
            summarizer_provider: "provider".to_string(),
            summarizer_route: "route".to_string(),
            summarizer_runtime_identity_sha256: "c".repeat(64),
            prompt_schema_version: 1,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn graph_rejects_duplicate_ids() {
        let err = validate_context_graph(&[node("a", &[]), node("a", &[])], None)
            .expect_err("duplicate IDs must fail");
        assert!(err.contains("duplicate"));
    }

    #[test]
    fn graph_rejects_legacy_node_schema_without_identity_provenance_contract() {
        let mut legacy = node("legacy", &[]);
        legacy.schema_version = 1;
        let err = validate_context_graph(&[legacy], None)
            .expect_err("schema v1 nodes must be rebuilt from canonical raw history");
        assert!(err.contains("invalid metadata"));
    }

    #[test]
    fn graph_rejects_missing_summary_proof() {
        let mut unproven = node("unproven", &[]);
        unproven.summary_sha256 = None;
        let err = validate_context_graph(&[unproven], None)
            .expect_err("schema v2 nodes must carry a summary integrity proof");
        assert!(err.contains("invalid metadata"));
    }

    #[test]
    fn graph_rejects_missing_children() {
        let err = validate_context_graph(&[node("parent", &["missing"])], None)
            .expect_err("missing children must fail");
        assert!(err.contains("missing child"));
    }

    #[test]
    fn graph_rejects_cycles() {
        let err = validate_context_graph(&[node("a", &["b"]), node("b", &["a"])], None)
            .expect_err("cycles must fail");
        assert!(err.contains("cycle"));
    }
}
