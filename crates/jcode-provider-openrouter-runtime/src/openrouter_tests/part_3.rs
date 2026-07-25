#[test]
fn named_profile_set_model_keeps_unknown_prefix_with_colon() {
    // A `:`-bearing id whose prefix is neither this profile nor a known
    // built-in profile must be preserved verbatim (it may be a real model id).
    let provider = OpenRouterProvider {
        profile_id: Some("tokenrouter".to_string()),
        supports_provider_features: false,
        supports_model_catalog: false,
        ..make_custom_compatible_provider()
    };

    provider.set_model("some-vendor:weird-model").unwrap();
    assert_eq!(provider.model(), "some-vendor:weird-model");
}

#[test]
fn openrouter_provider_normalizes_bare_pinned_model_ids() {
    let provider = make_provider();

    provider.set_model("gpt-5.4@OpenAI").unwrap();

    assert_eq!(provider.model(), "openai/gpt-5.4");
}

#[test]
fn test_rank_providers_cache_priority() {
    let endpoints = vec![
        make_endpoint("FastCache", 50.0, 99.0, true, 0.0000002),
        make_endpoint("FasterNoCache", 60.0, 99.0, false, 0.0000001),
    ];

    let ranked = OpenRouterProvider::rank_providers_from_endpoints(&endpoints);
    assert_eq!(ranked.first().map(|s| s.as_str()), Some("FastCache"));
}

#[test]
fn test_rank_providers_speed_priority_among_cache_capable() {
    let endpoints = vec![
        make_endpoint("Fireworks", 120.0, 99.0, true, 0.0000013),
        make_endpoint("Moonshot AI", 80.0, 99.0, true, 0.0000010),
    ];

    let ranked = OpenRouterProvider::rank_providers_from_endpoints(&endpoints);
    assert_eq!(ranked.first().map(|s| s.as_str()), Some("Fireworks"));
}

#[test]
fn test_rank_providers_filters_down_providers() {
    let mut down_ep = make_endpoint("DownProvider", 200.0, 100.0, true, 0.0000001);
    down_ep.status = Some(1); // down
    let endpoints = vec![
        down_ep,
        make_endpoint("UpProvider", 50.0, 99.0, true, 0.0000002),
    ];

    let ranked = OpenRouterProvider::rank_providers_from_endpoints(&endpoints);
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0], "UpProvider");
}

#[test]
fn test_background_refresh_waits_for_soft_ttl() {
    let provider = make_provider();

    assert!(!provider.should_background_refresh_model_catalog(
        MODEL_CATALOG_SOFT_REFRESH_SECS.saturating_sub(1)
    ));
    assert!(provider.should_background_refresh_model_catalog(MODEL_CATALOG_SOFT_REFRESH_SECS));
}

#[test]
fn test_background_refresh_is_throttled_between_attempts() {
    let provider = make_provider();
    assert!(provider.begin_background_model_catalog_refresh());
    assert!(!provider.should_background_refresh_model_catalog(MODEL_CATALOG_SOFT_REFRESH_SECS));

    OpenRouterProvider::finish_background_model_catalog_refresh(&provider.model_catalog_refresh);

    assert!(!provider.should_background_refresh_model_catalog(MODEL_CATALOG_SOFT_REFRESH_SECS));
}

