use super::*;

pub(super) fn compile_static_regex(pattern: &str) -> Option<Regex> {
    match Regex::new(pattern) {
        Ok(regex) => Some(regex),
        Err(err) => {
            logging::error(&format!("failed to compile static message regex: {err}"));
            eprintln!("jcode: failed to compile static regex: {err}");
            None
        }
    }
}

fn compile_static_regexes(patterns: &[&str]) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|pattern| compile_static_regex(pattern))
        .collect()
}

/// Redact likely secrets from persisted tool output.
///
/// This is a best-effort safeguard for local session history files. It targets
/// high-confidence token/key patterns and common `KEY=VALUE` assignments used by
/// auth flows.
pub fn redact_secrets(text: &str) -> String {
    // Fast path to avoid regex work for most tool outputs.
    let lower = text.to_ascii_lowercase();

    if !text.contains("sk-")
        && !text.contains("ghp_")
        && !text.contains("github_pat_")
        && !text.contains("AIza")
        && !text.contains("ya29.")
        && !text.contains("xox")
        && !text.contains("AKIA")
        && !text.contains("PRIVATE KEY-----")
        && !lower.contains("api_key")
        && !lower.contains("api-key")
        && !lower.contains("password")
        && !lower.contains("client_secret")
        && !lower.contains("secret")
        && !lower.contains("cookie")
        && !lower.contains("credential")
        && !lower.contains("database_url")
        && !lower.contains("postgres://")
        && !lower.contains("postgresql://")
        && !lower.contains("mysql://")
        && !lower.contains("mongodb://")
        && !lower.contains("mongodb+srv://")
        && !lower.contains("redis://")
        && !lower.contains("token")
        && !lower.contains("bearer ")
    {
        logging::debug("secret redaction fast path skipped regex scan");
        return text.to_string();
    }

    logging::debug(&format!(
        "running secret redaction scan bytes={}",
        text.len()
    ));

    static DIRECT_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    static ASSIGNMENT_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

    let direct_patterns = DIRECT_PATTERNS.get_or_init(|| {
        compile_static_regexes(&[
            r"sk-[A-Za-z0-9_-]{12,}",
            r"sk-ant-(?:oat|ort)01-[A-Za-z0-9_-]{20,}",
            r"sk-or-v1-[A-Za-z0-9_-]{20,}",
            r"ghp_[A-Za-z0-9]{20,}",
            r"github_pat_[A-Za-z0-9_]{20,}",
            r"ya29\.[A-Za-z0-9._-]{20,}",
            r"AIza[0-9A-Za-z_-]{20,}",
            r"xox[baprs]-[A-Za-z0-9-]{10,}",
            r"AKIA[0-9A-Z]{16}",
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/-]{12,}",
            r"(?im)^\s*(?:cookie|set-cookie)\s*:\s*[^\r\n]+",
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            r#"(?i)(?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis)://[^\s\"']+"#,
        ])
    });

    let assignment_patterns = ASSIGNMENT_PATTERNS.get_or_init(|| {
        compile_static_regexes(&[
            r"(?m)^\s*(OPENROUTER_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(OPENCODE_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(OPENCODE_GO_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(ZHIPU_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(ZAI_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(302AI_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(BASETEN_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(CORTECS_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(DEEPSEEK_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(FIRMWARE_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(HF_TOKEN\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(MOONSHOT_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(NEBIUS_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(SCALEWAY_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(STACKIT_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(GROQ_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(MISTRAL_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(PERPLEXITY_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(TOGETHER_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(DEEPINFRA_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(XAI_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(LMSTUDIO_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(OLLAMA_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(CHUTES_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(CEREBRAS_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(OPENAI_COMPAT_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(ANTHROPIC_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(OPENAI_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(AZURE_OPENAI_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(CURSOR_API_KEY\s*=\s*)[^\r\n]+",
            r"(?m)^\s*(GITHUB_TOKEN\s*=\s*)[^\r\n]+",
            r#"(?i)([\"']?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|auth[_-]?token|password|client[_-]?secret|aws[_-]?(?:secret[_-]?access[_-]?key|session[_-]?token)|session[_-]?cookie|cookie|database[_-]?url|credential)[\"']?\s*[:=]\s*[\"']?)[^\"'\s,}\r\n]+"#,
        ])
    });

    let mut redacted = text.to_string();
    let mut redacted_keys: HashSet<String> = [
        "OPENROUTER_API_KEY",
        "OPENCODE_API_KEY",
        "OPENCODE_GO_API_KEY",
        "ZHIPU_API_KEY",
        "ZAI_API_KEY",
        "302AI_API_KEY",
        "BASETEN_API_KEY",
        "CORTECS_API_KEY",
        "DEEPSEEK_API_KEY",
        "FIRMWARE_API_KEY",
        "HF_TOKEN",
        "MOONSHOT_API_KEY",
        "NEBIUS_API_KEY",
        "SCALEWAY_API_KEY",
        "STACKIT_API_KEY",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "PERPLEXITY_API_KEY",
        "TOGETHER_API_KEY",
        "DEEPINFRA_API_KEY",
        "XAI_API_KEY",
        "LMSTUDIO_API_KEY",
        "OLLAMA_API_KEY",
        "CHUTES_API_KEY",
        "CEREBRAS_API_KEY",
        "OPENAI_COMPAT_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "AZURE_OPENAI_API_KEY",
        "CURSOR_API_KEY",
        "GITHUB_TOKEN",
    ]
    .iter()
    .map(|k| (*k).to_string())
    .collect();

    for re in direct_patterns {
        redacted = re.replace_all(&redacted, "[REDACTED_SECRET]").into_owned();
    }

    for re in assignment_patterns {
        redacted = re
            .replace_all(&redacted, "${1}[REDACTED_SECRET]")
            .into_owned();
    }

    // Also redact custom API key variable names configured at runtime.
    for source in [
        "JCODE_OPENROUTER_API_KEY_NAME",
        "JCODE_OPENAI_COMPAT_API_KEY_NAME",
    ] {
        let Some(key_name) = std::env::var(source)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        else {
            continue;
        };

        if !key_name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            logging::warn(&format!(
                "ignoring invalid custom secret key name from {source}"
            ));
            continue;
        }
        if !redacted_keys.insert(key_name.clone()) {
            continue;
        }

        let pattern = format!(r"(?m)^\s*({}\s*=\s*)[^\r\n]+", regex::escape(&key_name));
        if let Ok(re) = Regex::new(&pattern) {
            logging::debug(&format!(
                "adding custom secret redaction pattern for key={key_name}"
            ));
            redacted = re
                .replace_all(&redacted, "${1}[REDACTED_SECRET]")
                .into_owned();
        }
    }

    if redacted != text {
        logging::info("redacted secrets from message text");
    }

    redacted
}

