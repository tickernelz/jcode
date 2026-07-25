use super::{Tool, ToolContext, ToolExecutionMode, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt;
use std::time::Duration;
use std::time::Instant;

/// Hard timeout for discovery requests. Discovery is optional by design: if
/// the endpoint is slow or unreachable the tool fails plainly and the agent
/// continues with its normal toolset. No cache, no offline fallback, no retry.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const DISCOVERY_REQUEST_ID_HEADER: &str = "x-jcode-discovery-request-id";
const DISCOVERY_BENCHMARK_HEADER: &str = "x-jcode-discovery-benchmark";
const DISCOVERY_SESSION_ID_HEADER: &str = "x-jcode-discovery-session-id";
const DISCOVERY_SESSION_METADATA_HEADER: &str = "x-jcode-discovery-session-metadata";
const DISCOVERY_SELF_DEV_HEADER: &str = "x-jcode-discovery-self-dev";
const DISCOVERY_DEBUG_HEADER: &str = "x-jcode-discovery-debug";
const DISCOVERY_CANARY_HEADER: &str = "x-jcode-discovery-canary";
const DISCOVERY_EXECUTION_MODE_HEADER: &str = "x-jcode-discovery-execution-mode";
const DISCOVERY_BUILD_CHANNEL_HEADER: &str = "x-jcode-discovery-build-channel";
const DISCOVERY_GIT_CHECKOUT_HEADER: &str = "x-jcode-discovery-git-checkout";
const DISCOVERY_CI_HEADER: &str = "x-jcode-discovery-ci";
const DISCOVERY_RAN_FROM_CARGO_HEADER: &str = "x-jcode-discovery-ran-from-cargo";
const DISCOVERY_BENCHMARK_ENV: &str = "JCODE_DISCOVERY_BENCHMARK";
const DISCOVERY_QUERY_MIN_CHARS: usize = 20;
const DISCOVERY_QUERY_MAX_CHARS: usize = 500;
const DISCOVERY_REASON_MIN_CHARS: usize = 40;
const DISCOVERY_REASON_MAX_CHARS: usize = 2_000;

fn discovery_benchmark_run() -> bool {
    std::env::var(DISCOVERY_BENCHMARK_ENV)
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
}

#[derive(Debug)]
struct DiscoveryFetchResult {
    listing: Value,
    http_status: u16,
    response_bytes: u64,
}

#[derive(Debug)]
struct DiscoveryFetchError {
    message: String,
    failure_reason: &'static str,
    http_status: Option<u16>,
    response_bytes: Option<u64>,
}

struct DiscoveryRequestContext<'a> {
    client: &'a reqwest::Client,
    endpoint: &'a str,
    request_id: &'a str,
    category: &'a str,
    query: &'a str,
    reason: &'a str,
    benchmark_run: bool,
    provenance: DiscoveryRequestProvenance,
}

#[derive(Debug, Clone)]
struct DiscoveryRequestProvenance {
    session_id: String,
    session_metadata_available: bool,
    is_self_dev: bool,
    is_debug: bool,
    is_canary: bool,
    execution_mode: &'static str,
    build_channel: String,
    is_git_checkout: bool,
    is_ci: bool,
    ran_from_cargo: bool,
}

impl DiscoveryRequestProvenance {
    fn from_tool_context(ctx: &ToolContext) -> Self {
        let session = crate::session::Session::load(&ctx.session_id).ok();
        let runtime = crate::telemetry::runtime_provenance();
        Self {
            session_id: ctx.session_id.clone(),
            session_metadata_available: session.is_some(),
            is_self_dev: session
                .as_ref()
                .is_some_and(|session| session.is_self_dev()),
            is_debug: session.as_ref().is_some_and(|session| session.is_debug),
            is_canary: session.as_ref().is_some_and(|session| session.is_canary),
            execution_mode: match ctx.execution_mode {
                ToolExecutionMode::AgentTurn => "agent_turn",
                ToolExecutionMode::Direct => "direct",
            },
            build_channel: runtime.build_channel,
            is_git_checkout: runtime.is_git_checkout,
            is_ci: runtime.is_ci,
            ran_from_cargo: runtime.ran_from_cargo,
        }
    }

    fn apply(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header(DISCOVERY_SESSION_ID_HEADER, &self.session_id)
            .header(
                DISCOVERY_SESSION_METADATA_HEADER,
                bool_header(self.session_metadata_available),
            )
            .header(DISCOVERY_SELF_DEV_HEADER, bool_header(self.is_self_dev))
            .header(DISCOVERY_DEBUG_HEADER, bool_header(self.is_debug))
            .header(DISCOVERY_CANARY_HEADER, bool_header(self.is_canary))
            .header(DISCOVERY_EXECUTION_MODE_HEADER, self.execution_mode)
            .header(DISCOVERY_BUILD_CHANNEL_HEADER, &self.build_channel)
            .header(
                DISCOVERY_GIT_CHECKOUT_HEADER,
                bool_header(self.is_git_checkout),
            )
            .header(DISCOVERY_CI_HEADER, bool_header(self.is_ci))
            .header(
                DISCOVERY_RAN_FROM_CARGO_HEADER,
                bool_header(self.ran_from_cargo),
            )
    }
}

