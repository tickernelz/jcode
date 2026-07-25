use super::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PremiumMode {
    Normal = 0,
    OnePerSession = 1,
    Zero = 2,
}

/// Explicit OAuth-vs-API-key credential pin for dual-auth providers
/// (Anthropic and OpenAI). `Auto` means "prefer OAuth when present, fall back
/// to an API key"; the explicit variants pin one route for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CredentialMode {
    #[default]
    Auto,
    OAuth,
    ApiKey,
}

impl CredentialMode {
    /// Resolve the runtime-env pin (JCODE_*_AUTH / route aliases) for a
    /// dual-auth provider through the canonical auth_mode vocabulary.
    pub fn from_runtime_env(provider: DualAuthProvider) -> Self {
        match runtime_env_pinned_mode(provider) {
            Some(AuthMode::ApiKey) => Self::ApiKey,
            Some(AuthMode::Oauth) => Self::OAuth,
            None => Self::Auto,
        }
    }

    /// The canonical dual-auth route this explicit mode pins, if any.
    /// `Auto` has no explicit pin and returns `None`.
    pub fn auth_route(self, provider: DualAuthProvider) -> Option<AuthRoute> {
        match (self, provider) {
            (Self::Auto, _) => None,
            (Self::OAuth, DualAuthProvider::Anthropic) => {
                Some(AuthRoute::anthropic(AuthMode::Oauth))
            }
            (Self::ApiKey, DualAuthProvider::Anthropic) => {
                Some(AuthRoute::anthropic(AuthMode::ApiKey))
            }
            (Self::OAuth, DualAuthProvider::OpenAI) => Some(AuthRoute::openai(AuthMode::Oauth)),
            (Self::ApiKey, DualAuthProvider::OpenAI) => Some(AuthRoute::openai(AuthMode::ApiKey)),
        }
    }
}

/// Channel for sending provider-native tool results back to a provider bridge.
pub type NativeToolResultSender = tokio::sync::mpsc::Sender<NativeToolResult>;

/// Native tool result to send back to provider bridges that delegate tool execution to jcode.
#[derive(Debug, Clone, Serialize)]
pub struct NativeToolResult {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub request_id: String,
    pub result: NativeToolResultPayload,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeToolResultPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl NativeToolResult {
    pub fn success(request_id: String, output: String) -> Self {
        Self {
            msg_type: "native_tool_result",
            request_id,
            result: NativeToolResultPayload {
                output: Some(output),
                error: None,
            },
            is_error: false,
        }
    }

    pub fn error(request_id: String, error: String) -> Self {
        Self {
            msg_type: "native_tool_result",
            request_id,
            result: NativeToolResultPayload {
                output: None,
                error: Some(error),
            },
            is_error: true,
        }
    }
}

/// Canonical User-Agent for generic outbound Jcode HTTP requests.
pub const JCODE_USER_AGENT: &str = concat!("jcode/", env!("CARGO_PKG_VERSION"));

/// Read an HTTP error body without hiding failures behind an empty string.
///
/// This is useful after a non-success status when the response is about to be
/// converted into an error. If reading the body itself fails, the returned text
/// preserves that failure so callers can include it in their error message.
pub async fn http_error_body(response: reqwest::Response, context: &str) -> String {
    match response.text().await {
        Ok(body) => body,
        Err(err) => format!("<failed to read {context} response body: {err}>"),
    }
}

/// Shared HTTP client for all generic provider requests. Creating a `reqwest::Client` is expensive
/// (~10ms due to TLS init, connection pool setup), so we reuse a single instance. Provider-specific
/// transports may override the User-Agent on individual requests when they intentionally need to
/// match an official client.
pub fn shared_http_client() -> reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(JCODE_USER_AGENT)
                .connect_timeout(Duration::from_secs(15))
                .tcp_keepalive(Some(Duration::from_secs(30)))
                // Proactively detect half-dead pooled HTTP/2 connections before we
                // reuse them. Without keepalive pings, a stale multiplexed connection
                // (common behind NAT/VPN/proxy or flaky Wi-Fi) surfaces as
                // "http2 error: stream error received: unspecific protocol error".
                // Pinging while idle lets reqwest drop the connection instead.
                .http2_keep_alive_interval(Some(Duration::from_secs(30)))
                .http2_keep_alive_timeout(Duration::from_secs(15))
                .http2_keep_alive_while_idle(true)
                .pool_idle_timeout(Duration::from_secs(90))
                .pool_max_idle_per_host(8)
                .build()
                .unwrap_or_else(|err| {
                    eprintln!("jcode: failed to build shared provider HTTP client: {err}");
                    match reqwest::Client::builder()
                        .user_agent(JCODE_USER_AGENT)
                        .build()
                    {
                        Ok(client) => client,
                        Err(fallback_err) => {
                            eprintln!(
                                "jcode: failed to build fallback provider HTTP client: {fallback_err}"
                            );
                            reqwest::Client::new()
                        }
                    }
                })
        })
        .clone()
}

