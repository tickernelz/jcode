#[test]
fn test_parse_model_spec() {
    let (model, provider) = parse_model_spec("anthropic/claude-sonnet-4@Fireworks");
    assert_eq!(model, "anthropic/claude-sonnet-4");
    let provider = provider.expect("provider");
    assert_eq!(provider.name, "Fireworks");
    assert!(provider.allow_fallbacks);

    let (model, provider) = parse_model_spec("anthropic/claude-sonnet-4@Fireworks!");
    assert_eq!(model, "anthropic/claude-sonnet-4");
    let provider = provider.expect("provider");
    assert_eq!(provider.name, "Fireworks");
    assert!(!provider.allow_fallbacks);

    let (model, provider) = parse_model_spec("moonshotai/kimi-k2.5@moonshot");
    assert_eq!(model, "moonshotai/kimi-k2.5");
    let provider = provider.expect("provider");
    assert_eq!(provider.name, "Moonshot AI");

    let (model, provider) = parse_model_spec("anthropic/claude-sonnet-4@auto");
    assert_eq!(model, "anthropic/claude-sonnet-4");
    assert!(provider.is_none());
}

fn make_endpoint(name: &str, throughput: f64, uptime: f64, cache: bool, cost: f64) -> EndpointInfo {
    EndpointInfo {
        provider_name: name.to_string(),
        tag: None,
        pricing: ModelPricing {
            prompt: Some(format!("{:.10}", cost)),
            completion: None,
            input_cache_read: if cache {
                Some("0.00000007".to_string())
            } else {
                None
            },
            input_cache_write: None,
        },
        context_length: None,
        max_completion_tokens: None,
        quantization: None,
        uptime_last_30m: Some(uptime),
        latency_last_30m: None,
        throughput_last_30m: Some(serde_json::json!({"p50": throughput})),
        supports_implicit_caching: Some(cache),
        status: Some(0),
    }
}

fn make_provider() -> OpenRouterProvider {
    OpenRouterProvider {
        client: jcode_provider_core::shared_http_client(),
        model: Arc::new(RwLock::new(DEFAULT_MODEL.to_string())),
        reasoning_effort: Arc::new(RwLock::new(None)),
        api_base: DEFAULT_API_BASE.to_string(),
        auth: ProviderAuth::AuthorizationBearer {
            token: "test".to_string(),
            label: DEFAULT_API_KEY_NAME.to_string(),
        },
        supports_provider_features: true,
        supports_model_catalog: true,
        profile_id: None,
        reasoning_effort_support: None,
        max_tokens: None,
        extra_body: None,
        wire_api: None,
        swarm_reasoning_effort: None,
        service_tier: Arc::new(std::sync::RwLock::new(None)),
        static_models: Vec::new(),
        static_context_limits: HashMap::new(),
        static_image_input_support: HashMap::new(),
        send_openrouter_headers: true,
        models_cache: Arc::new(RwLock::new(ModelsCache::default())),
        model_catalog_refresh: Arc::new(Mutex::new(ModelCatalogRefreshState::default())),
        endpoint_refresh: Arc::new(Mutex::new(EndpointRefreshTracker::default())),
        provider_routing: Arc::new(RwLock::new(ProviderRouting::default())),
        provider_pin: Arc::new(Mutex::new(None)),
        endpoints_cache: Arc::new(RwLock::new(HashMap::new())),
    }
}

fn make_custom_compatible_provider() -> OpenRouterProvider {
    OpenRouterProvider {
        client: jcode_provider_core::shared_http_client(),
        model: Arc::new(RwLock::new(DEFAULT_MODEL.to_string())),
        reasoning_effort: Arc::new(RwLock::new(None)),
        api_base: "https://compat.example.test/v1".to_string(),
        auth: ProviderAuth::AuthorizationBearer {
            token: "test".to_string(),
            label: "OPENAI_COMPAT_API_KEY".to_string(),
        },
        supports_provider_features: false,
        supports_model_catalog: true,
        profile_id: None,
        reasoning_effort_support: None,
        max_tokens: None,
        extra_body: None,
        wire_api: None,
        swarm_reasoning_effort: None,
        service_tier: Arc::new(std::sync::RwLock::new(None)),
        static_models: Vec::new(),
        static_context_limits: HashMap::new(),
        static_image_input_support: HashMap::new(),
        send_openrouter_headers: false,
        models_cache: Arc::new(RwLock::new(ModelsCache::default())),
        model_catalog_refresh: Arc::new(Mutex::new(ModelCatalogRefreshState::default())),
        endpoint_refresh: Arc::new(Mutex::new(EndpointRefreshTracker::default())),
        provider_routing: Arc::new(RwLock::new(ProviderRouting::default())),
        provider_pin: Arc::new(Mutex::new(None)),
        endpoints_cache: Arc::new(RwLock::new(HashMap::new())),
    }
}

