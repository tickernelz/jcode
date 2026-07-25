use super::*;

pub(super) fn should_refresh_token(status: StatusCode, body: &str) -> bool {
    if status == StatusCode::UNAUTHORIZED {
        return true;
    }
    if status == StatusCode::FORBIDDEN {
        let lower = body.to_lowercase();
        return lower.contains("token")
            || lower.contains("expired")
            || lower.contains("unauthorized");
    }
    false
}

pub(super) fn maybe_record_runtime_model_unavailable_from_stream_error(model: &str, message: &str) {
    let reason = classify_unavailable_model_error(StatusCode::BAD_REQUEST, message)
        .or_else(|| classify_unavailable_model_error(StatusCode::FORBIDDEN, message));

    if let Some(reason) = reason {
        jcode_base::provider::record_model_unavailable_for_account(model, &reason);
        jcode_base::logging::warn(&format!(
            "Recorded OpenAI model '{}' as unavailable from stream error: {}",
            model, reason
        ));
    }
}

pub(super) fn classify_unavailable_model_error(status: StatusCode, body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();

    let mentions_model = lower.contains("model")
        || lower.contains("slug")
        || lower.contains("engine")
        || lower.contains("deployment");
    let unavailable = lower.contains("not available")
        || lower.contains("unavailable")
        || lower.contains("does not have access")
        || lower.contains("not enabled")
        || lower.contains("not found")
        || lower.contains("unknown model")
        || lower.contains("unsupported model")
        || lower.contains("invalid model");

    if !mentions_model || !unavailable {
        return None;
    }

    if status == StatusCode::NOT_FOUND
        || status == StatusCode::FORBIDDEN
        || status == StatusCode::BAD_REQUEST
        || status == StatusCode::UNPROCESSABLE_ENTITY
    {
        let trimmed = body.trim();
        let reason = if trimmed.is_empty() {
            format!("model denied by OpenAI API (status {})", status)
        } else {
            format!(
                "model denied by OpenAI API (status {}): {}",
                status, trimmed
            )
        };
        return Some(reason);
    }

    None
}

/// Check if an error is transient and should be retried
pub(super) fn is_retryable_error(error_str: &str) -> bool {
    // Shared transport-layer classifier used by every other provider. This
    // covers transient TLS/network faults (connection reset/closed/refused/
    // aborted, broken pipe, timeouts, unexpected EOF, error decoding/reading,
    // TLS BadRecordMac / fatal-alert, TLS handshake EOF, DNS/route failures,
    // and HTTP/2 stream/protocol faults). Keeping the OpenAI path delegated
    // here ensures retry behavior is unified across providers (issue #338).
    jcode_provider_core::is_transient_transport_error(error_str)
        // OpenAI-specific transport wrapper.
        || error_str.contains("failed to send request to openai api")
        // Stream/decode errors specific to the OpenAI streaming runtime.
        || error_str.contains("incomplete message")
        || error_str.contains("stream disconnected before completion")
        || error_str.contains("ended before message completion marker")
        || error_str.contains("falling back from websockets to https transport")
        // Server errors (5xx)
        || error_str.contains("500 internal server error")
        || error_str.contains("502 bad gateway")
        || error_str.contains("503 service unavailable")
        || error_str.contains("504 gateway timeout")
        || error_str.contains("overloaded")
        // Rate limiting (429): transient, recovers on retry. Unified with the
        // other providers (Anthropic/Copilot) which already retry these.
        || error_str.contains("429 too many requests")
        || error_str.contains("rate limit")
        || error_str.contains("rate_limit")
        // API-level server errors
        || error_str.contains("api_error")
        || error_str.contains("server_error")
        || error_str.contains("internal server error")
        || error_str.contains("an error occurred while processing your request")
        || error_str.contains("please include the request id")
        // Auth: we just force-refreshed the OpenAI token in place and want the
        // retry loop to reconnect with the fresh credentials.
        || error_str.contains("openai token refreshed, retrying")
}

#[cfg(test)]
mod stream_runtime_tests {
    use super::*;

    #[test]
    fn unauthorized_triggers_token_refresh() {
        assert!(should_refresh_token(StatusCode::UNAUTHORIZED, ""));
    }

    #[test]
    fn forbidden_triggers_refresh_only_for_token_bodies() {
        assert!(should_refresh_token(
            StatusCode::FORBIDDEN,
            "access token expired"
        ));
        assert!(!should_refresh_token(
            StatusCode::FORBIDDEN,
            "region not allowed"
        ));
    }

    #[test]
    fn refreshed_token_marker_is_retryable() {
        // After a 401/403 we force-refresh the OpenAI token and surface this
        // marker so the retry loop reconnects with the new credentials.
        assert!(is_retryable_error(
            "openai token refreshed, retrying: 401 unauthorized"
        ));
    }

    #[test]
    fn missing_or_failed_refresh_is_not_retryable() {
        assert!(!is_retryable_error(
            "openai rejected the access token and no refresh token is available; run /login to re-authenticate: 401"
        ));
        assert!(!is_retryable_error(
            "openai token refresh failed; run /login to re-authenticate: network error"
        ));
    }

    #[test]
    fn tls_transient_errors_are_retryable() {
        // Regression for issue #338: transient TLS faults must be retried on
        // the OpenAI path, matching every other provider. Callers pass the
        // error string already lowercased.
        assert!(is_retryable_error(
            "stream error: io error: received fatal alert: badrecordmac"
        ));
        assert!(is_retryable_error("received fatal alert: badrecordmac"));
        assert!(is_retryable_error("decryption failed or bad record mac"));
        assert!(is_retryable_error("tls handshake eof"));
        assert!(is_retryable_error("connection aborted"));
        assert!(is_retryable_error("temporary failure in name resolution"));
        assert!(is_retryable_error("no route to host"));
        assert!(is_retryable_error("network is unreachable"));
        // A send-level cause that callers now surface via the full anyhow
        // chain ({:#}) instead of the masked top-level context alone.
        assert!(is_retryable_error(
            "failed to send request to openai api: error sending request: received fatal alert: badrecordmac"
        ));
    }

    #[test]
    fn rate_limit_is_retryable() {
        // Regression for issue #338 (gap #2): 429s should be retried, unifying
        // behavior with Anthropic/Copilot.
        assert!(is_retryable_error("429 too many requests"));
        assert!(is_retryable_error("rate limit exceeded"));
        assert!(is_retryable_error("rate_limit_exceeded"));
    }

    #[test]
    fn auth_errors_remain_non_retryable() {
        assert!(!is_retryable_error("401 unauthorized"));
        assert!(!is_retryable_error("invalid api key"));
    }
}