/// Fresh HTTP client for transport-fault retries.
///
/// Retrying on the shared pooled client can reuse *other* idle connections
/// established through the same broken network path (corrupting middlebox,
/// flaky NAT/VPN) that produced a TLS fault like `BadRecordMac` - so the
/// retry fails the same way. This client disables connection pooling, which
/// guarantees the retry opens a brand-new TCP+TLS connection (the property
/// that makes transport-fault retries actually succeed). Building a client
/// costs ~10ms, which is fine on a retry path that already backs off >=1s.
pub fn fresh_transport_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(JCODE_USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .http2_keep_alive_interval(Some(Duration::from_secs(30)))
        .http2_keep_alive_timeout(Duration::from_secs(15))
        .http2_keep_alive_while_idle(true)
        // No pooled reuse: every request gets a fresh connection.
        .pool_max_idle_per_host(0)
        .build()
        .unwrap_or_else(|_| shared_http_client())
}

#[derive(Debug, Clone)]
pub struct NativeCompactionResult {
    pub summary_text: Option<String>,
    pub openai_encrypted_content: Option<String>,
}

/// A single route to access a model: model + provider + API method
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRoute {
    pub model: String,
    pub provider: String,
    pub api_method: String,
    pub available: bool,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheapness: Option<RouteCheapnessEstimate>,
}

/// Exact runtime identity for a selected model route.
///
/// A runtime key identifies the concrete endpoint/auth/account slot that will
/// send requests. It is intentionally more precise than a display provider
/// label: for example, OpenRouter and NVIDIA NIM both speak an OpenAI-compatible
/// protocol, but they must have different runtime keys because they use
/// different endpoints, auth, catalogs, and routing semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RuntimeKey {
    JcodeSubscription,
    ClaudeOAuth,
    AnthropicApiKey,
    OpenAIOAuth,
    OpenAIApiKey,
    OpenRouter,
    OpenAiCompatible {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile_id: Option<String>,
    },
    Copilot,
    Gemini,
    Cursor,
    Bedrock,
    Antigravity,
    CodeAssistOAuth,
    RemoteCatalog,
    Current,
    Other(String),
}

impl RuntimeKey {
    pub fn from_api_method(api_method: &ModelRouteApiMethod, _provider_label: &str) -> Self {
        match api_method {
            ModelRouteApiMethod::JcodeSubscription => Self::JcodeSubscription,
            ModelRouteApiMethod::ClaudeOAuth => Self::ClaudeOAuth,
            ModelRouteApiMethod::AnthropicApiKey => Self::AnthropicApiKey,
            ModelRouteApiMethod::OpenAIOAuth => Self::OpenAIOAuth,
            ModelRouteApiMethod::OpenAIApiKey => Self::OpenAIApiKey,
            ModelRouteApiMethod::OpenRouter => Self::OpenRouter,
            ModelRouteApiMethod::OpenAiCompatible { profile_id } => Self::OpenAiCompatible {
                profile_id: profile_id.clone(),
            },
            ModelRouteApiMethod::Copilot => Self::Copilot,
            ModelRouteApiMethod::Cursor => Self::Cursor,
            ModelRouteApiMethod::Bedrock => Self::Bedrock,
            ModelRouteApiMethod::CodeAssistOAuth => Self::CodeAssistOAuth,
            ModelRouteApiMethod::AntigravityHttps => Self::Antigravity,
            ModelRouteApiMethod::RemoteCatalog => Self::RemoteCatalog,
            ModelRouteApiMethod::Current => Self::Current,
            ModelRouteApiMethod::Other(method) => Self::Other(method.clone()),
        }
    }

    pub fn stable_id(&self) -> String {
        match self {
            Self::JcodeSubscription => "jcode-subscription".to_string(),
            Self::ClaudeOAuth => "claude-oauth".to_string(),
            Self::AnthropicApiKey => "anthropic-api-key".to_string(),
            Self::OpenAIOAuth => "openai-oauth".to_string(),
            Self::OpenAIApiKey => "openai-api-key".to_string(),
            Self::OpenRouter => "openrouter".to_string(),
            Self::OpenAiCompatible { profile_id } => profile_id
                .as_deref()
                .map(|profile_id| format!("openai-compatible:{profile_id}"))
                .unwrap_or_else(|| "openai-compatible".to_string()),
            Self::Copilot => "copilot".to_string(),
            Self::Gemini => "gemini".to_string(),
            Self::Cursor => "cursor".to_string(),
            Self::Bedrock => "bedrock".to_string(),
            Self::Antigravity => "antigravity".to_string(),
            Self::CodeAssistOAuth => "code-assist-oauth".to_string(),
            Self::RemoteCatalog => "remote-catalog".to_string(),
            Self::Current => "current".to_string(),
            Self::Other(value) => value.clone(),
        }
    }
}