fn bool_header(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

impl fmt::Display for DiscoveryFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DiscoveryFetchError {}

#[allow(clippy::too_many_arguments)]
fn record_discovery_telemetry(
    request_id: &str,
    started_at: Instant,
    endpoint: &str,
    phase: &str,
    category: Option<&str>,
    selected_tool: Option<&str>,
    outcome: &str,
    failure_reason: Option<&str>,
    http_status: Option<u16>,
    response_bytes: Option<u64>,
    result_count: Option<u32>,
    query_present: bool,
    reason_present: bool,
) {
    crate::telemetry::record_discovery_event(crate::telemetry::DiscoveryTelemetry {
        request_id,
        phase,
        category,
        selected_tool,
        outcome,
        failure_reason,
        http_status,
        latency_ms: started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        response_bytes,
        result_count,
        query_present,
        reason_present,
        benchmark_run: discovery_benchmark_run(),
        endpoint,
    });
}

/// `discover_tools`: fetch discoverable third-party tools for a category from
/// the hosted partner directory.
///
/// Disclosure contract: some providers may share revenue with Jcode, but
/// partnership status never influences recommendations. Every session that
/// uses this tool renders a concise disclosure with a learn-more link on first
/// use. The request carries the category, a short search query, a reason string,
/// and coarse session/build provenance used to separate likely user demand from
/// self-dev and test traffic. It never includes transcript content, file paths,
/// credentials, or user identity.
pub struct DiscoverToolsTool {
    client: reqwest::Client,
}

impl DiscoverToolsTool {
    pub fn new() -> Self {
        Self {
            client: crate::provider::shared_http_client(),
        }
    }
}

#[derive(Deserialize)]
struct DiscoverToolsInput {
    #[serde(default)]
    action: Option<String>,
    category: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    suggestion_kind: Option<String>,
    #[serde(default)]
    product_name: Option<String>,
    #[serde(default)]
    product_url: Option<String>,
    #[serde(default)]
    gap_evidence: Option<String>,
    #[serde(default)]
    requirements: Option<Vec<String>>,
    #[serde(default)]
    prior_request_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoveryAction {
    Browse,
    Select,
    Suggest,
}

impl DiscoveryAction {
    fn parse(action: Option<&str>, has_tool: bool) -> Result<Self> {
        match action.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(if has_tool { Self::Select } else { Self::Browse }),
            Some("browse") if !has_tool => Ok(Self::Browse),
            Some("select") if has_tool => Ok(Self::Select),
            Some("suggest") if !has_tool => Ok(Self::Suggest),
            Some("browse") => Err(anyhow::anyhow!(
                "discovery action 'browse' cannot include `tool`; use action 'select'"
            )),
            Some("select") => Err(anyhow::anyhow!(
                "discovery action 'select' requires the selected `tool` name"
            )),
            Some("suggest") => Err(anyhow::anyhow!(
                "discovery action 'suggest' cannot include `tool`; use `product_name` for a known product"
            )),
            Some(other) => Err(anyhow::anyhow!(
                "unknown discovery action '{other}'. Available: browse, select, suggest"
            )),
        }
    }
}

struct ValidatedSuggestion {
    kind: String,
    product_name: Option<String>,
    product_url: Option<String>,
    gap_evidence: Option<String>,
    requirements: Vec<String>,
    prior_request_id: String,
}

#[derive(Debug)]
struct DiscoveryInputError {
    message: String,
    failure_reason: &'static str,
}

fn validate_discovery_text(
    value: Option<&str>,
    field: &'static str,
    min_chars: usize,
    max_chars: usize,
) -> std::result::Result<String, DiscoveryInputError> {
    let value = value.unwrap_or_default().trim();
    if value.is_empty() {
        return Err(DiscoveryInputError {
            message: format!(
                "discovery {field} is required; write a specific summary without private data"
            ),
            failure_reason: if field == "query" {
                "missing_query"
            } else {
                "missing_reason"
            },
        });
    }

    let chars = value.chars().count();
    if chars < min_chars {
        return Err(DiscoveryInputError {
            message: format!(
                "discovery {field} is too short; provide at least {min_chars} characters of specific, non-private context"
            ),
            failure_reason: if field == "query" {
                "query_too_short"
            } else {
                "reason_too_short"
            },
        });
    }
    if chars > max_chars {
        return Err(DiscoveryInputError {
            message: format!(
                "discovery {field} is too long; summarize it in at most {max_chars} characters without private data"
            ),
            failure_reason: if field == "query" {
                "query_too_long"
            } else {
                "reason_too_long"
            },
        });
    }
    if contains_recognizable_secret(value) {
        return Err(DiscoveryInputError {
            message: format!(
                "discovery {field} appears to contain a secret or financial credential; replace it with a non-sensitive description"
            ),
            failure_reason: if field == "query" {
                "query_sensitive_data"
            } else {
                "reason_sensitive_data"
            },
        });
    }
    if !has_sufficient_detail(value, field) {
        return Err(DiscoveryInputError {
            message: format!(
                "discovery {field} is not specific enough; describe the capability and task constraints in distinct words without private data"
            ),
            failure_reason: if field == "query" {
                "query_not_specific"
            } else {
                "reason_not_specific"
            },
        });
    }
    Ok(value.to_string())
}

fn has_sufficient_detail(value: &str, field: &str) -> bool {
    let words: Vec<String> = value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_ascii_lowercase)
        .collect();
    let mut unique = words.clone();
    unique.sort_unstable();
    unique.dedup();
    let (min_words, min_unique) = if field == "query" { (4, 3) } else { (7, 5) };
    words.len() >= min_words && unique.len() >= min_unique
}

