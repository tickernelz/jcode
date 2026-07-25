pub mod anthropic;
pub mod attempt_tracker;
pub mod auth_mode;
pub mod catalog_refresh;
pub mod failover;
pub mod fallback_pick;
pub mod fingerprint;
pub mod model_id;
pub mod models;
pub mod openai_schema;
pub mod pricing;
pub mod reasoning;
pub mod retry_after;
pub mod selection;
pub mod transport;

pub use transport::is_transient_transport_error;

pub use anthropic::{
    ANTHROPIC_OAUTH_BETA_HEADERS, ANTHROPIC_OAUTH_BETA_HEADERS_1M, AnthropicContextMode,
    AnthropicReasoningCaps, anthropic_context_mode, anthropic_effectively_1m,
    anthropic_is_1m_model, anthropic_map_tool_name_for_oauth, anthropic_map_tool_name_from_oauth,
    anthropic_oauth_beta_headers, anthropic_reasoning_caps, anthropic_stainless_arch,
    anthropic_stainless_os, anthropic_strip_1m_suffix,
};
pub use auth_mode::{
    AuthMode, AuthRoute, DualAuthProvider, pinned_mode_for, runtime_env_auth_route,
    runtime_env_pinned_mode,
};
pub use catalog_refresh::{ModelCatalogRefreshSummary, summarize_model_catalog_refresh};
pub use failover::{
    FailoverDecision, ProviderFailoverPrompt, classify_failover_error_message,
    parse_failover_prompt_message,
};
pub use fallback_pick::{
    FallbackPickOptions, error_looks_like_credential_failure, pick_next_fallback_route,
    pick_next_fallback_route_with_options,
};
pub use fingerprint::{log_provider_canonical_input, stable_hash_json, stable_hash_str};
pub use models::{
    ALL_CLAUDE_MODELS, ALL_OPENAI_MODELS, CHATGPT_WEB_MODEL, DEFAULT_CLAUDE_MODEL,
    DEFAULT_CONTEXT_LIMIT, DEFAULT_OPENAI_MODEL, ModelCapabilities, OPENAI_API_ONLY_PRO_MODELS,
    context_limit_for_model, context_limit_for_model_with_provider,
    context_limit_for_model_with_provider_and_cache, is_listable_model_name,
    is_openai_api_only_pro_model, normalize_copilot_model_name,
    provider_for_model as core_provider_for_model,
    provider_for_model_with_hint as core_provider_for_model_with_hint, provider_key_from_hint,
};
pub use reasoning::{
    DEEPSEEK_SELECTABLE_EFFORTS, OPENAI_SELECTABLE_EFFORTS, OPENROUTER_SELECTABLE_EFFORTS,
    canonical_reasoning_effort, inferred_reasoning_efforts,
};
pub use selection::{
    ActiveProvider, ProviderAvailability, auto_default_provider, cli_provider_arg_for_session_key,
    dedupe_model_routes, explicit_model_provider_prefix, fallback_sequence,
    model_name_for_provider, parse_provider_hint, provider_from_model_key, provider_key,
    provider_label,
};

use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use jcode_message_types::{
    ContentBlock, Message, Role, StreamEvent, ToolDefinition, messages_with_dynamic_system_context,
};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Stream of events from a provider.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