fn spawn_single_response_models_server(body: &'static str) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider server");
    let addr = listener.local_addr().expect("fake provider addr");
    let (request_tx, request_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept fake provider request");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let mut request = vec![0u8; 8192];
        let n = stream.read(&mut request).unwrap_or(0);
        let request = String::from_utf8_lossy(&request[..n]).into_owned();
        let _ = request_tx.send(request);

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write fake provider response");
    });

    (format!("http://{addr}/v1"), request_rx)
}

fn spawn_single_response_chat_server() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider server");
    let addr = listener.local_addr().expect("fake provider addr");
    let (request_tx, request_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept fake provider request");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let mut request = vec![0u8; 16384];
        let n = stream.read(&mut request).unwrap_or(0);
        let request = String::from_utf8_lossy(&request[..n]).into_owned();
        let _ = request_tx.send(request);

        let body = "data: [DONE]\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write fake provider response");
    });

    (format!("http://{addr}/v1"), request_rx)
}

#[test]
fn direct_deepseek_profile_exposes_max_reasoning_effort() {
    let provider = OpenRouterProvider {
        profile_id: Some("deepseek".to_string()),
        supports_provider_features: false,
        ..make_custom_compatible_provider()
    };

    assert_eq!(
        provider.available_efforts(),
        vec![
            "none",
            "low",
            "medium",
            "high",
            "max",
            "swarm",
            "swarm-deep"
        ]
    );
    provider
        .set_reasoning_effort("max")
        .expect("DeepSeek direct profile should accept max effort");
    assert_eq!(provider.reasoning_effort().as_deref(), Some("max"));
}

#[test]
fn openrouter_profile_exposes_unified_reasoning_effort() {
    let provider = make_provider();

    assert_eq!(
        provider.available_efforts(),
        vec![
            "none",
            "minimal",
            "low",
            "medium",
            "high",
            "xhigh",
            "swarm",
            "swarm-deep"
        ]
    );
    provider
        .set_reasoning_effort("minimal")
        .expect("OpenRouter minimal effort should be accepted");
    assert_eq!(provider.reasoning_effort().as_deref(), Some("minimal"));
    provider
        .set_reasoning_effort("max")
        .expect("OpenRouter max alias should be accepted");
    assert_eq!(provider.reasoning_effort().as_deref(), Some("xhigh"));
}

#[test]
fn openrouter_with_openrouter_profile_id_exposes_unified_reasoning_effort() {
    // The default OpenRouter api base matches the "openrouter" OpenAI-compat
    // doctor profile, so `new()` can assign profile_id = Some("openrouter").
    // That runtime is still real OpenRouter and must keep unified reasoning
    // (regression: /effort failed with "Reasoning effort is not supported").
    let provider = OpenRouterProvider {
        profile_id: Some("openrouter".to_string()),
        ..make_provider()
    };

    assert_eq!(
        provider.available_efforts(),
        vec![
            "none",
            "minimal",
            "low",
            "medium",
            "high",
            "xhigh",
            "swarm",
            "swarm-deep"
        ]
    );
    provider
        .set_reasoning_effort("high")
        .expect("OpenRouter with doctor profile id should accept effort");
    assert_eq!(provider.reasoning_effort().as_deref(), Some("high"));
}

