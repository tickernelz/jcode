use super::*;

#[expect(
    clippy::upper_case_acronyms,
    reason = "transport names mirror user-facing configuration values like https and websocket"
)]
#[derive(Clone, Copy)]
pub(super) enum OpenAITransportMode {
    Auto,
    WebSocket,
    HTTPS,
}

impl OpenAITransportMode {
    pub(super) fn from_config(raw: Option<&str>) -> Self {
        let Some(raw) = raw else {
            return Self::Auto;
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Self::Auto,
            "websocket" | "ws" | "wss" => Self::WebSocket,
            "https" | "http" | "sse" => Self::HTTPS,
            other => {
                jcode_base::logging::warn(&format!(
                    "Unknown JCODE_OPENAI_TRANSPORT '{}'; using auto. Use: auto, websocket, or https.",
                    other
                ));
                Self::Auto
            }
        }
    }

    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::WebSocket => "websocket",
            Self::HTTPS => "https",
        }
    }
}

#[derive(Debug)]
pub(super) enum OpenAIStreamFailure {
    FallbackToHttps(anyhow::Error),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for OpenAIStreamFailure {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err)
    }
}

#[expect(
    clippy::upper_case_acronyms,
    reason = "transport names mirror user-facing configuration values like https and websocket"
)]
#[derive(Clone, Copy)]
pub(super) enum OpenAITransport {
    WebSocket,
    HTTPS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenAINativeCompactionMode {
    Auto,
    Explicit,
    Off,
}

/// Shared dual-auth credential pin (see `jcode_provider_core::CredentialMode`).
/// The OpenAI-specific alias is kept so existing call sites read naturally.
pub(crate) use jcode_provider_core::CredentialMode as OpenAICredentialMode;

/// Load Codex credentials for the given credential pin.
pub(crate) fn load_credentials_for_mode(mode: OpenAICredentialMode) -> Result<CodexCredentials> {
    match mode {
        OpenAICredentialMode::Auto => jcode_base::auth::codex::load_credentials(),
        OpenAICredentialMode::OAuth => jcode_base::auth::codex::load_oauth_credentials(),
        OpenAICredentialMode::ApiKey => jcode_base::auth::codex::load_api_key_credentials(),
    }
}

impl OpenAINativeCompactionMode {
    pub(super) fn from_config(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Self::Auto,
            "explicit" | "manual" => Self::Explicit,
            "off" | "disabled" | "none" => Self::Off,
            other => {
                jcode_base::logging::warn(&format!(
                    "Unknown OpenAI native compaction mode '{}'; using auto. Use: auto, explicit, or off.",
                    other
                ));
                Self::Auto
            }
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Explicit => "explicit",
            Self::Off => "off",
        }
    }
}

impl OpenAITransport {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::WebSocket => "websocket",
            Self::HTTPS => "https",
        }
    }
}

pub(super) fn log_openai_stream_lifecycle(
    level: jcode_base::logging::LogLevel,
    phase: &str,
    fields: Vec<(&str, String)>,
) {
    let mut owned = vec![
        ("phase".to_string(), phase.to_string()),
        ("provider".to_string(), "openai".to_string()),
    ];
    owned.extend(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value)),
    );
    jcode_base::logging::event(level, "PROVIDER_STREAM_LIFECYCLE", owned);
}

pub(super) fn openai_request_model(request: &Value) -> String {
    request
        .get("model")
        .and_then(|model| model.as_str())
        .unwrap_or("unknown")
        .to_string()
}