#[test]
fn test_kimi_routing_uses_endpoints_or_fallback() {
    let provider = OpenRouterProvider {
        model: Arc::new(RwLock::new("moonshotai/kimi-k2.5".to_string())),
        ..make_provider()
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let routing = rt.block_on(provider.effective_routing("moonshotai/kimi-k2.5"));
    let order = routing.order.expect("provider order should be set");
    // Should have providers - either from endpoint API or Kimi fallback
    assert!(
        !order.is_empty(),
        "Kimi routing should always produce a provider order"
    );
}

#[test]
fn observed_session_provider_pin_sticks_without_fallbacks() {
    // Simulates the KV-cache stickiness contract: after OpenRouter serves a
    // request for this model from a concrete provider (recorded as an
    // observed pin), every subsequent request must route to that exact same
    // provider with fallbacks disabled so the upstream prompt cache stays warm.
    let model = "anthropic/claude-sonnet-4.6";
    let provider = OpenRouterProvider {
        model: Arc::new(RwLock::new(model.to_string())),
        provider_pin: Arc::new(Mutex::new(Some(ProviderPin {
            model: model.to_string(),
            provider: "anthropic".to_string(),
            source: PinSource::Observed,
            allow_fallbacks: true,
            last_cache_read: None,
        }))),
        ..make_provider()
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let routing = rt.block_on(provider.effective_routing(model));

    assert_eq!(
        routing.order.as_deref(),
        Some(["anthropic".to_string()].as_slice()),
        "observed session provider should be pinned exactly"
    );
    assert!(
        !routing.allow_fallbacks,
        "observed session pin must disable fallbacks to preserve the KV cache"
    );
}

#[test]
fn observed_pin_yields_to_explicit_user_routing_order() {
    // If the user explicitly narrowed routing themselves (base order set),
    // their configured order wins over the auto-observed session pin.
    let model = "anthropic/claude-sonnet-4.6";
    let base = ProviderRouting {
        order: Some(vec!["fireworks".to_string()]),
        ..Default::default()
    };
    let provider = OpenRouterProvider {
        model: Arc::new(RwLock::new(model.to_string())),
        provider_routing: Arc::new(RwLock::new(base)),
        provider_pin: Arc::new(Mutex::new(Some(ProviderPin {
            model: model.to_string(),
            provider: "anthropic".to_string(),
            source: PinSource::Observed,
            allow_fallbacks: true,
            last_cache_read: None,
        }))),
        ..make_provider()
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let routing = rt.block_on(provider.effective_routing(model));

    assert_eq!(
        routing.order.as_deref(),
        Some(["fireworks".to_string()].as_slice()),
        "explicit user routing order should win over an observed session pin"
    );
}

#[test]
fn test_kimi_coding_header_detection_matches_endpoint_and_model() {
    assert!(should_send_kimi_coding_agent_headers(
        "https://api.kimi.com/coding/v1",
        None,
    ));
    assert!(should_send_kimi_coding_agent_headers(
        "https://coding.dashscope.aliyuncs.com/v1",
        None,
    ));
    assert!(should_send_kimi_coding_agent_headers(
        "https://coding-intl.dashscope.aliyuncs.com/v1",
        None,
    ));
    assert!(should_send_kimi_coding_agent_headers(
        "https://api.z.ai/api/coding/paas/v4",
        None,
    ));
    assert!(should_send_kimi_coding_agent_headers(
        "https://example.com/v1",
        Some("kimi-for-coding"),
    ));
    assert!(should_send_kimi_coding_agent_headers(
        "https://openrouter.ai/api/v1",
        Some("moonshotai/kimi-k2.5"),
    ));
    assert!(!should_send_kimi_coding_agent_headers(
        "https://api.openrouter.ai/api/v1",
        Some("anthropic/claude-sonnet-4"),
    ));
}

#[test]
fn test_openrouter_kimi_chat_request_includes_compat_user_agent() {
    let request = apply_kimi_coding_agent_headers(
        Client::new().post("https://openrouter.ai/api/v1/chat/completions"),
        "https://openrouter.ai/api/v1",
        Some("moonshotai/kimi-k2.5"),
    )
    .build()
    .expect("build request");
    assert!(
        request
            .headers()
            .get("User-Agent")
            .and_then(|value| value.to_str().ok())
            == Some(KIMI_CODING_USER_AGENT),
        "Kimi OpenRouter chat request should include compatibility User-Agent"
    );
}

#[test]
fn test_parse_next_event_accepts_compact_sse_data_and_reasoning_content() {
    let bytes = Bytes::from_static(
        b"data:{\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n",
    );
    let mut stream = OpenRouterStream::new(
        futures::stream::once(async move { Ok::<Bytes, reqwest::Error>(bytes) }),
        "kimi-for-coding".to_string(),
        Arc::new(Mutex::new(None)),
    );

    match futures::executor::block_on(stream.next()) {
        Some(Ok(StreamEvent::ThinkingDelta(text))) => assert_eq!(text, "thinking"),
        other => panic!("expected ThinkingDelta, got {:?}", other),
    }
}

#[test]
fn test_parse_next_event_emits_only_incremental_reasoning_content() {
    let chunks = vec![
        Ok::<Bytes, reqwest::Error>(Bytes::from_static(
            b"data:{\"choices\":[{\"delta\":{\"reasoning_content\":\"Thinking\"}}]}\n\n",
        )),
        Ok::<Bytes, reqwest::Error>(Bytes::from_static(
            b"data:{\"choices\":[{\"delta\":{\"reasoning_content\":\"Thinking more\"}}]}\n\n",
        )),
    ];
    let mut stream = OpenRouterStream::new(
        futures::stream::iter(chunks),
        "moonshotai/kimi-k2.5".to_string(),
        Arc::new(Mutex::new(None)),
    );

    match futures::executor::block_on(stream.next()) {
        Some(Ok(StreamEvent::ThinkingDelta(text))) => assert_eq!(text, "Thinking"),
        other => panic!("expected first ThinkingDelta, got {:?}", other),
    }
    match futures::executor::block_on(stream.next()) {
        Some(Ok(StreamEvent::ThinkingDelta(text))) => assert_eq!(text, " more"),
        other => panic!("expected incremental ThinkingDelta, got {:?}", other),
    }
}

#[test]
fn test_endpoint_detail_string() {
    let ep = EndpointInfo {
        provider_name: "TestProvider".to_string(),
        tag: None,
        pricing: ModelPricing {
            prompt: Some("0.00000045".to_string()),
            completion: Some("0.00000225".to_string()),
            input_cache_read: Some("0.00000007".to_string()),
            input_cache_write: Some("0.00000012".to_string()),
        },
        context_length: Some(131072),
        max_completion_tokens: Some(8192),
        quantization: Some("fp8".to_string()),
        uptime_last_30m: Some(99.5),
        latency_last_30m: Some(serde_json::json!({"p50": 500, "p75": 800})),
        throughput_last_30m: Some(serde_json::json!({"p50": 42, "p75": 55})),
        supports_implicit_caching: Some(true),
        status: Some(0),
    };
    let detail = ep.detail_string();
    assert!(
        detail.contains("$0.45/M"),
        "should contain price: {}",
        detail
    );
    assert!(detail.contains("100%"), "should contain uptime: {}", detail);
    assert!(
        detail.contains("out $2.25/M"),
        "should contain output price: {}",
        detail
    );
    assert!(
        detail.contains("cache write $0.12/M"),
        "should contain cache write price: {}",
        detail
    );
    assert!(
        detail.contains("cache read $0.07/M"),
        "should contain cache read price: {}",
        detail
    );
    assert!(
        detail.contains("500ms p50"),
        "should contain latency: {}",
        detail
    );
    assert!(
        detail.contains("42tps"),
        "should contain throughput: {}",
        detail
    );
    assert!(
        detail.contains("cache on"),
        "should contain cache: {}",
        detail
    );
    assert!(
        detail.contains("fp8"),
        "should contain quantization: {}",
        detail
    );
}

#[test]
fn strict_openai_schema_endpoint_detects_mistral_profile() {
    // Mistral direct profile rejects non-standard reasoning_content/thinking
    // fields with a 422 (issue #261), so it must be flagged strict.
    assert!(OpenRouterProvider::strict_openai_schema_endpoint(
        Some("mistral"),
        "https://api.mistral.ai/v1"
    ));
    assert!(OpenRouterProvider::strict_openai_schema_endpoint(
        Some("MISTRAL"),
        "https://example.com/v1"
    ));
}

#[test]
fn strict_openai_schema_endpoint_detects_mistral_api_base() {
    assert!(OpenRouterProvider::strict_openai_schema_endpoint(
        None,
        "https://api.mistral.ai/v1"
    ));
    assert!(OpenRouterProvider::strict_openai_schema_endpoint(
        Some("custom"),
        "https://API.MISTRAL.AI/v1"
    ));
}

#[test]
fn strict_openai_schema_endpoint_allows_other_providers() {
    assert!(!OpenRouterProvider::strict_openai_schema_endpoint(
        Some("deepseek"),
        "https://api.deepseek.com"
    ));
    assert!(!OpenRouterProvider::strict_openai_schema_endpoint(
        None,
        "https://openrouter.ai/api/v1"
    ));
    assert!(!OpenRouterProvider::strict_openai_schema_endpoint(
        Some("openai"),
        "https://api.openai.com/v1"
    ));
}

#[test]
fn runtime_display_name_for_profile_runtime_instance() {
    // Direct unit coverage of the per-instance resolver used by
    // `Provider::display_name`.
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp home");
    let jcode_home = temp.path().join("jcode-home");
    let _jcode_home = EnvVarGuard::set("JCODE_HOME", &jcode_home);
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();
    let _key = EnvVarGuard::set("NVIDIA_API_KEY", "nim-test-key");

    let nim = OpenRouterProvider::new_openai_compatible_profile_runtime(
        jcode_base::provider_catalog::NVIDIA_NIM_PROFILE,
    )
    .expect("build nvidia-nim runtime");
    assert_eq!(nim.runtime_display_name(), "NVIDIA NIM");
    assert_eq!(Provider::name(&nim), "openrouter");
}

#[test]
fn jcode_subscription_runtime_has_explicit_display_and_route_identity() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp home");
    let jcode_home = temp.path().join("jcode-home");
    let _jcode_home = EnvVarGuard::set("JCODE_HOME", &jcode_home);
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();
    let _base = EnvVarGuard::set(
        "JCODE_OPENROUTER_API_BASE",
        jcode_base::subscription_catalog::DEFAULT_JCODE_API_BASE,
    );
    let _key_name = EnvVarGuard::set(
        "JCODE_OPENROUTER_API_KEY_NAME",
        jcode_base::subscription_catalog::JCODE_API_KEY_ENV,
    );
    let _env_file = EnvVarGuard::set(
        "JCODE_OPENROUTER_ENV_FILE",
        jcode_base::subscription_catalog::JCODE_ENV_FILE,
    );
    let _provider_features = EnvVarGuard::set("JCODE_OPENROUTER_PROVIDER_FEATURES", "0");
    let _transport = EnvVarGuard::set("JCODE_OPENROUTER_TRANSPORT_STATE", "jcode-subscription");
    let _key = EnvVarGuard::set(
        jcode_base::subscription_catalog::JCODE_API_KEY_ENV,
        "jcode_test_subscription_key",
    );

    let provider = OpenRouterProvider::new().expect("build jcode subscription runtime");
    assert_eq!(provider.runtime_display_name(), "Jcode Subscription");
    assert_eq!(Provider::display_name(&provider), "Jcode Subscription");
    assert_eq!(Provider::name(&provider), "openrouter");
    assert_eq!(
        provider.direct_openai_compatible_route_parts(),
        Some((
            "Jcode Subscription".to_string(),
            "jcode-subscription".to_string(),
            jcode_base::subscription_catalog::DEFAULT_JCODE_API_BASE.to_string(),
        ))
    );
}

#[test]
fn non_subscription_runtimes_keep_existing_display_and_route_identity() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp home");
    let jcode_home = temp.path().join("jcode-home");
    let _jcode_home = EnvVarGuard::set("JCODE_HOME", &jcode_home);
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();
    let _openrouter_key = EnvVarGuard::set("OPENROUTER_API_KEY", "openrouter-test-key");

    let openrouter =
        OpenRouterProvider::new_openrouter_api_key_runtime().expect("build OpenRouter runtime");
    assert_eq!(openrouter.runtime_display_name(), "OpenRouter");
    assert_eq!(Provider::display_name(&openrouter), "OpenRouter");
    assert_eq!(openrouter.direct_openai_compatible_route_parts(), None);

    let _base = EnvVarGuard::set("JCODE_OPENROUTER_API_BASE", "https://example.com/v1");
    let _key_name = EnvVarGuard::set("JCODE_OPENROUTER_API_KEY_NAME", "GENERIC_API_KEY");
    let _provider_features = EnvVarGuard::set("JCODE_OPENROUTER_PROVIDER_FEATURES", "0");
    let _transport = EnvVarGuard::set("JCODE_OPENROUTER_TRANSPORT_STATE", "direct-compatible");
    let _generic_key = EnvVarGuard::set("GENERIC_API_KEY", "generic-test-key");

    let compatible = OpenRouterProvider::new().expect("build generic compatible runtime");
    assert_eq!(compatible.runtime_display_name(), "OpenAI-compatible");
    assert_eq!(Provider::display_name(&compatible), "OpenAI-compatible");
    assert_eq!(
        compatible.direct_openai_compatible_route_parts(),
        Some((
            "OpenAI-compatible".to_string(),
            "openai-compatible".to_string(),
            "https://example.com/v1".to_string(),
        ))
    );
}

#[test]
fn custom_endpoint_using_jcode_key_name_is_not_a_subscription_runtime() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp home");
    let jcode_home = temp.path().join("jcode-home");
    let _jcode_home = EnvVarGuard::set("JCODE_HOME", &jcode_home);
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();
    let _base = EnvVarGuard::set("JCODE_OPENROUTER_API_BASE", "https://example.com/v1");
    let _key_name = EnvVarGuard::set(
        "JCODE_OPENROUTER_API_KEY_NAME",
        jcode_base::subscription_catalog::JCODE_API_KEY_ENV,
    );
    let _provider_features = EnvVarGuard::set("JCODE_OPENROUTER_PROVIDER_FEATURES", "0");
    let _key = EnvVarGuard::set(
        jcode_base::subscription_catalog::JCODE_API_KEY_ENV,
        "custom-endpoint-test-key",
    );

    let provider = OpenRouterProvider::new().expect("build custom endpoint runtime");
    assert_eq!(provider.runtime_display_name(), "OpenAI-compatible");
    assert_eq!(
        provider.direct_openai_compatible_route_parts(),
        Some((
            "OpenAI-compatible".to_string(),
            "openai-compatible".to_string(),
            "https://example.com/v1".to_string(),
        ))
    );
}

