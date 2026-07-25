use super::*;

#[derive(Debug, Clone, Default)]
struct OAuthClientMetadata {
    device_id: Option<String>,
    account_uuid: Option<String>,
    organization_uuid: Option<String>,
    email_address: Option<String>,
}

fn load_official_claude_client_metadata() -> OAuthClientMetadata {
    let path = match jcode_base::storage::user_home_path(".claude.json") {
        Ok(path) => path,
        Err(_) => return OAuthClientMetadata::default(),
    };
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return OAuthClientMetadata::default(),
    };
    let parsed: Value = match serde_json::from_str(&content) {
        Ok(parsed) => parsed,
        Err(_) => return OAuthClientMetadata::default(),
    };
    let oauth = parsed.get("oauthAccount");
    OAuthClientMetadata {
        device_id: parsed
            .get("userID")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        account_uuid: oauth
            .and_then(|v| v.get("accountUuid"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        organization_uuid: oauth
            .and_then(|v| v.get("organizationUuid"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        email_address: oauth
            .and_then(|v| v.get("emailAddress"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }
}

pub(super) fn oauth_request_metadata(session_id: &str) -> ApiMetadata {
    let official = load_official_claude_client_metadata();
    let device_id = official.device_id.unwrap_or_else(|| {
        Uuid::new_v5(&Uuid::NAMESPACE_DNS, session_id.as_bytes())
            .simple()
            .to_string()
    });
    let account_uuid = official
        .account_uuid
        .unwrap_or_else(|| "unknown-account".to_string());
    let user_id = json!({
        "device_id": device_id,
        "account_uuid": account_uuid,
        "session_id": session_id,
    })
    .to_string();
    ApiMetadata { user_id }
}

#[derive(Serialize)]
struct OAuthEvalRequest {
    attributes: OAuthEvalAttributes,
    #[serde(rename = "forcedVariations")]
    forced_variations: std::collections::BTreeMap<String, Value>,
    #[serde(rename = "forcedFeatures")]
    forced_features: Vec<String>,
    url: String,
}

#[derive(Serialize)]
struct OAuthEvalAttributes {
    id: String,
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "deviceID")]
    device_id: String,
    platform: String,
    #[serde(rename = "organizationUUID")]
    organization_uuid: String,
    #[serde(rename = "accountUUID")]
    account_uuid: String,
    #[serde(rename = "userType")]
    user_type: String,
    #[serde(rename = "subscriptionType")]
    subscription_type: String,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: String,
    #[serde(rename = "firstTokenTime")]
    first_token_time: i64,
    email: String,
    #[serde(rename = "appVersion")]
    app_version: String,
}

async fn oauth_preflight_get(
    client: &Client,
    headers: &reqwest::header::HeaderMap,
    label: &str,
    url: &str,
) -> Result<()> {
    let resp = client
        .get(url)
        .headers(headers.clone())
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = jcode_base::util::http_error_body(resp, "HTTP error").await;
        anyhow::bail!("{} returned {}: {}", label, status, body);
    }

    Ok(())
}

async fn oauth_preflight_post_json<T: Serialize + ?Sized>(
    client: &Client,
    headers: &reqwest::header::HeaderMap,
    label: &str,
    url: &str,
    body: &T,
) -> Result<()> {
    let resp = client
        .post(url)
        .headers(headers.clone())
        .timeout(std::time::Duration::from_secs(5))
        .json(body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = jcode_base::util::http_error_body(resp, "HTTP error").await;
        anyhow::bail!("{} returned {}: {}", label, status, body);
    }

    Ok(())
}

fn record_oauth_preflight_result(label: &str, result: Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(err) => {
            jcode_base::logging::warn(&format!(
                "Claude OAuth preflight {} failed; continuing because Claude Code treats this bootstrap traffic as nonessential: {:#}",
                label, err
            ));
            false
        }
    }
}

pub(super) async fn ensure_oauth_preflight(
    client: &Client,
    token: &str,
    session_id: &str,
    done_flag: &AtomicBool,
) -> Result<()> {
    if done_flag.load(Ordering::Relaxed) {
        return Ok(());
    }

    let official = load_official_claude_client_metadata();
    let Some(device_id) = official.device_id else {
        jcode_base::logging::warn(
            "Skipping Claude OAuth preflight: missing userID in ~/.claude.json",
        );
        return Ok(());
    };
    let Some(account_uuid) = official.account_uuid else {
        jcode_base::logging::warn(
            "Skipping Claude OAuth preflight: missing accountUuid in ~/.claude.json",
        );
        return Ok(());
    };
    let Some(organization_uuid) = official.organization_uuid else {
        jcode_base::logging::warn(
            "Skipping Claude OAuth preflight: missing organizationUuid in ~/.claude.json",
        );
        return Ok(());
    };
    let Some(email_address) = official.email_address else {
        jcode_base::logging::warn(
            "Skipping Claude OAuth preflight: missing emailAddress in ~/.claude.json",
        );
        return Ok(());
    };

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))?,
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(CLAUDE_CLI_USER_AGENT),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::HeaderName::from_static("anthropic-beta"),
        reqwest::header::HeaderValue::from_static("oauth-2025-04-20"),
    );

    let mut all_ok = true;
    all_ok &= record_oauth_preflight_result(
        "bootstrap",
        oauth_preflight_get(
            client,
            &headers,
            "bootstrap",
            "https://api.anthropic.com/api/claude_cli/bootstrap",
        )
        .await,
    );
    all_ok &= record_oauth_preflight_result(
        "account settings",
        oauth_preflight_get(
            client,
            &headers,
            "account settings",
            "https://api.anthropic.com/api/oauth/account/settings",
        )
        .await,
    );
    all_ok &= record_oauth_preflight_result(
        "grove",
        oauth_preflight_get(
            client,
            &headers,
            "grove",
            "https://api.anthropic.com/api/claude_code_grove",
        )
        .await,
    );

    let eval = OAuthEvalRequest {
        attributes: OAuthEvalAttributes {
            id: device_id.clone(),
            session_id: session_id.to_string(),
            device_id: device_id.clone(),
            platform: std::env::consts::OS.to_string(),
            organization_uuid,
            account_uuid,
            user_type: "external".to_string(),
            subscription_type: jcode_base::auth::claude::get_subscription_type()
                .unwrap_or_else(|| "pro".to_string()),
            rate_limit_tier: "default_claude_ai".to_string(),
            first_token_time: 1_740_976_801_491,
            email: email_address,
            app_version: "2.1.123".to_string(),
        },
        forced_variations: Default::default(),
        forced_features: Vec::new(),
        url: String::new(),
    };

    all_ok &= record_oauth_preflight_result(
        "eval",
        oauth_preflight_post_json(
            client,
            &headers,
            "eval",
            "https://api.anthropic.com/api/eval/sdk-zAZezfDKGoZuXXKe",
            &eval,
        )
        .await,
    );

    done_flag.store(true, Ordering::Relaxed);
    if all_ok {
        jcode_base::logging::info("Claude OAuth preflight completed successfully");
    }
    Ok(())
}
