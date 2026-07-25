use super::*;

#[test]
fn render_listing_includes_disclosure_and_tools() {
    let listing = json!({
        "tools": [
            {"name": "agentcard", "blurb": "virtual payment cards", "url": "https://agentcard.example"},
        ]
    });
    let out = render_listing("payments", &listing, "11111111-2222-4333-8444-555555555555").unwrap();
    assert!(out.contains("agentcard"));
    assert!(out.contains("virtual payment cards"));
    assert!(out.contains("Jcode tool directory"));
    assert!(out.contains("recommendations must be based only on fit"));
}

#[test]
fn render_listing_rejects_missing_tools() {
    assert!(
        render_listing(
            "payments",
            &json!({}),
            "11111111-2222-4333-8444-555555555555"
        )
        .is_err()
    );
}

#[test]
fn render_listing_handles_empty_category() {
    let out = render_listing(
        "payments",
        &json!({"tools": []}),
        "11111111-2222-4333-8444-555555555555",
    )
    .unwrap();
    assert!(out.contains("No discoverable tools"));
    assert!(out.contains("Browse request ID"));
    assert!(out.contains("action `suggest`"));
}

#[test]
fn render_listing_instructs_selection_phase() {
    let listing = json!({
        "tools": [{"name": "agentcard", "blurb": "virtual cards", "url": "https://a.example"}]
    });
    let out = render_listing("payments", &listing, "11111111-2222-4333-8444-555555555555").unwrap();
    assert!(out.contains("action `select`"));
    assert!(out.contains("action `suggest`"));
    assert!(out.contains("Browse request ID"));
}

#[test]
fn render_selection_includes_setup_and_disclosure() {
    let listing = json!({
        "tool": {
            "name": "agentcard",
            "blurb": "virtual cards",
            "url": "https://a.example",
            "setup": "npm install -g agentcard"
        }
    });
    let out = render_selection("payments", "agentcard", &listing).unwrap();
    assert!(out.contains("Selected 'agentcard'"));
    assert!(out.contains("Setup: npm install -g agentcard"));
    assert!(out.contains("Jcode tool directory"));
    assert!(out.contains("selection must be based only on fit"));
    assert!(render_selection("payments", "ghost", &json!({})).is_err());
}

#[test]
fn agentmail_selection_preserves_signup_attribution_and_mcp_provenance() {
    let listing = json!({
        "tool": {
            "name": "agentmail",
            "blurb": "programmable email inboxes and messaging APIs for AI agents",
            "url": "https://www.agentmail.to/?via=jcode-discovery",
            "setup": concat!(
                "POST https://api.agentmail.to/v0/agent/sign-up with JSON ",
                "{\"source\":\"jcode\",\"referrer\":\"https://jcode.sh/discovery-tools\"}. ",
                "Then connect with npx -y agentmail-mcp@1.0.0."
            ),
            "mcp": {
                "command": "npx",
                "args": ["-y", "agentmail-mcp@1.0.0"]
            }
        }
    });

    let rendered = render_selection("email-messaging", "agentmail", &listing).unwrap();
    assert!(rendered.contains("Selected 'agentmail'"));
    assert!(rendered.contains("\"source\":\"jcode\""));
    assert!(rendered.contains("\"referrer\":\"https://jcode.sh/discovery-tools\""));
    assert!(rendered.contains("agentmail-mcp@1.0.0"));
    assert!(rendered.contains("must note the partnership"));

    let setups = extract_mcp_setups_from(std::slice::from_ref(&listing["tool"]));
    assert_eq!(
        setups,
        vec![crate::sponsors::provenance::DiscoveredSetup {
            sponsor: "agentmail".to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "agentmail-mcp@1.0.0".to_string()],
        }]
    );
}