#[test]
fn resolve_extra_body_returns_none_when_unset() {
    let _lock = ENV_LOCK.lock();
    let _guard = EnvVarGuard::remove("JCODE_OPENAI_EXTRA_BODY");
    assert!(OpenRouterProvider::resolve_extra_body(None, "nonexistent.env").is_none());
}

#[test]
fn resolve_extra_body_parses_env_json_object() {
    let _lock = ENV_LOCK.lock();
    let _guard = EnvVarGuard::set(
        "JCODE_OPENAI_EXTRA_BODY",
        r#"{"chat_template_kwargs":{"thinking":true,"reasoning_effort":"high"}}"#,
    );
    let extra =
        OpenRouterProvider::resolve_extra_body(None, "nonexistent.env").expect("extra body");
    let kwargs = extra
        .get("chat_template_kwargs")
        .and_then(|v| v.as_object())
        .expect("chat_template_kwargs object");
    assert_eq!(kwargs.get("thinking"), Some(&serde_json::json!(true)));
    assert_eq!(
        kwargs.get("reasoning_effort"),
        Some(&serde_json::json!("high"))
    );
}

#[test]
fn resolve_extra_body_ignores_invalid_env_json() {
    let _lock = ENV_LOCK.lock();
    let _guard = EnvVarGuard::set("JCODE_OPENAI_EXTRA_BODY", "not-json");
    assert!(OpenRouterProvider::resolve_extra_body(None, "nonexistent.env").is_none());
}

