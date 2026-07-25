use serde::{Deserialize, Serialize};

pub mod keybindings;
pub use keybindings::{
    KEYBINDING_DEFAULTS, KeybindingDefault, KeybindingIssue, KeybindingIssueKind,
    KeybindingPlatform, KeybindingProvenance, PlatformDefault, default_binding, default_binding_or,
    keybinding_default, keybinding_defaults_report, validate_keybinding_defaults,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompactionMode {
    #[default]
    Reactive,
    Proactive,
    Semantic,
}

impl CompactionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reactive => "reactive",
            Self::Proactive => "proactive",
            Self::Semantic => "semantic",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "reactive" => Some(Self::Reactive),
            "proactive" => Some(Self::Proactive),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompactionEngine {
    #[default]
    Rolling,
    Lcm,
}

impl CompactionEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rolling => "rolling",
            Self::Lcm => "lcm",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "rolling" => Some(Self::Rolling),
            "lcm" => Some(Self::Lcm),
            _ => None,
        }
    }
}

impl std::fmt::Display for CompactionEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SessionPickerResumeAction {
    NewTerminal,
    #[default]
    CurrentTerminal,
}

impl SessionPickerResumeAction {
    pub fn alternate(self) -> Self {
        match self {
            Self::NewTerminal => Self::CurrentTerminal,
            Self::CurrentTerminal => Self::NewTerminal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffDisplayMode {
    Off,
    #[default]
    Inline,
    #[serde(
        rename = "full-inline",
        alias = "full_inline",
        alias = "fullinline",
        alias = "inline-full",
        alias = "inline_full",
        alias = "inlinefull",
        alias = "full"
    )]
    FullInline,
    Pinned,
    File,
}

impl DiffDisplayMode {
    pub fn is_inline(&self) -> bool {
        matches!(self, Self::Inline | Self::FullInline)
    }

    pub fn is_full_inline(&self) -> bool {
        matches!(self, Self::FullInline)
    }

    pub fn is_pinned(&self) -> bool {
        matches!(self, Self::Pinned)
    }

    pub fn is_file(&self) -> bool {
        matches!(self, Self::File)
    }

    pub fn has_side_pane(&self) -> bool {
        matches!(self, Self::Pinned | Self::File)
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Inline,
            Self::Inline => Self::FullInline,
            Self::FullInline => Self::Pinned,
            Self::Pinned => Self::File,
            Self::File => Self::Off,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Inline => "Inline",
            Self::FullInline => "Inline Full",
            Self::Pinned => "Pinned",
            Self::File => "File",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverscrollStatusMode {
    Off,
    On,
    #[default]
    Overscroll,
}

impl OverscrollStatusMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Overscroll => "overscroll",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagramDisplayMode {
    #[default]
    None,
    Margin,
    Pinned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagramPanePosition {
    #[default]
    Side,
    Top,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkdownSpacingMode {
    #[default]
    Compact,
    Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LatexRenderingMode {
    None,
    Unicode,
    #[default]
    Image,
}

impl LatexRenderingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unicode => "unicode",
            Self::Image => "image",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "raw" | "off" => Some(Self::None),
            "unicode" | "terminal" | "text" => Some(Self::Unicode),
            "image" | "images" | "png" => Some(Self::Image),
            _ => None,
        }
    }
}

impl MarkdownSpacingMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Document => "Document",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningDisplayMode {
    #[default]
    Off,
    Full,
    Current,
}

impl ReasoningDisplayMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Full => "Full",
            Self::Current => "Current",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Current,
            Self::Current => Self::Full,
            Self::Full => Self::Off,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "off" | "none" | "false" | "0" | "no" => Some(Self::Off),
            "full" | "all" | "true" | "1" | "yes" | "on" => Some(Self::Full),
            "current" | "live" | "ephemeral" | "collapse" => Some(Self::Current),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Main,
}

impl UpdateChannel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" | "release" => Some(Self::Stable),
            "main" | "nightly" | "edge" => Some(Self::Main),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for UpdateChannel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value).unwrap_or_default())
    }
}

