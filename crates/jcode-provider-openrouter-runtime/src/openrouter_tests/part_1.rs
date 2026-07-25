use super::*;
use bytes::Bytes;
use futures::StreamExt;
use jcode_provider_openrouter::stream::OpenRouterStream;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;
use tempfile::TempDir;

struct SharedEnvLock;

static ENV_LOCK: SharedEnvLock = SharedEnvLock;

impl SharedEnvLock {
    /// Acquire the process-global test env lock.
    ///
    /// This recovers from a poisoned mutex (`into_inner`) instead of
    /// propagating the `PoisonError`. The env guard only protects shared
    /// process env state, so a panic in one test must not cascade into a
    /// flood of unrelated `PoisonError` failures across every other test
    /// that takes this lock.
    fn lock(&self) -> std::sync::MutexGuard<'static, ()> {
        jcode_base::storage::lock_test_env()
    }
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        jcode_base::env::set_var(key, value);
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        jcode_base::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            jcode_base::env::set_var(self.key, previous);
        } else {
            jcode_base::env::remove_var(self.key);
        }
    }
}

fn test_config_dir(temp: &TempDir) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        temp.path().join("Library").join("Application Support")
    }
    #[cfg(target_os = "windows")]
    {
        temp.path().join("AppData").join("Roaming")
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        temp.path().to_path_buf()
    }
}

fn write_test_api_key(temp: &TempDir, env_file: &str, env_key: &str, value: &str) {
    let config_dir = test_config_dir(temp).join("jcode");
    std::fs::create_dir_all(&config_dir).expect("create test config dir");
    std::fs::write(config_dir.join(env_file), format!("{env_key}={value}\n"))
        .expect("write test api key");
}

fn isolate_openrouter_autodetect_env() -> Vec<EnvVarGuard> {
    let mut guards = vec![
        EnvVarGuard::remove("JCODE_OPENROUTER_API_BASE"),
        EnvVarGuard::remove("JCODE_OPENROUTER_API_KEY_NAME"),
        EnvVarGuard::remove("JCODE_OPENROUTER_ENV_FILE"),
        EnvVarGuard::remove("JCODE_OPENROUTER_DYNAMIC_BEARER_PROVIDER"),
        EnvVarGuard::remove("JCODE_OPENROUTER_MODEL"),
        EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE"),
        EnvVarGuard::remove("JCODE_OPENROUTER_ALLOW_NO_AUTH"),
        EnvVarGuard::remove("JCODE_OPENROUTER_TRANSPORT_STATE"),
        EnvVarGuard::remove("JCODE_OPENROUTER_PROVIDER_FEATURES"),
        EnvVarGuard::remove("JCODE_OPENROUTER_MODEL_CATALOG"),
        EnvVarGuard::remove("JCODE_OPENROUTER_AUTH_HEADER"),
        EnvVarGuard::remove("JCODE_OPENROUTER_AUTH_HEADER_NAME"),
        EnvVarGuard::remove("JCODE_OPENROUTER_STATIC_MODELS"),
        EnvVarGuard::remove("JCODE_ACTIVE_PROVIDER"),
        EnvVarGuard::remove("JCODE_RUNTIME_PROVIDER"),
        EnvVarGuard::remove("JCODE_NAMED_PROVIDER_PROFILE"),
        EnvVarGuard::remove("JCODE_PROVIDER_PROFILE_NAME"),
        EnvVarGuard::remove("JCODE_PROVIDER_PROFILE_ACTIVE"),
        EnvVarGuard::remove("JCODE_OPENAI_COMPAT_API_BASE"),
        EnvVarGuard::remove("JCODE_OPENAI_COMPAT_API_KEY_NAME"),
        EnvVarGuard::remove("JCODE_OPENAI_COMPAT_ENV_FILE"),
        EnvVarGuard::remove("JCODE_OPENAI_COMPAT_SETUP_URL"),
        EnvVarGuard::remove("JCODE_OPENAI_COMPAT_DEFAULT_MODEL"),
        EnvVarGuard::remove("JCODE_OPENAI_COMPAT_LOCAL_ENABLED"),
    ];
    guards.extend(
        jcode_base::provider_catalog::openai_compatible_profiles()
            .iter()
            .map(|profile| EnvVarGuard::remove(profile.api_key_env)),
    );
    guards
}