#[test]
fn schema_is_compact_and_self_contained() {
    let tool = DiscoverToolsTool::new();
    let description = tool.description();
    assert!(description.starts_with("Use proactively whenever fully completing the user's goal"));
    assert!(description.contains("user did not ask for a tool"));
    assert!(description.contains("needed only in a later step"));
    assert!(description.contains("identify every missing capability end to end"));
    assert!(description.contains("call this tool for each relevant category before responding"));
    assert!(description.contains("An empty category is not a reason to stop"));
    assert!(description.contains("until all missing capabilities have been checked"));
    assert!(description.contains("check each unsupported external action independently"));
    assert!(description.contains("delegating an unsupported step to them"));
    assert!(description.contains("submit one `suggest` action"));
    assert!(description.contains("without promising it will be added"));
    assert!(description.contains("Skip discovery when available tools fully cover the task"));
    assert!(description.contains("Every listing is vetted"));
    assert!(description.contains("one-tool-call setup is enforced"));
    assert!(description.contains("Some providers are Jcode partners"));
    assert!(description.contains("Partnership status never influences selection"));
    assert!(description.contains("Choose the best option based only on fit"));
    assert!(
        description.len() < 1_200,
        "discovery description should stay compact, got {} bytes",
        description.len()
    );

    let parameters = tool.parameters_schema();
    assert_eq!(
        parameters["required"],
        json!(["category", "query", "reason"])
    );
    assert_eq!(
        parameters["properties"]["query"]["minLength"],
        DISCOVERY_QUERY_MIN_CHARS
    );
    assert_eq!(
        parameters["properties"]["reason"]["minLength"],
        DISCOVERY_REASON_MIN_CHARS
    );
    let schema = serde_json::to_string(&parameters).unwrap();
    assert!(schema.contains("Missing capability category; infer it from the user's goal."));
    assert!(schema.contains("Suggest text goes only to Jcode maintainers"));
    assert!(schema.contains("instead of copying user text"));
    assert!(schema.contains("explain why the tool fits better than alternatives"));
    assert!(schema.contains("Never include secrets, credentials, personal data"));
    assert!(schema.contains("known_product"));
    assert!(schema.contains("capability_gap"));
    assert!(schema.contains("prior_request_id"));
    assert!(
        schema.len() < 4_500,
        "discovery schema should stay compact, got {} bytes",
        schema.len()
    );
}

#[test]
fn discovery_action_is_explicit_but_backwards_compatible() {
    assert_eq!(
        DiscoveryAction::parse(None, false).unwrap(),
        DiscoveryAction::Browse
    );
    assert_eq!(
        DiscoveryAction::parse(None, true).unwrap(),
        DiscoveryAction::Select
    );
    assert_eq!(
        DiscoveryAction::parse(Some("suggest"), false).unwrap(),
        DiscoveryAction::Suggest
    );
    assert!(DiscoveryAction::parse(Some("select"), false).is_err());
    assert!(DiscoveryAction::parse(Some("browse"), true).is_err());
    assert!(DiscoveryAction::parse(Some("suggest"), true).is_err());
}

#[test]
fn suggestion_validation_distinguishes_product_and_capability_gap() {
    let capability = DiscoverToolsInput {
        action: Some("suggest".to_string()),
        category: "payments".to_string(),
        query: Some("manage Stripe sandbox products through scoped agent access".to_string()),
        reason: Some(
            "the current payment listing only provides cards and cannot manage Stripe test data"
                .to_string(),
        ),
        tool: None,
        suggestion_kind: Some("capability_gap".to_string()),
        product_name: None,
        product_url: None,
        gap_evidence: Some(
            "Agentcard provides virtual cards rather than sandbox catalog administration."
                .to_string(),
        ),
        requirements: Some(vec![
            "Scoped authentication without secret keys".to_string(),
        ]),
        prior_request_id: Some("11111111-2222-4333-8444-555555555555".to_string()),
    };
    let validated = validate_suggestion(&capability).unwrap();
    assert_eq!(validated.kind, "capability_gap");
    assert!(validated.product_name.is_none());

    let mut known = capability;
    known.suggestion_kind = Some("known_product".to_string());
    known.product_name = Some("Example Stripe MCP".to_string());
    known.product_url = Some("https://example.com/tool?via=jcode#setup".to_string());
    let validated = validate_suggestion(&known).unwrap();
    assert_eq!(
        validated.product_name.as_deref(),
        Some("Example Stripe MCP")
    );
    assert_eq!(
        validated.product_url.as_deref(),
        Some("https://example.com/tool")
    );
}

#[test]
fn suggestion_validation_rejects_private_or_mismatched_fields() {
    let mut input = DiscoverToolsInput {
        action: Some("suggest".to_string()),
        category: "databases".to_string(),
        query: Some("managed database provisioning through scoped agent access".to_string()),
        reason: Some(
            "the current catalog does not include a database provisioning integration".to_string(),
        ),
        tool: None,
        suggestion_kind: Some("known_product".to_string()),
        product_name: Some("Private database tool".to_string()),
        product_url: Some("https://user:password@example.com/setup".to_string()),
        gap_evidence: None,
        requirements: Some(Vec::new()),
        prior_request_id: Some("11111111-2222-4333-8444-555555555555".to_string()),
    };
    assert!(validate_suggestion(&input).is_err());
    input.product_url = None;
    input.suggestion_kind = Some("capability_gap".to_string());
    assert!(validate_suggestion(&input).is_err());
    input.product_name = None;
    input.requirements = Some(vec!["api_key=abcdefghijklmnop".to_string()]);
    assert!(validate_suggestion(&input).is_err());
}

