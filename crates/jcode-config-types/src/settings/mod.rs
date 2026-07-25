use super::*;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureConfig {
    /// Enable memory retrieval/extraction features (default: true)
    pub memory: bool,
    /// Enable swarm coordination features (default: true)
    pub swarm: bool,
    /// Enable Mermaid rendering and Mermaid-specific model guidance (default: true)
    pub mermaid: bool,
    /// Inject timestamps into user messages and tool results sent to the model (default: true)
    pub message_timestamps: bool,
    /// Persist auto-recalled memory injections into normal session history instead of sending
    /// them as request-only ephemeral suffix messages (default: false)
    pub persist_memory_injections: bool,
    /// Surface an in-chat system message whenever a request misses the KV cache
    /// for a harness-caused (avoidable) reason: the system prompt, tool set, or
    /// message prefix changed without the conversation legitimately growing.
    /// These should essentially never happen, so the notice acts as a loud alarm
    /// that something in the harness silently invalidated the prefix cache
    /// (default: true).
    pub kv_cache_miss_notices: bool,
    /// Update channel: "stable" (releases only) or "main" (latest commits)
    pub update_channel: UpdateChannel,
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            memory: true,
            swarm: true,
            mermaid: true,
            message_timestamps: true,
            persist_memory_injections: false,
            kv_cache_miss_notices: true,
            update_channel: UpdateChannel::default(),
        }
    }
}

/// Search engine used by the websearch tool.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchEngine {
    /// DuckDuckGo HTML search, no API key required.
    #[default]
    Duckduckgo,
    /// Bing search. Uses the Bing API when configured, otherwise Bing HTML search.
    Bing,
    /// SearXNG metasearch instance (JSON API). Requires `searxng_url` (or the
    /// `JCODE_SEARXNG_URL` env var) to point at a SearXNG instance. Useful on
    /// hosts where DuckDuckGo/Bing block the request via TLS fingerprinting.
    Searxng,
}

impl WebSearchEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Duckduckgo => "duckduckgo",
            Self::Bing => "bing",
            Self::Searxng => "searxng",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" => Some(Self::Duckduckgo),
            "bing" => Some(Self::Bing),
            "searxng" | "searx" => Some(Self::Searxng),
            _ => None,
        }
    }
}

/// Configuration for the websearch tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// Preferred engine when the tool input does not specify one.
    pub engine: WebSearchEngine,
    /// Keyless HTML engines to try after the preferred engine fails.
    pub fallback_engines: Vec<WebSearchEngine>,
    /// Optional Bing API key for primary Bing searches. Fallback Bing uses keyless HTML search.
    pub bing_api_key: Option<String>,
    /// Environment variable containing the Bing API key.
    pub bing_api_key_env: String,
    /// Bing market, e.g. "en-US" or "zh-CN".
    pub bing_market: String,
    /// Base URL of a SearXNG instance (e.g. "https://searx.example.org"), used
    /// by the `searxng` engine. When empty, the `searxng_url_env` variable is
    /// consulted instead.
    pub searxng_url: Option<String>,
    /// Environment variable containing the SearXNG base URL.
    pub searxng_url_env: String,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            engine: WebSearchEngine::Duckduckgo,
            fallback_engines: vec![WebSearchEngine::Bing],
            bing_api_key: None,
            bing_api_key_env: "JCODE_BING_API_KEY".to_string(),
            bing_market: "en-US".to_string(),
            searxng_url: None,
            searxng_url_env: "JCODE_SEARXNG_URL".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Default model to use (e.g. "claude-opus-4-8", "copilot:claude-opus-4.6")
    pub default_model: Option<String>,
    /// Default provider to use (claude|openai|copilot|openrouter)
    pub default_provider: Option<String>,
    /// Reasoning effort for OpenAI Responses API (none|minimal|low|medium|high|xhigh|max)
    pub openai_reasoning_effort: Option<String>,
    /// Reasoning effort for Anthropic Messages API output_config (none|low|medium|high|xhigh; max aliases to strongest supported)
    pub anthropic_reasoning_effort: Option<String>,
    /// OpenAI transport mode (auto|websocket|https)
    pub openai_transport: Option<String>,
    /// OpenAI service tier override (priority|flex)
    pub openai_service_tier: Option<String>,
    /// OpenAI native compaction mode: "auto", "explicit", or "off".
    pub openai_native_compaction_mode: String,
    /// Token threshold at which OpenAI auto native compaction should trigger.
    pub openai_native_compaction_threshold_tokens: usize,
    /// Preserve provider-native reasoning/thinking items for future-turn context when supported.
    pub preserve_reasoning_context: bool,
    /// How to handle cross-provider failover when the same input would be resent elsewhere.
    pub cross_provider_failover: CrossProviderFailoverMode,
    /// Whether jcode should automatically try another account on the same provider
    /// before falling back to a different provider.
    pub same_provider_account_failover: bool,
    /// Copilot premium request mode: "normal", "one", or "zero"
    /// "zero" means all requests are free (no premium requests consumed)
    pub copilot_premium: Option<String>,
    /// When set (non-empty), /model only lists routes from these providers.
    /// Entries match provider labels ("openai", "anthropic", "copilot",
    /// "openrouter", ...), api methods ("claude-oauth",
    /// "openai-compatible:myprofile", ...), or openai-compatible profile ids
    /// ("myprofile"). The active model's routes always stay visible.
    pub model_picker_providers: Option<Vec<String>>,
    /// Max seconds to wait for streaming data before timing out a request with
    /// no data received. Raise this for slow reasoning models (e.g. DeepSeek)
    /// that think silently for minutes before emitting tokens. Default: 180.
    /// Overridable per-launch via `JCODE_STREAM_IDLE_TIMEOUT_SECS`.
    pub stream_idle_timeout_secs: u64,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            default_model: None,
            default_provider: None,
            openai_reasoning_effort: Some("low".to_string()),
            anthropic_reasoning_effort: None,
            openai_transport: None,
            openai_service_tier: Some("priority".to_string()),
            openai_native_compaction_mode: "auto".to_string(),
            openai_native_compaction_threshold_tokens: 200_000,
            preserve_reasoning_context: true,
            cross_provider_failover: CrossProviderFailoverMode::Countdown,
            same_provider_account_failover: true,
            copilot_premium: None,
            model_picker_providers: None,
            stream_idle_timeout_secs: 180,
        }
    }
}