impl std::fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stable => write!(f, "stable"),
            Self::Main => write!(f, "main"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CrossProviderFailoverMode {
    #[default]
    Countdown,
    Manual,
}

impl CrossProviderFailoverMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Countdown => "countdown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "manual" => Some(Self::Manual),
            "countdown" | "auto" | "automatic" => Some(Self::Countdown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    pub engine: CompactionEngine,
    pub model: Option<String>,
    pub mode: CompactionMode,

    pub lookahead_turns: usize,

    pub ewma_alpha: f32,

    pub proactive_floor: f32,

    pub min_samples: usize,

    pub stall_window: usize,

    pub min_turns_between_compactions: usize,

    pub topic_shift_threshold: f32,

    pub relevance_keep_threshold: f32,

    pub goal_window_turns: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            engine: CompactionEngine::default(),
            model: None,
            mode: CompactionMode::Reactive,
            lookahead_turns: 15,
            ewma_alpha: 0.3,
            proactive_floor: 0.40,
            min_samples: 3,
            stall_window: 5,
            min_turns_between_compactions: 10,
            topic_shift_threshold: 0.45,
            relevance_keep_threshold: 0.65,
            goal_window_turns: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NamedProviderType {
    #[serde(alias = "openai-compatible", alias = "openai_compatible")]
    #[default]
    OpenAiCompatible,
    OpenRouter,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NamedProviderAuth {
    #[default]
    Bearer,
    Header,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default)]
pub struct NamedProviderModelConfig {
    pub id: String,
    #[serde(
        default,
        alias = "context_limit",
        alias = "context-length",
        alias = "context-window",
        alias = "context_length",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_window: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NamedProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: NamedProviderType,
    pub base_url: String,
    pub api: Option<String>,
    #[serde(default, alias = "wire_api", alias = "wire-api")]
    pub wire_api: Option<String>,
    #[serde(
        default,
        alias = "swarm-reasoning-effort",
        skip_serializing_if = "Option::is_none"
    )]
    pub swarm_reasoning_effort: Option<String>,
    pub auth: NamedProviderAuth,
    pub auth_header: Option<String>,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
    pub env_file: Option<String>,
    pub default_model: Option<String>,
    pub requires_api_key: Option<bool>,
    #[serde(default)]
    pub provider_routing: bool,
    #[serde(default)]
    pub model_catalog: bool,
    #[serde(default)]
    pub allow_provider_pinning: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<NamedProviderModelConfig>,
    #[serde(default, alias = "extra-body", skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
    #[serde(
        default,
        alias = "supports-reasoning-effort",
        alias = "reasoning_effort",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_reasoning_effort: Option<bool>,
}