/// A deliberately high-confidence last-line defense before model-authored
/// Discovery text leaves the client. This complements, rather than replaces,
/// the schema instruction to summarize the need instead of copying user data.
fn contains_recognizable_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if (lower.contains("-----begin ") && lower.contains("private key-----"))
        || contains_credential_assignment(&lower)
        || contains_email_address(value)
        || contains_ssn(value)
        || contains_credential_url(value)
        || contains_international_phone_number(value)
    {
        return true;
    }

    if contains_prefixed_secret(value) || contains_payment_card_sequence(value) {
        return true;
    }

    value.split_whitespace().any(|token| {
        let token = token.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
            )
        });
        looks_like_jwt(token)
    }) || contains_bearer_token(&lower)
}

fn contains_prefixed_secret(value: &str) -> bool {
    const SECRET_PREFIXES: &[&str] = &[
        "sk_live_",
        "rk_live_",
        "sk_test_",
        "rk_test_",
        "sk-proj-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
        "npm_",
        "jck_live_",
    ];
    value.split_whitespace().any(|token| {
        let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && !"_-".contains(c));
        let lower = token.to_ascii_lowercase();
        SECRET_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix) && token.len() >= prefix.len() + 8)
            || (token.starts_with("AKIA") && token.len() == 20)
            || (token.starts_with("AIza") && token.len() >= 35)
    })
}

fn contains_credential_assignment(lower: &str) -> bool {
    const LABELS: &[&str] = &[
        "api_key",
        "api-key",
        "apikey",
        "access_token",
        "auth_token",
        "client_secret",
        "secret_key",
        "password",
        "passwd",
    ];
    LABELS.iter().any(|label| {
        lower.match_indices(label).any(|(index, _)| {
            let rest = &lower[index + label.len()..];
            let rest = rest.trim_start();
            let Some(rest) = rest.strip_prefix(['=', ':']) else {
                return false;
            };
            let candidate =
                rest.trim_start_matches(|c: char| c.is_whitespace() || "'\"`".contains(c));
            candidate
                .split(|c: char| c.is_whitespace() || "'\"`,;".contains(c))
                .next()
                .is_some_and(|token| token.len() >= 8)
        })
    })
}

fn contains_bearer_token(lower: &str) -> bool {
    lower.match_indices("bearer ").any(|(index, _)| {
        lower[index + "bearer ".len()..]
            .split_whitespace()
            .next()
            .is_some_and(|token| token.trim_matches(|c: char| ",;.'\"`".contains(c)).len() >= 12)
    })
}

fn contains_email_address(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        let token = token.trim_matches(|c: char| ",;:()[]{}<>\"'`".contains(c));
        let Some((local, domain)) = token.split_once('@') else {
            return false;
        };
        !local.is_empty()
            && domain
                .rsplit_once('.')
                .is_some_and(|(host, suffix)| !host.is_empty() && suffix.len() >= 2)
    })
}

fn contains_ssn(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        let token = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '-');
        let parts: Vec<&str> = token.split('-').collect();
        parts.len() == 3
            && parts[0].len() == 3
            && parts[1].len() == 2
            && parts[2].len() == 4
            && parts
                .iter()
                .all(|part| part.chars().all(|c| c.is_ascii_digit()))
    })
}

fn contains_credential_url(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        let Some((_, rest)) = token.split_once("://") else {
            return false;
        };
        let authority = rest.split('/').next().unwrap_or_default();
        authority.contains('@')
            && authority
                .split('@')
                .next()
                .is_some_and(|user| user.contains(':'))
    })
}

fn contains_international_phone_number(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        if !token.starts_with('+') {
            return false;
        }
        let digits = token.chars().filter(|c| c.is_ascii_digit()).count();
        (10..=15).contains(&digits)
            && token
                .chars()
                .all(|c| c.is_ascii_digit() || "+-().".contains(c))
    })
}

fn looks_like_jwt(token: &str) -> bool {
    token.len() >= 40 && token.starts_with("eyJ") && token.matches('.').count() == 2
}

fn contains_payment_card_sequence(value: &str) -> bool {
    value
        .split(|c: char| !c.is_ascii_digit() && c != '-' && c != ' ')
        .any(|candidate| looks_like_payment_card(candidate.trim()))
}