/// Structured model route selection.
///
/// This is the internal source of truth for picker/RPC driven model selection.
/// Human string specs such as `openai-api:gpt-5` should be parsed into this type
/// at the command boundary instead of being used as the runtime identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteSelection {
    pub model: String,
    pub runtime_key: RuntimeKey,
    pub api_method: String,
    pub provider_label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Exact, non-secret identity of the runtime that owns a session and any
/// provider-side resumable conversation bound to it.
///
/// Account IDs are locally generated stable opaque identifiers when an upstream
/// provider does not expose one. `account_generation` changes when credentials
/// for that account are replaced or refreshed. This lets resume fail closed on
/// an account swap even when the human label and model are unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExactRuntimeIdentity {
    pub provider_key: String,
    pub route: RouteSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl ExactRuntimeIdentity {
    /// Provider-side conversation IDs are credential-scoped. An unlabeled or
    /// partially migrated credential cannot prove that a future request uses
    /// the same account, so resumability must remain disabled for it.
    pub fn has_verifiable_account_binding(&self) -> bool {
        self.account_label.is_some()
            && self.account_id.is_some()
            && self.account_generation.is_some()
    }
}

impl RouteSelection {
    pub fn from_model_route(route: &ModelRoute) -> Self {
        let api_method = route.api_method_kind();
        Self {
            model: route.model.clone(),
            runtime_key: RuntimeKey::from_api_method(&api_method, &route.provider),
            api_method: route.api_method.clone(),
            provider_label: route.provider.clone(),
            detail: route.detail.clone(),
        }
    }

    /// The string model spec that applies this route selection, including any
    /// provider routing prefix/suffix (`openai-oauth:`, `claude-api:`,
    /// `openai/gpt-5@OpenAI`, `copilot:`, ...).
    ///
    /// This is the single source of truth for translating a structured
    /// [`RouteSelection`] back into the `set_model` spec string. Both the
    /// trait-default `set_route_selection` and `MultiProvider`'s override use
    /// it so the routing-prefix policy is never duplicated or allowed to
    /// drift between provider implementations.
    pub fn routed_model_spec(&self) -> String {
        let model = self.model.trim();
        match &self.runtime_key {
            RuntimeKey::JcodeSubscription => model.to_string(),
            RuntimeKey::ClaudeOAuth => format!("claude-oauth:{model}"),
            RuntimeKey::AnthropicApiKey => format!("claude-api:{model}"),
            RuntimeKey::OpenAIOAuth => format!("openai-oauth:{model}"),
            RuntimeKey::OpenAIApiKey => format!("openai-api:{model}"),
            RuntimeKey::OpenAiCompatible {
                profile_id: Some(profile_id),
            } => format!("{}:{model}", profile_id.trim()),
            RuntimeKey::OpenAiCompatible { profile_id: None } => model.to_string(),
            RuntimeKey::OpenRouter => {
                let provider = self.provider_label.trim();
                let catalog_id = openrouter_catalog_model_id(model);
                if provider.is_empty()
                    || provider.eq_ignore_ascii_case("auto")
                    || model.contains('@')
                {
                    catalog_id
                } else {
                    format!("{catalog_id}@{provider}")
                }
            }
            RuntimeKey::Copilot => format!("copilot:{model}"),
            RuntimeKey::Cursor => format!("cursor:{model}"),
            RuntimeKey::Bedrock => format!("bedrock:{model}"),
            RuntimeKey::Antigravity => format!("antigravity:{model}"),
            RuntimeKey::Gemini
            | RuntimeKey::CodeAssistOAuth
            | RuntimeKey::RemoteCatalog
            | RuntimeKey::Current
            | RuntimeKey::Other(_) => model.to_string(),
        }
    }
}

/// OpenRouter catalog id for a bare model: claude models gain an `anthropic/`
/// prefix, OpenAI models an `openai/` prefix, already-qualified ids pass
/// through. Mirrors `jcode_base::provider::openrouter_catalog_model_id` but
/// lives here so [`RouteSelection::routed_model_spec`] has no upward dep.
fn openrouter_catalog_model_id(model: &str) -> String {
    let trimmed = model.trim();
    match crate::models::provider_for_model(trimmed) {
        Some("claude") => format!("anthropic/{trimmed}"),
        Some("openai") => format!("openai/{trimmed}"),
        _ => trimmed.to_string(),
    }
}

/// Typed view of [`ModelRoute::api_method`].
///
/// The wire format intentionally remains a string so older clients and saved
/// catalogs continue to round-trip, but routing/picker code should parse it at
/// module boundaries instead of scattering string comparisons everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRouteApiMethod {
    JcodeSubscription,
    ClaudeOAuth,
    AnthropicApiKey,
    OpenAIOAuth,
    OpenAIApiKey,
    OpenRouter,
    OpenAiCompatible { profile_id: Option<String> },
    Copilot,
    Cursor,
    Bedrock,
    CodeAssistOAuth,
    AntigravityHttps,
    RemoteCatalog,
    Current,
    Other(String),
}