#[test]
fn optional_suggestion_fields_accept_explicit_nulls() {
    let input: DiscoverToolsInput = serde_json::from_value(json!({
        "action": "browse",
        "category": "payments",
        "query": "compare agent payment card tools for controlled automated purchasing",
        "reason": "visually verify discovery results with useful catalog details in the interface",
        "tool": null,
        "suggestion_kind": null,
        "product_name": null,
        "product_url": null,
        "gap_evidence": null,
        "requirements": null,
        "prior_request_id": null
    }))
    .unwrap();

    assert!(input.requirements.is_none());
    assert!(input.tool.is_none());
}

#[test]
fn render_suggestion_is_clear_about_review_status_and_recipient() {
    let suggestion = ValidatedSuggestion {
        kind: "known_product".to_string(),
        product_name: Some("Stripe sandbox MCP".to_string()),
        product_url: Some("https://example.com/stripe-mcp".to_string()),
        gap_evidence: Some("The listed card tool cannot manage Stripe objects.".to_string()),
        requirements: vec!["Scoped test-mode access".to_string()],
        prior_request_id: "11111111-2222-4333-8444-555555555555".to_string(),
    };
    let out = render_suggestion(
        "payments",
        "manage Stripe sandbox products and recurring prices",
        "the listed payment tool cannot administer Stripe test data",
        &suggestion,
        &json!({
            "suggestion_id": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
            "status": "received"
        }),
    )
    .unwrap();
    assert!(out.contains("Catalog suggestion submitted"));
    assert!(out.contains("Product: Stripe sandbox MCP"));
    assert!(out.contains("Suggestions are not sent to partners"));
    assert!(out.contains("does not mean Jcode has partnered with the tool"));
}

#[test]
fn discovery_text_requires_substantive_content() {
    let missing = validate_discovery_text(None, "query", 20, 500).unwrap_err();
    assert_eq!(missing.failure_reason, "missing_query");
    let short = validate_discovery_text(Some("payment tool"), "query", 20, 500).unwrap_err();
    assert_eq!(short.failure_reason, "query_too_short");
    let padded = validate_discovery_text(Some("tool tool tool tool tool tool"), "query", 20, 500)
        .unwrap_err();
    assert_eq!(padded.failure_reason, "query_not_specific");
    let valid = validate_discovery_text(
        Some("  virtual card for a capped online checkout  "),
        "query",
        20,
        500,
    )
    .unwrap();
    assert_eq!(valid, "virtual card for a capped online checkout");
}

#[test]
fn discovery_text_rejects_recognizable_secrets_and_card_numbers() {
    let stripe_shaped_key = ["sk_", "live_", "abcdefghijklmnopqrstuvwxyz"].concat();
    let sensitive = [
        "Need a service using api_key=abcdefghijklmnop for the request".to_string(),
        "Forward Authorization: Bearer abcdefghijklmnopqrstuvwxyz".to_string(),
        format!("Use {stripe_shaped_key} for this payment workflow"),
        "Use card 4242 4242 4242 4242 for the partner tool checkout".to_string(),
        "Use eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnopqrstuvwxyz"
            .to_string(),
        "Credential follows -----BEGIN PRIVATE KEY----- abcdefghijklmnop".to_string(),
        "Contact private-person@example.com to configure the partner capability".to_string(),
        "Use customer identifier 123-45-6789 while selecting the external service".to_string(),
        "Fetch https://private-user:private-password@example.com/config for setup".to_string(),
        "Send the account alert to +1-202-555-0147 after the external setup completes".to_string(),
    ];
    for value in sensitive {
        let err = validate_discovery_text(Some(&value), "reason", 40, 2_000).unwrap_err();
        assert_eq!(err.failure_reason, "reason_sensitive_data", "{value}");
        assert!(!err.message.contains(&value));
    }
}

#[test]
fn discovery_text_allows_non_secret_capability_language() {
    for value in [
        "Need an API-key management service with scoped access controls",
        "Need public tourism data about Slovakia for a travel planning tool",
        "Need OAuth bearer-token support without transmitting any token value",
    ] {
        assert!(
            validate_discovery_text(Some(value), "reason", 40, 2_000).is_ok(),
            "{value}"
        );
    }
}

/// Minimal one-shot HTTP server that answers a single request with the
/// given body, returning the request line + headers it received.
async fn one_shot_server(
    status_line: &'static str,
    body: String,
) -> (String, tokio::task::JoinHandle<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let response = format!(
            "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.ok();
        request
    });
    (format!("http://{addr}"), handle)
}