#[test]
fn resolve_extra_body_ignores_non_object_env_json() {
    let _lock = ENV_LOCK.lock();
    let _guard = EnvVarGuard::set("JCODE_OPENAI_EXTRA_BODY", "[1,2,3]");
    assert!(OpenRouterProvider::resolve_extra_body(None, "nonexistent.env").is_none());
}

#[test]
fn resolve_extra_body_merges_config_and_env_with_env_override() {
    let _lock = ENV_LOCK.lock();
    let config = serde_json::json!({
        "chat_template_kwargs": {"thinking": false},
        "config_only": 1,
    });
    let _guard = EnvVarGuard::set(
        "JCODE_OPENAI_EXTRA_BODY",
        r#"{"chat_template_kwargs":{"thinking":true},"env_only":2}"#,
    );
    let extra = OpenRouterProvider::resolve_extra_body(Some(&config), "nonexistent.env")
        .expect("merged extra body");
    // Env overrides the colliding key.
    assert_eq!(
        extra
            .get("chat_template_kwargs")
            .and_then(|v| v.get("thinking")),
        Some(&serde_json::json!(true))
    );
    // Non-colliding keys from both sources survive.
    assert_eq!(extra.get("config_only"), Some(&serde_json::json!(1)));
    assert_eq!(extra.get("env_only"), Some(&serde_json::json!(2)));
}