fn looks_like_payment_card(candidate: &str) -> bool {
    let digits: String = candidate.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(13..=19).contains(&digits.len())
        || candidate
            .chars()
            .any(|c| !c.is_ascii_digit() && c != '-' && c != ' ')
    {
        return false;
    }
    let mut sum = 0u32;
    let parity = digits.len() % 2;
    for (index, byte) in digits.bytes().enumerate() {
        let mut digit = u32::from(byte - b'0');
        if index % 2 == parity {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
    }
    sum.is_multiple_of(10)
}

#[async_trait]
impl Tool for DiscoverToolsTool {
    fn name(&self) -> &str {
        "discover_tools"
    }

    fn description(&self) -> &str {
        "Use proactively whenever fully completing the user's goal requires an external \
         capability, service, account, or data source that available tools do not provide, even \
         if it is needed only in a later step or the user did not ask for a tool. For multi-step \
         tasks, identify every missing capability end to end and call this tool for each relevant \
         category before responding. An empty category is not a reason to stop; do not conclude \
         the task is blocked until all missing capabilities have been checked. Do not treat a \
         prerequisite as covering a later action; check each unsupported external action \
         independently. Search before saying you cannot proceed, asking the user to find a \
         service, or delegating an unsupported step to them. If a browse has no suitable result, \
         submit one `suggest` action for maintainer review, then continue without promising it \
         will be added. Skip discovery when available tools fully cover the task. Every listing is \
         vetted, and one-tool-call setup is enforced. Some providers are Jcode partners and may \
         share revenue with Jcode when a referred user becomes a customer. Partnership status \
         never influences selection. Choose the best option based only on fit."
    }

    fn parameters_schema(&self) -> Value {
        let categories: Vec<&str> = crate::sponsors::DISCOVERY_CATEGORIES.to_vec();
        json!({
            "type": "object",
            "required": ["category", "query", "reason"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["browse", "select", "suggest"],
                    "description": "Discovery phase. Defaults to select when `tool` is set, otherwise browse. Use suggest only after a browse found no suitable catalog entry."
                },
                "category": {
                    "type": "string",
                    "enum": categories,
                    "description": "Missing capability category; infer it from the user's goal."
                },
                "query": {
                    "type": "string",
                    "minLength": DISCOVERY_QUERY_MIN_CHARS,
                    "maxLength": DISCOVERY_QUERY_MAX_CHARS,
                    "description": "Required capability summary. Browse/select text may be sent to relevant partners for demand reporting. Suggest text goes only to Jcode maintainers. Write a fresh summary instead of copying user text. Never include secrets, credentials, personal data, or private content."
                },
                "reason": {
                    "type": "string",
                    "minLength": DISCOVERY_REASON_MIN_CHARS,
                    "maxLength": DISCOVERY_REASON_MAX_CHARS,
                    "description": "Required rationale. For select, explain why the tool fits better than alternatives. For suggest, explain why browse results were unsuitable. Browse/select text may reach relevant partners; suggest text goes only to Jcode maintainers. Never include private data."
                },
                "tool": {
                    "type": "string",
                    "description": "Catalog tool name to select when action=select."
                },
                "suggestion_kind": {
                    "type": "string",
                    "enum": ["known_product", "capability_gap"],
                    "description": "Required for action=suggest. Use known_product only when confident the public product exists; otherwise use capability_gap."
                },
                "product_name": {
                    "type": "string",
                    "minLength": 2,
                    "maxLength": 100,
                    "description": "Required only for a known_product suggestion. Public product, package, service, or MCP name."
                },
                "product_url": {
                    "type": "string",
                    "maxLength": 500,
                    "description": "Optional public HTTPS URL for a known_product suggestion. Never include credentials or private URLs."
                },
                "gap_evidence": {
                    "type": "string",
                    "maxLength": 500,
                    "description": "Optional concise explanation of which browse results were close and why they did not fit. Sent only to Jcode maintainers."
                },
                "requirements": {
                    "type": "array",
                    "maxItems": 8,
                    "items": { "type": "string", "minLength": 3, "maxLength": 240 },
                    "description": "Optional concrete public constraints the catalog addition should satisfy. Sent only to Jcode maintainers."
                },
                "prior_request_id": {
                    "type": "string",
                    "description": "Required for action=suggest. Use the Browse request ID returned by the preceding successful browse in this category."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let started_at = Instant::now();
        let request_id = uuid::Uuid::new_v4().to_string();
        let config = crate::config::config();
        let endpoint = config.sponsors.endpoint.clone();
        let benchmark_run = discovery_benchmark_run();
        if !config.sponsors.enabled {
            record_discovery_telemetry(
                &request_id,
                started_at,
                &endpoint,
                "unknown",
                None,
                None,
                "failure",
                Some("disabled"),
                None,
                None,
                None,
                false,
                false,
            );
            return Err(anyhow::anyhow!(
                "partner discovery is disabled (set [sponsors] enabled = true in config.toml)"
            ));
        }

        let params: DiscoverToolsInput = match serde_json::from_value(input) {
            Ok(params) => params,
            Err(err) => {
                record_discovery_telemetry(
                    &request_id,
                    started_at,
                    &endpoint,
                    "unknown",
                    None,
                    None,
                    "failure",
                    Some("invalid_input"),
                    None,
                    None,
                    None,
                    false,
                    false,
                );
                return Err(err.into());
            }
        };
        let category = params.category.trim().to_ascii_lowercase();
        let query_present = params
            .query
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let reason_present = params
            .reason
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        if !crate::sponsors::DISCOVERY_CATEGORIES.contains(&category.as_str()) {
            record_discovery_telemetry(
                &request_id,
                started_at,
                &endpoint,
                "unknown",
                None,
                None,
                "failure",
                Some("invalid_category"),
                None,
                None,
                None,
                query_present,
                reason_present,
            );
            return Err(anyhow::anyhow!(
                "unknown discovery category '{}'. Available: {}",
                category,
                crate::sponsors::DISCOVERY_CATEGORIES.join(", ")
            ));
        }

        let query = match validate_discovery_text(
            params.query.as_deref(),
            "query",
            DISCOVERY_QUERY_MIN_CHARS,
            DISCOVERY_QUERY_MAX_CHARS,
        ) {
            Ok(query) => query,
            Err(err) => {
                record_discovery_telemetry(
                    &request_id,
                    started_at,
                    &endpoint,
                    "unknown",
                    Some(&category),
                    None,
                    "failure",
                    Some(err.failure_reason),
                    None,
                    None,
                    None,
                    query_present,
                    reason_present,
                );
                return Err(anyhow::anyhow!(err.message));
            }
        };
        let reason = match validate_discovery_text(
            params.reason.as_deref(),
            "reason",
            DISCOVERY_REASON_MIN_CHARS,
            DISCOVERY_REASON_MAX_CHARS,
        ) {
            Ok(reason) => reason,
            Err(err) => {
                record_discovery_telemetry(
                    &request_id,
                    started_at,
                    &endpoint,
                    "unknown",
                    Some(&category),
                    None,
                    "failure",
                    Some(err.failure_reason),
                    None,
                    None,
                    None,
                    query_present,
                    reason_present,
                );
                return Err(anyhow::anyhow!(err.message));
            }
        };

        let tool_selection = params
            .tool
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_ascii_lowercase);
        let action = DiscoveryAction::parse(params.action.as_deref(), tool_selection.is_some())?;
        let discovery_request = DiscoveryRequestContext {
            client: &self.client,
            endpoint: &endpoint,
            request_id: &request_id,
            category: &category,
            query: &query,
            reason: &reason,
            benchmark_run,
            provenance: DiscoveryRequestProvenance::from_tool_context(&ctx),
        };

        if action == DiscoveryAction::Suggest {
            let suggestion = validate_suggestion(&params)?;
            let fetched = match submit_suggestion(&discovery_request, &suggestion).await {
                Ok(result) => result,
                Err(err) => {
                    record_discovery_telemetry(
                        &request_id,
                        started_at,
                        &endpoint,
                        "suggest",
                        Some(&category),
                        None,
                        "failure",
                        Some(err.failure_reason),
                        err.http_status,
                        err.response_bytes,
                        None,
                        query_present,
                        reason_present,
                    );
                    return Err(err.into());
                }
            };
            let rendered =
                render_suggestion(&category, &query, &reason, &suggestion, &fetched.listing)?;
            record_discovery_telemetry(
                &request_id,
                started_at,
                &endpoint,
                "suggest",
                Some(&category),
                None,
                "success",
                None,
                Some(fetched.http_status),
                Some(fetched.response_bytes),
                Some(1),
                query_present,
                reason_present,
            );
            return Ok(ToolOutput::new(rendered)
                .with_title("catalog suggestion".to_string())
                .with_metadata(json!({
                    "catalog_suggestion": true,
                    "category": category,
                    "suggestion_kind": suggestion.kind,
                    "suggestion_status": fetched.listing.get("status").and_then(Value::as_str),
                })));
        }

        // Select phase: return one tool's full setup instructions. The
        // selection (and the agent's reason for it) is recorded server-side.
        if let Some(tool_name) = tool_selection {
            let fetched = match fetch_listing(&discovery_request, Some(&tool_name)).await {
                Ok(result) => result,
                Err(err) => {
                    record_discovery_telemetry(
                        &request_id,
                        started_at,
                        &endpoint,
                        "select",
                        Some(&category),
                        None,
                        "failure",
                        Some(err.failure_reason),
                        err.http_status,
                        err.response_bytes,
                        None,
                        query_present,
                        reason_present,
                    );
                    return Err(err.into());
                }
            };
            let rendered = match render_selection(&category, &tool_name, &fetched.listing) {
                Ok(rendered) => rendered,
                Err(err) => {
                    record_discovery_telemetry(
                        &request_id,
                        started_at,
                        &endpoint,
                        "select",
                        Some(&category),
                        None,
                        "failure",
                        Some("invalid_response"),
                        Some(fetched.http_status),
                        Some(fetched.response_bytes),
                        None,
                        query_present,
                        reason_present,
                    );
                    return Err(err);
                }
            };
            crate::sponsors::provenance::record_discovered_setups(extract_mcp_setups_from(
                fetched
                    .listing
                    .get("tool")
                    .map(std::slice::from_ref)
                    .unwrap_or(&[]),
            ));
            let canonical_tool = fetched
                .listing
                .get("tool")
                .and_then(|tool| tool.get("name"))
                .and_then(Value::as_str);
            record_discovery_telemetry(
                &request_id,
                started_at,
                &endpoint,
                "select",
                Some(&category),
                canonical_tool,
                "success",
                None,
                Some(fetched.http_status),
                Some(fetched.response_bytes),
                Some(1),
                query_present,
                reason_present,
            );
            return Ok(ToolOutput::new(rendered)
                .with_title(format!(
                    "{tool_name} {}",
                    crate::sponsors::DISCOVERY_DISCLOSURE_TAG
                ))
                .with_metadata(json!({
                    "sponsored_discovery": true,
                    "category": category,
                    "selected_tool": tool_name,
                    "disclosure_url": crate::sponsors::DISCOVERY_PARTNERS_URL,
                })));
        }

        let fetched = match fetch_listing(&discovery_request, None).await {
            Ok(result) => result,
            Err(err) => {
                record_discovery_telemetry(
                    &request_id,
                    started_at,
                    &endpoint,
                    "browse",
                    Some(&category),
                    None,
                    "failure",
                    Some(err.failure_reason),
                    err.http_status,
                    err.response_bytes,
                    None,
                    query_present,
                    reason_present,
                );
                return Err(err.into());
            }
        };
        let rendered = match render_listing(&category, &fetched.listing, &request_id) {
            Ok(rendered) => rendered,
            Err(err) => {
                record_discovery_telemetry(
                    &request_id,
                    started_at,
                    &endpoint,
                    "browse",
                    Some(&category),
                    None,
                    "failure",
                    Some("invalid_response"),
                    Some(fetched.http_status),
                    Some(fetched.response_bytes),
                    None,
                    query_present,
                    reason_present,
                );
                return Err(err);
            }
        };
        let result_count = fetched
            .listing
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| tools.len().min(u32::MAX as usize) as u32);

        // Remember MCP setups from this listing so a later `mcp connect`
        // matching one of them is tagged with discovery provenance (and
        // metered coarsely; see jcode_base::sponsors::provenance).
        crate::sponsors::provenance::record_discovered_setups(extract_mcp_setups(&fetched.listing));
        record_discovery_telemetry(
            &request_id,
            started_at,
            &endpoint,
            "browse",
            Some(&category),
            None,
            "success",
            None,
            Some(fetched.http_status),
            Some(fetched.response_bytes),
            result_count,
            query_present,
            reason_present,
        );

        Ok(ToolOutput::new(rendered)
            .with_title(format!(
                "{} {}",
                category,
                crate::sponsors::DISCOVERY_DISCLOSURE_TAG
            ))
            .with_metadata(json!({
                "sponsored_discovery": true,
                "category": category,
                "disclosure_url": crate::sponsors::DISCOVERY_PARTNERS_URL,
            })))
    }
}

/// Fetch a category listing (browse) or one tool's entry (select) from the
/// discovery endpoint. Sends the category, a required capability query, a
/// required reason string, and the selected tool name only. Hard fails on
/// any error: no cache, no fallback, no retry.
async fn fetch_listing(
    context: &DiscoveryRequestContext<'_>,
    tool: Option<&str>,
) -> std::result::Result<DiscoveryFetchResult, DiscoveryFetchError> {
    let endpoint = context.endpoint.trim_end_matches('/');
    let mut request = context.provenance.apply(
        context
            .client
            .get(endpoint)
            .query(&[
                ("category", context.category),
                ("q", context.query),
                ("reason", context.reason),
            ])
            .header(
                reqwest::header::USER_AGENT,
                format!("jcode/{}", env!("CARGO_PKG_VERSION")),
            )
            .header(DISCOVERY_REQUEST_ID_HEADER, context.request_id)
            .timeout(DISCOVERY_TIMEOUT),
    );
    if let Some(tool) = tool.filter(|t| !t.trim().is_empty()) {
        request = request.query(&[("tool", tool.trim())]);
    }
    if context.benchmark_run {
        request = request.header(DISCOVERY_BENCHMARK_HEADER, "1");
    }

    let response = request.send().await.map_err(|err| DiscoveryFetchError {
        message: format!("discovery unavailable: {err}"),
        failure_reason: if err.is_timeout() {
            "timeout"
        } else if err.is_connect() {
            "connect_error"
        } else {
            "transport_error"
        },
        http_status: None,
        response_bytes: None,
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(DiscoveryFetchError {
            message: format!("discovery unavailable: HTTP {status}"),
            failure_reason: "http_error",
            http_status: Some(status.as_u16()),
            response_bytes: response.content_length(),
        });
    }
    let body = response.bytes().await.map_err(|err| DiscoveryFetchError {
        message: format!("discovery unavailable: {err}"),
        failure_reason: "body_error",
        http_status: Some(status.as_u16()),
        response_bytes: None,
    })?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(DiscoveryFetchError {
            message: format!("discovery response too large ({} bytes)", body.len()),
            failure_reason: "response_too_large",
            http_status: Some(status.as_u16()),
            response_bytes: Some(body.len() as u64),
        });
    }
    let listing = serde_json::from_slice(&body).map_err(|err| DiscoveryFetchError {
        message: format!("discovery returned invalid JSON: {err}"),
        failure_reason: "invalid_json",
        http_status: Some(status.as_u16()),
        response_bytes: Some(body.len() as u64),
    })?;
    Ok(DiscoveryFetchResult {
        listing,
        http_status: status.as_u16(),
        response_bytes: body.len() as u64,
    })
}

async fn submit_suggestion(
    context: &DiscoveryRequestContext<'_>,
    suggestion: &ValidatedSuggestion,
) -> std::result::Result<DiscoveryFetchResult, DiscoveryFetchError> {
    let endpoint = format!("{}/suggestions", context.endpoint.trim_end_matches('/'));
    let mut request = context.provenance.apply(
        context
            .client
            .post(endpoint)
            .header(
                reqwest::header::USER_AGENT,
                format!("jcode/{}", env!("CARGO_PKG_VERSION")),
            )
            .header(DISCOVERY_REQUEST_ID_HEADER, context.request_id)
            .json(&json!({
                "category": context.category,
                "query": context.query,
                "reason": context.reason,
                "suggestion_kind": suggestion.kind,
                "product_name": suggestion.product_name,
                "product_url": suggestion.product_url,
                "gap_evidence": suggestion.gap_evidence,
                "requirements": suggestion.requirements,
                "prior_request_id": suggestion.prior_request_id,
            }))
            .timeout(DISCOVERY_TIMEOUT),
    );
    if context.benchmark_run {
        request = request.header(DISCOVERY_BENCHMARK_HEADER, "1");
    }
    let response = request.send().await.map_err(|err| DiscoveryFetchError {
        message: format!("catalog suggestion unavailable: {err}"),
        failure_reason: if err.is_timeout() {
            "timeout"
        } else if err.is_connect() {
            "connect_error"
        } else {
            "transport_error"
        },
        http_status: None,
        response_bytes: None,
    })?;
    let status = response.status();
    let duplicate = status == reqwest::StatusCode::CONFLICT;
    if !status.is_success() && !duplicate {
        return Err(DiscoveryFetchError {
            message: format!("catalog suggestion unavailable: HTTP {status}"),
            failure_reason: "http_error",
            http_status: Some(status.as_u16()),
            response_bytes: response.content_length(),
        });
    }
    let body = response.bytes().await.map_err(|err| DiscoveryFetchError {
        message: format!("catalog suggestion unavailable: {err}"),
        failure_reason: "body_error",
        http_status: Some(status.as_u16()),
        response_bytes: None,
    })?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(DiscoveryFetchError {
            message: format!(
                "catalog suggestion response too large ({} bytes)",
                body.len()
            ),
            failure_reason: "response_too_large",
            http_status: Some(status.as_u16()),
            response_bytes: Some(body.len() as u64),
        });
    }
    let listing = serde_json::from_slice(&body).map_err(|err| DiscoveryFetchError {
        message: format!("catalog suggestion returned invalid JSON: {err}"),
        failure_reason: "invalid_json",
        http_status: Some(status.as_u16()),
        response_bytes: Some(body.len() as u64),
    })?;
    Ok(DiscoveryFetchResult {
        listing,
        http_status: status.as_u16(),
        response_bytes: body.len() as u64,
    })
}

fn validate_suggestion(params: &DiscoverToolsInput) -> Result<ValidatedSuggestion> {
    let kind = params
        .suggestion_kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("action 'suggest' requires `suggestion_kind`"))?;
    if !matches!(kind, "known_product" | "capability_gap") {
        return Err(anyhow::anyhow!(
            "unknown suggestion_kind '{kind}'. Available: known_product, capability_gap"
        ));
    }

    let product_name = params
        .product_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if kind == "known_product" && product_name.is_none() {
        return Err(anyhow::anyhow!(
            "known_product suggestions require a public `product_name`"
        ));
    }
    if kind == "capability_gap" && product_name.is_some() {
        return Err(anyhow::anyhow!(
            "capability_gap suggestions cannot include `product_name`; use known_product instead"
        ));
    }
    if let Some(name) = product_name.as_deref() {
        validate_suggestion_text(name, "product_name", 2, 100, false)?;
    }

    let product_url = normalize_suggestion_url(params.product_url.as_deref())?;
    if kind == "capability_gap" && product_url.is_some() {
        return Err(anyhow::anyhow!(
            "capability_gap suggestions cannot include `product_url`; use known_product instead"
        ));
    }

    let gap_evidence = params
        .gap_evidence
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(evidence) = gap_evidence.as_deref() {
        validate_suggestion_text(evidence, "gap_evidence", 10, 500, true)?;
    }

    let supplied_requirements = params.requirements.as_deref().unwrap_or_default();
    if supplied_requirements.len() > 8 {
        return Err(anyhow::anyhow!(
            "catalog suggestions accept at most 8 public requirements"
        ));
    }
    let requirements = supplied_requirements
        .iter()
        .map(|requirement| {
            let requirement = requirement.trim();
            validate_suggestion_text(requirement, "requirement", 3, 240, false)?;
            Ok(requirement.to_string())
        })
        .collect::<Result<Vec<_>>>()?;

    let prior_request_id = params
        .prior_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("action 'suggest' requires `prior_request_id` from a successful browse")
        })?;
    let parsed = uuid::Uuid::parse_str(prior_request_id)
        .map_err(|_| anyhow::anyhow!("prior_request_id must be a valid browse request UUID"))?;
    if parsed.get_version_num() != 4 {
        return Err(anyhow::anyhow!(
            "prior_request_id must be the version-4 UUID returned by a browse"
        ));
    }

    Ok(ValidatedSuggestion {
        kind: kind.to_string(),
        product_name,
        product_url,
        gap_evidence,
        requirements,
        prior_request_id: prior_request_id.to_string(),
    })
}

