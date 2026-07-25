use super::*;
#[async_trait]
#[rustfmt::skip]
impl Provider for MultiProvider {
    async fn complete( &self, messages: &[Message], tools: &[ToolDefinition], system: &str, resume_session_id: Option<&str>, ) -> Result<EventStream> {
        self.complete_with_failover(
            messages,
            tools,
            CompletionMode::Unified { system },
            resume_session_id, )
        .await
    }
    async fn complete_split( &self, messages: &[Message], tools: &[ToolDefinition], system_static: &str, system_dynamic: &str, resume_session_id: Option<&str>, ) -> Result<EventStream> {
        self.complete_with_failover(
            messages,
            tools,
            CompletionMode::Split {
                system_static,
                system_dynamic, },
            resume_session_id, )
        .await
    }
    fn name(&self) -> &str {
        match self.active_provider() {
            ActiveProvider::Claude => "Claude",
            ActiveProvider::OpenAI => "OpenAI",
            ActiveProvider::Copilot => "Copilot",
            ActiveProvider::Antigravity => "Antigravity",
            ActiveProvider::Gemini => "Gemini",
            ActiveProvider::Cursor => "Cursor",
            ActiveProvider::Bedrock => "Bedrock",
            ActiveProvider::OpenRouter => "OpenRouter",
        }
    }
    fn display_name(&self) -> String {
        if matches!(self.active_provider(), ActiveProvider::OpenRouter)
            && let Some(execution) = self.active_openrouter_execution_provider() {
            return execution.runtime_display_name();
        }
        self.name().to_string()
    }
    fn model(&self) -> String {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if let Some(anthropic) = self.anthropic_provider() {
                    anthropic.model()
                } else if let Some(claude) = self.claude_provider() {
                    claude.model()
                } else {
                    jcode_provider_core::DEFAULT_CLAUDE_MODEL.to_string()
                }
            }
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|o| o.model())
                .unwrap_or_else(|| jcode_provider_core::DEFAULT_OPENAI_MODEL.to_string()),
            ActiveProvider::Copilot => self
                .copilot_provider()
                .map(|o| o.model())
                .unwrap_or_else(|| "claude-sonnet-4".to_string()),
            ActiveProvider::Antigravity => self
                .antigravity_provider()
                .map(|o| o.model())
                .unwrap_or_else(|| "default".to_string()),
            ActiveProvider::Gemini => self
                .gemini_provider()
                .map(|o| o.model())
                .unwrap_or_else(|| "gemini-2.5-pro".to_string()),
            ActiveProvider::Cursor => self
                .cursor_provider()
                .map(|o| o.model())
                .unwrap_or_else(|| "composer-2.5".to_string()),
            ActiveProvider::Bedrock => self
                .bedrock_provider()
                .map(|o| o.model())
                .unwrap_or_else(|| "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string()),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .map(|o| o.model())
                .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string()),
        }
    }
    fn exact_runtime_identity(&self) -> Option<jcode_provider_core::ExactRuntimeIdentity> {
        use jcode_provider_core::{ExactRuntimeIdentity, RouteSelection, RuntimeKey};
        let active = self.active_provider();
        let model = self.model();
        let display_name = self.display_name();
        let compatible_profile = self
            .active_openai_compatible_profile
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let (provider_key, runtime_key, api_method) = match active {
            ActiveProvider::Claude if self.use_claude_cli => (
                "claude-cli".to_string(),
                RuntimeKey::Other("claude-cli".to_string()),
                "claude-cli".to_string(),
            ),
            ActiveProvider::Claude => match self.active_resolved_credential() {
                Some(jcode_provider_core::ResolvedCredential::ApiKey) => (
                    "claude-api".to_string(),
                    RuntimeKey::AnthropicApiKey,
                    "anthropic-api-key".to_string(),
                ),
                _ => (
                    "claude".to_string(),
                    RuntimeKey::ClaudeOAuth,
                    "claude-oauth".to_string(),
                ), },
            ActiveProvider::OpenAI => match self.active_resolved_credential() {
                Some(jcode_provider_core::ResolvedCredential::ApiKey) => (
                    "openai-api".to_string(),
                    RuntimeKey::OpenAIApiKey,
                    "openai-api-key".to_string(),
                ),
                _ => (
                    "openai".to_string(),
                    RuntimeKey::OpenAIOAuth,
                    "openai-oauth".to_string(),
                ), },
            ActiveProvider::Copilot => (
                "copilot".to_string(),
                RuntimeKey::Copilot,
                "copilot".to_string(),
            ),
            ActiveProvider::Antigravity => (
                "antigravity".to_string(),
                RuntimeKey::Antigravity,
                "antigravity".to_string(),
            ),
            ActiveProvider::Gemini => (
                "gemini".to_string(),
                RuntimeKey::Gemini,
                "gemini".to_string(),
            ),
            ActiveProvider::Cursor => (
                "cursor".to_string(),
                RuntimeKey::Cursor,
                "cursor".to_string(),
            ),
            ActiveProvider::Bedrock => (
                "bedrock".to_string(),
                RuntimeKey::Bedrock,
                "bedrock".to_string(),
            ),
            ActiveProvider::OpenRouter => match compatible_profile {
                Some(profile_id) => (
                    profile_id.clone(),
                    RuntimeKey::OpenAiCompatible {
                        profile_id: Some(profile_id.clone()), },
                    format!("openai-compatible:{profile_id}"),
                ),
                None => (
                    "openrouter".to_string(),
                    RuntimeKey::OpenRouter,
                    "openrouter".to_string(),
                ), },
        };
        let account = match (&runtime_key, self.active_resolved_credential()) {
            (RuntimeKey::ClaudeOAuth, Some(jcode_provider_core::ResolvedCredential::Oauth)) => {
                Some(crate::auth::claude::active_account_identity()?)
            }
            (RuntimeKey::OpenAIOAuth, Some(jcode_provider_core::ResolvedCredential::Oauth)) => {
                Some(crate::auth::codex::active_account_identity()?)
            }
            _ => {
                let key = runtime_key.stable_id();
                Some(crate::auth::account_store::runtime_credential_identity(&key))
            }
        };
        let (account_label, account_id, account_generation) = account
            .map_or((None, None, None), |(label, id, generation)| {
                (Some(label), Some(id), Some(generation))
            });
        Some(ExactRuntimeIdentity {
            provider_key,
            route: RouteSelection {
                model,
                runtime_key,
                api_method,
                provider_label: display_name,
                detail: String::new(), },
            account_label,
            account_id,
            account_generation,
            reasoning_effort: self.reasoning_effort(),
        })
    }
    fn active_resolved_credential(&self) -> Option<jcode_provider_core::ResolvedCredential> {
        use jcode_provider_core::ResolvedCredential;
        match self.active_provider() {
            ActiveProvider::Claude => {
                let anthropic = self.anthropic_provider()?;
                Some(match anthropic.credential_mode() {
                    anthropic::AnthropicCredentialMode::OAuth => ResolvedCredential::Oauth,
                    anthropic::AnthropicCredentialMode::ApiKey => ResolvedCredential::ApiKey,
                    anthropic::AnthropicCredentialMode::Auto => {
                        if crate::auth::claude::load_credentials().is_ok() {
                            ResolvedCredential::Oauth
                        } else {
                            ResolvedCredential::ApiKey
                        }
                    }
                })
            }
            ActiveProvider::OpenAI => {
                let openai = self.openai_provider()?;
                Some(match openai.credential_mode() {
                    openai::OpenAICredentialMode::OAuth => ResolvedCredential::Oauth,
                    openai::OpenAICredentialMode::ApiKey => ResolvedCredential::ApiKey,
                    openai::OpenAICredentialMode::Auto => {
                        if crate::auth::codex::load_oauth_credentials().is_ok() {
                            ResolvedCredential::Oauth
                        } else {
                            ResolvedCredential::ApiKey
                        }
                    }
                })
            }
            _ => None,
        }
    }
    fn credential_mode(&self) -> CredentialMode {
        let active = self
            .forced_provider
            .unwrap_or_else(|| self.active_provider());
        match active {
            ActiveProvider::Claude => self
                .anthropic_provider()
                .map(|provider| provider.credential_mode())
                .unwrap_or(CredentialMode::Auto),
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|provider| provider.credential_mode())
                .unwrap_or(CredentialMode::Auto),
            _ => CredentialMode::Auto,
        }
    }
    fn set_credential_mode(&self, mode: CredentialMode) -> Result<()> {
        let active = self
            .forced_provider
            .unwrap_or_else(|| self.active_provider());
        match active {
            ActiveProvider::Claude => self
                .anthropic_provider()
                .ok_or_else(|| anyhow!("Anthropic provider is not configured"))?
                .set_credential_mode(mode)?,
            ActiveProvider::OpenAI => self
                .openai_provider()
                .ok_or_else(|| anyhow!("OpenAI provider is not configured"))?
                .set_credential_mode(mode)?,
            _ if mode == CredentialMode::Auto => return Ok(()),
            _ => anyhow::bail!(
                "Provider {} does not support OAuth/API-key credential selection",
                Self::provider_label(active)
            ),
        }
        self.set_active_provider(active);
        Ok(())
    }
    fn active_explicit_credential(&self) -> Option<jcode_provider_core::ResolvedCredential> {
        use jcode_provider_core::ResolvedCredential;
        match self.active_provider() {
            ActiveProvider::Claude => match self.anthropic_provider()?.credential_mode() {
                anthropic::AnthropicCredentialMode::OAuth => Some(ResolvedCredential::Oauth),
                anthropic::AnthropicCredentialMode::ApiKey => Some(ResolvedCredential::ApiKey),
                anthropic::AnthropicCredentialMode::Auto => None, },
            ActiveProvider::OpenAI => match self.openai_provider()?.credential_mode() {
                openai::OpenAICredentialMode::OAuth => Some(ResolvedCredential::Oauth),
                openai::OpenAICredentialMode::ApiKey => Some(ResolvedCredential::ApiKey),
                openai::OpenAICredentialMode::Auto => None, },
            _ => None,
        }
    }
    fn supports_image_input(&self) -> bool {
        match self.active_provider() {
            ActiveProvider::Claude => self
                .anthropic_provider()
                .map(|provider| provider.supports_image_input())
                .or_else(|| {
                    self.claude_provider()
                        .map(|provider| provider.supports_image_input())
                })
                .unwrap_or(false),
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|provider| provider.supports_image_input())
                .unwrap_or(false),
            ActiveProvider::Copilot => self
                .copilot_provider()
                .map(|provider| provider.supports_image_input())
                .unwrap_or(false),
            ActiveProvider::Antigravity => self
                .antigravity_provider()
                .map(|provider| provider.supports_image_input())
                .unwrap_or(false),
            ActiveProvider::Gemini => self
                .gemini_provider()
                .map(|provider| provider.supports_image_input())
                .unwrap_or(false),
            ActiveProvider::Cursor => self
                .cursor_provider()
                .map(|provider| provider.supports_image_input())
                .unwrap_or(false),
            ActiveProvider::Bedrock => self
                .bedrock_provider()
                .map(|provider| provider.supports_image_input())
                .unwrap_or(false),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .map(|provider| provider.supports_image_input())
                .unwrap_or(false),
        }
    }
    fn set_model(&self, model: &str) -> Result<()> {
        self.spawn_anthropic_catalog_refresh_if_needed();
        self.spawn_openai_catalog_refresh_if_needed();
        self.invalidate_routes_memo();
        let requested_model = model.trim();
        if requested_model.is_empty() {
            anyhow::bail!("Model cannot be empty");
        }
        if let Some((profile, target_model)) = Self::openai_compatible_model_prefix(requested_model) {
            self.ensure_provider_lock_allows_openai_compatible_profile(requested_model)?;
            return self.set_model_on_openai_compatible_profile(profile, target_model);
        }
        if let Some((profile_name, target_model)) =
            Self::named_provider_profile_model_prefix(requested_model) {
            self.ensure_provider_lock_allows_openai_compatible_profile(requested_model)?;
            return self.set_model_on_named_provider_profile(&profile_name, &target_model);
        }
        if let Some((target, prefix, target_model)) =
            explicit_model_provider_prefix(requested_model) {
            self.ensure_provider_lock_allows_model_target(target, requested_model)?;
            let pinned = jcode_provider_core::AuthRoute::parse_explicit_credential_prefix(prefix);
            let openai_credential_mode = pinned.and_then(|route| {
                matches!(
                    route.provider,
                    jcode_provider_core::DualAuthProvider::OpenAI )
                .then(|| match route.mode {
                    jcode_provider_core::AuthMode::ApiKey => openai::OpenAICredentialMode::ApiKey,
                    jcode_provider_core::AuthMode::Oauth => openai::OpenAICredentialMode::OAuth,
                })
            });
            let anthropic_credential_mode = pinned.and_then(|route| {
                matches!(
                    route.provider,
                    jcode_provider_core::DualAuthProvider::Anthropic )
                .then(|| match route.mode {
                    jcode_provider_core::AuthMode::ApiKey => {
                        anthropic::AnthropicCredentialMode::ApiKey
                    }
                    jcode_provider_core::AuthMode::Oauth => {
                        anthropic::AnthropicCredentialMode::OAuth
                    }
                })
            });
            if openai_credential_mode.is_some() || anthropic_credential_mode.is_some() {
                return self.set_model_on_provider_with_credential_modes(
                    target,
                    target_model,
                    openai_credential_mode,
                    anthropic_credential_mode, );
            }
            return self.set_model_on_provider(target, target_model);
        }
        if let Some(forced) = self.forced_provider {
            return self.set_model_on_provider(forced, requested_model);
        }
        let model = if let Some(canonical) = normalize_copilot_model_name(requested_model) {
            canonical
        } else {
            requested_model
        };
        if let Some((base_model, provider_pin)) = model.rsplit_once('@')
            && !provider_pin.trim().is_empty()
            && let Some(openrouter_model) = openrouter_catalog_model_id(base_model) {
            return self.set_model_on_provider(
                ActiveProvider::OpenRouter,
                &format!("{}@{}", openrouter_model, provider_pin), );
        }
        let target_provider = provider_for_model(model);
        if let Some(target_provider) = target_provider
            && let Some(target) = provider_from_model_key(target_provider) {
            self.set_model_on_provider(target, model)
        } else {
            self.set_model_on_provider(self.active_provider(), model)
        }
    }
    fn set_route_selection(&self, selection: &RouteSelection) -> Result<()> {
        if selection.model.trim().is_empty() {
            anyhow::bail!("Model cannot be empty");
        }
        let gemini_developer_api = matches!(
            &selection.runtime_key,
            RuntimeKey::Other(method)
                if matches!(
                    method.trim().to_ascii_lowercase().as_str(),
                    "gemini-api-key" | "gemini-developer-api"
                ) );
        if matches!(
            selection.runtime_key,
            RuntimeKey::CodeAssistOAuth | RuntimeKey::Gemini
        ) || gemini_developer_api
        {
            let Some(gemini) = self.gemini_provider() else {
                anyhow::bail!(
                    "Gemini credentials not available. Run `jcode login --provider gemini` first." );
            };
            if gemini_developer_api {
                let mut typed = selection.clone();
                typed.runtime_key = RuntimeKey::Gemini;
                gemini.set_route_selection(&typed)?;
            } else {
                gemini.set_route_selection(selection)?;
            }
            self.set_active_provider(ActiveProvider::Gemini);
            return Ok(());
        }
        if matches!(selection.runtime_key, RuntimeKey::RemoteCatalog) {
            anyhow::bail!(
                "Remote catalog route is not a concrete runtime identity; refresh routes before selecting it" );
        }
        if let RuntimeKey::Other(method) = &selection.runtime_key {
            anyhow::bail!("Route API method {method} is not a concrete supported runtime identity");
        }
        self.set_model(&selection.routed_model_spec())
    }
    fn available_models(&self) -> Vec<&'static str> {
        let mut models = Vec::new();
        models.extend_from_slice(ALL_CLAUDE_MODELS);
        models.extend_from_slice(ALL_OPENAI_MODELS);
        models
    }
    fn available_models_for_switching(&self) -> Vec<String> {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if let Some(anthropic) = self.anthropic_provider() {
                    anthropic.available_models_for_switching()
                } else if let Some(claude) = self.claude_provider() {
                    claude.available_models_for_switching()
                } else {
                    Vec::new()
                }
            }
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|openai| openai.available_models_for_switching())
                .unwrap_or_default(),
            ActiveProvider::Copilot => self
                .copilot_provider()
                .map(|copilot| copilot.available_models_for_switching())
                .unwrap_or_default(),
            ActiveProvider::Antigravity => self
                .antigravity_provider()
                .map(|antigravity| antigravity.available_models_for_switching())
                .unwrap_or_default(),
            ActiveProvider::Gemini => self
                .gemini_provider()
                .map(|gemini| gemini.available_models_for_switching())
                .unwrap_or_default(),
            ActiveProvider::Cursor => self
                .cursor_provider()
                .map(|cursor| cursor.available_models_for_switching())
                .unwrap_or_default(),
            ActiveProvider::Bedrock => self
                .bedrock_provider()
                .map(|bedrock| bedrock.available_models_for_switching())
                .unwrap_or_default(),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .map(|openrouter| openrouter.available_models_for_switching())
                .unwrap_or_default(),
        }
    }
    fn available_models_display(&self) -> Vec<String> {
        self.fresh_routes_memo_entry().listable_models
    }
    fn available_providers_for_model(&self, model: &str) -> Vec<String> {
        if let Some(model) = openrouter_catalog_model_id(model)
            && let Some(openrouter) = self.openrouter_provider() {
            return openrouter.available_providers_for_model(&model);
        }
        Vec::new()
    }
    fn provider_details_for_model(&self, model: &str) -> Vec<(String, String)> {
        if let Some(model) = openrouter_catalog_model_id(model)
            && let Some(openrouter) = self.openrouter_provider() {
            return openrouter.provider_details_for_model(&model);
        }
        Vec::new()
    }
    fn preferred_provider(&self) -> Option<String> {
        if let Some(openrouter) = self.openrouter_provider()
            && matches!(
                *self
                    .active
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
                ActiveProvider::OpenRouter
            ) {
            return openrouter.preferred_provider();
        }
        None
    }
    fn model_routes(&self) -> Vec<ModelRoute> {
        self.fresh_routes_memo_entry().routes
    }
    async fn prefetch_models(&self) -> Result<()> {
        let anthropic = self.anthropic_provider();
        let claude = self.claude_provider();
        let openai = self.openai_provider();
        let openrouter = self.openrouter_provider();
        let copilot = self
            .copilot_api
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let antigravity = self.antigravity_provider();
        let gemini = self.gemini_provider();
        let cursor = self.cursor_provider();
        let bedrock = self.bedrock_provider();
        let (
            anthropic_result,
            claude_result,
            openai_result,
            openrouter_result,
            copilot_result,
            antigravity_result,
            gemini_result,
            cursor_result,
            bedrock_result,
        ) = tokio::join!(
            async {
                match anthropic {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                } },
            async {
                match claude {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                } },
            async {
                match openai {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                } },
            async {
                match openrouter {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                } },
            async {
                match copilot {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                } },
            async {
                match antigravity {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                } },
            async {
                match gemini {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                } },
            async {
                match cursor {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                } },
            async {
                match bedrock {
                    Some(provider) => provider.prefetch_models().await,
                    None => Ok(()),
                }
            }, );
        let active_provider = self.active_provider();
        let mut errors = Vec::new();
        let mut optional_errors = Vec::new();
        for (provider_name, result) in [
            ("anthropic", anthropic_result),
            ("claude", claude_result),
            ("openai", openai_result),
            ("openrouter", openrouter_result),
            ("copilot", copilot_result),
            ("antigravity", antigravity_result),
            ("gemini", gemini_result),
            ("cursor", cursor_result),
            ("bedrock", bedrock_result),
        ] {
            if let Err(err) = result {
                let is_active = matches!(
                    (active_provider, provider_name),
                    (ActiveProvider::Claude, "anthropic" | "claude")
                        | (ActiveProvider::OpenAI, "openai")
                        | (ActiveProvider::OpenRouter, "openrouter")
                        | (ActiveProvider::Copilot, "copilot")
                        | (ActiveProvider::Antigravity, "antigravity")
                        | (ActiveProvider::Gemini, "gemini")
                        | (ActiveProvider::Cursor, "cursor")
                        | (ActiveProvider::Bedrock, "bedrock") );
                if !is_active || matches!(provider_name, "bedrock") {
                    optional_errors.push(format!("{provider_name}: {err}"));
                } else {
                    errors.push(format!("{provider_name}: {err}"));
                }
            }
        }
        if !optional_errors.is_empty() {
            crate::logging::warn(&format!(
                "Optional model catalog refresh failed: {}",
                optional_errors.join("; ")
            ));
        }
        if !errors.is_empty() {
            return Err(anyhow!("{}", errors.join("; ")));
        }
        self.invalidate_routes_memo_globally();
        Ok(())
    }
    fn on_auth_changed(&self) {
        self.handle_auth_changed(false);
    }
    fn on_auth_changed_preserve_current_provider(&self) {
        self.handle_auth_changed(true);
    }
    fn auth_model_refresh_pending(&self) -> bool {
        self.post_auth_refreshes_pending
            .load(std::sync::atomic::Ordering::Acquire)
            > 0
    }
    async fn invalidate_credentials(&self) {
        if let Some(anthropic) = self.anthropic_provider() {
            anthropic.invalidate_credentials().await;
        }
        if let Some(openai) = self.openai_provider() {
            openai.invalidate_credentials().await;
        }
    }
    async fn ensure_credentials_current(&self) -> Result<()> {
        match self.active_provider() {
            ActiveProvider::Claude if !self.use_claude_cli => {
                if let Some(provider) = self.anthropic_provider() {
                    provider.ensure_credentials_current().await?;
                }
            }
            ActiveProvider::OpenAI => {
                if let Some(provider) = self.openai_provider() {
                    provider.ensure_credentials_current().await?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn handles_tools_internally(&self) -> bool {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if self.anthropic_provider().is_some() {
                    false
                } else {
                    self.claude_provider()
                        .map(|c| c.handles_tools_internally())
                        .unwrap_or(false)
                }
            }
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|o| o.handles_tools_internally())
                .unwrap_or(false),
            ActiveProvider::Copilot => self
                .copilot_provider()
                .map(|o| o.handles_tools_internally())
                .unwrap_or(false),
            ActiveProvider::Antigravity => false,
            ActiveProvider::Gemini => false,
            ActiveProvider::Cursor => self
                .cursor_provider()
                .map(|o| o.handles_tools_internally())
                .unwrap_or(false),
            ActiveProvider::Bedrock => false, // jcode executes Bedrock tool calls
            ActiveProvider::OpenRouter => false, // jcode executes tools
        }
    }
    fn reasoning_effort(&self) -> Option<String> {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if self.use_claude_cli {
                    None
                } else {
                    self.anthropic_provider()
                        .and_then(|provider| provider.reasoning_effort())
                }
            }
            ActiveProvider::OpenAI => self.openai_provider().and_then(|o| o.reasoning_effort()),
            ActiveProvider::Copilot => None,
            ActiveProvider::Antigravity => None,
            ActiveProvider::Gemini => None,
            ActiveProvider::Cursor => None,
            ActiveProvider::Bedrock => None,
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .and_then(|o| o.reasoning_effort()),
        }
    }
    fn set_reasoning_effort(&self, effort: &str) -> Result<()> {
        match self.active_provider() {
            ActiveProvider::Claude if !self.use_claude_cli => self
                .anthropic_provider()
                .ok_or_else(|| anyhow::anyhow!("Anthropic provider not available"))?
                .set_reasoning_effort(effort),
            ActiveProvider::OpenAI => self
                .openai_provider()
                .ok_or_else(|| anyhow::anyhow!("OpenAI provider not available"))?
                .set_reasoning_effort(effort),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .ok_or_else(|| anyhow::anyhow!("OpenAI-compatible provider not available"))?
                .set_reasoning_effort(effort),
            _ => Err(anyhow::anyhow!(
                "Reasoning effort is only supported for OpenAI, Anthropic, and compatible reasoning models"
            )),
        }
    }
    fn available_efforts(&self) -> Vec<&'static str> {
        match self.active_provider() {
            ActiveProvider::Claude if !self.use_claude_cli => self
                .anthropic_provider()
                .map(|provider| provider.available_efforts())
                .unwrap_or_default(),
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|o| o.available_efforts())
                .unwrap_or_default(),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .map(|o| o.available_efforts())
                .unwrap_or_default(),
            ActiveProvider::Copilot => vec![],
            ActiveProvider::Antigravity => vec![],
            ActiveProvider::Gemini => vec![],
            ActiveProvider::Cursor => vec![],
            _ => vec![],
        }
    }
    fn service_tier(&self) -> Option<String> {
        match self.active_provider() {
            ActiveProvider::Claude if !self.use_claude_cli => {
                self.anthropic_provider().and_then(|a| a.service_tier())
            }
            ActiveProvider::OpenAI => self.openai_provider().and_then(|o| o.service_tier()),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .and_then(|o| o.service_tier()),
            _ => None,
        }
    }
    fn set_service_tier(&self, service_tier: &str) -> Result<()> {
        match self.active_provider() {
            ActiveProvider::Claude if !self.use_claude_cli => self
                .anthropic_provider()
                .ok_or_else(|| anyhow::anyhow!("Anthropic provider not available"))?
                .set_service_tier(service_tier),
            ActiveProvider::OpenAI => self
                .openai_provider()
                .ok_or_else(|| anyhow::anyhow!("OpenAI provider not available"))?
                .set_service_tier(service_tier),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .ok_or_else(|| anyhow::anyhow!("OpenAI-compatible provider not available"))?
                .set_service_tier(service_tier),
            _ => Err(anyhow::anyhow!(
                "Service tier switching is only supported for OpenAI models and Claude Opus 4.8"
            )),
        }
    }
    fn available_service_tiers(&self) -> Vec<&'static str> {
        match self.active_provider() {
            ActiveProvider::Claude if !self.use_claude_cli => self
                .anthropic_provider()
                .map(|a| a.available_service_tiers())
                .unwrap_or_default(),
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|o| o.available_service_tiers())
                .unwrap_or_default(),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .map(|o| o.available_service_tiers())
                .unwrap_or_default(),
            _ => vec![],
        }
    }
    fn native_compaction_mode(&self) -> Option<String> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .openai_provider()
                .and_then(|o| o.native_compaction_mode()),
            _ => None,
        }
    }
    fn native_compaction_threshold_tokens(&self) -> Option<usize> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .openai_provider()
                .and_then(|o| o.native_compaction_threshold_tokens()),
            _ => None,
        }
    }
    fn transport(&self) -> Option<String> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self.openai_provider().and_then(|o| o.transport()),
            _ => None,
        }
    }
    fn set_transport(&self, transport: &str) -> Result<()> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .openai_provider()
                .ok_or_else(|| anyhow::anyhow!("OpenAI provider not available"))?
                .set_transport(transport),
            _ => Err(anyhow::anyhow!(
                "Transport switching is only supported for OpenAI models"
            )),
        }
    }
    fn available_transports(&self) -> Vec<&'static str> {
        match self.active_provider() {
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|o| o.available_transports())
                .unwrap_or_default(),
            ActiveProvider::Gemini => vec![],
            ActiveProvider::Cursor => vec![],
            _ => vec![],
        }
    }
    fn supports_compaction(&self) -> bool {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if self.anthropic_provider().is_some() {
                    true
                } else {
                    self.claude_provider()
                        .map(|c| c.supports_compaction())
                        .unwrap_or(false)
                }
            }
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|o| o.supports_compaction())
                .unwrap_or(false),
            ActiveProvider::Copilot => self
                .copilot_provider()
                .map(|o| o.supports_compaction())
                .unwrap_or(false),
            ActiveProvider::Antigravity => self
                .antigravity_provider()
                .map(|o| o.supports_compaction())
                .unwrap_or(false),
            ActiveProvider::Gemini => self
                .gemini_provider()
                .map(|o| o.supports_compaction())
                .unwrap_or(false),
            ActiveProvider::Cursor => self
                .cursor_provider()
                .map(|o| o.supports_compaction())
                .unwrap_or(false),
            ActiveProvider::Bedrock => self
                .bedrock_provider()
                .map(|o| o.uses_jcode_compaction())
                .unwrap_or(false),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .map(|o| o.supports_compaction())
                .unwrap_or(false),
        }
    }
    fn uses_jcode_compaction(&self) -> bool {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if self.anthropic_provider().is_some() {
                    true
                } else {
                    self.claude_provider()
                        .map(|c| c.uses_jcode_compaction())
                        .unwrap_or(false)
                }
            }
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|o| o.uses_jcode_compaction())
                .unwrap_or(false),
            ActiveProvider::Copilot => self
                .copilot_provider()
                .map(|o| o.uses_jcode_compaction())
                .unwrap_or(false),
            ActiveProvider::Antigravity => self
                .antigravity_provider()
                .map(|o| o.uses_jcode_compaction())
                .unwrap_or(false),
            ActiveProvider::Gemini => self
                .gemini_provider()
                .map(|o| o.uses_jcode_compaction())
                .unwrap_or(false),
            ActiveProvider::Cursor => self
                .cursor_provider()
                .map(|o| o.uses_jcode_compaction())
                .unwrap_or(false),
            ActiveProvider::Bedrock => false,
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .map(|o| o.uses_jcode_compaction())
                .unwrap_or(false),
        }
    }
    async fn native_compact( &self, messages: &[Message], existing_summary_text: Option<&str>, existing_openai_encrypted_content: Option<&str>, ) -> Result<NativeCompactionResult> {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if let Some(anthropic) = self.anthropic_provider() {
                    anthropic
                        .native_compact(
                            messages,
                            existing_summary_text,
                            existing_openai_encrypted_content, )
                        .await
                } else if let Some(claude) = self.claude_provider() {
                    claude
                        .native_compact(
                            messages,
                            existing_summary_text,
                            existing_openai_encrypted_content, )
                        .await
                } else {
                    Err(anyhow::anyhow!("Claude provider unavailable"))
                }
            }
            ActiveProvider::OpenAI => {
                if let Some(openai) = self.openai_provider() {
                    openai
                        .native_compact(
                            messages,
                            existing_summary_text,
                            existing_openai_encrypted_content, )
                        .await
                } else {
                    Err(anyhow::anyhow!("OpenAI provider unavailable"))
                }
            }
            ActiveProvider::Copilot => {
                let provider = self.copilot_provider();
                if let Some(copilot) = provider {
                    copilot
                        .native_compact(
                            messages,
                            existing_summary_text,
                            existing_openai_encrypted_content, )
                        .await
                } else {
                    Err(anyhow::anyhow!("Copilot provider unavailable"))
                }
            }
            ActiveProvider::Antigravity => Err(anyhow::anyhow!(
                "Antigravity does not support native compaction"
            )),
            ActiveProvider::Gemini => {
                let provider = self.gemini_provider();
                if let Some(gemini) = provider {
                    gemini
                        .native_compact(
                            messages,
                            existing_summary_text,
                            existing_openai_encrypted_content, )
                        .await
                } else {
                    Err(anyhow::anyhow!("Gemini provider unavailable"))
                }
            }
            ActiveProvider::Cursor => {
                let provider = self.cursor_provider();
                if let Some(cursor) = provider {
                    cursor
                        .native_compact(
                            messages,
                            existing_summary_text,
                            existing_openai_encrypted_content, )
                        .await
                } else {
                    Err(anyhow::anyhow!("Cursor provider unavailable"))
                }
            }
            ActiveProvider::Bedrock => Err(anyhow::anyhow!(
                "AWS Bedrock does not support native compaction"
            )),
            ActiveProvider::OpenRouter => {
                let provider = self.active_openrouter_execution_provider();
                if let Some(openrouter) = provider {
                    openrouter
                        .native_compact(
                            messages,
                            existing_summary_text,
                            existing_openai_encrypted_content, )
                        .await
                } else {
                    Err(anyhow::anyhow!("OpenRouter provider unavailable"))
                }
            }
        }
    }
    fn set_premium_mode(&self, mode: PremiumMode) {
        if let Some(copilot) = self.copilot_provider() {
            copilot.set_premium_mode(mode);
        }
    }
    fn premium_mode(&self) -> PremiumMode {
        if let Some(copilot) = self.copilot_provider() {
            copilot.premium_mode()
        } else {
            PremiumMode::Normal
        }
    }
    fn drain_startup_notices(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .startup_notices
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()), )
    }
    fn context_window(&self) -> usize {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if let Some(anthropic) = self.anthropic_provider() {
                    anthropic.context_window()
                } else if let Some(claude) = self.claude_provider() {
                    claude.context_window()
                } else {
                    DEFAULT_CONTEXT_LIMIT
                }
            }
            ActiveProvider::OpenAI => self
                .openai_provider()
                .map(|o| o.context_window())
                .unwrap_or(DEFAULT_CONTEXT_LIMIT),
            ActiveProvider::Copilot => self
                .copilot_provider()
                .map(|o| o.context_window())
                .unwrap_or(DEFAULT_CONTEXT_LIMIT),
            ActiveProvider::Antigravity => self
                .antigravity_provider()
                .map(|o| o.context_window())
                .unwrap_or(DEFAULT_CONTEXT_LIMIT),
            ActiveProvider::Gemini => self
                .gemini_provider()
                .map(|o| o.context_window())
                .unwrap_or(DEFAULT_CONTEXT_LIMIT),
            ActiveProvider::Cursor => self
                .cursor_provider()
                .map(|o| o.context_window())
                .unwrap_or(DEFAULT_CONTEXT_LIMIT),
            ActiveProvider::Bedrock => self
                .bedrock_provider()
                .map(|o| o.context_window())
                .unwrap_or(DEFAULT_CONTEXT_LIMIT),
            ActiveProvider::OpenRouter => self
                .active_openrouter_execution_provider()
                .map(|o| o.context_window())
                .unwrap_or(DEFAULT_CONTEXT_LIMIT),
        }
    }
    fn fork(&self) -> Arc<dyn Provider> {
        let current_model = self.model();
        let active = self.active_provider();
        let claude = if matches!(active, ActiveProvider::Claude) && self.claude_provider().is_some() {
            external::instantiate_expected_external_provider(external::CLAUDE_CLI_RUNTIME)
        } else {
            None
        };
        let anthropic = if self.anthropic_provider().is_some() {
            external::instantiate_expected_external_provider(external::ANTHROPIC_RUNTIME)
        } else {
            None
        };
        let openai = if self.openai_provider().is_some() {
            external::instantiate_expected_external_provider(external::OPENAI_RUNTIME)
        } else {
            None
        };
        let copilot_api = self
            .copilot_api
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let antigravity_provider = self
            .antigravity
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let gemini_provider = self
            .gemini
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let cursor_provider = if self
            .cursor
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some() {
            external::instantiate_expected_external_provider(external::CURSOR_RUNTIME)
        } else {
            None
        };
        let bedrock_provider = if self.bedrock_provider().is_some() {
            Some(Arc::new(bedrock::BedrockProvider::new()))
        } else {
            None
        };
        let openrouter = if self
            .openrouter
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some() {
            external::instantiate_openrouter_runtime(external::OpenRouterRuntimeSpec::Default).ok()
        } else {
            None
        };
        let provider = Self {
            claude: RwLock::new(claude),
            anthropic: RwLock::new(anthropic),
            openai: RwLock::new(openai),
            copilot_api: RwLock::new(copilot_api),
            antigravity: RwLock::new(antigravity_provider),
            gemini: RwLock::new(gemini_provider),
            cursor: RwLock::new(cursor_provider),
            bedrock: RwLock::new(bedrock_provider),
            openrouter: RwLock::new(openrouter),
            openai_compatible_profiles: RwLock::new(HashMap::new()),
            active_openai_compatible_profile: RwLock::new(None),
            active: RwLock::new(active),
            use_claude_cli: self.use_claude_cli,
            startup_notices: RwLock::new(Vec::new()),
            forced_provider: self.forced_provider,
            routes_memo: Mutex::new(None),
            post_auth_refreshes_pending: Arc::clone(&self.post_auth_refreshes_pending),
        };
        provider.spawn_anthropic_catalog_refresh_if_needed();
        provider.spawn_openai_catalog_refresh_if_needed();
        let switch_request = self.fork_model_switch_request(active, &current_model);
        let _ = provider.set_model(&switch_request);
        Arc::new(provider)
    }
    fn fork_for_new_session(&self) -> Arc<dyn Provider> {
        let provider = Self::new_fast();
        if self.forced_provider.is_some() {
            let active = self.active_provider();
            let current_model = self.model();
            let switch_request = self.fork_model_switch_request(active, &current_model);
            if let Err(error) = provider.set_model(&switch_request) {
                crate::logging::warn(&format!(
                    "Failed to preserve forced provider model '{}' for new session: {}",
                    switch_request, error
                ));
            }
        }
        Arc::new(provider)
    }
    fn native_result_sender(&self) -> Option<NativeToolResultSender> {
        match self.active_provider() {
            ActiveProvider::Claude => {
                if self.anthropic_provider().is_some() {
                    None
                } else {
                    self.claude_provider()
                        .and_then(|c| c.native_result_sender())
                }
            }
            ActiveProvider::OpenAI => None,
            ActiveProvider::Copilot => None,
            ActiveProvider::Antigravity => None,
            ActiveProvider::Gemini => None,
            ActiveProvider::Cursor => None,
            ActiveProvider::Bedrock => None,
            ActiveProvider::OpenRouter => None,
        }
    }
    fn switch_active_provider_to(&self, provider: &str) -> Result<()> {
        let target = Self::parse_provider_hint(provider)
            .ok_or_else(|| anyhow::anyhow!("Unknown provider `{}`", provider))?;
        if !self.provider_is_configured(target) {
            anyhow::bail!(
                "Provider `{}` is not configured in this session",
                Self::provider_key(target) );
        }
        self.set_active_provider(target);
        self.auto_select_multi_account_for_provider(target);
        Ok(())
    }
}