impl ModelRouteApiMethod {
    /// The route-vocabulary api_method for a canonical dual-auth route.
    pub fn from_auth_route(route: crate::auth_mode::AuthRoute) -> Self {
        use crate::auth_mode::{AuthMode, DualAuthProvider};
        match (route.provider, route.mode) {
            (DualAuthProvider::Anthropic, AuthMode::Oauth) => Self::ClaudeOAuth,
            (DualAuthProvider::Anthropic, AuthMode::ApiKey) => Self::AnthropicApiKey,
            (DualAuthProvider::OpenAI, AuthMode::Oauth) => Self::OpenAIOAuth,
            (DualAuthProvider::OpenAI, AuthMode::ApiKey) => Self::OpenAIApiKey,
        }
    }

    pub fn parse(value: &str) -> Self {
        let trimmed = value.trim();
        let lower = trimmed.to_ascii_lowercase();
        // Dual-auth (Anthropic/OpenAI OAuth-vs-API) tokens share one canonical
        // alias table so the route vocabulary never drifts from the runtime/CLI
        // vocabularies. Anything else falls through to the route-only methods.
        if let Some(route) = crate::auth_mode::AuthRoute::parse(&lower) {
            return Self::from_auth_route(route);
        }
        match lower.as_str() {
            "jcode-subscription" => Self::JcodeSubscription,
            "openrouter" => Self::OpenRouter,
            "openai-compatible" => Self::OpenAiCompatible { profile_id: None },
            "copilot" => Self::Copilot,
            "cursor" => Self::Cursor,
            "bedrock" => Self::Bedrock,
            "code-assist-oauth" => Self::CodeAssistOAuth,
            "https" => Self::AntigravityHttps,
            "remote-catalog" => Self::RemoteCatalog,
            "current" => Self::Current,
            _ => {
                if let Some(("openai-compatible", profile_id)) = lower.split_once(':') {
                    let profile_id = profile_id.trim();
                    Self::OpenAiCompatible {
                        profile_id: (!profile_id.is_empty()).then(|| profile_id.to_string()),
                    }
                } else {
                    Self::Other(trimmed.to_string())
                }
            }
        }
    }

    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::OpenAiCompatible {
                profile_id: Some(profile_id),
            } => Some(profile_id.as_str()),
            _ => None,
        }
    }

    pub fn is_openai_compatible(&self) -> bool {
        matches!(self, Self::OpenAiCompatible { .. })
    }

    pub fn is_openrouter(&self) -> bool {
        matches!(self, Self::OpenRouter)
    }

    pub fn is_copilot(&self) -> bool {
        matches!(self, Self::Copilot)
    }

    pub fn is_cursor(&self) -> bool {
        matches!(self, Self::Cursor)
    }

    pub fn is_bedrock(&self) -> bool {
        matches!(self, Self::Bedrock)
    }

    pub fn matches_openai_compatible_profile(&self, provider_id: &str) -> bool {
        self.profile_id()
            .is_some_and(|profile_id| profile_id.eq_ignore_ascii_case(provider_id))
    }

    pub fn is_anthropic_credential_route(&self) -> bool {
        matches!(self, Self::ClaudeOAuth | Self::AnthropicApiKey)
    }

    pub fn is_openai_credential_route(&self) -> bool {
        matches!(self, Self::OpenAIOAuth | Self::OpenAIApiKey)
    }

    pub fn display_label(&self) -> String {
        match self {
            Self::JcodeSubscription => "subscription".to_string(),
            Self::ClaudeOAuth | Self::OpenAIOAuth | Self::CodeAssistOAuth => "oauth".to_string(),
            Self::AnthropicApiKey | Self::OpenAIApiKey | Self::OpenAiCompatible { .. } => {
                "api key".to_string()
            }
            Self::OpenRouter => "openrouter".to_string(),
            Self::Copilot => "copilot".to_string(),
            Self::Cursor => "cursor".to_string(),
            Self::Bedrock => "bedrock".to_string(),
            Self::AntigravityHttps => "https".to_string(),
            Self::RemoteCatalog => "remote-catalog".to_string(),
            Self::Current => "current".to_string(),
            Self::Other(method) => method
                .split_once(':')
                .map(|(method, _)| method)
                .unwrap_or(method)
                .to_string(),
        }
    }
}

pub fn normalize_model_route_provider_label(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '_', '-'], "")
}

