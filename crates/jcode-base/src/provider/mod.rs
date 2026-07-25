mod accessors;
mod account_failover;
pub mod activation;
pub mod anthropic;
pub mod antigravity;
pub mod bedrock;
mod catalog_routes;
pub mod claude;
pub mod copilot;
pub mod cursor;
mod dispatch;
pub mod external;
mod failover;
mod fingerprint;
pub mod gemini;
mod image_clamp;
pub mod jcode;
pub mod models;
mod multi_provider;
pub mod openai;
pub mod openai_request;
pub mod openrouter;
pub mod pricing;
mod registry;
mod route_builders;
mod routing;
mod selection;
mod startup;
mod state;

use crate::auth;
use crate::message::{Message, ToolDefinition};
use account_failover::{
    account_usage_probe, active_account_label_for_provider, maybe_annotate_limit_summary,
    same_provider_account_candidates, same_provider_account_failover_enabled,
    set_account_override_for_provider,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
#[cfg(test)]
use jcode_provider_core::FailoverDecision;
use registry::ProviderRegistry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

static ACCOUNT_TRANSITIONS: AtomicUsize = AtomicUsize::new(0);

pub struct AccountTransitionGuard;

impl Drop for AccountTransitionGuard {
    fn drop(&mut self) {
        ACCOUNT_TRANSITIONS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn begin_account_transition() -> AccountTransitionGuard {
    ACCOUNT_TRANSITIONS.fetch_add(1, Ordering::SeqCst);
    AccountTransitionGuard
}

pub fn ensure_no_account_transition() -> Result<()> {
    if ACCOUNT_TRANSITIONS.load(Ordering::SeqCst) == 0 {
        Ok(())
    } else {
        anyhow::bail!("Provider account transition is still invalidating cached credentials")
    }
}

pub use catalog_routes::{
    append_simplified_anthropic_model_routes, remote_current_openai_compatible_route_for_model,
    remote_model_is_server_copilot_only, remote_model_routes_fallback,
    remote_model_routes_lightweight_fallback, remote_model_should_offer_copilot_route,
    remote_openai_compatible_route_for_model, simplified_model_routes_for_picker,
};
pub use jcode_provider_core::attempt_tracker;
pub use jcode_provider_core::cli_provider_arg_for_session_key;
pub use jcode_provider_core::{
    ALL_CLAUDE_MODELS, ALL_OPENAI_MODELS, CHATGPT_WEB_MODEL, CHEAPNESS_REFERENCE_INPUT_TOKENS,
    CHEAPNESS_REFERENCE_OUTPUT_TOKENS, CredentialMode, DEFAULT_CONTEXT_LIMIT, EventStream,
    JCODE_USER_AGENT, ModelCapabilities, ModelCatalogRefreshSummary, ModelRoute,
    ModelRouteApiMethod, NativeCompactionResult, NativeToolResult, NativeToolResultSender,
    PremiumMode, Provider, RouteBillingKind, RouteCheapnessEstimate, RouteCostConfidence,
    RouteCostSource, RouteSelection, RuntimeKey, dedupe_model_routes,
    explicit_model_provider_prefix, fresh_transport_client, model_name_for_provider,
    normalize_copilot_model_name, provider_from_model_key, shared_http_client,
    summarize_model_catalog_refresh,
};
pub use jcode_provider_core::{
    FallbackPickOptions, error_looks_like_credential_failure, model_route_provider_labels_match,
    normalize_model_route_provider_label, pick_next_fallback_route,
    pick_next_fallback_route_with_options,
};
pub use jcode_provider_core::{ProviderFailoverPrompt, parse_failover_prompt_message};
pub use route_builders::{
    build_anthropic_oauth_route, build_chatgpt_web_route, build_copilot_route,
    build_openai_api_key_route, build_openai_oauth_route, build_openrouter_auto_route,
    build_openrouter_endpoint_route, build_openrouter_fallback_provider_route,
    is_listable_model_name, listable_model_names_from_routes, openrouter_catalog_model_id,
};
pub(crate) use routing::{
    anthropic_api_key_route_availability, anthropic_oauth_route_availability,
};

/// Process-wide handle to the live agent provider.
///
/// The memory sidecar ([`crate::sidecar::Sidecar`]) needs to make small,
/// cheap model calls (rerank / relevance / extraction). It has dedicated fast
/// paths for OpenAI (codex-spark) and Claude (haiku) OAuth, but jcode also runs
/// on Copilot, Antigravity, Gemini, Cursor, Bedrock, and OpenRouter. For those
/// providers there is no standalone sidecar HTTP client, so the sidecar falls
/// back to *this* handle and dispatches through the already-working
/// [`Provider::complete_simple`] path. `Server::new` registers the active
/// provider here at startup.
static ACTIVE_PROVIDER: RwLock<Option<Arc<dyn Provider>>> = RwLock::new(None);

/// Register the live agent provider so background helpers (memory sidecar) can
/// reach whatever provider the user is actually running on. Safe to call more
/// than once; the most recent registration wins.
pub fn set_active_provider(provider: Arc<dyn Provider>) {
    *ACTIVE_PROVIDER
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(provider);
}

/// Fetch the registered active provider, if any. Returns a forked handle so the
/// caller gets an independent provider instance (per the [`Provider::fork`]
/// contract) that will not interfere with the main agent's model selection.
pub fn active_provider_fork() -> Option<Arc<dyn Provider>> {
    ACTIVE_PROVIDER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|p| p.fork())
}

/// Provider-agnostic streaming idle timeout: max seconds to wait between
/// streamed chunks/events before treating the connection as dead. Resolved
/// from `[provider] stream_idle_timeout_secs` / `JCODE_STREAM_IDLE_TIMEOUT_SECS`
/// (default 180). Shared by every streaming provider path so slow reasoning
/// models that think silently for minutes don't trip a premature timeout on
/// one transport but not another (issue #434).
pub fn stream_idle_timeout() -> std::time::Duration {
    let secs = crate::config::config()
        .provider
        .stream_idle_timeout_secs
        .max(1);
    std::time::Duration::from_secs(secs)
}

/// Whether reasoning deltas should be persisted in session history for later
/// provider context reconstruction.
///
/// Display is controlled separately by `display.show_thinking`. Persist only
/// when a provider request builder can safely send the stored block back in
/// the provider-native shape. Anthropic is included only because we preserve
/// its thinking signatures in `ContentBlock::AnthropicThinking`.
pub fn stores_reasoning_content_for_context(provider_name: &str) -> bool {
    if !crate::config::config().provider.preserve_reasoning_context {
        return false;
    }
    matches!(
        provider_name.to_ascii_lowercase().as_str(),
        "openrouter" | "anthropic" | "openai"
    )
}

// Keep inactive direct profiles on the same 15-minute soft-refresh cadence as
// the active OpenRouter/OpenAI-compatible runtime. We continue serving the
// cached routes immediately while a background refresh updates the catalog.
const OPENAI_COMPATIBLE_PROFILE_CATALOG_SOFT_REFRESH_SECS: u64 = 15 * 60;

fn openai_compatible_profile_catalog_cache_is_stale(cached_at: u64, now: u64) -> bool {
    now.saturating_sub(cached_at) >= OPENAI_COMPATIBLE_PROFILE_CATALOG_SOFT_REFRESH_SECS
}

fn cached_live_models_for_openai_compatible_profile(
    resolved: &crate::provider_catalog::ResolvedOpenAiCompatibleProfile,
) -> Option<(Vec<String>, bool)> {
    let cache = jcode_provider_openrouter::load_disk_cache_entry_for_namespace(&resolved.id)?;
    let cache_is_stale = jcode_provider_openrouter::current_unix_secs()
        .map(|now| openai_compatible_profile_catalog_cache_is_stale(cache.cached_at, now))
        .unwrap_or(false);
    let source_api_base = cache
        .source_api_base
        .as_deref()
        .and_then(crate::provider_catalog::normalize_api_base)?;
    let expected_api_base = crate::provider_catalog::normalize_api_base(&resolved.api_base)?;
    if source_api_base != expected_api_base {
        return None;
    }

    let models = cache
        .models
        .into_iter()
        .map(|model| model.id.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    if models.is_empty() {
        None
    } else {
        Some((models, cache_is_stale))
    }
}

fn direct_openai_compatible_profile_routes(
    profile: crate::provider_catalog::OpenAiCompatibleProfile,
) -> Vec<ModelRoute> {
    let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
    let static_models = crate::provider_catalog::openai_compatible_profile_static_models(profile);
    let (mut models, from_live_catalog) = if let Some((models, cache_is_stale)) =
        cached_live_models_for_openai_compatible_profile(&resolved)
    {
        if cache_is_stale {
            crate::provider::openrouter::maybe_schedule_openai_compatible_profile_catalog_refresh(
                profile,
                "inactive direct profile stale route cache",
            );
        }
        (models, true)
    } else {
        crate::provider::openrouter::maybe_schedule_openai_compatible_profile_catalog_refresh(
            profile,
            "inactive direct profile route cache miss",
        );
        let mut models = static_models;
        if models.is_empty()
            && let Some(default_model) = resolved.default_model.as_ref()
            && !default_model.trim().is_empty()
        {
            models.push(default_model.trim().to_string());
        }
        (models, false)
    };

    let provider = resolved.display_name.clone();
    let api_method = format!("openai-compatible:{}", resolved.id);
    let detail = if from_live_catalog {
        resolved.api_base.clone()
    } else if resolved.api_base.trim().is_empty() {
        "fallback: static provider model list".to_string()
    } else {
        format!(
            "{}; fallback: static provider model list",
            resolved.api_base
        )
    };

    let mut routes = Vec::new();
    for model in models.drain(..) {
        if !is_listable_model_name(&model)
            || !crate::provider_catalog::openai_compatible_profile_model_supports_chat(
                &resolved.id,
                &model,
            )
            || routes.iter().any(|route: &ModelRoute| route.model == model)
        {
            continue;
        }

        routes.push(ModelRoute {
            model,
            provider: provider.clone(),
            api_method: api_method.clone(),
            available: true,
            detail: detail.clone(),
            cheapness: None,
        });
    }

    routes
}

fn standard_openrouter_profile_configured() -> bool {
    crate::provider_catalog::load_env_value_from_env_or_config(
        "OPENROUTER_API_KEY",
        "openrouter.env",
    )
    .is_some()
}

fn configured_standard_openrouter_profile_routes() -> Vec<ModelRoute> {
    let Some(cache) = jcode_provider_openrouter::load_disk_cache_entry_for_namespace("openrouter")
    else {
        return Vec::new();
    };

    let source_matches_openrouter = cache
        .source_api_base
        .as_deref()
        .and_then(crate::provider_catalog::normalize_api_base)
        .map(|base| base.contains("openrouter.ai"))
        .unwrap_or(false);
    if !source_matches_openrouter {
        return Vec::new();
    }

    let available = standard_openrouter_profile_configured();
    cache
        .models
        .into_iter()
        .map(|model| model.id.trim().to_string())
        .filter(|model| is_listable_model_name(model))
        .map(|model| build_openrouter_auto_route(&model, available, String::new()))
        .collect()
}

pub fn set_model_with_auth_refresh(provider: &dyn Provider, model: &str) -> Result<()> {
    match provider.set_model(model) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            let first_message = first_err.to_string();
            crate::logging::auth_event(
                "auth_changed_retry_after_set_model_failure",
                provider.name(),
                &[("reason", first_message.as_str())],
            );
            // Use the preserve-current-provider variant: this is a retry for an
            // already-open session, so refreshing auth from disk must NOT swap a
            // user-defined named OpenAI-compatible profile slot for a generic
            // OpenRouter runtime (which would lose `profile_id` and re-introduce
            // the `<profile>:<model>` prefix on the wire). See #408.
            provider.on_auth_changed_preserve_current_provider();
            provider.set_model(model).map_err(|second_err| {
                anyhow::anyhow!(
                    "{} (retried after reloading auth from disk: {})",
                    first_message,
                    second_err
                )
            })
        }
    }
}

/// Persist enough model identity to reconstruct the exact selected runtime.
/// OpenRouter's provider object exposes only its normalized catalog model after
/// selection, while the explicit `@provider` pin is held separately in memory.
/// Keep that pin in the session model so restart, transfer, and LCM compactor
/// forks do not silently fall back to OpenRouter auto-routing.
pub fn persisted_session_model_for_route(
    selection: &RouteSelection,
    resolved_model: &str,
) -> String {
    if matches!(selection.runtime_key, RuntimeKey::OpenRouter)
        && !selection.provider_label.trim().is_empty()
        && !selection.provider_label.eq_ignore_ascii_case("auto")
    {
        selection.routed_model_spec()
    } else {
        resolved_model.to_string()
    }
}

use self::dispatch::CompletionMode;
pub use self::models::{
    AccountModelAvailability, AccountModelAvailabilityState, AnthropicModelCatalog,
    ModelCatalogHttpStatus, OpenAIModelCatalog, begin_anthropic_model_catalog_refresh,
    begin_openai_model_catalog_refresh, cached_anthropic_model_ids, cached_openai_model_ids,
    cached_openai_reasoning_efforts, clear_all_model_unavailability_for_account,
    clear_all_provider_unavailability_for_account, clear_model_unavailable_for_account,
    clear_provider_unavailable_for_account, context_limit_for_model,
    context_limit_for_model_with_provider, fetch_anthropic_model_catalog,
    fetch_anthropic_model_catalog_oauth, fetch_openai_api_key_model_catalog,
    fetch_openai_context_limits, fetch_openai_model_catalog,
    finish_anthropic_model_catalog_refresh_for_scope, finish_openai_model_catalog_refresh,
    format_account_model_availability_detail, get_best_available_openai_model,
    is_model_available_for_account, known_anthropic_model_ids, known_openai_model_ids,
    model_availability_for_account, model_unavailability_detail_for_account,
    note_openai_model_catalog_refresh_attempt, openai_platform_api_key_configured,
    persist_anthropic_model_catalog, persist_openai_model_catalog, populate_account_models,
    populate_anthropic_models, populate_context_limits, populate_context_limits_from_config,
    populate_context_limits_from_config_value, provider_for_model, provider_for_model_with_hint,
    provider_unavailability_detail_for_account, record_model_unavailable_for_account,
    record_provider_unavailable_for_account, refresh_openai_model_catalog_in_background,
    resolve_model_capabilities, should_refresh_anthropic_model_catalog,
    should_refresh_openai_model_catalog,
};
pub use self::selection::DefaultModelSelection;
use self::selection::{ActiveProvider, ProviderAvailability};
use self::state::ProviderState;
pub use self::state::{ProviderModelSelectionSource, ProviderRuntimeState, ProviderStateEvent};

/// MultiProvider wraps multiple providers and allows seamless model switching
pub struct MultiProvider {
    /// Claude Code CLI provider
    claude: RwLock<Option<Arc<dyn Provider>>>,
    /// Direct Anthropic API provider (no Python dependency)
    anthropic: RwLock<Option<Arc<dyn Provider>>>,
    openai: RwLock<Option<Arc<dyn Provider>>>,
    /// GitHub Copilot API provider (direct API, hot-swappable after login).
    /// Held as `dyn Provider`: the concrete runtime lives downstream in
    /// `jcode-provider-copilot-runtime` and is instantiated through
    /// `external::instantiate_external_provider`.
    copilot_api: RwLock<Option<Arc<dyn Provider>>>,
    /// Antigravity provider (direct HTTPS, hot-swappable after login). Held as
    /// `dyn Provider`: the concrete runtime lives downstream in
    /// `jcode-provider-antigravity-runtime` and is instantiated through
    /// `external::instantiate_external_provider`.
    antigravity: RwLock<Option<Arc<dyn Provider>>>,
    /// Gemini provider (hot-swappable after login). Held as `dyn Provider`:
    /// the concrete runtime lives downstream in `jcode-provider-gemini-runtime`
    /// and is instantiated through `external::instantiate_external_provider`.
    gemini: RwLock<Option<Arc<dyn Provider>>>,
    /// Cursor provider (native/direct API, hot-swappable after login). Held as
    /// `dyn Provider`: the concrete runtime lives downstream in
    /// `jcode-provider-cursor-runtime` and is instantiated through
    /// `external::instantiate_external_provider`.
    cursor: RwLock<Option<Arc<dyn Provider>>>,
    /// AWS Bedrock provider (native Converse/ConverseStream, IAM/SigV4)
    bedrock: RwLock<Option<Arc<bedrock::BedrockProvider>>>,
    /// OpenRouter API provider
    openrouter: RwLock<Option<Arc<dyn Provider>>>,
    /// Direct OpenAI-compatible runtimes keyed by profile id.
    ///
    /// These use the same wire protocol implementation as OpenRouter, but must
    /// not occupy the real OpenRouter slot. Keeping them separate prevents a
    /// compatible endpoint selection from corrupting later OpenRouter model
    /// switches, catalog display, or auth refresh handling.
    openai_compatible_profiles: RwLock<HashMap<String, Arc<dyn Provider>>>,
    active_openai_compatible_profile: RwLock<Option<String>>,
    active: RwLock<ActiveProvider>,
    /// Use Claude CLI instead of direct API (legacy mode)
    use_claude_cli: bool,
    /// Notifications generated during provider/account auto-selection.
    /// The TUI should drain and display these on session start.
    startup_notices: RwLock<Vec<String>>,
    /// Optional explicit provider lock set by CLI `--provider`.
    /// When present, cross-provider fallback is disabled.
    forced_provider: Option<ActiveProvider>,
    /// Short-TTL memo for the full route-catalog build.
    ///
    /// Building the catalog is expensive (per-route pricing lookups, endpoint
    /// cache reads, credential probes) and the shared server rebuilds it for
    /// every connection whenever a `ModelsUpdated` bus event fans out. During
    /// a burst of client spawns that multiplied into hundreds of builds within
    /// a couple of seconds, saturating every core. The memo collapses those
    /// into one build per TTL window; auth/model changes invalidate it
    /// explicitly so pickers never see stale routes after a switch.
    routes_memo: Mutex<Option<RoutesMemoEntry>>,
    /// Number of model-catalog prefetches launched by the latest auth refresh.
    /// Shared by forks so the server can wait for real work rather than sleeping
    /// through a fixed quiet period after login.
    post_auth_refreshes_pending: Arc<std::sync::atomic::AtomicUsize>,
}

/// Memoized route catalog with the inputs that decide its freshness: build
/// time (short TTL), the auth generation at build time (bumped by
/// `AuthStatus::invalidate_cache()` on login/logout/credential edits), and the
/// catalog generation (bumped by prefetch/refresh completions).
#[derive(Clone)]
struct RoutesMemoEntry {
    built_at: std::time::Instant,
    auth_generation: u64,
    catalog_generation: u64,
    routes: Vec<ModelRoute>,
    /// `listable_model_names_from_routes(&routes)`, cached because the
    /// non-chat-model heuristic string-scans every route name and callers
    /// (catalog snapshots) ask for names and routes together.
    listable_models: Vec<String>,
}

/// Process-wide route-catalog memo shared across `MultiProvider` instances.
///
/// The shared server forks one `MultiProvider` per client connection, so a
/// per-instance memo cannot deduplicate the builds triggered by a burst of
/// simultaneous client spawns: every fresh fork still built its own catalog.
/// Catalog content is derived almost entirely from process-global state
/// (credential files, disk caches, config), so identical forks can share one
/// build. Instance-specific inputs (active provider/model/profile) are folded
/// into the memo key; anything not captured is bounded by the short TTL and
/// the auth/catalog generations.
static GLOBAL_ROUTES_MEMO: LazyLock<Mutex<HashMap<String, RoutesMemoEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Single-flight guard for catalog builds. During a client connect burst every
/// connection calls `model_routes()` at nearly the same instant; without this
/// they all miss the still-empty memo and build the same catalog in parallel
/// (a thundering herd that pegs every core). Holding this lock across the
/// build makes followers block (sleep, not spin) until the leader publishes
/// its result, which they then serve from the shared memo.
static GLOBAL_ROUTES_BUILD_LOCK: Mutex<()> = Mutex::new(());

/// Bumped whenever provider catalogs change out-of-band (prefetch completion,
/// forced catalog refresh, auth changes). Invalidates every shared memo entry.
static CATALOG_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn catalog_generation() -> u64 {
    CATALOG_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

impl Default for MultiProvider {
    fn default() -> Self {
        Self::new()
    }
}

mod operations;
mod provider_impl;

/// Get the prompt cache TTL in seconds for a given provider name.
/// Returns None if the provider doesn't support prompt caching or TTL is unknown.
pub fn cache_ttl_for_provider(provider: &str) -> Option<u64> {
    cache_ttl_for_provider_model(provider, None)
}

/// Get the prompt cache TTL in seconds for a given provider/model pair.
///
/// This is provider cache-retention policy: it depends only on provider
/// families (anthropic/openai/...) and their model capabilities, so it lives
/// in `provider` rather than the UI layer.
pub fn cache_ttl_for_provider_model(provider: &str, model: Option<&str>) -> Option<u64> {
    match provider.to_lowercase().as_str() {
        "anthropic" | "claude" => Some(if anthropic::is_cache_ttl_1h() {
            60 * 60
        } else {
            300
        }),
        "openai" => {
            if model
                .map(openai::supports_extended_prompt_cache_retention)
                .unwrap_or(false)
            {
                Some(24 * 60 * 60)
            } else {
                Some(300)
            }
        }
        "openrouter" => Some(300),
        "jcode subscription" => Some(300),
        "gemini" => Some(300),
        "copilot" => None,
        "cursor" => None,
        "antigravity" => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests;