/// Ambient mode configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AmbientConfig {
    /// Enable ambient mode (default: false)
    pub enabled: bool,
    /// Provider override (default: auto-select)
    pub provider: Option<String>,
    /// Model override (default: provider's strongest)
    pub model: Option<String>,
    /// Allow API key usage (default: false, only OAuth)
    pub allow_api_keys: bool,
    /// Daily token budget when using API keys
    pub api_daily_budget: Option<u64>,
    /// Minimum interval between cycles in minutes (default: 5)
    pub min_interval_minutes: u32,
    /// Maximum interval between cycles in minutes (default: 120)
    pub max_interval_minutes: u32,
    /// Pause ambient when user has active session (default: true)
    pub pause_on_active_session: bool,
    /// Enable proactive work vs garden-only (default: true)
    pub proactive_work: bool,
    /// Proactive work branch prefix (default: "ambient/")
    pub work_branch_prefix: String,
    /// Show ambient cycle in a terminal window (default: true)
    pub visible: bool,
}

impl Default for AmbientConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: None,
            model: None,
            allow_api_keys: false,
            api_daily_budget: None,
            min_interval_minutes: 5,
            max_interval_minutes: 120,
            pause_on_active_session: true,
            proactive_work: true,
            work_branch_prefix: "ambient/".to_string(),
            visible: true,
        }
    }
}

/// Desktop notification configuration for interactive sessions.
///
/// Unlike `[safety]` (ambient-mode ntfy/email/channel notifications), this
/// section controls lightweight local desktop notifications for the normal
/// interactive TUI, e.g. "agent finished a long turn".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    /// Send a desktop notification when an agent turn completes (default: true).
    /// Notifications fire only for long turns (see thresholds below) and, by
    /// default, only while the terminal window is unfocused.
    pub turn_complete: bool,
    /// Minimum turn duration, in seconds, before a completed turn notifies
    /// (default: 120).
    pub turn_complete_min_secs: u64,
    /// Lower duration threshold, in seconds, used when the session has todos
    /// recorded, since todos indicate longer task-style work (default: 30).
    pub turn_complete_todo_min_secs: u64,
    /// Only notify while the terminal window is unfocused (default: true).
    /// Requires a terminal that reports focus events (most modern terminals).
    pub turn_complete_only_when_unfocused: bool,
    /// macOS Notification Center sound name played on turn completion
    /// (e.g. "Glass", "Ping", "Hero"). Empty string disables the sound.
    /// Ignored on non-macOS platforms. Default: "Glass".
    pub turn_complete_sound: String,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            turn_complete: true,
            turn_complete_min_secs: 120,
            turn_complete_todo_min_secs: 30,
            turn_complete_only_when_unfocused: true,
            turn_complete_sound: "Glass".to_string(),
        }
    }
}