#[test]
fn resolve_extra_body_ignores_non_object_config() {
    let _lock = ENV_LOCK.lock();
    let _guard = EnvVarGuard::remove("JCODE_OPENAI_EXTRA_BODY");
    let config = serde_json::json!("not an object");
    assert!(OpenRouterProvider::resolve_extra_body(Some(&config), "nonexistent.env").is_none());
}

#[test]
fn named_profile_extra_body_threads_into_provider() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp home");
    let jcode_home = temp.path().join("jcode-home");
    let _jcode_home = EnvVarGuard::set("JCODE_HOME", &jcode_home);
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();
    let _extra_guard = EnvVarGuard::remove("JCODE_OPENAI_EXTRA_BODY");

    let mut profile = jcode_base::config::NamedProviderConfig {
        base_url: "https://integrate.api.nvidia.com/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        requires_api_key: Some(false),
        ..Default::default()
    };
    profile.extra_body = Some(serde_json::json!({
        "chat_template_kwargs": {"thinking": true, "reasoning_effort": "high"}
    }));

    let provider = OpenRouterProvider::new_named_openai_compatible("my-nim", &profile)
        .expect("build named provider");
    let extra = provider.extra_body.as_ref().expect("extra body present");
    assert_eq!(
        extra
            .get("chat_template_kwargs")
            .and_then(|v| v.get("reasoning_effort")),
        Some(&serde_json::json!("high"))
    );
}