#[test]
fn test_has_credentials() {
    let _has_creds = OpenRouterProvider::has_credentials();
}

#[test]
fn openai_compatible_models_endpoint_allows_minimal_model_objects() {
    let parsed = parse_openai_compatible_models_response(
        r#"{
            "object": "list",
            "data": [
                {"id": "glm-51-nvfp4", "object": "model", "created": null, "owned_by": null},
                {"id": "gte-qwen2-7b", "object": "model"}
            ]
        }"#,
    )
    .expect("minimal OpenAI-compatible /models response should parse");

    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].id, "glm-51-nvfp4");
    assert_eq!(parsed[0].name, "");
}

#[test]
fn openai_compatible_models_endpoint_allows_chutes_numeric_pricing() {
    let parsed = parse_openai_compatible_models_response(
        r#"{
            "object": "list",
            "data": [{
                "id": "Qwen/Qwen3-32B-TEE",
                "root": "Qwen/Qwen3-32B-FP8",
                "price": {
                    "input": {"tao": 0.0002439746644509701, "usd": 0.08},
                    "output": {"tao": 0.0007319239933529102, "usd": 0.24}
                },
                "object": "model",
                "parent": null,
                "created": 1778439139,
                "pricing": {
                    "prompt": 0.08,
                    "completion": 0.24,
                    "input_cache_read": 0.04
                },
                "owned_by": "sglang",
                "context_length": 40960,
                "supported_features": ["json_mode", "tools"]
            }]
        }"#,
    )
    .expect("Chutes /models response with numeric pricing should parse");

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, "Qwen/Qwen3-32B-TEE");
    assert_eq!(parsed[0].pricing.prompt.as_deref(), Some("0.08"));
    assert_eq!(parsed[0].pricing.completion.as_deref(), Some("0.24"));
    assert_eq!(parsed[0].pricing.input_cache_read.as_deref(), Some("0.04"));
}

#[test]
fn openai_compatible_models_endpoint_allows_together_top_level_array() {
    let parsed = parse_openai_compatible_models_response(
        r#"[
            {
                "id": "Austism/chronos-hermes-13b",
                "object": "model",
                "created": 1692896905,
                "type": "chat",
                "display_name": "Chronos Hermes (13B)",
                "context_length": 2048,
                "pricing": {
                    "input": 0.3,
                    "output": 0.3,
                    "cached_input": 0.2
                }
            }
        ]"#,
    )
    .expect("Together /models top-level array should parse");

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, "Austism/chronos-hermes-13b");
    assert_eq!(parsed[0].name, "Chronos Hermes (13B)");
    assert_eq!(parsed[0].context_length, Some(2048));
    assert_eq!(parsed[0].pricing.prompt.as_deref(), Some("0.3"));
    assert_eq!(parsed[0].pricing.completion.as_deref(), Some("0.3"));
    assert_eq!(parsed[0].pricing.input_cache_read.as_deref(), Some("0.2"));
}

#[test]
fn openai_compatible_models_endpoint_allows_models_array_with_name_ids() {
    let parsed = parse_openai_compatible_models_response(
        r#"{
            "models": [{
                "name": "accounts/fireworks/models/example",
                "displayName": "Example Fireworks Model",
                "contextLength": 8192
            }]
        }"#,
    )
    .expect("models array with name-based identifiers should parse");

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, "accounts/fireworks/models/example");
    assert_eq!(parsed[0].name, "accounts/fireworks/models/example");
    assert_eq!(parsed[0].context_length, Some(8192));
}