pub fn model_route_provider_labels_match(route_provider: &str, current_provider: &str) -> bool {
    let route = normalize_model_route_provider_label(route_provider);
    let current = normalize_model_route_provider_label(current_provider);
    if route.is_empty() || current.is_empty() {
        return false;
    }
    if route == current {
        return true;
    }

    matches!(
        (current.as_str(), route.as_str()),
        ("claude" | "anthropic", "anthropic" | "claude")
            | ("openai", "openai")
            | ("gemini" | "google", "gemini" | "google")
            | ("antigravity", "antigravity")
            | (
                "copilot" | "copilotcode" | "githubcopilot",
                "copilot" | "githubcopilot"
            )
            | ("cursor", "cursor")
            | ("bedrock" | "awsbedrock", "bedrock" | "awsbedrock")
            | ("openrouter", "openrouter" | "auto")
    )
}

pub fn model_route_provider_labels_related(route_provider: &str, login_provider: &str) -> bool {
    let route = normalize_model_route_provider_label(route_provider);
    let login = normalize_model_route_provider_label(login_provider);
    if route.is_empty() || login.is_empty() {
        return false;
    }
    if route == login || route.contains(&login) || login.contains(&route) {
        return true;
    }
    model_route_provider_labels_match(&route, &login)
}

pub fn model_route_provider_matches_key(
    route_provider_key: Option<&str>,
    route_provider_label: &str,
    desired_provider: &str,
) -> bool {
    let desired_provider = desired_provider.trim();
    if desired_provider.is_empty() {
        return false;
    }
    // Fold the dual-auth (Anthropic/OpenAI OAuth-vs-API) vocabularies onto their
    // canonical session key first, so a config `default_provider =
    // "anthropic-api"` matches a route whose key is `claude-api` -- while still
    // keeping the API-vs-OAuth distinction (`claude-api` must NOT match
    // `claude-oauth`/`claude`). Without this fold the two spellings of the same
    // route normalize differently ("anthropicapi" != "claudeapi") and the model
    // picker fails to mark the user's actual default route.
    //
    // Only the explicit-credential desired keys (`claude-api`, `openai-oauth`,
    // ...) get this strict treatment. A bare desired alias (`claude`,
    // `anthropic`, `openai`) pins no credential, so it keeps the historical
    // auth-method-agnostic label match below and can light up either route.
    let desired_pins_credential = AuthRoute::parse_explicit_credential_prefix(desired_provider);
    if let Some(desired_route) = desired_pins_credential {
        let desired_key = desired_route.session_provider_key();
        if let Some(route_provider_key) = route_provider_key {
            let route_folded = AuthRoute::parse(route_provider_key)
                .map(|route| route.session_provider_key())
                .unwrap_or(route_provider_key);
            return normalize_model_route_provider_label(route_folded)
                == normalize_model_route_provider_label(desired_key);
        }
        // No structured route key to compare against: the bare label cannot
        // distinguish OAuth from API, so a credential-pinned default cannot be
        // confirmed for this route.
        return false;
    }
    if let Some(route_provider_key) = route_provider_key
        && normalize_model_route_provider_label(route_provider_key)
            == normalize_model_route_provider_label(desired_provider)
    {
        return true;
    }
    model_route_provider_labels_match(route_provider_label, desired_provider)
}

pub fn model_route_metadata_is_recommended(
    model: &str,
    provider: &str,
    api_method: &str,
    available: bool,
) -> bool {
    if !available {
        return false;
    }
    let api_method = ModelRouteApiMethod::parse(api_method);
    match model {
        "gpt-5.5" => {
            matches!(&api_method, ModelRouteApiMethod::OpenAIOAuth)
                && model_route_provider_labels_match(provider, "openai")
        }
        "claude-opus-4-8" => {
            matches!(
                &api_method,
                ModelRouteApiMethod::ClaudeOAuth | ModelRouteApiMethod::AnthropicApiKey
            ) && model_route_provider_labels_match(provider, "anthropic")
        }
        _ => false,
    }
}

impl ModelRoute {
    pub fn api_method_kind(&self) -> ModelRouteApiMethod {
        ModelRouteApiMethod::parse(&self.api_method)
    }

    pub fn estimated_reference_cost_micros(&self) -> Option<u64> {
        self.cheapness
            .as_ref()
            .and_then(|estimate| estimate.estimated_reference_cost_micros)
    }
}

/// Canonical snapshot of a provider's model catalog at a point in time.
///
/// This is the local contract shared by server-side providers, remote clients,
/// and persisted remote catalog caches. The websocket wire format may still
/// flatten these fields for backwards compatibility, but internal code should
/// pass catalog state as this single value instead of loose parallel vectors.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCatalogSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_routes: Vec<ModelRoute>,
}

impl ModelCatalogSnapshot {
    pub fn new(
        provider_name: Option<String>,
        provider_model: Option<String>,
        available_models: Vec<String>,
        model_routes: Vec<ModelRoute>,
    ) -> Self {
        Self {
            provider_name,
            provider_model,
            available_models,
            model_routes,
        }
    }

    pub fn from_provider(provider: &dyn Provider) -> Self {
        // Note: on multi-providers both calls below build the same route
        // catalog; MultiProvider memoizes it (short TTL) so this stays one
        // build instead of two.
        Self::new(
            Some(provider.display_name()),
            Some(provider.model()),
            provider.available_models_display(),
            provider.model_routes(),
        )
    }