/// Safety system & notification configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    /// ntfy.sh topic name (required for push notifications)
    pub ntfy_topic: Option<String>,
    /// ntfy.sh server URL (default: https://ntfy.sh)
    pub ntfy_server: String,
    /// Enable desktop notifications via notify-send (default: true)
    pub desktop_notifications: bool,
    /// Enable email notifications (default: false)
    pub email_enabled: bool,
    /// Email recipient
    pub email_to: Option<String>,
    /// SMTP host (e.g. smtp.gmail.com)
    pub email_smtp_host: Option<String>,
    /// SMTP port (default: 587)
    pub email_smtp_port: u16,
    /// Email sender address
    pub email_from: Option<String>,
    /// SMTP password (prefer JCODE_SMTP_PASSWORD env var)
    pub email_password: Option<String>,
    /// IMAP host for receiving email replies (e.g. imap.gmail.com)
    pub email_imap_host: Option<String>,
    /// IMAP port (default: 993)
    pub email_imap_port: u16,
    /// Enable email reply → agent directive feature (default: false)
    pub email_reply_enabled: bool,
    /// Enable Telegram notifications (default: false)
    pub telegram_enabled: bool,
    /// Telegram bot token (from @BotFather)
    pub telegram_bot_token: Option<String>,
    /// Telegram chat ID to send messages to
    pub telegram_chat_id: Option<String>,
    /// Enable Telegram reply → agent directive feature (default: false)
    pub telegram_reply_enabled: bool,
    /// Enable Discord notifications (default: false)
    pub discord_enabled: bool,
    /// Discord bot token
    pub discord_bot_token: Option<String>,
    /// Discord channel ID to send messages to
    pub discord_channel_id: Option<String>,
    /// Discord bot user ID (for filtering own messages in polling)
    pub discord_bot_user_id: Option<String>,
    /// Enable Discord reply → agent directive feature (default: false)
    pub discord_reply_enabled: bool,
    /// Enable the Jade cloud relay channel (remote control via cloud mailbox, default: false)
    pub jade_relay_enabled: bool,
    /// Jade relay API base URL (e.g. https://...lambda-url.us-east-1.on.aws/)
    pub jade_relay_api_base: Option<String>,
    /// Jade relay bearer token (prefer JCODE_JADE_RELAY_TOKEN env var)
    pub jade_relay_token: Option<String>,
    /// Jade relay token id header (x-jade-token-id), used for fast token lookup
    pub jade_relay_token_id: Option<String>,
    /// Jade relay user id (channel scope; defaults to the token's user when omitted)
    pub jade_relay_user_id: Option<String>,
    /// Jade relay session id to bind this laptop's listener to (the channel = user_id/session_id)
    pub jade_relay_session_id: Option<String>,
    /// Enable Jade relay prompt → agent directive feature (default: false)
    pub jade_relay_reply_enabled: bool,
    /// Enable Jade relay device launch commands that open headed local sessions (default: false)
    pub jade_relay_launch_enabled: bool,
    /// Default working directory for remotely launched headed sessions
    pub jade_relay_launch_working_dir: Option<String>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            ntfy_topic: None,
            ntfy_server: "https://ntfy.sh".to_string(),
            desktop_notifications: true,
            email_enabled: false,
            email_to: None,
            email_smtp_host: None,
            email_smtp_port: 587,
            email_from: None,
            email_password: None,
            email_imap_host: None,
            email_imap_port: 993,
            email_reply_enabled: false,
            telegram_enabled: false,
            telegram_bot_token: None,
            telegram_chat_id: None,
            telegram_reply_enabled: false,
            discord_enabled: false,
            discord_bot_token: None,
            discord_channel_id: None,
            discord_bot_user_id: None,
            discord_reply_enabled: false,
            jade_relay_enabled: false,
            jade_relay_api_base: None,
            jade_relay_token: None,
            jade_relay_token_id: None,
            jade_relay_user_id: None,
            jade_relay_session_id: None,
            jade_relay_reply_enabled: false,
            jade_relay_launch_enabled: false,
            jade_relay_launch_working_dir: None,
        }
    }
}