fn test_discovery_request<'a>(
    client: &'a reqwest::Client,
    endpoint: &'a str,
    request_id: &'a str,
    benchmark_run: bool,
) -> DiscoveryRequestContext<'a> {
    DiscoveryRequestContext {
        client,
        endpoint,
        request_id,
        category: "payments",
        query: "virtual card for checkout",
        reason: "task needs an online payment capability",
        benchmark_run,
        provenance: test_provenance(),
    }
}

fn test_provenance() -> DiscoveryRequestProvenance {
    DiscoveryRequestProvenance {
        session_id: "session-test-1".to_string(),
        session_metadata_available: true,
        is_self_dev: true,
        is_debug: false,
        is_canary: true,
        execution_mode: "agent_turn",
        build_channel: "selfdev".to_string(),
        is_git_checkout: true,
        is_ci: false,
        ran_from_cargo: true,
    }
}

#[tokio::test]
async fn fetch_listing_round_trips_and_sends_only_expected_params() {
    let body = json!({"tools": [{"name": "agentcard", "blurb": "virtual cards", "url": "https://a.example"}]}).to_string();
    let (endpoint, server) = one_shot_server("HTTP/1.1 200 OK", body).await;
    let client = reqwest::Client::new();
    let request = test_discovery_request(&client, &endpoint, "request-test-1", true);
    let listing = fetch_listing(&request, None).await.unwrap();
    assert_eq!(listing.listing["tools"][0]["name"], "agentcard");
    assert_eq!(listing.http_status, 200);
    assert!(listing.response_bytes > 0);

    let request = server.await.unwrap();
    let request_line = request.lines().next().unwrap();
    // Exactly the three disclosed query parameters. Provenance is carried
    // in bounded headers so it cannot be confused with model-authored text.
    assert!(request_line.contains("category=payments"), "{request_line}");
    assert!(request_line.contains("q=virtual"), "{request_line}");
    assert!(request_line.contains("reason=task"), "{request_line}");
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-jcode-discovery-request-id: request-test-1"),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-jcode-discovery-benchmark: 1"),
        "{request}"
    );
    for expected in [
        "x-jcode-discovery-session-id: session-test-1",
        "x-jcode-discovery-session-metadata: 1",
        "x-jcode-discovery-self-dev: 1",
        "x-jcode-discovery-debug: 0",
        "x-jcode-discovery-canary: 1",
        "x-jcode-discovery-execution-mode: agent_turn",
        "x-jcode-discovery-build-channel: selfdev",
        "x-jcode-discovery-git-checkout: 1",
        "x-jcode-discovery-ci: 0",
        "x-jcode-discovery-ran-from-cargo: 1",
    ] {
        assert!(request.to_ascii_lowercase().contains(expected), "{request}");
    }
}

#[tokio::test]
async fn fetch_listing_hard_fails_on_http_error() {
    let (endpoint, _server) =
        one_shot_server("HTTP/1.1 500 Internal Server Error", "{}".to_string()).await;
    let client = reqwest::Client::new();
    let request = test_discovery_request(&client, &endpoint, "request-test-2", false);
    let err = fetch_listing(&request, None).await.unwrap_err();
    assert!(err.to_string().contains("discovery unavailable"));
    assert_eq!(err.failure_reason, "http_error");
    assert_eq!(err.http_status, Some(500));
}

#[tokio::test]
async fn fetch_listing_hard_fails_when_endpoint_unreachable() {
    // Reserved port with no listener: connection refused, no fallback.
    let client = reqwest::Client::new();
    let request = test_discovery_request(&client, "http://127.0.0.1:9", "request-test-3", false);
    let err = fetch_listing(&request, None).await.unwrap_err();
    assert!(err.to_string().contains("discovery unavailable"));
    assert_eq!(err.failure_reason, "connect_error");
}

