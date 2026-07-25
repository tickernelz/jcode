/// Regression: when the shared interactive server boots an `OpenRouterProvider`
/// without binding `profile_id` (the deferred-auth bootstrap path used by the
/// TUI server), a session-routing `<name>:` prefix for a *user-defined* named
/// provider profile (`[providers.<name>]` in config.toml) must still be
/// stripped before the model id reaches the upstream API. Without this, a
/// resumed/new TUI session sends e.g. `cline:cline-pass/qwen3.7-max` verbatim
/// and the gateway rejects it with 404 model_not_found, even though headless
/// `jcode run` (which binds profile_id in-process) works fine.
#[test]
fn user_named_profile_prefix_is_stripped_even_without_profile_id() {
    let _lock = ENV_LOCK.lock();
    let temp = TempDir::new().expect("create temp home");
    let jcode_home = temp.path().join("jcode-home");
    let _jcode_home = EnvVarGuard::set("JCODE_HOME", &jcode_home);
    let _home = EnvVarGuard::set("HOME", temp.path());
    let _appdata = EnvVarGuard::set("APPDATA", temp.path().join("AppData").join("Roaming"));
    let _env = isolate_openrouter_autodetect_env();
    let (api_base, request_rx) = spawn_single_response_chat_server();

    std::fs::create_dir_all(&jcode_home).expect("create test config dir");
    std::fs::write(
        jcode_home.join("config.toml"),
        r#"
[provider]
default_provider = "cline"

[providers.cline]
type = "openai-compatible"
base_url = "https://api.cline.bot/api/v1"
api_key_env = "TEST_CLINE_KEY"
default_model = "cline-pass/qwen3.7-max"
model_catalog = false
"#,
    )
    .expect("write test config");
    jcode_base::config::invalidate_config_cache();

    // Simulate the shared-server provider slot: a generic OpenAI-compatible
    // provider with NO profile_id bound (deferred-auth bootstrap path).
    let provider = OpenRouterProvider {
        api_base,
        profile_id: None,
        supports_provider_features: false,
        supports_model_catalog: false,
        ..make_custom_compatible_provider()
    };

    // Session restore / default-model routing hands the provider a
    // `<name>:<model>` spec for the user profile.
    provider
        .set_model("cline:cline-pass/qwen3.7-max")
        .expect("set prefixed model");

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
            if event.is_err() {
                break;
            }
        }
    });

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("capture fake provider request");
    let body = parse_captured_request_body(&request);
    assert_eq!(
        body.get("model").and_then(|v| v.as_str()),
        Some("cline-pass/qwen3.7-max"),
        "user-defined named profile prefix must be stripped from the outbound model id; got: {request}"
    );

    jcode_base::config::invalidate_config_cache();
}