pub const UNCERTAIN_SECRET_OMISSION: &str = "[LCM sensitive source omitted]";

fn line_has_sensitive_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    const SENSITIVE_MARKERS: [&str; 33] = [
        "-----begin ",
        "api key",
        "api-key",
        "api_key",
        "apikey",
        "access token",
        "access_token",
        "auth token",
        "auth_token",
        "authorization",
        "bearer ",
        "client_secret",
        "cookie",
        "credential",
        "database_url",
        "mongodb://",
        "mysql://",
        "passphrase",
        "password",
        "postgres://",
        "postgresql://",
        "private key",
        "private_key",
        "refresh token",
        "refresh_token",
        "secret",
        "session key",
        "session token",
        "session_token",
        "ssh-rsa ",
        "token =",
        "token:",
        "token=",
    ];
    SENSITIVE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn token_may_be_uncertain_secret(word: &str) -> bool {
    let token = word
        .trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
            )
        })
        .trim_end_matches(['.', ':', '!', '?']);
    if token.len() < 20 {
        return false;
    }
    let has_lower = token.bytes().any(|byte| byte.is_ascii_lowercase());
    let has_upper = token.bytes().any(|byte| byte.is_ascii_uppercase());
    let has_digit = token.bytes().any(|byte| byte.is_ascii_digit());
    let has_symbol = token.bytes().any(|byte| !byte.is_ascii_alphanumeric());
    let class_count = [has_lower, has_upper, has_digit, has_symbol]
        .into_iter()
        .filter(|present| *present)
        .count();
    let opaque_alphabet = token.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'/' | b'=')
    });
    let long_opaque = token.len() >= 32 && opaque_alphabet;
    class_count >= 3 || (has_upper && has_digit && !has_lower) || long_opaque
}

fn redact_long_repeated_runs(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut output = String::with_capacity(line.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !bytes[cursor].is_ascii() {
            let Some(character) = line[cursor..].chars().next() else {
                output.push_str(UNCERTAIN_SECRET_OMISSION);
                break;
            };
            output.push(character);
            cursor += character.len_utf8();
            continue;
        }
        let mut end = cursor + 1;
        while end < bytes.len() && bytes[end] == bytes[cursor] {
            end += 1;
        }
        if end - cursor >= 32 && bytes[cursor].is_ascii_alphanumeric() {
            output.push_str(UNCERTAIN_SECRET_OMISSION);
        } else {
            output.push_str(&line[cursor..end]);
        }
        cursor = end;
    }
    output
}

fn redact_uncertain_tokens(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|segment| {
            let content = segment.trim_end_matches(char::is_whitespace);
            let whitespace = &segment[content.len()..];
            if token_may_be_uncertain_secret(content) {
                format!("{UNCERTAIN_SECRET_OMISSION}{whitespace}")
            } else {
                segment.to_string()
            }
        })
        .collect()
}

fn redact_uncertain_tokens_preserving_omissions(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut remaining = line;
    while let Some(index) = remaining.find(UNCERTAIN_SECRET_OMISSION) {
        output.push_str(&redact_uncertain_tokens(&remaining[..index]));
        output.push_str(UNCERTAIN_SECRET_OMISSION);
        remaining = &remaining[index + UNCERTAIN_SECRET_OMISSION.len()..];
    }
    output.push_str(&redact_uncertain_tokens(remaining));
    output
}

/// Redact known credentials, then fail closed on opaque secret-shaped lines.
///
/// Use this at model-facing trust boundaries where returning an unlabeled token
/// is more dangerous than omitting an ordinary mixed-class value.
pub fn redact_uncertain_secrets(text: &str) -> String {
    let trimmed = text.trim();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return redact_uncertain_json(&value).to_string();
    }
    redact_secrets(text)
        .lines()
        .map(|line| {
            if !line.contains("[REDACTED_SECRET]") && line_has_sensitive_marker(line) {
                return UNCERTAIN_SECRET_OMISSION.to_string();
            }
            let line = redact_long_repeated_runs(line);
            redact_uncertain_tokens_preserving_omissions(&line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply fail-closed secret filtering to every JSON key and string value while
/// retaining non-secret structure for safe model-facing tool retrieval.
pub fn redact_uncertain_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(redact_uncertain_secrets(text))
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(redact_uncertain_json).collect())
        }
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (redact_uncertain_secrets(key), redact_uncertain_json(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}