#[test]
fn named_provider_config_deserializes_nested_extra_body_toml() {
    // Verifies the exact `config.toml` shape documented in the README:
    // a nested `[providers.<name>.extra_body.chat_template_kwargs]` table
    // round-trips into the `serde_json::Value` field correctly.
    let toml_str = r#"
type = "openai-compatible"
base_url = "https://integrate.api.nvidia.com/v1"
api_key_env = "NVIDIA_API_KEY"
default_model = "deepseek-ai/deepseek-v4-flash"

[extra_body.chat_template_kwargs]
thinking = true
reasoning_effort = "high"
"#;
    let profile: jcode_base::config::NamedProviderConfig =
        toml::from_str(toml_str).expect("parse named provider toml");
    let extra = profile.extra_body.as_ref().expect("extra_body present");
    let kwargs = extra
        .get("chat_template_kwargs")
        .and_then(|v| v.as_object())
        .expect("chat_template_kwargs object");
    assert_eq!(kwargs.get("thinking"), Some(&serde_json::json!(true)));
    assert_eq!(
        kwargs.get("reasoning_effort"),
        Some(&serde_json::json!("high"))
    );

    // And the resolver hands it back unchanged when no env override is set.
    let _lock = ENV_LOCK.lock();
    let _guard = EnvVarGuard::remove("JCODE_OPENAI_EXTRA_BODY");
    let resolved =
        OpenRouterProvider::resolve_extra_body(profile.extra_body.as_ref(), "nonexistent.env")
            .expect("resolved extra body");
    assert_eq!(
        resolved
            .get("chat_template_kwargs")
            .and_then(|v| v.get("reasoning_effort")),
        Some(&serde_json::json!("high"))
    );
}

// ============================================================================
// Mid-stream retry rollback (issue #338 gap #3)
// ============================================================================

/// Fake SSE server: the first connection streams partial output then drops the
/// socket mid-stream (transport fault); the second connection streams a clean,
/// complete response.
fn spawn_midstream_fault_then_complete_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider server");
    let addr = listener.local_addr().expect("fake provider addr");

    std::thread::spawn(move || {
        // Connection 1: partial output, then abrupt close (no [DONE]).
        {
            let (mut stream, _) = listener.accept().expect("accept first request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set read timeout");
            let mut request = vec![0u8; 65536];
            let _ = stream.read(&mut request);
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"partial answer that must not duplicate\"}}]}\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write partial response");
            stream.flush().expect("flush partial response");
            // Drop without terminating the chunked encoding: the client sees
            // an unexpected EOF mid-stream (transient transport fault).
            drop(stream);
        }

        // Connection 2 (the retry): clean complete response.
        {
            let (mut stream, _) = listener.accept().expect("accept retry request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set read timeout");
            let mut request = vec![0u8; 65536];
            let _ = stream.read(&mut request);
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"final answer\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write retry response");
        }
    });

    format!("http://{addr}/v1")
}