#[test]
fn openai_compatible_models_endpoint_reads_llamacpp_meta_n_ctx() {
    // llama.cpp's /v1/models only exposes the context window inside `meta`
    // (issue #447). The `data` entry mirrors llama.cpp's response shape.
    let parsed = parse_openai_compatible_models_response(
        r#"{
            "object": "list",
            "data": [{
                "id": "unsloth/gemma-4-31B-it-UD-Q8_K_XL",
                "object": "model",
                "created": 1783253170,
                "owned_by": "llamacpp",
                "meta": {
                    "vocab_type": 2,
                    "n_vocab": 262144,
                    "n_ctx": 262144,
                    "n_ctx_train": 262144,
                    "n_embd": 5376
                }
            }]
        }"#,
    )
    .expect("llama.cpp /v1/models response should parse");

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, "unsloth/gemma-4-31B-it-UD-Q8_K_XL");
    assert_eq!(parsed[0].context_length, Some(262144));
}

#[test]
fn named_openai_compatible_provider_sets_catalog_cache_namespace() {
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let _key = EnvVarGuard::set("TEST_NAMED_COMPAT_KEY", "test-key");

    let profile = jcode_base::config::NamedProviderConfig {
        base_url: "https://llm.example.com/v1".to_string(),
        api_key_env: Some("TEST_NAMED_COMPAT_KEY".to_string()),
        model_catalog: true,
        default_model: Some("example-model".to_string()),
        ..Default::default()
    };

    let _provider = OpenRouterProvider::new_named_openai_compatible("example-compat", &profile)
        .expect("named profile should initialize");

    assert_eq!(
        std::env::var("JCODE_OPENROUTER_CACHE_NAMESPACE").as_deref(),
        Ok("example-compat")
    );
}

