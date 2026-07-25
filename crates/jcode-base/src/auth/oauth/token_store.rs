use super::{OAuthTokens, claude, claude_auth, fetch_claude_profile_email_at_url};
use anyhow::Result;

/// Save Claude tokens for a specific stored account label.
pub(crate) fn save_claude_tokens_for_account(tokens: &OAuthTokens, label: &str) -> Result<()> {
    claude_auth::upsert_account_tokens(
        label,
        &tokens.access_token,
        &tokens.refresh_token,
        tokens.expires_at,
        &tokens.scopes,
    )?;
    Ok(())
}

pub async fn replace_claude_tokens_for_account(tokens: OAuthTokens, label: String) -> Result<()> {
    crate::auth::refresh_coordinator::replace_account_credentials(
        format!("claude:{label}"),
        move || save_claude_tokens_for_account(&tokens, &label),
    )
    .await
}

/// Replace a Claude login and its identity-derived profile in one per-account
/// epoch. A failed profile fetch still stores the new tokens, but clears stale
/// metadata that may belong to a different human using the same label.
pub async fn replace_claude_tokens_and_profile(
    tokens: OAuthTokens,
    label: String,
) -> Result<(Option<String>, Option<String>)> {
    crate::auth::refresh_coordinator::replace_account_credentials_async(
        format!("claude:{label}"),
        move || async move {
            let profile =
                fetch_claude_profile_email_at_url(&tokens.access_token, claude::PROFILE_URL).await;
            let (email, profile_error) = match profile {
                Ok(email) => (email, None),
                Err(error) => (None, Some(error.to_string())),
            };
            claude_auth::replace_account_tokens_and_profile(
                &label,
                &tokens.access_token,
                &tokens.refresh_token,
                tokens.expires_at,
                &tokens.scopes,
                email.clone(),
            )?;
            Ok((email, profile_error))
        },
    )
    .await
}

pub(super) async fn persist_claude_account_profile_if_current(
    label: &str,
    expected_access_token: &str,
    email: Option<String>,
) -> Result<()> {
    let label = label.to_string();
    let expected_access_token = expected_access_token.to_string();
    crate::auth::refresh_coordinator::replace_account_credentials(
        format!("claude:{label}"),
        move || {
            let current = claude_auth::list_accounts()?
                .into_iter()
                .find(|account| account.label == label)
                .ok_or_else(|| anyhow::anyhow!("Claude account '{label}' no longer exists"))?;
            if current.access != expected_access_token {
                anyhow::bail!(
                    "Claude account '{label}' changed while profile metadata was being fetched"
                );
            }
            claude_auth::update_account_profile(&label, email)
        },
    )
    .await
}

/// Save OpenAI tokens to auth file.
#[cfg(test)]
pub(crate) fn save_openai_tokens(tokens: &OAuthTokens) -> Result<()> {
    let label = crate::auth::codex::login_target_label(None)?;
    save_openai_tokens_for_account(tokens, &label)
}

/// Save OpenAI tokens for a specific stored account label.
pub(crate) fn save_openai_tokens_for_account(tokens: &OAuthTokens, label: &str) -> Result<()> {
    crate::auth::codex::upsert_account_from_tokens(
        label,
        &tokens.access_token,
        &tokens.refresh_token,
        tokens.id_token.clone(),
        Some(tokens.expires_at),
    )?;
    Ok(())
}

pub async fn replace_openai_tokens_for_account(tokens: OAuthTokens, label: String) -> Result<()> {
    crate::auth::refresh_coordinator::replace_account_credentials(
        format!("openai:{label}"),
        move || {
            crate::auth::codex::replace_account_from_tokens(
                &label,
                &tokens.access_token,
                &tokens.refresh_token,
                tokens.id_token,
                Some(tokens.expires_at),
            )?;
            Ok(())
        },
    )
    .await
}