impl Default for NamedProviderConfig {
    fn default() -> Self {
        Self {
            provider_type: NamedProviderType::OpenAiCompatible,
            base_url: String::new(),
            api: None,
            wire_api: None,
            swarm_reasoning_effort: None,
            auth: NamedProviderAuth::Bearer,
            auth_header: None,
            api_key_env: None,
            api_key: None,
            env_file: None,
            default_model: None,
            requires_api_key: None,
            provider_routing: false,
            model_catalog: false,
            allow_provider_pinning: false,
            models: Vec::new(),
            extra_body: None,
            supports_reasoning_effort: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AuthConfig {
    pub trusted_external_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted_external_source_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    pub swarm_model: Option<String>,
    pub swarm_spawn_mode: SwarmSpawnMode,
    pub swarm_gallery_max_pct: Option<u8>,
    #[serde(default)]
    pub swarm_strip_layout: SwarmStripLayout,
    pub memory_model: Option<String>,
    #[serde(default = "default_memory_sidecar_enabled")]
    pub memory_sidecar_enabled: bool,
    #[serde(default = "default_memory_rerank_cadence")]
    pub memory_rerank_cadence: usize,
    #[serde(default = "default_memory_rerank_votes")]
    pub memory_rerank_votes: usize,
    #[serde(default = "default_memory_rerank_min_agree")]
    pub memory_rerank_min_agree: usize,
    #[serde(default = "default_memory_embedding_backend")]
    pub memory_embedding_backend: String,
    #[serde(default)]
    pub memory_embedding_model: Option<String>,
    #[serde(default)]
    pub memory_embedding_base_url: Option<String>,
    #[serde(default)]
    pub memory_embedding_dim: Option<usize>,
    #[serde(default = "default_swarm_max_concurrent_agents")]
    pub swarm_max_concurrent_agents: usize,
}

fn default_swarm_max_concurrent_agents() -> usize {
    32
}

fn default_memory_embedding_backend() -> String {
    "local".to_string()
}

fn default_memory_sidecar_enabled() -> bool {
    true
}

fn default_memory_rerank_cadence() -> usize {
    3
}

fn default_memory_rerank_votes() -> usize {
    2
}

fn default_memory_rerank_min_agree() -> usize {
    2
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            swarm_model: None,
            swarm_spawn_mode: SwarmSpawnMode::default(),
            swarm_gallery_max_pct: None,
            swarm_strip_layout: SwarmStripLayout::default(),
            memory_model: None,
            memory_sidecar_enabled: default_memory_sidecar_enabled(),
            memory_rerank_cadence: default_memory_rerank_cadence(),
            memory_rerank_votes: default_memory_rerank_votes(),
            memory_rerank_min_agree: default_memory_rerank_min_agree(),
            memory_embedding_backend: default_memory_embedding_backend(),
            memory_embedding_model: None,
            memory_embedding_base_url: None,
            memory_embedding_dim: None,
            swarm_max_concurrent_agents: default_swarm_max_concurrent_agents(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SwarmSpawnMode {
    Visible,
    Headless,
    #[default]
    Inline,
    Auto,
}

impl SwarmSpawnMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "visible" | "headed" => Some(Self::Visible),
            "headless" => Some(Self::Headless),
            "inline" => Some(Self::Inline),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Headless => "headless",
            Self::Inline => "inline",
            Self::Auto => "auto",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SwarmStripLayout {
    #[default]
    Vertical,
    Horizontal,
}

impl SwarmStripLayout {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "vertical" | "list" => Some(Self::Vertical),
            "horizontal" | "chips" | "strip" => Some(Self::Horizontal),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TerminalConfig {
    pub spawn_hook: Option<String>,
    pub focus_hook: Option<String>,
    pub preferred: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    pub turn_start: Option<String>,
    pub turn_end: Option<String>,
    pub session_start: Option<String>,
    pub session_end: Option<String>,
    pub pre_tool: Option<String>,
    pub post_tool: Option<String>,
    pub pre_tool_timeout_ms: u64,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            turn_start: None,
            turn_end: None,
            session_start: None,
            session_end: None,
            pre_tool: None,
            post_tool: None,
            pre_tool_timeout_ms: 5000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutoReviewConfig {
    pub enabled: bool,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SponsorsConfig {
    pub enabled: bool,
    pub endpoint: String,
}

impl Default for SponsorsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            endpoint: "https://api.jcode.sh/v1/discovery".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutoJudgeConfig {
    pub enabled: bool,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KeybindingsConfig {
    pub scroll_up: String,
    pub scroll_down: String,
    pub scroll_page_up: String,
    pub scroll_page_down: String,
    pub model_switch_next: String,
    pub model_switch_prev: String,
    pub fallback_switch: String,
    pub effort_increase: String,
    pub effort_decrease: String,
    pub centered_toggle: String,
    pub scroll_prompt_up: String,
    pub scroll_prompt_down: String,
    pub scroll_bookmark: String,
    pub scroll_up_fallback: String,
    pub scroll_down_fallback: String,
    pub workspace_left: String,
    pub workspace_down: String,
    pub workspace_up: String,
    pub workspace_right: String,
    pub side_panel_toggle: String,
    pub copy_selection_toggle: String,
    pub diagram_pane_toggle: String,
    pub typing_scroll_lock_toggle: String,
    pub diff_mode_cycle: String,
    pub info_widget_toggle: String,
    pub todo_card_toggle: String,
    pub swarm_panel_focus: String,
    pub new_terminal: String,
    pub open_resume: String,
    pub session_picker_enter: SessionPickerResumeAction,
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        // Pull platform-appropriate defaults from the single source of truth in
        // `keybindings.rs`. This is where the macOS vs Windows/Linux split takes
        // effect: each field resolves to its own platform's default binding.
        let p = KeybindingPlatform::current();
        let get = |id: &str, fallback: &'static str| {
            default_binding(id, p).unwrap_or(fallback).to_string()
        };
        Self {
            scroll_up: get("scroll_up", "ctrl+k"),
            scroll_down: get("scroll_down", "ctrl+j"),
            scroll_page_up: get("scroll_page_up", "alt+u"),
            scroll_page_down: get("scroll_page_down", "alt+d"),
            model_switch_next: get("model_switch_next", "ctrl+tab"),
            model_switch_prev: get("model_switch_prev", "ctrl+shift+tab"),
            fallback_switch: get("fallback_switch", "ctrl+y"),
            effort_increase: get("effort_increase", "alt+right"),
            effort_decrease: get("effort_decrease", "alt+left"),
            centered_toggle: get("centered_toggle", "alt+c"),
            scroll_prompt_up: get("scroll_prompt_up", "ctrl+["),
            scroll_prompt_down: get("scroll_prompt_down", "ctrl+]"),
            scroll_bookmark: get("scroll_bookmark", "ctrl+g"),
            scroll_up_fallback: get("scroll_up_fallback", ""),
            scroll_down_fallback: get("scroll_down_fallback", ""),
            workspace_left: get("workspace_left", "alt+h"),
            workspace_down: get("workspace_down", "alt+j"),
            workspace_up: get("workspace_up", "alt+k"),
            workspace_right: get("workspace_right", "alt+l"),
            side_panel_toggle: get("side_panel_toggle", "alt+m"),
            copy_selection_toggle: get("copy_selection_toggle", "alt+y"),
            diagram_pane_toggle: get("diagram_pane_toggle", "alt+t"),
            typing_scroll_lock_toggle: get("typing_scroll_lock_toggle", "alt+s"),
            diff_mode_cycle: get("diff_mode_cycle", "alt+g"),
            info_widget_toggle: get("info_widget_toggle", "alt+i"),
            todo_card_toggle: get("todo_card_toggle", "alt+x"),
            swarm_panel_focus: get("swarm_panel_focus", "alt+n"),
            new_terminal: get("new_terminal", ""),
            open_resume: get(
                "open_resume",
                if cfg!(target_os = "macos") {
                    "cmd+b"
                } else {
                    "alt+r"
                },
            ),
            session_picker_enter: SessionPickerResumeAction::CurrentTerminal,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeScrollbarConfig {
    pub chat: bool,
    pub side_panel: bool,
}

impl Default for NativeScrollbarConfig {
    fn default() -> Self {
        Self {
            chat: true,
            side_panel: true,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub diff_mode: DiffDisplayMode,
    #[serde(default)]
    show_diffs: Option<bool>,
    pub queue_mode: bool,
    pub auto_server_reload: bool,
    pub mouse_capture: bool,
    pub debug_socket: bool,
    pub centered: bool,
    pub show_thinking: bool,
    #[serde(default)]
    reasoning_display: Option<ReasoningDisplayMode>,
    pub diagram_mode: DiagramDisplayMode,
    pub markdown_spacing: MarkdownSpacingMode,
    pub latex_rendering: LatexRenderingMode,
    pub pin_images: bool,
    pub idle_animation: bool,
    pub prompt_entry_animation: bool,
    pub disabled_animations: Vec<String>,
    pub diff_line_wrap: bool,
    pub performance: String,
    pub animation_fps: u32,
    pub redraw_fps: u32,
    pub prompt_preview: bool,
    pub compact_notifications: bool,
    pub copy_badge_alt_label: String,
    #[serde(default)]
    pub show_agentgrep_output: bool,
    #[serde(default)]
    pub tool_call_details: bool,
    pub native_scrollbars: NativeScrollbarConfig,
    #[serde(default = "default_true")]
    pub keybinding_hints: bool,
    #[serde(default)]
    pub theme: String,
    #[serde(default)]
    pub active_sessions_manager: bool,
    #[serde(default)]
    pub overscroll_status: OverscrollStatusMode,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            diff_mode: DiffDisplayMode::default(),
            show_diffs: None,
            pin_images: true,
            queue_mode: false,
            auto_server_reload: true,
            mouse_capture: true,
            debug_socket: false,
            centered: false,
            show_thinking: true,
            reasoning_display: Some(ReasoningDisplayMode::Current),
            diagram_mode: DiagramDisplayMode::default(),
            markdown_spacing: MarkdownSpacingMode::default(),
            latex_rendering: LatexRenderingMode::default(),
            idle_animation: true,
            prompt_entry_animation: true,
            disabled_animations: Vec::new(),
            diff_line_wrap: true,
            performance: String::new(),
            animation_fps: 60,
            redraw_fps: 60,
            prompt_preview: true,
            compact_notifications: false,
            copy_badge_alt_label: String::new(),
            show_agentgrep_output: false,
            tool_call_details: false,
            native_scrollbars: NativeScrollbarConfig::default(),
            keybinding_hints: true,
            theme: String::new(),
            active_sessions_manager: false,
            overscroll_status: OverscrollStatusMode::default(),
        }
    }
}

impl DisplayConfig {
    pub fn apply_legacy_compat(&mut self) {
        if let Some(show) = self.show_diffs.take() {
            self.diff_mode = if show {
                DiffDisplayMode::Inline
            } else {
                DiffDisplayMode::Off
            };
        }
    }

    pub fn reasoning_display(&self) -> ReasoningDisplayMode {
        self.reasoning_display.unwrap_or(if self.show_thinking {
            ReasoningDisplayMode::Full
        } else {
            ReasoningDisplayMode::Off
        })
    }

    pub fn set_reasoning_display(&mut self, mode: ReasoningDisplayMode) {
        self.reasoning_display = Some(mode);
        self.show_thinking = !matches!(mode, ReasoningDisplayMode::Off);
    }

    pub fn reasoning_enabled(&self) -> bool {
        !matches!(self.reasoning_display(), ReasoningDisplayMode::Off)
    }
}

mod settings;

pub use settings::*;