/// Regression for issue #338 gap #3: a transient transport fault that hits
/// mid-stream, after partial output has already been emitted, must surface a
/// `RetryRollback` before the replayed response so consumers can discard the
/// partial attempt instead of rendering duplicated output.
#[test]
fn midstream_transport_fault_emits_retry_rollback_before_replay() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    rt.block_on(async {
        let api_base = spawn_midstream_fault_then_complete_server();
        let client = reqwest::Client::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<anyhow::Result<StreamEvent>>(64);

        let request = serde_json::json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        });

        super::openrouter_sse_stream::run_stream_with_retries(
            client,
            api_base,
            ProviderAuth::None {
                label: "test".to_string(),
            },
            false,
            request,
            tx,
            Arc::new(Mutex::new(None)),
            "test-model".to_string(),
        )
        .await;

        let mut events = Vec::new();
        while let Some(item) = rx.recv().await {
            events.push(item);
        }

        let mut saw_partial = false;
        let mut rollback_after_partial = false;
        let mut final_after_rollback = false;
        let mut duplicate_partial_without_rollback = false;
        for item in &events {
            let Ok(event) = item else {
                panic!("stream surfaced an error instead of retrying: {item:?}");
            };
            match event {
                StreamEvent::TextDelta(text) => {
                    if text.contains("partial answer") {
                        if saw_partial && !rollback_after_partial {
                            duplicate_partial_without_rollback = true;
                        }
                        saw_partial = true;
                    }
                    if text.contains("final answer") {
                        assert!(
                            rollback_after_partial,
                            "replayed response arrived without a RetryRollback after partial output"
                        );
                        final_after_rollback = true;
                    }
                }
                StreamEvent::RetryRollback { .. } => {
                    assert!(
                        saw_partial,
                        "RetryRollback must only be emitted after partial output was streamed"
                    );
                    rollback_after_partial = true;
                }
                _ => {}
            }
        }

        assert!(saw_partial, "first attempt's partial output never arrived");
        assert!(
            rollback_after_partial,
            "no RetryRollback emitted for the mid-stream fault"
        );
        assert!(
            final_after_rollback,
            "retry never delivered the complete response"
        );
        assert!(
            !duplicate_partial_without_rollback,
            "partial output duplicated without an interleaved rollback"
        );
    });
}

/// Issue #352: reasoning effort must follow the *model family*, not just the
/// dedicated `deepseek` profile id. A custom compat endpoint (named profile or
/// generic openai-compatible) serving a DeepSeek model supports `/effort`.
#[test]
fn compat_profile_serving_deepseek_model_supports_reasoning_effort() {
    let provider = make_custom_compatible_provider();

    // Non-DeepSeek model on a custom endpoint: no effort support.
    provider.set_model("some-random-model").unwrap();
    assert!(provider.available_efforts().is_empty());
    assert!(provider.set_reasoning_effort("high").is_err());
    assert_eq!(provider.reasoning_effort(), None);

    // DeepSeek-family model: DeepSeek-style efforts become available.
    provider.set_model("deepseek-v4-flash").unwrap();
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
        .set_reasoning_effort("high")
        .expect("deepseek model on compat endpoint accepts effort");
    assert_eq!(provider.reasoning_effort(), Some("high".to_string()));
}

/// GPT-family reasoning models served by a direct OpenAI-compatible gateway
/// (e.g. OpenCode Zen's `gpt-5.3-codex-spark`) accept the standard OpenAI
/// `reasoning_effort` field, so the effort command must work for them.
#[test]
fn compat_profile_serving_gpt_family_model_supports_reasoning_effort() {
    let provider = make_custom_compatible_provider();

    for model in [
        "gpt-5.3-codex-spark",
        "gpt-5.5",
        "gpt-5.1-codex-mini",
        "o1",
        "o5-mini",
    ] {
        provider.set_model(model).unwrap();
        assert_eq!(
            provider.available_efforts(),
            vec![
                "none",
                "minimal",
                "low",
                "medium",
                "high",
                "xhigh",
                "max",
                "swarm",
                "swarm-deep"
            ],
            "{model} should expose OpenAI effort vocabulary"
        );
        provider
            .set_reasoning_effort("high")
            .unwrap_or_else(|e| panic!("{model} on compat endpoint accepts effort: {e}"));
        assert_eq!(provider.reasoning_effort(), Some("high".to_string()));
        // A direct compatible endpoint receives OpenAI's real max value.
        provider.set_reasoning_effort("max").unwrap();
        assert_eq!(provider.reasoning_effort(), Some("max".to_string()));
    }

    // Explicit config override still wins in the off direction.
    let force_off = OpenRouterProvider {
        reasoning_effort_support: Some(false),
        ..make_custom_compatible_provider()
    };
    force_off.set_model("gpt-5.3-codex-spark").unwrap();
    assert!(force_off.available_efforts().is_empty());
    assert!(force_off.set_reasoning_effort("high").is_err());
}