#[test]
fn named_openai_compatible_provider_exposes_static_models_as_routes() {
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let _key = EnvVarGuard::set("TEST_NAMED_COMPAT_KEY", "test-key");

    let profile = jcode_base::config::NamedProviderConfig {
        base_url: "https://llm.example.com/v1".to_string(),
        api_key_env: Some("TEST_NAMED_COMPAT_KEY".to_string()),
        model_catalog: true,
        default_model: Some("glm-51-nvfp4".to_string()),
        models: vec![jcode_base::config::NamedProviderModelConfig {
            id: "glm-51-nvfp4".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };

    let provider = OpenRouterProvider::new_named_openai_compatible("comtegra-test", &profile)
        .expect("named profile should initialize");
    let routes = provider.model_routes();

    assert!(routes.iter().any(|route| {
        route.model == "glm-51-nvfp4"
            && route.api_method == "openai-compatible:comtegra-test"
            && route.available
    }));
}

#[test]
fn direct_openai_compatible_provider_advertises_image_input_support() {
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");

    let profile = jcode_base::config::NamedProviderConfig {
        base_url: "http://localhost:1234/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        default_model: Some("local-vision-model".to_string()),
        ..Default::default()
    };

    let provider = OpenRouterProvider::new_named_openai_compatible("local-compat", &profile)
        .expect("local named profile should initialize without auth");

    assert!(provider.supports_image_input());
}

#[test]
fn named_openai_compatible_provider_uses_per_model_image_input_support() {
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");

    let profile = jcode_base::config::NamedProviderConfig {
        base_url: "http://localhost:1234/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        default_model: Some("vision-model".to_string()),
        models: vec![
            jcode_base::config::NamedProviderModelConfig {
                id: "vision-model".to_string(),
                input: vec!["text".to_string(), "image".to_string()],
                ..Default::default()
            },
            jcode_base::config::NamedProviderModelConfig {
                id: "text-model".to_string(),
                input: vec!["text".to_string()],
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let provider = OpenRouterProvider::new_named_openai_compatible("local-compat", &profile)
        .expect("local named profile should initialize without auth");

    assert!(provider.supports_image_input());
    provider.set_model("text-model").expect("switch model");
    assert!(!provider.supports_image_input());
    provider
        .set_model("local-compat:vision-model")
        .expect("switch using qualified model");
    assert!(provider.supports_image_input());
}

#[test]
fn direct_deepseek_profile_does_not_advertise_image_input_support() {
    let provider = OpenRouterProvider {
        profile_id: Some("deepseek".to_string()),
        supports_provider_features: false,
        ..make_custom_compatible_provider()
    };

    assert!(!provider.supports_image_input());
}

#[test]
fn direct_deepseek_profile_omits_image_url_parts() {
    let _lock = ENV_LOCK.lock();
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        profile_id: Some("deepseek".to_string()),
        supports_provider_features: false,
        supports_model_catalog: false,
        ..make_custom_compatible_provider()
    };
    let messages = vec![Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: "describe this".to_string(),
                cache_control: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "aW1hZ2U=".to_string(),
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    }];

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut stream = provider
            .complete(&messages, &[], "", None)
            .await
            .expect("fake chat request should start");
        while let Some(event) = stream.next().await {
            if event.is_err() {
                break;
            }
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    assert!(
        !request.contains(r#""type":"image_url""#),
        "DeepSeek request must not contain unsupported image_url content parts: {request}"
    );
    assert!(
        request.contains("Image omitted"),
        "DeepSeek request should preserve a textual placeholder for omitted images: {request}"
    );
}

/// Extract the JSON request body from a captured raw HTTP request.
fn parse_captured_request_body(request: &str) -> serde_json::Value {
    let body = request
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(request);
    serde_json::from_str(body)
        .unwrap_or_else(|err| panic!("captured request body should be JSON ({err}): {body}"))
}

/// Regression for issue #321: when an assistant turn is interrupted mid-thinking
/// on a direct OpenAI-compatible provider that does not support reasoning replay
/// (e.g. DeepSeek), the persisted assistant message contains only a `Reasoning`
/// block. The request builder must not emit an assistant message that has
/// neither `content` nor `tool_calls`, otherwise the provider rejects the whole
/// request with 400 "Invalid assistant message: content or tool_calls must be
/// set" and the session can never recover.
#[test]
fn interrupted_reasoning_only_assistant_message_is_not_sent_empty() {
    let _lock = ENV_LOCK.lock();
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        profile_id: Some("deepseek".to_string()),
        supports_provider_features: false,
        supports_model_catalog: false,
        ..make_custom_compatible_provider()
    };

    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "do a thing".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        // Assistant turn that was interrupted while only reasoning had streamed,
        // so it carries a Reasoning block but no text or tool calls.
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Reasoning {
                text: "thinking about the request".to_string(),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "actually do this instead".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut stream = provider
            .complete(&messages, &[], "", None)
            .await
            .expect("fake chat request should start");
        while let Some(event) = stream.next().await {
            if event.is_err() {
                break;
            }
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    let body = parse_captured_request_body(&request);
    let api_messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .expect("request should contain messages array");

    for msg in api_messages {
        if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let has_content = msg
            .get("content")
            .map(|v| !v.is_null() && v.as_str().map(|s| !s.is_empty()).unwrap_or(true))
            .unwrap_or(false);
        let has_tool_calls = msg
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .map(|calls| !calls.is_empty())
            .unwrap_or(false);
        assert!(
            has_content || has_tool_calls,
            "assistant message must carry content or tool_calls (issue #321); got: {msg}"
        );
    }
}

/// Companion to issue #321: when the provider *does* support reasoning replay
/// (e.g. a generic OpenRouter-style endpoint with provider features enabled and
/// thinking on), an interrupted reasoning-only assistant turn should be sent
/// with both a `reasoning_content` field and a valid (empty) `content`, so the
/// turn is preserved without violating the "content or tool_calls" requirement.
#[test]
fn interrupted_reasoning_only_assistant_message_keeps_reasoning_with_content() {
    let _lock = ENV_LOCK.lock();
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        profile_id: None,
        supports_provider_features: true,
        supports_model_catalog: false,
        ..make_custom_compatible_provider()
    };

    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "do a thing".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Reasoning {
                text: "thinking about the request".to_string(),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "actually do this instead".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut stream = provider
            .complete(&messages, &[], "", None)
            .await
            .expect("fake chat request should start");
        while let Some(event) = stream.next().await {
            if event.is_err() {
                break;
            }
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    let body = parse_captured_request_body(&request);
    let api_messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .expect("request should contain messages array");

    let assistant = api_messages
        .iter()
        .find(|msg| msg.get("role").and_then(|v| v.as_str()) == Some("assistant"))
        .expect("request should retain the interrupted assistant turn");

    assert!(
        assistant.get("reasoning_content").is_some(),
        "reasoning-capable provider should keep reasoning_content; got: {assistant}"
    );
    assert!(
        assistant.get("content").is_some(),
        "interrupted reasoning-only assistant turn must still carry content (issue #321); got: {assistant}"
    );
}

/// Regression for issue #322: the dedicated Kimi coding endpoint
/// (`https://api.kimi.com/coding/v1`, model `kimi-for-coding`) enables thinking
/// server-side and rejects any assistant tool-call message that lacks
/// `reasoning_content` with 400 "thinking is enabled but reasoning_content is
/// missing in assistant tool call message". When an assistant turn produced a
/// tool call without an accompanying reasoning block (the common case once the
/// thinking stream is not persisted), the request builder must still attach a
/// `reasoning_content` field to that assistant message so the endpoint accepts
/// the request.
#[test]
fn kimi_for_coding_tool_call_message_includes_reasoning_content() {
    let _lock = ENV_LOCK.lock();
    let _thinking = EnvVarGuard::remove("JCODE_OPENROUTER_THINKING");
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        // The dedicated Kimi coding endpoint is a direct OpenAI-compatible
        // profile (no OpenRouter provider routing features).
        profile_id: Some("kimi".to_string()),
        supports_provider_features: false,
        supports_model_catalog: false,
        model: Arc::new(RwLock::new("kimi-for-coding".to_string())),
        ..make_custom_compatible_provider()
    };

    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "list the files".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        // Assistant turn that emitted a tool call but whose hidden reasoning was
        // not persisted (so there is no Reasoning block to replay).
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "a.txt\nb.txt".to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut stream = provider
            .complete(&messages, &[], "", None)
            .await
            .expect("fake chat request should start");
        while let Some(event) = stream.next().await {
            if event.is_err() {
                break;
            }
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    let body = parse_captured_request_body(&request);
    let api_messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .expect("request should contain messages array");

    let assistant = api_messages
        .iter()
        .find(|msg| {
            msg.get("role").and_then(|v| v.as_str()) == Some("assistant")
                && msg.get("tool_calls").is_some()
        })
        .expect("request should retain the assistant tool-call turn");

    let reasoning = assistant.get("reasoning_content");
    assert!(
        reasoning.is_some_and(|value| value.as_str().is_some_and(|s| !s.is_empty())),
        "Kimi coding endpoint requires reasoning_content on assistant tool-call messages (issue #322); got: {assistant}"
    );
}

#[test]
fn minimax_profile_exposes_static_models_before_catalog_refresh() {
    let models = jcode_base::provider_catalog::openai_compatible_profile_static_models(
        jcode_provider_metadata::MINIMAX_PROFILE,
    );
    assert!(models.iter().any(|model| model == "MiniMax-M2.7"));
    assert!(models.iter().any(|model| model == "MiniMax-M2.7-highspeed"));
    assert!(models.iter().any(|model| model == "MiniMax-M2"));
}

#[test]
fn cerebras_profile_exposes_live_chat_models_before_catalog_refresh() {
    assert_eq!(
        jcode_provider_metadata::CEREBRAS_PROFILE.default_model,
        Some("gpt-oss-120b")
    );

    let models = jcode_base::provider_catalog::openai_compatible_profile_static_models(
        jcode_provider_metadata::CEREBRAS_PROFILE,
    );

    assert!(
        !models.iter().any(|model| model == "qwen-3-coder-480b"),
        "old Cerebras default is no longer returned by the live /models catalog"
    );
    assert!(models.iter().any(|model| model == "gpt-oss-120b"));
    assert!(models.iter().any(|model| model == "zai-glm-4.7"));
    assert!(
        !models
            .iter()
            .any(|model| model == "qwen-3-235b-a22b-instruct-2507")
    );
    assert!(!models.iter().any(|model| model == "llama3.1-8b"));
}

#[test]
fn openai_compatible_profiles_with_unverified_live_catalogs_have_static_fallbacks() {
    let cases = [
        (jcode_provider_metadata::OPENCODE_PROFILE, "minimax-m2.7"),
        (jcode_provider_metadata::OPENCODE_GO_PROFILE, "kimi-k2.5"),
        (jcode_provider_metadata::ZAI_PROFILE, "glm-4.7"),
        (
            jcode_provider_metadata::AI302_PROFILE,
            "qwen3-235b-a22b-instruct-2507",
        ),
        (jcode_provider_metadata::BASETEN_PROFILE, "zai-org/GLM-4.7"),
        (jcode_provider_metadata::CORTECS_PROFILE, "kimi-k2.5"),
        (jcode_provider_metadata::KIMI_PROFILE, "kimi-for-coding"),
        (jcode_provider_metadata::FIRMWARE_PROFILE, "kimi-k2.5"),
        (
            jcode_provider_metadata::HUGGING_FACE_PROFILE,
            "Qwen/Qwen3-Coder-480B-A35B-Instruct",
        ),
        (jcode_provider_metadata::MOONSHOT_PROFILE, "kimi-k2.5"),
        (
            jcode_provider_metadata::NEBIUS_PROFILE,
            "openai/gpt-oss-120b",
        ),
        (
            jcode_provider_metadata::SCALEWAY_PROFILE,
            "qwen3-coder-30b-a3b-instruct",
        ),
        (
            jcode_provider_metadata::STACKIT_PROFILE,
            "openai/gpt-oss-120b",
        ),
        (jcode_provider_metadata::PERPLEXITY_PROFILE, "sonar"),
        (
            jcode_provider_metadata::DEEPINFRA_PROFILE,
            "moonshotai/Kimi-K2-Instruct",
        ),
        (
            jcode_provider_metadata::FIREWORKS_PROFILE,
            "accounts/fireworks/routers/kimi-k2p5-turbo",
        ),
        (jcode_provider_metadata::XIAOMI_MIMO_PROFILE, "mimo-v2.5"),
        (
            jcode_provider_metadata::ALIBABA_CODING_PLAN_PROFILE,
            "qwen3-coder-plus",
        ),
    ];

    for (profile, expected_model) in cases {
        let models = jcode_base::provider_catalog::openai_compatible_profile_static_models(profile);
        assert!(
            models.iter().any(|model| model == expected_model),
            "{} should expose static fallback model {expected_model}; got {models:?}",
            profile.id
        );
    }
}

#[test]
fn comtegra_profile_uses_endpoint_default_max_tokens() {
    let _lock = ENV_LOCK.lock();
    let _override = EnvVarGuard::remove("JCODE_OPENROUTER_MAX_TOKENS");

    assert_eq!(
        OpenRouterProvider::configured_max_tokens(Some("comtegra")),
        None
    );
    assert_eq!(
        OpenRouterProvider::configured_max_tokens(Some("deepseek")),
        None
    );
}

#[test]
fn max_tokens_env_overrides_profile_default() {
    let _lock = ENV_LOCK.lock();
    let _override = EnvVarGuard::set("JCODE_OPENROUTER_MAX_TOKENS", "4096");

    assert_eq!(
        OpenRouterProvider::configured_max_tokens(Some("comtegra")),
        Some(4096)
    );
}

#[test]
fn test_configured_api_base_accepts_https() {
    let _lock = ENV_LOCK.lock();
    let prev = std::env::var("JCODE_OPENROUTER_API_BASE").ok();
    jcode_base::env::set_var(
        "JCODE_OPENROUTER_API_BASE",
        "https://api.groq.com/openai/v1/",
    );
    assert_eq!(configured_api_base(), "https://api.groq.com/openai/v1");
    if let Some(value) = prev {
        jcode_base::env::set_var("JCODE_OPENROUTER_API_BASE", value);
    } else {
        jcode_base::env::remove_var("JCODE_OPENROUTER_API_BASE");
    }
}

#[test]
fn test_configured_api_base_rejects_insecure_http_remote() {
    let _lock = ENV_LOCK.lock();
    let prev = std::env::var("JCODE_OPENROUTER_API_BASE").ok();
    jcode_base::env::set_var("JCODE_OPENROUTER_API_BASE", "http://example.com/v1");
    assert_eq!(configured_api_base(), DEFAULT_API_BASE);
    if let Some(value) = prev {
        jcode_base::env::set_var("JCODE_OPENROUTER_API_BASE", value);
    } else {
        jcode_base::env::remove_var("JCODE_OPENROUTER_API_BASE");
    }
}

#[test]
fn autodetects_single_saved_openai_compatible_profile() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp dir");
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", temp.path());
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();

    let opencode = jcode_base::provider_catalog::resolve_openai_compatible_profile(
        jcode_base::provider_catalog::OPENCODE_PROFILE,
    );
    write_test_api_key(
        &temp,
        &opencode.env_file,
        &opencode.api_key_env,
        "test-opencode-key",
    );

    assert_eq!(configured_api_base(), opencode.api_base);
    assert_eq!(configured_api_key_name(), opencode.api_key_env);
    assert_eq!(configured_env_file_name(), opencode.env_file);
    assert!(OpenRouterProvider::has_credentials());
}

#[test]
fn autodetects_single_saved_local_openai_compatible_profile() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp dir");
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", temp.path());
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();

    let lmstudio = jcode_base::provider_catalog::resolve_openai_compatible_profile(
        jcode_base::provider_catalog::LMSTUDIO_PROFILE,
    );
    let config_dir = test_config_dir(&temp).join("jcode");
    std::fs::create_dir_all(&config_dir).expect("create test config dir");
    std::fs::write(
        config_dir.join(&lmstudio.env_file),
        format!(
            "{}=1\n",
            jcode_base::provider_catalog::OPENAI_COMPAT_LOCAL_ENABLED_ENV
        ),
    )
    .expect("write local config");

    assert_eq!(configured_api_base(), lmstudio.api_base);
    assert_eq!(configured_api_key_name(), lmstudio.api_key_env);
    assert_eq!(configured_env_file_name(), lmstudio.env_file);
    assert!(configured_allow_no_auth());
    assert!(OpenRouterProvider::has_credentials());
}

#[test]
fn openrouter_transport_state_distinguishes_runtime_identities() {
    let _lock = ENV_LOCK.lock();
    // Isolate the on-disk config/credential lookup the same way the sibling
    // autodetect tests do, so this test does not read whatever provider
    // profile happens to be configured on the host machine.
    let temp = TempDir::new().expect("create temp dir");
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", temp.path());
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();

    assert_eq!(
        OpenRouterTransportState::from_current_env(None),
        OpenRouterTransportState::OpenRouterApiKey
    );
    assert!(OpenRouterTransportState::from_current_env(None).accrues_user_api_key_cost());
    assert!(OpenRouterTransportState::from_current_env(None).is_real_openrouter());

    jcode_base::env::set_var("JCODE_OPENROUTER_TRANSPORT_STATE", "direct-api-key");
    assert_eq!(
        OpenRouterTransportState::from_current_env(None),
        OpenRouterTransportState::DirectApiKey
    );
    jcode_base::env::remove_var("JCODE_OPENROUTER_TRANSPORT_STATE");

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "openrouter");
    assert_eq!(
        OpenRouterTransportState::from_current_env(Some("openrouter")),
        OpenRouterTransportState::OpenRouterApiKey
    );
    assert!(OpenRouterTransportState::from_current_env(Some("openrouter")).is_real_openrouter());
    jcode_base::env::remove_var("JCODE_RUNTIME_PROVIDER");

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "jcode");
    assert_eq!(
        OpenRouterTransportState::from_current_env(Some("jcode")),
        OpenRouterTransportState::JcodeSubscription
    );
    assert!(!OpenRouterTransportState::from_current_env(Some("jcode")).accrues_user_api_key_cost());

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "openai-compatible");
    assert_eq!(
        OpenRouterTransportState::from_current_env(Some("openai-compatible")),
        OpenRouterTransportState::DirectApiKey
    );

    jcode_base::env::set_var("JCODE_OPENROUTER_ALLOW_NO_AUTH", "1");
    assert_eq!(
        OpenRouterTransportState::from_current_env(Some("openai-compatible")),
        OpenRouterTransportState::DirectNoAuth
    );
    assert!(
        !OpenRouterTransportState::from_current_env(Some("openai-compatible"))
            .accrues_user_api_key_cost()
    );

    jcode_base::env::remove_var("JCODE_OPENROUTER_ALLOW_NO_AUTH");
    jcode_base::env::remove_var("JCODE_RUNTIME_PROVIDER");
    jcode_base::env::set_var("JCODE_NAMED_PROVIDER_PROFILE", "my-gateway");
    assert_eq!(
        OpenRouterTransportState::from_current_env(None),
        OpenRouterTransportState::DirectApiKey
    );
}

#[test]
fn does_not_guess_when_multiple_saved_openai_compatible_profiles_exist() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp dir");
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", temp.path());
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();

    let opencode = jcode_base::provider_catalog::resolve_openai_compatible_profile(
        jcode_base::provider_catalog::OPENCODE_PROFILE,
    );
    let chutes = jcode_base::provider_catalog::resolve_openai_compatible_profile(
        jcode_base::provider_catalog::CHUTES_PROFILE,
    );
    write_test_api_key(
        &temp,
        &opencode.env_file,
        &opencode.api_key_env,
        "test-opencode-key",
    );
    write_test_api_key(
        &temp,
        &chutes.env_file,
        &chutes.api_key_env,
        "test-chutes-key",
    );

    assert_eq!(configured_api_base(), DEFAULT_API_BASE);
    assert_eq!(configured_api_key_name(), DEFAULT_API_KEY_NAME);
    assert_eq!(configured_env_file_name(), DEFAULT_ENV_FILE);
    assert!(!OpenRouterProvider::has_credentials());
}

#[test]
fn autodetected_profile_seeds_default_model_and_cache_namespace() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp dir");
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", temp.path());
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();

    let zai = jcode_base::provider_catalog::resolve_openai_compatible_profile(
        jcode_base::provider_catalog::ZAI_PROFILE,
    );
    write_test_api_key(&temp, &zai.env_file, &zai.api_key_env, "test-zai-key");

    let provider = OpenRouterProvider::new().expect("provider");
    assert_eq!(provider.model.blocking_read().clone(), "glm-4.5");
    assert_eq!(
        std::env::var("JCODE_OPENROUTER_CACHE_NAMESPACE")
            .ok()
            .as_deref(),
        Some("zai")
    );
}