fn validate_suggestion_text(
    value: &str,
    field: &str,
    min_chars: usize,
    max_chars: usize,
    require_detail: bool,
) -> Result<()> {
    let chars = value.chars().count();
    if chars < min_chars {
        return Err(anyhow::anyhow!(
            "catalog suggestion {field} is too short; provide at least {min_chars} characters"
        ));
    }
    if chars > max_chars {
        return Err(anyhow::anyhow!(
            "catalog suggestion {field} is too long; use at most {max_chars} characters"
        ));
    }
    if contains_recognizable_secret(value) {
        return Err(anyhow::anyhow!(
            "catalog suggestion {field} appears to contain private or sensitive data"
        ));
    }
    if require_detail && !has_sufficient_detail(value, "query") {
        return Err(anyhow::anyhow!(
            "catalog suggestion {field} is not specific enough"
        ));
    }
    Ok(())
}

fn normalize_suggestion_url(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > 500 {
        return Err(anyhow::anyhow!(
            "catalog suggestion product_url is too long; use at most 500 characters"
        ));
    }
    let mut url = reqwest::Url::parse(value)
        .map_err(|_| anyhow::anyhow!("product_url must be a valid public HTTPS URL"))?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let private_host = host == "localhost"
        || host.ends_with(".local")
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.")
        || host
            .strip_prefix("172.")
            .and_then(|rest| rest.split('.').next())
            .and_then(|octet| octet.parse::<u8>().ok())
            .is_some_and(|octet| (16..=31).contains(&octet));
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || host.is_empty()
        || private_host
    {
        return Err(anyhow::anyhow!(
            "product_url must be a public HTTPS URL without credentials"
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(Some(url.to_string()))
}

/// Extract structured MCP setups (`mcp: { command, args }`) from a listing
/// for provenance matching. Entries without an `mcp` descriptor are skipped.
fn extract_mcp_setups(listing: &Value) -> Vec<crate::sponsors::provenance::DiscoveredSetup> {
    let Some(tools) = listing.get("tools").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    extract_mcp_setups_from(tools)
}

/// Extract MCP setups from a slice of tool entries.
fn extract_mcp_setups_from(tools: &[Value]) -> Vec<crate::sponsors::provenance::DiscoveredSetup> {
    tools
        .iter()
        .filter_map(|tool| {
            let sponsor = tool.get("name")?.as_str()?.trim().to_ascii_lowercase();
            let mcp = tool.get("mcp")?;
            let command = mcp.get("command")?.as_str()?.to_string();
            let args = mcp
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some(crate::sponsors::provenance::DiscoveredSetup {
                sponsor,
                command,
                args,
            })
        })
        .collect()
}

/// Render a discovery listing (browse phase) for the model. Expected shape:
/// `{ "tools": [{ "name": "...", "blurb": "...", "url": "..." }] }`. Setup
/// instructions are not part of browse results: the agent selects a tool
/// (with a reason) to get them.
fn render_listing(category: &str, listing: &Value, request_id: &str) -> Result<String> {
    let tools = listing
        .get("tools")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("discovery returned no tool list"))?;
    if tools.is_empty() {
        return Ok(format!(
            "No discoverable tools in category '{category}' right now.\n\nBrowse request ID: `{request_id}`\n\nIf this catalog gap matters to the task, call discover_tools again with action `suggest` and this `prior_request_id`."
        ));
    }
    let mut out = format!(
        "Discoverable tools in '{category}' (Jcode tool directory; recommendations must be based \
         only on fit; details: {}):\n",
        crate::sponsors::DISCOVERY_PARTNERS_URL
    );
    for tool in tools {
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let blurb = tool.get("blurb").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("\n- {name}: {blurb}"));
        if let Some(url) = tool.get("url").and_then(|v| v.as_str()) {
            out.push_str(&format!(" ({url})"));
        }
        if let Some(setup) = tool.get("setup").and_then(|v| v.as_str()) {
            out.push_str(&format!("\n  setup: {setup}"));
        }
    }
    out.push_str(
        "\n\nOnly select one of these if it is genuinely the best option for the task. \
         To get a tool's setup instructions, call discover_tools again with action `select` \
         and `tool` set to its name. If none is suitable, call it with action `suggest` and \
         the browse request ID below so maintainers receive the catalog gap. Consequential \
         actions (signups, spending) must note the partnership in the confirmation \
         shown to the user.",
    );
    out.push_str(&format!("\n\nBrowse request ID: `{request_id}`"));
    Ok(out)
}

fn render_suggestion(
    category: &str,
    query: &str,
    reason: &str,
    suggestion: &ValidatedSuggestion,
    response: &Value,
) -> Result<String> {
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("catalog suggestion returned no status"))?;
    if !matches!(status, "received" | "duplicate") {
        return Err(anyhow::anyhow!(
            "catalog suggestion returned unknown status '{status}'"
        ));
    }
    let suggestion_id = response
        .get("suggestion_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut out = format!(
        "Catalog suggestion {}.\n\nSuggestion ID: {suggestion_id}\nCategory: {category}\nKind: {}\nCapability: {query}\nCatalog gap: {reason}",
        if status == "duplicate" {
            "already recorded"
        } else {
            "submitted"
        },
        suggestion.kind
    );
    if let Some(name) = suggestion.product_name.as_deref() {
        out.push_str(&format!("\nProduct: {name}"));
    }
    if let Some(url) = suggestion.product_url.as_deref() {
        out.push_str(&format!("\nPublic URL: {url}"));
    }
    if let Some(evidence) = suggestion.gap_evidence.as_deref() {
        out.push_str(&format!("\nGap evidence: {evidence}"));
    }
    if !suggestion.requirements.is_empty() {
        out.push_str("\nRequirements:");
        for requirement in &suggestion.requirements {
            out.push_str(&format!("\n- {requirement}"));
        }
    }
    out.push_str(
        "\n\nStatus: received for Jcode maintainer review. Suggestions are not sent to partners. This does not mean Jcode has partnered with the tool or that it is approved or available.",
    );
    Ok(out)
}

/// Render a selected tool's full entry (select phase). Expected shape:
/// `{ "tool": { "name": "...", "blurb": "...", "url": "...", "setup": "..." } }`.
fn render_selection(category: &str, tool_name: &str, listing: &Value) -> Result<String> {
    let tool = listing
        .get("tool")
        .ok_or_else(|| anyhow::anyhow!("discovery returned no tool entry for '{tool_name}'"))?;
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(tool_name);
    let blurb = tool.get("blurb").and_then(|v| v.as_str()).unwrap_or("");
    let mut out = format!(
        "Selected '{name}' from '{category}' (Jcode tool directory; selection must be based only \
         on fit; details: {}):\n\n{name}: {blurb}",
        crate::sponsors::DISCOVERY_PARTNERS_URL
    );
    if let Some(url) = tool.get("url").and_then(|v| v.as_str()) {
        out.push_str(&format!(" ({url})"));
    }
    if let Some(setup) = tool.get("setup").and_then(|v| v.as_str()) {
        out.push_str(&format!("\n\nSetup: {setup}"));
    }
    out.push_str(
        "\n\nConsequential actions (signups, spending) must note the partnership in \
         the confirmation shown to the user.",
    );
    Ok(out)
}

#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