#[test]
fn compatible_model_switch_clears_an_effort_invalid_for_the_new_vocabulary() {
    let provider = make_custom_compatible_provider();
    provider.set_model("gpt-5.5").unwrap();
    provider.set_reasoning_effort("minimal").unwrap();
    provider.set_model("deepseek-v4").unwrap();
    assert_eq!(provider.reasoning_effort(), None);
    assert!(
        provider.set_reasoning_effort("minimal").is_err(),
        "DeepSeek must reject rather than silently promote minimal to max"
    );
}

/// Issue #352: named-profile config can override effort support explicitly in
/// both directions.
#[test]
fn named_profile_supports_reasoning_effort_config_override() {
    let force_on = OpenRouterProvider {
        reasoning_effort_support: Some(true),
        ..make_custom_compatible_provider()
    };
    force_on.set_model("not-a-deepseek-model").unwrap();
    assert_eq!(
        force_on.available_efforts(),
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
    force_on
        .set_reasoning_effort("medium")
        .expect("explicit supports_reasoning_effort=true enables effort");
    assert_eq!(force_on.reasoning_effort(), Some("medium".to_string()));

    let force_off = OpenRouterProvider {
        reasoning_effort_support: Some(false),
        ..make_custom_compatible_provider()
    };
    force_off.set_model("deepseek-v4-flash").unwrap();
    assert!(force_off.available_efforts().is_empty());
    assert!(
        force_off.set_reasoning_effort("high").is_err(),
        "explicit supports_reasoning_effort=false suppresses model auto-detection"
    );
}

/// Issue #352: named profiles construct with the user's configured
/// `openai_reasoning_effort` when the profile supports effort, instead of
/// silently ignoring the config.
#[test]
fn named_profile_construction_reads_openai_reasoning_effort_config() {
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");

    let config = jcode_base::config::NamedProviderConfig {
        base_url: "https://compat.example.test/v1".to_string(),
        api_key: Some("test".to_string()),
        default_model: Some("deepseek-v4".to_string()),
        supports_reasoning_effort: Some(true),
        ..Default::default()
    };

    let provider =
        OpenRouterProvider::new_named_openai_compatible("custom", &config).expect("provider");
    // The config default is only applied when openai_reasoning_effort is set;
    // with no config value the provider starts with no effort but still
    // supports setting one.
    let initial = provider.reasoning_effort();
    let configured = jcode_base::config::config()
        .provider
        .openai_reasoning_effort
        .clone();
    match configured {
        Some(_) => assert!(initial.is_some(), "configured effort must be honored"),
        None => assert_eq!(initial, None),
    }
    provider
        .set_reasoning_effort("max")
        .expect("explicitly-enabled profile accepts effort");
}

#[test]
fn named_gpt_profile_uses_configured_default_and_swarm_wire_efforts() {
    let _lock = ENV_LOCK.lock();
    let display = EnvVarGuard::set("JCODE_REASONING_DISPLAY", "full");
    let temp = TempDir::new().expect("temp home");
    let (api_base, request_rx) = spawn_single_response_chat_server();
    let jcode_home = temp.path().join("jcode-home");
    std::fs::create_dir_all(&jcode_home).expect("create config dir");
    std::fs::write(
        jcode_home.join("config.toml"),
        format!(
            r#"[provider]
openai_reasoning_effort = "xhigh"
openai_service_tier = "flex"

[providers.sub2api-codex]
type = "openai-compatible"
base_url = {api_base:?}
auth = "none"
default_model = "gpt-5.6-sol"
wire_api = "responses"
swarm_reasoning_effort = "medium"
"#
        ),
    )
    .expect("write config");
    let home = EnvVarGuard::set("JCODE_HOME", &jcode_home);
    let namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    jcode_base::config::invalidate_config_cache();

    let config = jcode_base::config::config()
        .providers
        .get("sub2api-codex")
        .cloned()
        .expect("configured provider");
    let provider = OpenRouterProvider::new_named_openai_compatible("sub2api-codex", &config)
        .expect("provider");

    assert_eq!(provider.reasoning_effort().as_deref(), Some("xhigh"));
    assert!(provider.available_efforts().contains(&"xhigh"));
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
    assert!(
        request.contains(r#""reasoning":{"effort":"medium","summary":"auto"}"#),
        "configured swarm wire effort must reach the wire: {request}"
    );
    assert!(
        request.contains(r#""service_tier":"flex""#),
        "configured service tier must reach the wire: {request}"
    );

    drop(provider);
    drop(namespace);
    drop(home);
    drop(display);
    jcode_base::config::invalidate_config_cache();
}