/// Provider trait for LLM backends.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Send messages and get a streaming response.
    /// resume_session_id: Optional session ID to resume a previous conversation (provider-specific).
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream>;

    /// Send messages with split system prompt for better caching.
    async fn complete_split(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        system_static: &str,
        system_dynamic: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let dynamic_messages = messages_with_dynamic_system_context(messages, system_dynamic);
        self.complete(&dynamic_messages, tools, system_static, resume_session_id)
            .await
    }

    /// Get the provider name.
    ///
    /// This is the stable, machine-facing identifier (e.g. `"openrouter"`,
    /// `"claude"`). Several surfaces key billing and routing decisions off this
    /// value, so it must stay constant for a given provider class even when the
    /// underlying runtime is a specific OpenAI-compatible profile. Use
    /// [`Provider::display_name`] for anything shown to the user.
    fn name(&self) -> &str;

    /// Human-facing provider label for the *current runtime selection*.
    ///
    /// Defaults to [`Provider::name`]. Provider orchestrators that multiplex
    /// several backends behind one `name()` (notably the OpenRouter slot, which
    /// also serves direct OpenAI-compatible profiles such as NVIDIA NIM or
    /// DeepSeek) override this so the UI reflects the profile the user actually
    /// selected at runtime instead of a fixed aggregator label.
    fn display_name(&self) -> String {
        self.name().to_string()
    }

    /// Get the model identifier being used.
    fn model(&self) -> String {
        "unknown".to_string()
    }

    /// Return the exact effective route/account identity that the next request
    /// will use. Multiplexing providers must override this. Returning `None`
    /// means the provider cannot authenticate a persisted exact identity, so a
    /// session that already carries one must not resume provider-side state.
    fn exact_runtime_identity(&self) -> Option<ExactRuntimeIdentity> {
        None
    }

    /// Human-readable description of the auth method the active provider will
    /// actually use for the next request (e.g. "OAuth" or "API key"), or `None`
    /// when there is no meaningful OAuth-vs-API-key distinction. UI surfaces use
    /// this to report the auth method accurately instead of inferring it from
    /// which credentials happen to be configured.
    fn active_auth_method_label(&self) -> Option<&'static str> {
        self.active_resolved_credential()
            .map(ResolvedCredential::auth_method_label)
    }

    /// The credential the active provider will actually use for the next
    /// request, when the provider supports both OAuth and API-key auth
    /// (currently Anthropic and OpenAI). Returns `None` for providers with no
    /// OAuth-vs-API-key ambiguity.
    ///
    /// This is the authoritative, server-side answer to "subscription or
    /// cost-based billing?". It is computed from the provider's live credential
    /// mode rather than re-derived from credential probes or env strings, so
    /// every surface (header tag, info-widget usage, model-switch line) and
    /// every transport (local or remote) agrees. Remote clients receive the
    /// resolved value over the wire instead of guessing from a provider name.
    fn active_resolved_credential(&self) -> Option<ResolvedCredential> {
        None
    }

    /// The credential the active dual-auth provider (Anthropic / OpenAI) will
    /// use *when the user has explicitly pinned one* (OAuth or API key), without
    /// resolving "auto".
    ///
    /// Unlike [`Provider::active_resolved_credential`], this never touches disk
    /// or env to resolve an auto/default choice: it returns `Some` only for an
    /// explicit in-memory pin and `None` for auto mode (or providers with no
    /// OAuth-vs-API-key ambiguity). UI surfaces that rebuild every frame (the
    /// info widget) use it to reflect an explicit OAuth<->API switch instantly
    /// while leaving the cheap cached heuristic to handle the auto case.
    fn active_explicit_credential(&self) -> Option<ResolvedCredential> {
        None
    }

    /// Whether this provider path can safely receive `ContentBlock::Image` inputs.
    fn supports_image_input(&self) -> bool {
        false
    }

    /// Set the model to use (returns error if model not supported).
    fn set_model(&self, _model: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "This provider does not support model switching"
        ))
    }

    /// Select a structured model route.
    ///
    /// Most single-runtime providers can treat this as `set_model(model)`. Provider
    /// orchestrators should override this to activate the exact runtime identified
    /// by [`RouteSelection::runtime_key`] instead of reparsing a lossy model string.
    fn set_route_selection(&self, selection: &RouteSelection) -> Result<()> {
        self.set_model(&selection.routed_model_spec())
    }

    /// List available models for this provider.
    fn available_models(&self) -> Vec<&'static str> {
        vec![]
    }

    /// List available models for display/autocomplete (may be dynamic).
    fn available_models_display(&self) -> Vec<String> {
        self.available_models()
            .iter()
            .map(|m| (*m).to_string())
            .filter(|model| is_listable_model_name(model))
            .collect()
    }

    /// List models that should participate in cycle-model switching.
    fn available_models_for_switching(&self) -> Vec<String> {
        self.available_models()
            .iter()
            .map(|m| (*m).to_string())
            .collect()
    }

    /// List known providers for a model (OpenRouter-style @provider autocomplete).
    fn available_providers_for_model(&self, _model: &str) -> Vec<String> {
        Vec::new()
    }

    /// Provider details for model picker: Vec<(provider_name, detail_string)>.
    fn provider_details_for_model(&self, _model: &str) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Return the currently preferred upstream provider.
    fn preferred_provider(&self) -> Option<String> {
        None
    }

    /// Get all model routes for the unified picker.
    fn model_routes(&self) -> Vec<ModelRoute> {
        Vec::new()
    }

    /// Prefetch any dynamic model lists (default: no-op).
    async fn prefetch_models(&self) -> Result<()> {
        Ok(())
    }

    /// Force-refresh model catalog data and return a before/after summary.
    async fn refresh_model_catalog(&self) -> Result<ModelCatalogRefreshSummary> {
        let before_models = self.available_models_display();
        let before_routes = self.model_routes();
        self.prefetch_models().await?;
        let after_models = self.available_models_display();
        let after_routes = self.model_routes();
        Ok(summarize_model_catalog_refresh(
            before_models,
            after_models,
            before_routes,
            after_routes,
        ))
    }

    /// Called when auth credentials change (e.g., after login).
    fn on_auth_changed(&self) {}

    /// Called when auth credentials change for an already-open session that
    /// should learn about refreshed credentials without being silently moved to
    /// a newly activated provider/profile.
    fn on_auth_changed_preserve_current_provider(&self) {
        self.on_auth_changed();
    }

    /// Whether asynchronous model-catalog work started by [`Provider::on_auth_changed`]
    /// is still running. Providers that only reload credentials synchronously can
    /// keep the default `false`; orchestrators should override this so callers can
    /// finish auth without an arbitrary debounce delay.
    fn auth_model_refresh_pending(&self) -> bool {
        false
    }

    /// Get the reasoning effort level (if applicable).
    fn reasoning_effort(&self) -> Option<String> {
        None
    }

    /// Set the reasoning effort level (if applicable).
    fn set_reasoning_effort(&self, _effort: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "This provider does not support reasoning effort"
        ))
    }

    /// Get ordered list of available reasoning effort levels.
    fn available_efforts(&self) -> Vec<&'static str> {
        vec![]
    }

    /// Get the active service tier override (if applicable).
    fn service_tier(&self) -> Option<String> {
        None
    }

    /// Set the active service tier override (if applicable).
    fn set_service_tier(&self, _service_tier: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "This provider does not support service tier switching"
        ))
    }

    /// Get ordered list of available service tiers.
    fn available_service_tiers(&self) -> Vec<&'static str> {
        vec![]
    }

    /// Get the native compaction mode for the active provider, if any.
    fn native_compaction_mode(&self) -> Option<String> {
        None
    }

    /// Get the native compaction threshold in tokens for the active provider, if any.
    fn native_compaction_threshold_tokens(&self) -> Option<usize> {
        None
    }

    fn transport(&self) -> Option<String> {
        None
    }

    fn set_transport(&self, _transport: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "This provider does not support transport switching"
        ))
    }

    fn available_transports(&self) -> Vec<&'static str> {
        vec![]
    }

    /// Returns true if the provider executes tools internally.
    fn handles_tools_internally(&self) -> bool {
        false
    }

    /// Invalidate any cached credentials.
    async fn invalidate_credentials(&self) {}

    /// Refresh cached credentials only when shared credential storage changed.
    /// OAuth providers use this at request admission so another Jcode process
    /// cannot switch the active account while this process keeps sending with
    /// the previous account's cached token.
    async fn ensure_credentials_current(&self) -> Result<()> {
        Ok(())
    }

    /// Set Copilot premium request conservation mode.
    fn set_premium_mode(&self, _mode: PremiumMode) {}

    /// Get the current Copilot premium mode.
    fn premium_mode(&self) -> PremiumMode {
        PremiumMode::Normal
    }

    /// Current OAuth-vs-API-key credential pin for dual-auth providers.
    /// Non-dual-auth providers report `Auto`.
    fn credential_mode(&self) -> CredentialMode {
        CredentialMode::Auto
    }

    /// Pin the OAuth-vs-API-key credential route for dual-auth providers.
    fn set_credential_mode(&self, _mode: CredentialMode) -> Result<()> {
        Ok(())
    }

    /// Re-read credentials from disk immediately (e.g. after an OAuth refresh
    /// by another process). Providers with in-memory credential caches override
    /// this; the default is a no-op.
    fn reload_credentials(&self) {}

    /// Human-facing label for the runtime backing this provider instance.
    /// Unlike `display_name`, this reflects instance state (e.g. which
    /// OpenAI-compatible profile an aggregator runtime currently serves).
    fn runtime_display_name(&self) -> String {
        self.display_name()
    }

    /// Whether this runtime speaks the real OpenRouter aggregator API with
    /// provider-routing features (provider pins, per-provider endpoints), as
    /// opposed to a plain OpenAI-compatible endpoint.
    fn supports_provider_routing_features(&self) -> bool {
        false
    }

    /// For direct OpenAI-compatible endpoints: the (provider label,
    /// api_method, detail) triple used to build the route entry. `None` for
    /// everything else (including the real OpenRouter aggregator).
    fn direct_openai_compatible_route_parts(&self) -> Option<(String, String, String)> {
        None
    }

    /// The explicit upstream-provider pin for the current model, when the
    /// user pinned one on an aggregator runtime.
    fn explicit_provider_pin_for_current_model(&self) -> Option<String> {
        None
    }

    /// Give aggregator runtimes a chance to refresh per-model endpoint data
    /// used by display surfaces. Returns true when a refresh was scheduled.
    fn maybe_schedule_endpoint_refresh_for_display(
        &self,
        _model: &str,
        _cache_age_secs: Option<u64>,
        _context: &'static str,
    ) -> bool {
        false
    }

    /// Human-readable freshness note for this provider's model catalog, shown
    /// as route detail in the model picker (e.g. "cached live catalog" or
    /// "catalog still loading"). Empty when the catalog is live/authoritative.
    fn model_catalog_detail(&self) -> String {
        String::new()
    }

    /// Returns true if jcode should use its own compaction for this provider.
    fn supports_compaction(&self) -> bool {
        false
    }

    /// Returns true if jcode should proactively run its own summary-based compaction.
    fn uses_jcode_compaction(&self) -> bool {
        self.supports_compaction()
    }

    /// Ask the provider to produce a native compaction artifact.
    async fn native_compact(
        &self,
        _messages: &[Message],
        _existing_summary_text: Option<&str>,
        _existing_openai_encrypted_content: Option<&str>,
    ) -> Result<NativeCompactionResult> {
        Err(anyhow::anyhow!(
            "This provider does not support native compaction"
        ))
    }

    /// Return the context window size (in tokens) for the current model.
    fn context_window(&self) -> usize {
        context_limit_for_model_with_provider(&self.model(), Some(self.name()))
            .unwrap_or(DEFAULT_CONTEXT_LIMIT)
    }

    /// Create a new provider instance with independent mutable state.
    fn fork(&self) -> Arc<dyn Provider>;

    /// Create an independent provider for a brand-new user session.
    ///
    /// The default preserves the current runtime selection, matching [`Self::fork`].
    /// Provider orchestrators backed by reloadable configuration may override this
    /// to reapply the latest persisted defaults without changing ordinary forks
    /// used by compaction, resumed sessions, and other in-flight work.
    fn fork_for_new_session(&self) -> Arc<dyn Provider> {
        self.fork()
    }

    /// Get a sender for native tool results (if the provider supports it).
    fn native_result_sender(&self) -> Option<NativeToolResultSender> {
        None
    }

    /// Drain any startup notices.
    fn drain_startup_notices(&self) -> Vec<String> {
        Vec::new()
    }

    /// Switch the active provider for the current session when supported.
    fn switch_active_provider_to(&self, _provider: &str) -> Result<()> {
        Err(anyhow::anyhow!(
            "This provider does not support active provider switching"
        ))
    }

    /// Simple completion that returns text directly (no streaming).
    async fn complete_simple(&self, prompt: &str, system: &str) -> Result<String> {
        use futures::StreamExt;

        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }];

        let response = self.complete(&messages, &[], system, None).await?;
        let mut result = String::new();
        tokio::pin!(response);

        while let Some(event) = response.next().await {
            match event {
                Ok(StreamEvent::TextDelta(text)) => result.push_str(&text),
                Ok(_) => {}
                Err(err) => return Err(err),
            }
        }

        Ok(result)
    }
}

/// Premium request conservation mode for Copilot-compatible providers.
/// 0 = normal (every user message is premium)
/// 1 = one premium per session (first user message only, rest are agent)
/// 2 = zero premium (all requests sent as agent)
mod types;

pub use types::*;