/// WebSocket gateway configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewayConfig {
    /// Enable the WebSocket gateway (default: false)
    pub enabled: bool,
    /// TCP port to listen on (default: 7643)
    pub port: u16,
    /// Bind address (default: 0.0.0.0)
    pub bind_addr: String,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 7643,
            bind_addr: "0.0.0.0".to_string(),
        }
    }
}

/// Power-management configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerConfig {
    /// Prevent automatic system sleep while any jcode session is actively
    /// streaming/processing. Linux also asks logind to block lid-switch suspend.
    /// Windows cannot override a user-initiated lid close or power-button action;
    /// those remain controlled by the active Windows power plan. The display is
    /// still allowed to sleep. Default: true.
    ///
    /// Honored by the shared `jcode serve` daemon. The `JCODE_DISABLE_POWER_INHIBIT`
    /// environment variable forces this off regardless of the config value.
    pub prevent_sleep_while_streaming: bool,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            prevent_sleep_while_streaming: true,
        }
    }
}

/// A single global launch hotkey: a chord plus the directory it opens jcode in.
///
/// `dir` is usually an absolute path, but a few sentinels keep dynamic targets
/// working without rewriting config on every launch:
/// - `$HOME` -> the user's home directory.
/// - `$LAST_DIR` -> the most recent non-home project directory jcode ran in.
/// - `$LAST_REPO` -> the most recent jcode repo (for self-dev).
///
/// `self_dev = true` opens the directory as a self-dev session (passes the
/// `self-dev` subcommand). `label` is an optional human name used in notices.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchHotkeyEntry {
    /// jcode-style chord string, e.g. `cmd+;`, `cmd+[`, `cmd+shift+'`.
    pub chord: String,
    /// Directory to open (absolute path or a `$HOME`/`$LAST_DIR`/`$LAST_REPO`
    /// sentinel).
    pub dir: String,
    /// Optional short label (e.g. the repo's directory name) for notices.
    #[serde(default)]
    pub label: String,
    /// Open as a self-dev session instead of a normal session.
    #[serde(default)]
    pub self_dev: bool,
}

/// Configuration for the global "launch a new jcode" hotkeys (macOS).
///
/// When `entries` is empty, jcode uses its built-in defaults (`Cmd+;` -> home,
/// `Cmd+'` -> last project, `Cmd+Shift+'` -> self-dev). Auto-import can bake a
/// richer, per-repo mapping here once: the top repo on `Cmd+;`, home on
/// `Cmd+'`, and the next repos on `Cmd+[` / `Cmd+]` / `Cmd+\`. Once baked the
/// mapping is static and does not move around as the user's activity changes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LaunchHotkeysConfig {
    /// Whether the global launch hotkeys are installed at all. `None` means
    /// "not decided yet" (fall back to the legacy auto-install gating); `Some`
    /// is an explicit user/import choice.
    pub enabled: Option<bool>,
    /// Explicit chord -> directory mapping. Empty = use built-in defaults.
    pub entries: Vec<LaunchHotkeyEntry>,
    /// Set true once auto-import has populated `entries`, so we only bake the
    /// per-repo mapping a single time and never clobber later user edits.
    pub imported: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_engine_defaults_to_rolling_and_displays_stably() {
        let config = CompactionConfig::default();
        assert_eq!(config.engine, CompactionEngine::Rolling);
        assert_eq!(config.engine.to_string(), "rolling");
        assert_eq!(config.model, None);
        assert_eq!(CompactionEngine::parse("LCM"), Some(CompactionEngine::Lcm));
    }

    #[test]
    fn compaction_config_deserializes_new_fields_without_changing_old_defaults() {
        let config: CompactionConfig =
            serde_json::from_str(r#"{"engine":"lcm","model":"openai-api:gpt-5.5"}"#)
                .expect("compaction config should deserialize");
        assert_eq!(config.engine, CompactionEngine::Lcm);
        assert_eq!(config.model.as_deref(), Some("openai-api:gpt-5.5"));

        let legacy: CompactionConfig =
            serde_json::from_str("{}").expect("legacy config should deserialize");
        assert_eq!(legacy.engine, CompactionEngine::Rolling);
        assert_eq!(legacy.model, None);
    }
}