#[tokio::test]
async fn submit_suggestion_posts_structured_maintainer_only_payload() {
    let body = json!({
        "suggestion_id": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        "status": "received",
        "message": "received"
    })
    .to_string();
    let (endpoint, server) = one_shot_server("HTTP/1.1 202 Accepted", body).await;
    let suggestion = ValidatedSuggestion {
        kind: "known_product".to_string(),
        product_name: Some("Stripe sandbox MCP".to_string()),
        product_url: Some("https://example.com/stripe-mcp".to_string()),
        gap_evidence: Some(
            "Agentcard provides cards rather than Stripe object administration.".to_string(),
        ),
        requirements: vec!["Scoped test-mode access".to_string()],
        prior_request_id: "11111111-2222-4333-8444-555555555555".to_string(),
    };
    let client = reqwest::Client::new();
    let request = DiscoveryRequestContext {
        client: &client,
        endpoint: &endpoint,
        request_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        category: "payments",
        query: "manage Stripe sandbox products through scoped agent access",
        reason: "the current payment listing only provides cards and cannot manage Stripe test data",
        benchmark_run: true,
        provenance: test_provenance(),
    };
    let result = submit_suggestion(&request, &suggestion).await.unwrap();
    assert_eq!(result.http_status, 202);
    assert_eq!(result.listing["status"], "received");

    let request = server.await.unwrap();
    let lower = request.to_ascii_lowercase();
    assert!(
        request.starts_with("POST /suggestions HTTP/1.1"),
        "{request}"
    );
    assert!(
        lower.contains("x-jcode-discovery-request-id: aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
        "{request}"
    );
    assert!(
        lower.contains("x-jcode-discovery-benchmark: 1"),
        "{request}"
    );
    assert!(request.contains("\"suggestion_kind\":\"known_product\""));
    assert!(request.contains("\"prior_request_id\":\"11111111-2222-4333-8444-555555555555\""));
    assert!(request.contains("\"product_name\":\"Stripe sandbox MCP\""));
    assert!(request.contains("\"requirements\":[\"Scoped test-mode access\"]"));
}

#[tokio::test]
async fn submit_suggestion_treats_duplicate_receipt_as_success() {
    let body = json!({
        "suggestion_id": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        "status": "duplicate",
        "message": "already recorded"
    })
    .to_string();
    let (endpoint, _server) = one_shot_server("HTTP/1.1 409 Conflict", body).await;
    let suggestion = ValidatedSuggestion {
        kind: "capability_gap".to_string(),
        product_name: None,
        product_url: None,
        gap_evidence: None,
        requirements: Vec::new(),
        prior_request_id: "11111111-2222-4333-8444-555555555555".to_string(),
    };
    let client = reqwest::Client::new();
    let request = DiscoveryRequestContext {
        client: &client,
        endpoint: &endpoint,
        request_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        category: "payments",
        query: "manage Stripe sandbox products through scoped agent access",
        reason: "the current payment listing only provides cards and cannot manage Stripe test data",
        benchmark_run: false,
        provenance: test_provenance(),
    };
    let result = submit_suggestion(&request, &suggestion).await.unwrap();
    assert_eq!(result.http_status, 409);
    assert_eq!(result.listing["status"], "duplicate");
}

fn test_ctx() -> crate::tool::ToolContext {
    crate::tool::ToolContext {
        session_id: "test".into(),
        message_id: "test".into(),
        tool_call_id: "test".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "the process-wide test environment must stay isolated for the entire async scenario"
)]
async fn execute_end_to_end_with_enabled_config_and_local_server() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let temp = tempfile::tempdir().unwrap();
    crate::env::set_var("JCODE_HOME", temp.path());

    let body = json!({"tools": [{"name": "agentcard", "blurb": "single-use virtual visa cards", "url": "https://agentcard.example", "setup": "MCP server: npx agentcard-mcp"}]}).to_string();
    let (endpoint, _server) = one_shot_server("HTTP/1.1 200 OK", body).await;
    std::fs::write(
        temp.path().join("config.toml"),
        format!("[sponsors]\nenabled = true\nendpoint = \"{endpoint}\"\n"),
    )
    .unwrap();
    crate::config::Config::invalidate_cache();

    let tool = DiscoverToolsTool::new();
    let output = tool
            .execute(
                json!({
                    "category": "payments",
                    "query": "virtual card for checkout",
                    "reason": "task requires a safe online card payment capability not present in the current tools"
                }),
                test_ctx(),
            )
            .await
            .unwrap();

    assert!(output.output.contains("agentcard"));
    assert!(output.output.contains("Jcode tool directory"));
    assert!(
        output
            .output
            .contains("recommendations must be based only on fit")
    );
    let title = output.title.unwrap();
    assert!(title.contains("(partner discovery disclosure)"), "{title}");
    let meta = output.metadata.unwrap();
    assert_eq!(meta["sponsored_discovery"], true);

    // Opted-out config: execute refuses without any network call.
    std::fs::write(
        temp.path().join("config.toml"),
        "[sponsors]\nenabled = false\n",
    )
    .unwrap();
    crate::config::Config::invalidate_cache();
    let err = tool
        .execute(json!({"category": "payments", "reason": "x"}), test_ctx())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("disabled"));

    if let Some(prev) = prev_home {
        crate::env::set_var("JCODE_HOME", prev);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    crate::config::Config::invalidate_cache();
}