    pub fn has_routes(&self) -> bool {
        !self.model_routes.is_empty()
    }
}

pub const CHEAPNESS_REFERENCE_INPUT_TOKENS: u64 = 25_000;
pub const CHEAPNESS_REFERENCE_OUTPUT_TOKENS: u64 = 5_000;

/// The credential a dual-auth provider (Anthropic / OpenAI) will actually use
/// for the next request. This is the authoritative billing identity: `Oauth`
/// means subscription usage, `ApiKey` means cost-based usage. It is resolved
/// once, server-side, from the provider's live credential mode and shipped to
/// remote clients so they never have to re-derive it from a provider name.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedCredential {
    /// OAuth / subscription login (Claude subscription, Codex login, ...).
    Oauth,
    /// Direct provider API key (cost-based billing).
    ApiKey,
}

impl ResolvedCredential {
    /// Human-readable label used by header/auth surfaces.
    pub fn auth_method_label(self) -> &'static str {
        match self {
            Self::Oauth => "OAuth",
            Self::ApiKey => "API key",
        }
    }

    /// True when requests bill against a subscription (OAuth) rather than a
    /// metered API key.
    pub fn is_subscription(self) -> bool {
        matches!(self, Self::Oauth)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteBillingKind {
    Metered,
    Subscription,
    IncludedQuota,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteCostSource {
    PublicApiPricing,
    PublicPlanPricing,
    RuntimePlan,
    OpenRouterEndpoint,
    OpenRouterCatalog,
    /// Live models.dev pricing catalog (https://models.dev/api.json).
    ModelsDevCatalog,
    Heuristic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteCostConfidence {
    Exact,
    High,
    Medium,
    Low,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteCheapnessEstimate {
    pub billing_kind: RouteBillingKind,
    pub source: RouteCostSource,
    pub confidence: RouteCostConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_price_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_price_per_mtok_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_price_per_mtok_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_price_per_mtok_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub included_requests_per_month: Option<u64>,
    pub reference_input_tokens: u64,
    pub reference_output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_reference_cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl RouteCheapnessEstimate {
    pub fn metered(
        source: RouteCostSource,
        confidence: RouteCostConfidence,
        input_price_per_mtok_micros: u64,
        output_price_per_mtok_micros: u64,
        cache_read_price_per_mtok_micros: Option<u64>,
        note: impl Into<Option<String>>,
    ) -> Self {
        Self {
            billing_kind: RouteBillingKind::Metered,
            source,
            confidence,
            monthly_price_micros: None,
            input_price_per_mtok_micros: Some(input_price_per_mtok_micros),
            output_price_per_mtok_micros: Some(output_price_per_mtok_micros),
            cache_read_price_per_mtok_micros,
            included_requests_per_month: None,
            reference_input_tokens: CHEAPNESS_REFERENCE_INPUT_TOKENS,
            reference_output_tokens: CHEAPNESS_REFERENCE_OUTPUT_TOKENS,
            estimated_reference_cost_micros: Some(reference_request_cost_micros(
                input_price_per_mtok_micros,
                output_price_per_mtok_micros,
            )),
            note: note.into(),
        }
    }

    pub fn subscription(
        source: RouteCostSource,
        confidence: RouteCostConfidence,
        monthly_price_micros: u64,
        included_requests_per_month: Option<u64>,
        note: impl Into<Option<String>>,
    ) -> Self {
        Self {
            billing_kind: RouteBillingKind::Subscription,
            source,
            confidence,
            monthly_price_micros: Some(monthly_price_micros),
            input_price_per_mtok_micros: None,
            output_price_per_mtok_micros: None,
            cache_read_price_per_mtok_micros: None,
            included_requests_per_month,
            reference_input_tokens: CHEAPNESS_REFERENCE_INPUT_TOKENS,
            reference_output_tokens: CHEAPNESS_REFERENCE_OUTPUT_TOKENS,
            estimated_reference_cost_micros: included_requests_per_month
                .map(|count| monthly_price_micros / count.max(1)),
            note: note.into(),
        }
    }

    pub fn included_quota(
        source: RouteCostSource,
        confidence: RouteCostConfidence,
        monthly_price_micros: u64,
        included_requests_per_month: Option<u64>,
        estimated_reference_cost_micros: Option<u64>,
        note: impl Into<Option<String>>,
    ) -> Self {
        Self {
            billing_kind: RouteBillingKind::IncludedQuota,
            source,
            confidence,
            monthly_price_micros: Some(monthly_price_micros),
            input_price_per_mtok_micros: None,
            output_price_per_mtok_micros: None,
            cache_read_price_per_mtok_micros: None,
            included_requests_per_month,
            reference_input_tokens: CHEAPNESS_REFERENCE_INPUT_TOKENS,
            reference_output_tokens: CHEAPNESS_REFERENCE_OUTPUT_TOKENS,
            estimated_reference_cost_micros,
            note: note.into(),
        }
    }
}

fn reference_request_cost_micros(
    input_price_per_mtok_micros: u64,
    output_price_per_mtok_micros: u64,
) -> u64 {
    input_price_per_mtok_micros.saturating_mul(CHEAPNESS_REFERENCE_INPUT_TOKENS) / 1_000_000
        + output_price_per_mtok_micros.saturating_mul(CHEAPNESS_REFERENCE_OUTPUT_TOKENS) / 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metered_estimate_computes_reference_cost() {
        let estimate = RouteCheapnessEstimate::metered(
            RouteCostSource::Heuristic,
            RouteCostConfidence::Low,
            2_000_000,
            8_000_000,
            None,
            None,
        );
        assert_eq!(estimate.estimated_reference_cost_micros, Some(90_000));
    }

    #[test]
    fn shared_http_client_reuses_builder() {
        let _a = shared_http_client();
        let _b = shared_http_client();
    }

    #[test]
    fn fresh_transport_client_builds_distinct_clients() {
        // Each call must produce a brand-new client (new connection pool), not
        // a cached one: the whole point is that a retry after a transport
        // fault (e.g. TLS BadRecordMac) never reuses a possibly-poisoned
        // pooled connection.
        let _a = fresh_transport_client();
        let _b = fresh_transport_client();
    }

    #[test]
    fn canonical_user_agent_identifies_jcode() {
        assert!(JCODE_USER_AGENT.starts_with("jcode/"));
    }

    #[test]
    fn model_route_api_method_parser_keeps_profile_identity() {
        assert_eq!(
            ModelRouteApiMethod::parse("openai-compatible:cerebras"),
            ModelRouteApiMethod::OpenAiCompatible {
                profile_id: Some("cerebras".to_string())
            }
        );
        assert!(
            ModelRouteApiMethod::parse("openai-compatible:cerebras")
                .matches_openai_compatible_profile("CEREBRAS")
        );
        assert_eq!(
            ModelRouteApiMethod::parse("openai-api"),
            ModelRouteApiMethod::OpenAIApiKey
        );
        assert_eq!(
            ModelRouteApiMethod::parse("claude-api"),
            ModelRouteApiMethod::AnthropicApiKey
        );
    }

    #[test]
    fn model_route_provider_label_matching_uses_aliases_without_substring_false_positives() {
        assert!(model_route_provider_labels_match("Anthropic", "Claude"));
        assert!(model_route_provider_labels_match("auto", "OpenRouter"));
        assert!(model_route_provider_labels_match(
            "GitHub Copilot",
            "Copilot"
        ));
        assert!(model_route_provider_labels_match("AWS Bedrock", "Bedrock"));
        assert!(!model_route_provider_labels_match(
            "OpenRouter/OpenAI",
            "OpenAI"
        ));
        assert!(!model_route_provider_labels_match("OpenAI", "OpenRouter"));
        assert!(!model_route_provider_labels_match("", ""));
        assert!(!model_route_provider_labels_related("OpenAI", ""));
    }

    #[test]
    fn model_route_provider_key_matching_prefers_explicit_route_key() {
        assert!(model_route_provider_matches_key(
            Some("cerebras"),
            "Cerebras Cloud",
            "CEREBRAS"
        ));
        assert!(model_route_provider_matches_key(
            None,
            "Anthropic",
            "Claude"
        ));
        assert!(!model_route_provider_matches_key(
            Some("cerebras"),
            "Cerebras",
            "groq"
        ));
    }

    #[test]
    fn model_route_provider_key_matching_folds_dual_auth_vocabularies() {
        // `default_provider = "anthropic-api"` and a route keyed `claude-api`
        // are two spellings of the same Anthropic API-key route, so they must
        // match even though their raw forms normalize differently.
        assert!(model_route_provider_matches_key(
            Some("claude-api"),
            "Anthropic",
            "anthropic-api",
        ));
        assert!(model_route_provider_matches_key(
            Some("anthropic-api-key"),
            "Anthropic",
            "claude-api",
        ));
        assert!(model_route_provider_matches_key(
            Some("openai-api"),
            "OpenAI",
            "openai-api-key",
        ));

        // The fold must NOT collapse the OAuth-vs-API distinction: an API-key
        // default must not light up the OAuth route (and vice versa).
        assert!(!model_route_provider_matches_key(
            Some("claude-oauth"),
            "Anthropic",
            "anthropic-api",
        ));
        assert!(!model_route_provider_matches_key(
            Some("openai-oauth"),
            "OpenAI",
            "openai-api",
        ));

        // A bare provider default pins no credential, so it keeps the historical
        // auth-method-agnostic behavior: it matches either dual-auth route via
        // the label fallback (model identity still narrows the picker default).
        assert!(model_route_provider_matches_key(
            Some("claude-oauth"),
            "Anthropic",
            "claude",
        ));
        assert!(model_route_provider_matches_key(
            Some("claude-api"),
            "Anthropic",
            "claude",
        ));
    }

    #[test]
    fn model_route_recommendation_policy_is_provider_aware() {
        assert!(model_route_metadata_is_recommended(
            "gpt-5.5",
            "OpenAI",
            "openai-oauth",
            true
        ));
        assert!(!model_route_metadata_is_recommended(
            "gpt-5.5",
            "OpenAI",
            "openai-api-key",
            true
        ));
        assert!(!model_route_metadata_is_recommended(
            "gpt-5.5", "Copilot", "copilot", true
        ));
        assert!(!model_route_metadata_is_recommended(
            "gpt-5.5",
            "OpenAI",
            "openai-oauth",
            false
        ));
        assert!(model_route_metadata_is_recommended(
            "claude-opus-4-8",
            "Anthropic",
            "claude-oauth",
            true
        ));
        assert!(model_route_metadata_is_recommended(
            "claude-opus-4-8",
            "Anthropic",
            "claude-api",
            true
        ));
        assert!(model_route_metadata_is_recommended(
            "claude-opus-4-8",
            "Anthropic",
            "claude-oauth",
            true
        ));
        assert!(model_route_metadata_is_recommended(
            "claude-opus-4-8",
            "Anthropic",
            "claude-api",
            true
        ));
        assert!(!model_route_metadata_is_recommended(
            "claude-opus-4-8",
            "Anthropic",
            "openrouter",
            true
        ));
        assert!(!model_route_metadata_is_recommended(
            "deepseek/deepseek-v4-pro",
            "auto",
            "openrouter",
            true
        ));
    }

    struct SnapshotTestProvider;

    #[async_trait]
    impl Provider for SnapshotTestProvider {
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<EventStream> {
            unreachable!("snapshot test does not call complete")
        }

        fn name(&self) -> &str {
            "snapshot-provider"
        }

        fn model(&self) -> String {
            "snapshot-model".to_string()
        }

        fn available_models_display(&self) -> Vec<String> {
            vec!["snapshot-model".to_string()]
        }

        fn model_routes(&self) -> Vec<ModelRoute> {
            vec![ModelRoute {
                model: "snapshot-model".to_string(),
                provider: "Snapshot".to_string(),
                api_method: "snapshot-api".to_string(),
                available: true,
                detail: "test route".to_string(),
                cheapness: None,
            }]
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(SnapshotTestProvider)
        }
    }

    #[test]
    fn model_catalog_snapshot_materializes_provider_catalog_contract() {
        let snapshot = ModelCatalogSnapshot::from_provider(&SnapshotTestProvider);

        assert_eq!(snapshot.provider_name.as_deref(), Some("snapshot-provider"));
        assert_eq!(snapshot.provider_model.as_deref(), Some("snapshot-model"));
        assert_eq!(snapshot.available_models, ["snapshot-model"]);
        assert!(snapshot.has_routes());
        assert_eq!(snapshot.model_routes[0].api_method, "snapshot-api");
    }

    #[test]
    fn runtime_key_distinguishes_openrouter_from_direct_compatible_profile() {
        assert_eq!(
            RuntimeKey::from_api_method(&ModelRouteApiMethod::parse("openrouter"), "auto"),
            RuntimeKey::OpenRouter
        );
        assert_eq!(
            RuntimeKey::from_api_method(
                &ModelRouteApiMethod::parse("openai-compatible:nvidia-nim"),
                "NVIDIA NIM",
            ),
            RuntimeKey::OpenAiCompatible {
                profile_id: Some("nvidia-nim".to_string())
            }
        );
    }

    #[test]
    fn route_selection_preserves_runtime_identity_from_model_route() {
        let selection = RouteSelection::from_model_route(&ModelRoute {
            model: "openrouter/owl-alpha".to_string(),
            provider: "OpenRouter".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: "https://openrouter.ai/api/v1".to_string(),
            cheapness: None,
        });
        assert_eq!(selection.model, "openrouter/owl-alpha");
        assert_eq!(selection.runtime_key, RuntimeKey::OpenRouter);
        assert_eq!(selection.api_method, "openrouter");

        let selection = RouteSelection::from_model_route(&ModelRoute {
            model: "nvidia/example".to_string(),
            provider: "NVIDIA NIM".to_string(),
            api_method: "openai-compatible:nvidia-nim".to_string(),
            available: true,
            detail: "https://integrate.api.nvidia.com/v1".to_string(),
            cheapness: None,
        });
        assert_eq!(
            selection.runtime_key,
            RuntimeKey::OpenAiCompatible {
                profile_id: Some("nvidia-nim".to_string())
            }
        );
        assert_eq!(selection.provider_label, "NVIDIA NIM");
    }
}