#[test]
fn non_deepseek_compatible_profile_does_not_expose_reasoning_effort() {
    let provider = make_custom_compatible_provider();

    assert!(provider.available_efforts().is_empty());
    let error = provider
        .set_reasoning_effort("max")
        .expect_err("generic compatible profile should not expose DeepSeek effort UX");
    assert!(
        error.to_string().contains("not supported"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn openrouter_chat_request_sends_unified_reasoning_effort() {
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        model: Arc::new(RwLock::new("anthropic/claude-sonnet-4.6".to_string())),
        supports_model_catalog: false,
        ..make_provider()
    };
    provider
        .set_reasoning_effort("high")
        .expect("OpenRouter unified reasoning should accept high effort");

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
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
            event.expect("stream event should parse");
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    assert!(
        request.contains(r#""reasoning":{"effort":"high"}"#),
        "OpenRouter request should include unified reasoning effort: {request}"
    );
    assert!(
        !request.contains(r#""thinking":{"type":"enabled"}"#),
        "unified reasoning should supersede legacy thinking override: {request}"
    );
}

fn live_openrouter_models() -> Vec<String> {
    std::env::var("JCODE_LIVE_OPENROUTER_MODELS")
        .or_else(|_| std::env::var("JCODE_OPENROUTER_MODEL"))
        .unwrap_or_else(|_| "anthropic/claude-sonnet-4.6".to_string())
        .split([',', '\n'])
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToString::to_string)
        .collect()
}

async fn collect_openrouter_live_smoke_stream(
    mut stream: EventStream,
    timeout: Duration,
) -> Result<(usize, usize, bool)> {
    tokio::time::timeout(timeout, async move {
        let mut text_bytes = 0usize;
        let mut thinking_bytes = 0usize;
        let mut saw_message_end = false;
        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::TextDelta(text) => {
                    text_bytes += text.len();
                }
                StreamEvent::ThinkingDelta(text) => {
                    thinking_bytes += text.len();
                }
                StreamEvent::MessageEnd { .. } => {
                    saw_message_end = true;
                    break;
                }
                StreamEvent::Error { message, .. } => anyhow::bail!(message),
                _ => {}
            }
        }
        Ok((text_bytes, thinking_bytes, saw_message_end))
    })
    .await
    .context("live OpenRouter smoke timed out")?
}

#[tokio::test]
#[ignore = "live smoke: requires OPENROUTER_API_KEY or configured OpenRouter credentials"]
async fn live_openrouter_unified_reasoning_smoke() -> Result<()> {
    let _env_lock = ENV_LOCK.lock();
    let Some(token) = OpenRouterProvider::get_api_key() else {
        eprintln!(
            "skipping live OpenRouter smoke: OPENROUTER_API_KEY or configured OpenRouter credentials not found"
        );
        return Ok(());
    };

    let models = live_openrouter_models();
    let effort = std::env::var("JCODE_LIVE_OPENROUTER_REASONING_EFFORT")
        .unwrap_or_else(|_| "low".to_string());
    let max_tokens = std::env::var("JCODE_LIVE_OPENROUTER_MAX_TOKENS")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(1024);

    for model in models {
        let provider = OpenRouterProvider {
            auth: ProviderAuth::AuthorizationBearer {
                token: token.clone(),
                label: configured_api_key_name(),
            },
            model: Arc::new(RwLock::new(model.clone())),
            max_tokens: Some(max_tokens),
            ..make_provider()
        };
        provider.set_reasoning_effort(&effort)?;

        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Live smoke test: answer exactly OK.".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }];

        let stream = provider
            .complete(
                &messages,
                &[],
                "You are a live provider smoke test. Keep the answer tiny.",
                None,
            )
            .await
            .with_context(|| format!("starting live OpenRouter stream for {model}"))?;
        let (text_bytes, thinking_bytes, saw_message_end) =
            collect_openrouter_live_smoke_stream(stream, Duration::from_secs(90))
                .await
                .with_context(|| format!("collecting live OpenRouter stream for {model}"))?;

        eprintln!(
            "live OpenRouter reasoning smoke passed: model={model}, effort={effort}, text_bytes={text_bytes}, thinking_bytes={thinking_bytes}, message_end={saw_message_end}"
        );
        assert!(
            text_bytes > 0 || thinking_bytes > 0,
            "live OpenRouter response for {model} contained neither text nor thinking deltas"
        );
    }

    Ok(())
}

#[test]
fn direct_deepseek_chat_request_sends_reasoning_effort() {
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        model: Arc::new(RwLock::new("deepseek-v4-pro".to_string())),
        profile_id: Some("deepseek".to_string()),
        supports_provider_features: false,
        supports_model_catalog: false,
        send_openrouter_headers: false,
        ..make_custom_compatible_provider()
    };
    provider
        .set_reasoning_effort("max")
        .expect("DeepSeek direct profile should accept max effort");

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
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
            event.expect("stream event should parse");
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    assert!(
        request.starts_with("POST /v1/chat/completions "),
        "unexpected chat request: {request}"
    );
    assert!(
        request.contains(r#""model":"deepseek-v4-pro""#),
        "request should contain model: {request}"
    );
    assert!(
        request.contains(r#""reasoning_effort":"max""#),
        "DeepSeek request should include max reasoning effort: {request}"
    );
}

#[test]
fn direct_openai_compatible_chat_request_preserves_max_reasoning_effort() {
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        model: Arc::new(RwLock::new("gpt-5.5".to_string())),
        supports_provider_features: false,
        supports_model_catalog: false,
        send_openrouter_headers: false,
        ..make_custom_compatible_provider()
    };
    provider
        .set_reasoning_effort("max")
        .expect("direct OpenAI-compatible profile should accept max effort");

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
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
            event.expect("stream event should parse");
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    assert!(
        request.contains(r#""reasoning_effort":"max""#),
        "direct compatible request must preserve OpenAI max: {request}"
    );
}

#[test]
fn named_provider_responses_api_sends_priority_tier_and_maps_swarm_effort() {
    let _lock = ENV_LOCK.lock();
    let display = EnvVarGuard::set("JCODE_REASONING_DISPLAY", "full");
    jcode_base::config::invalidate_config_cache();

    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        model: Arc::new(RwLock::new("gpt-5.6-sol".to_string())),
        wire_api: Some("responses".to_string()),
        swarm_reasoning_effort: Some("xhigh".to_string()),
        supports_provider_features: false,
        ..make_custom_compatible_provider()
    };
    provider
        .set_service_tier("priority")
        .expect("enable fast mode");
    provider
        .set_reasoning_effort("swarm-deep")
        .expect("enable deep swarm mode");

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut stream = provider
            .complete(&messages, &[], "system", None)
            .await
            .expect("responses request should start");
        while let Some(event) = stream.next().await {
            event.expect("stream event should parse");
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture Responses API request");
    assert!(request.starts_with("POST /v1/responses "), "{request}");
    assert!(request.contains(r#""input":"#), "{request}");
    assert!(
        request.contains(r#""service_tier":"priority""#),
        "{request}"
    );
    assert!(
        request.contains(r#""reasoning":{"effort":"xhigh","summary":"auto"}"#),
        "swarm sentinels must map to a valid maximum wire effort: {request}"
    );
    assert!(
        request.contains(r#""include":["reasoning.encrypted_content"]"#),
        "encrypted reasoning content must remain requested: {request}"
    );
    assert!(!request.contains("swarm-deep"), "{request}");
    assert!(!request.contains(r#""messages":"#), "{request}");

    drop(display);
    jcode_base::config::invalidate_config_cache();
}

#[test]
fn named_provider_responses_extra_body_overrides_generated_reasoning() {
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        model: Arc::new(RwLock::new("gpt-5.6-sol".to_string())),
        wire_api: Some("responses".to_string()),
        extra_body: Some(
            serde_json::json!({"reasoning": {"summary": "detailed"}})
                .as_object()
                .expect("extra body object")
                .clone(),
        ),
        supports_provider_features: false,
        ..make_custom_compatible_provider()
    };
    provider
        .set_reasoning_effort("high")
        .expect("enable reasoning");

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
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
            .expect("responses request should start");
        while let Some(event) = stream.next().await {
            event.expect("stream event should parse");
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture Responses API request");
    assert!(
        request.contains(r#""reasoning":{"summary":"detailed"}"#),
        "extra_body must override the generated reasoning object: {request}"
    );
    assert!(!request.contains(r#""effort":"high""#), "{request}");
}

#[test]
fn named_provider_responses_display_off_preserves_effort_without_summary() {
    let _lock = ENV_LOCK.lock();
    let display = EnvVarGuard::set("JCODE_REASONING_DISPLAY", "off");
    jcode_base::config::invalidate_config_cache();

    let (api_base, request_rx) = spawn_single_response_chat_server();
    let provider = OpenRouterProvider {
        api_base,
        model: Arc::new(RwLock::new("gpt-5.6-sol".to_string())),
        wire_api: Some("responses".to_string()),
        supports_provider_features: false,
        ..make_custom_compatible_provider()
    };
    provider
        .set_reasoning_effort("high")
        .expect("enable reasoning");

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "hello".to_string(),
            cache_control: None,
        }],
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
            .expect("responses request should start");
        while let Some(event) = stream.next().await {
            event.expect("stream event should parse");
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture Responses API request");
    assert!(
        request.contains(r#""reasoning":{"effort":"high"}"#),
        "display off must preserve effort without requesting summary: {request}"
    );
    assert!(!request.contains(r#""summary":"#), "{request}");

    drop(display);
    jcode_base::config::invalidate_config_cache();
}

#[test]
fn openai_compatible_model_catalog_refresh_calls_models_endpoint_and_updates_display() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp home");
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _namespace = EnvVarGuard::set(
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "test-openai-compatible-flow",
    );
    let (api_base, request_rx) = spawn_single_response_models_server(
        r#"{
            "object": "list",
            "data": [
                {"id": "live-login-flow-model", "object": "model", "context_length": 131072}
            ]
        }"#,
    );
    let provider = OpenRouterProvider {
        api_base,
        model: Arc::new(RwLock::new("live-login-flow-model".to_string())),
        auth: ProviderAuth::AuthorizationBearer {
            token: "sk-live-catalog".to_string(),
            label: "OPENAI_COMPAT_API_KEY".to_string(),
        },
        supports_provider_features: false,
        supports_model_catalog: true,
        profile_id: None,
        reasoning_effort_support: None,
        static_models: vec!["static-login-flow-fallback".to_string()],
        send_openrouter_headers: false,
        ..make_custom_compatible_provider()
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let fetched = rt
        .block_on(provider.refresh_models())
        .expect("refresh fake model catalog");
    assert_eq!(fetched[0].id, "live-login-flow-model");
    assert_eq!(provider.context_window(), 131_072);

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    assert!(
        request.starts_with("GET /v1/models "),
        "unexpected catalog request: {request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-live-catalog"),
        "catalog request should include saved API key auth header: {request}"
    );
    assert!(
        request.to_ascii_lowercase().contains("user-agent: jcode/"),
        "catalog requests must include a User-Agent because providers like Cerebras reject bare HTTP clients: {request}"
    );

    let display = provider.available_models_display();
    assert!(display.iter().any(|model| model == "live-login-flow-model"));
    assert!(
        display
            .iter()
            .any(|model| model == "static-login-flow-fallback"),
        "static fallback/default models should remain visible alongside live catalog models: {display:?}"
    );

    let fresh_provider = OpenRouterProvider {
        api_base: provider.api_base.clone(),
        model: Arc::new(RwLock::new("live-login-flow-model".to_string())),
        auth: provider.auth.clone(),
        supports_provider_features: false,
        supports_model_catalog: true,
        profile_id: None,
        reasoning_effort_support: None,
        send_openrouter_headers: false,
        ..make_custom_compatible_provider()
    };
    assert_eq!(fresh_provider.context_window(), 131_072);
}

#[test]
fn built_in_openai_compatible_static_models_drop_out_after_live_catalog() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp home");
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _namespace = EnvVarGuard::set(
        "JCODE_OPENROUTER_CACHE_NAMESPACE",
        "test-cerebras-live-catalog-filters-static-fallback",
    );
    let (api_base, _request_rx) = spawn_single_response_models_server(
        r#"{
            "object": "list",
            "data": [
                {"id": "qwen-3-235b-a22b-instruct-2507", "object": "model"},
                {"id": "zai-glm-4.7", "object": "model"},
                {"id": "gpt-oss-120b", "object": "model"}
            ]
        }"#,
    );
    let provider = OpenRouterProvider {
        api_base,
        auth: ProviderAuth::AuthorizationBearer {
            token: "sk-live-catalog".to_string(),
            label: "CEREBRAS_API_KEY".to_string(),
        },
        supports_provider_features: false,
        supports_model_catalog: true,
        profile_id: Some("cerebras".to_string()),
        static_models: vec!["gpt-oss-120b".to_string(), "zai-glm-4.7".to_string()],
        send_openrouter_headers: false,
        ..make_custom_compatible_provider()
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(provider.refresh_models())
        .expect("refresh fake model catalog");

    let display = provider.available_models_display();
    assert!(display.iter().any(|model| model == "gpt-oss-120b"));
    assert!(display.iter().any(|model| model == "zai-glm-4.7"));
    assert!(
        display
            .iter()
            .any(|model| model == "qwen-3-235b-a22b-instruct-2507"),
        "live catalog chat-capable models should remain visible: {display:?}"
    );
}

#[test]
fn direct_openai_compatible_static_models_are_marked_as_fallback_before_live_catalog() {
    let provider = OpenRouterProvider {
        supports_provider_features: false,
        supports_model_catalog: true,
        profile_id: Some("opencode".to_string()),
        static_models: vec!["minimax-m2.7".to_string()],
        send_openrouter_headers: false,
        ..make_custom_compatible_provider()
    };

    let routes = provider.model_routes();
    let route = routes
        .iter()
        .find(|route| route.model == "minimax-m2.7")
        .expect("static fallback route should be present before live catalog fetch");

    assert!(
        route
            .detail
            .contains("fallback: static provider model list"),
        "fallback routes should be clearly labeled in the model picker: {route:?}"
    );
}

#[test]
fn cerebras_live_catalog_models_are_selectable_on_explicit_switch() {
    let provider = OpenRouterProvider {
        supports_provider_features: false,
        supports_model_catalog: true,
        profile_id: Some("cerebras".to_string()),
        static_models: vec!["gpt-oss-120b".to_string()],
        send_openrouter_headers: false,
        ..make_custom_compatible_provider()
    };

    provider
        .set_model("zai-glm-4.7")
        .expect("live Cerebras model should be selectable");
    assert_eq!(provider.model(), "zai-glm-4.7");
    provider
        .set_model("gpt-oss-120b")
        .expect("default Cerebras model should remain selectable");
    assert_eq!(provider.model(), "gpt-oss-120b");
}

#[test]
fn direct_deepseek_profile_uses_static_1m_context_when_catalog_is_absent() {
    let _lock = ENV_LOCK.lock();
    let _base = EnvVarGuard::set("JCODE_OPENROUTER_API_BASE", "https://api.deepseek.com");
    let _key_name = EnvVarGuard::set("JCODE_OPENROUTER_API_KEY_NAME", "DEEPSEEK_API_KEY");
    let _api_key = EnvVarGuard::set("DEEPSEEK_API_KEY", "test");
    let _namespace = EnvVarGuard::set("JCODE_OPENROUTER_CACHE_NAMESPACE", "deepseek");
    let _model = EnvVarGuard::set("JCODE_OPENROUTER_MODEL", "deepseek-v4-flash");
    let _catalog = EnvVarGuard::set("JCODE_OPENROUTER_MODEL_CATALOG", "0");

    let provider = OpenRouterProvider::new().expect("provider");

    assert_eq!(provider.context_window(), 1_000_000);
}

#[test]
fn named_openai_compatible_model_context_window_overrides_default() {
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let mut config = jcode_base::config::NamedProviderConfig {
        base_url: "https://compat.example.test/v1".to_string(),
        api_key: Some("test".to_string()),
        default_model: Some("custom-long-context".to_string()),
        models: vec![jcode_base::config::NamedProviderModelConfig {
            id: "custom-long-context".to_string(),
            context_window: Some(512_000),
            input: Vec::new(),
        }],
        ..Default::default()
    };
    config.model_catalog = false;

    let provider =
        OpenRouterProvider::new_named_openai_compatible("custom", &config).expect("provider");

    assert_eq!(provider.context_window(), 512_000);
}

#[test]
fn named_profile_context_window_overrides_conflicting_live_catalog() {
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let config = jcode_base::config::NamedProviderConfig {
        base_url: "https://compat.example.test/v1".to_string(),
        api_key: Some("test".to_string()),
        default_model: Some("custom-budget".to_string()),
        models: vec![jcode_base::config::NamedProviderModelConfig {
            id: "custom-budget".to_string(),
            context_window: Some(16_384),
            input: Vec::new(),
        }],
        model_catalog: true,
        ..Default::default()
    };
    let provider =
        OpenRouterProvider::new_named_openai_compatible("custom", &config).expect("provider");
    {
        let mut cache = provider
            .models_cache
            .try_write()
            .expect("models cache lock");
        cache.models.push(ModelInfo {
            id: "custom-budget".to_string(),
            name: "Catalog claims a larger window".to_string(),
            context_length: Some(272_000),
            pricing: ModelPricing::default(),
            created: None,
        });
        cache.fetched = true;
    }

    assert_eq!(provider.context_window(), 16_384);
}

#[test]
fn named_profile_context_window_survives_provider_qualified_model() {
    // Regression for #403: if the runtime model transiently carries the
    // session-routing `<profile>:<model>` prefix, context_window() must still
    // resolve the configured per-model context_window rather than falling
    // through to the (large) provider default and over-budgeting the request.
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let mut config = jcode_base::config::NamedProviderConfig {
        base_url: "http://10.15.15.53:8080/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        default_model: Some("qwen3.6-35b-a2000-128k".to_string()),
        models: vec![jcode_base::config::NamedProviderModelConfig {
            id: "qwen3.6-35b-a2000-128k".to_string(),
            context_window: Some(131_072),
            input: Vec::new(),
        }],
        ..Default::default()
    };
    config.model_catalog = false;
    config.requires_api_key = Some(false);

    let provider = OpenRouterProvider::new_named_openai_compatible("cachyai-a2000", &config)
        .expect("provider");

    // Simulate the poisoned/qualified runtime model that #403 reported.
    {
        let mut model = provider.model.try_write().expect("model lock");
        *model = "cachyai-a2000:qwen3.6-35b-a2000-128k".to_string();
    }

    assert_eq!(provider.context_window(), 131_072);
}

#[test]
fn named_openai_compatible_loads_api_key_from_env_file() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp dir");
    let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", temp.path());
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let _api_key = EnvVarGuard::remove("CUSTOM_API_KEY");
    write_test_api_key(&temp, "custom.env", "CUSTOM_API_KEY", "from-env-file");

    let config = jcode_base::config::NamedProviderConfig {
        base_url: "https://compat.example.test/v1".to_string(),
        api_key_env: Some("CUSTOM_API_KEY".to_string()),
        env_file: Some("custom.env".to_string()),
        default_model: Some("custom-model".to_string()),
        ..Default::default()
    };

    OpenRouterProvider::new_named_openai_compatible("custom", &config)
        .expect("provider should load key from env file");
}

#[test]
fn custom_compatible_provider_preserves_claude_like_model_ids() {
    let provider = make_custom_compatible_provider();

    provider.set_model("claude-opus4.6-thinking").unwrap();

    assert_eq!(provider.model(), "claude-opus4.6-thinking");
}

#[test]
fn custom_compatible_provider_preserves_at_sign_model_ids() {
    let provider = make_custom_compatible_provider();

    provider.set_model("gpt-5.4@OpenAI").unwrap();

    assert_eq!(provider.model(), "gpt-5.4@OpenAI");
}

#[test]
fn named_profile_set_model_strips_own_session_routing_prefix() {
    // Session restore persists `<profile>:<model>`; the standalone provider
    // must normalize its own profile prefix back to the bare model id so the
    // upstream API never sees `tokenrouter:MiniMax-M3` (issues #382/#383/#363).
    let provider = OpenRouterProvider {
        profile_id: Some("tokenrouter".to_string()),
        supports_provider_features: false,
        supports_model_catalog: false,
        ..make_custom_compatible_provider()
    };

    provider.set_model("tokenrouter:MiniMax-M3").unwrap();
    assert_eq!(provider.model(), "MiniMax-M3");

    // Bare ids still work unchanged.
    provider.set_model("MiniMax-M3").unwrap();
    assert_eq!(provider.model(), "MiniMax-M3");
}

#[test]
fn named_profile_set_model_strips_other_known_profile_prefix() {
    // A session saved under one built-in OpenAI-compatible profile and
    // reattached under another must still normalize to the bare model id.
    let provider = OpenRouterProvider {
        profile_id: Some("tokenrouter".to_string()),
        supports_provider_features: false,
        supports_model_catalog: false,
        ..make_custom_compatible_provider()
    };

    provider.set_model("kimi:kimi-for-coding").unwrap();
    assert_eq!(provider.model(), "kimi-for-coding");
}

#[test]
fn named_profile_set_model_keeps_builtin_routing_prefixes() {
    // Built-in provider routing prefixes must round-trip verbatim so a user can
    // switch the active provider from a saved session.
    let provider = OpenRouterProvider {
        profile_id: Some("tokenrouter".to_string()),
        supports_provider_features: false,
        supports_model_catalog: false,
        ..make_custom_compatible_provider()
    };

    for spec in [
        "claude-oauth:claude-opus-4-8",
        "openai-api:gpt-5.4",
        "copilot:gpt-5.4",
    ] {
        provider.set_model(spec).unwrap();
        assert_eq!(provider.model(), spec, "spec {spec} must be preserved");
    }
}

