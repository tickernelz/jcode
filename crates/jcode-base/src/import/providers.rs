use super::*;
pub(super) fn find_codex_session_file(session_id: &str) -> Result<PathBuf> {
    let root = crate::storage::user_home_path(".codex/sessions")?;
    for path in collect_files_recursive(&root, "jsonl") {
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let mut lines = BufReader::new(file).lines();
        let Some(Ok(first_line)) = lines.next() else {
            continue;
        };
        let Ok(header) = serde_json::from_str::<serde_json::Value>(&first_line) else {
            continue;
        };
        let meta = if header.get("type").and_then(|v| v.as_str()) == Some("session_meta") {
            header.get("payload").unwrap_or(&header)
        } else {
            &header
        };
        if meta.get("id").and_then(|v| v.as_str()) == Some(session_id) {
            return Ok(path);
        }
    }
    anyhow::bail!("Codex session {} not found", session_id)
}

pub fn import_codex_session(session_id: &str) -> Result<Session> {
    let path = find_codex_session_file(session_id)?;
    import_codex_session_from_path(&path, Some(session_id))
}

pub fn import_codex_session_from_path(
    path: &Path,
    session_id_hint: Option<&str>,
) -> Result<Session> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let Some(first_line) = lines.next() else {
        anyhow::bail!("Codex session file is empty: {}", path.display())
    };
    let header: serde_json::Value = serde_json::from_str(&first_line?)?;
    let meta = if header.get("type").and_then(|v| v.as_str()) == Some("session_meta") {
        header.get("payload").unwrap_or(&header)
    } else {
        &header
    };

    let session_id = meta
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
        .or(session_id_hint)
        .ok_or_else(|| anyhow::anyhow!("Codex session id missing in {}", path.display()))?;

    let created_at = parse_rfc3339_json(meta.get("timestamp"))
        .or_else(|| parse_rfc3339_json(header.get("timestamp")))
        .unwrap_or_else(Utc::now);
    let mut updated_at = Some(created_at);
    let mut title: Option<String> = None;
    let mut working_dir = meta
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut model: Option<String> = None;
    let mut session = Session::create_with_id(imported_codex_session_id(session_id), None, None);
    session.provider_session_id = Some(session_id.to_string());
    session.provider_key = Some("openai-codex".to_string());

    for line in lines {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Codex rollouts are dominated by reasoning, world-state, tool-output,
        // and telemetry records. Only message records carry a user/assistant
        // role, so reject the rest before serde allocates their often-large JSON.
        if !json_line_has_message_role(trimmed) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let line_type = value
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let (role, content_value, timestamp_value, model_value) = if line_type == "message" {
            let Some(role) = value.get("role").and_then(|v| v.as_str()) else {
                continue;
            };
            (
                role,
                value.get("content").unwrap_or(&serde_json::Value::Null),
                value.get("timestamp"),
                value.get("model"),
            )
        } else if line_type == "response_item" {
            let Some(payload) = value.get("payload") else {
                continue;
            };
            if payload.get("type").and_then(|v| v.as_str()) != Some("message") {
                continue;
            }
            let Some(role) = payload.get("role").and_then(|v| v.as_str()) else {
                continue;
            };
            (
                role,
                payload.get("content").unwrap_or(&serde_json::Value::Null),
                value.get("timestamp").or_else(|| payload.get("timestamp")),
                payload.get("model"),
            )
        } else {
            continue;
        };

        let role = match role {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            _ => continue,
        };
        let text = extract_text_from_json_value(content_value);
        if title.is_none() && role == Role::User {
            title = codex_title_candidate(&text);
        }
        if working_dir.is_none() {
            let cwd_text = extract_text_from_json_value(content_value);
            if let Some(cwd_line) = cwd_text.lines().find(|line| line.contains("<cwd>")) {
                let cwd = cwd_line
                    .replace("<cwd>", "")
                    .replace("</cwd>", "")
                    .trim()
                    .to_string();
                if !cwd.is_empty() {
                    working_dir = Some(cwd);
                }
            }
        }
        if model.is_none() {
            model = model_value.and_then(|v| v.as_str()).map(|s| s.to_string());
        }
        let timestamp = parse_rfc3339_json(timestamp_value);
        if timestamp.is_some() {
            updated_at = timestamp;
        }
        append_text_message(&mut session, role, text, timestamp);
    }

    session.title = title.or_else(|| Some(format!("Codex session {}", session_id)));
    session.working_dir = working_dir;
    session.model = model;
    finalize_imported_session(session, created_at, updated_at)
}

pub fn import_pi_session(session_path: &str) -> Result<Session> {
    let path = PathBuf::from(session_path);
    let file = File::open(&path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let Some(first_line) = lines.next() else {
        anyhow::bail!("Pi session file is empty: {}", path.display())
    };
    let header: serde_json::Value = serde_json::from_str(&first_line?)?;
    if header.get("type").and_then(|v| v.as_str()) != Some("session") {
        anyhow::bail!("Invalid Pi session header in {}", path.display())
    }

    let provider_session_id = header
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let created_at = parse_rfc3339_json(header.get("timestamp")).unwrap_or_else(Utc::now);
    let mut updated_at = Some(created_at);
    let mut title: Option<String> = None;
    let mut model: Option<String> = None;
    let mut provider_key: Option<String> = Some("pi".to_string());
    let mut session = Session::create_with_id(imported_pi_session_id(session_path), None, None);
    session.provider_session_id = if provider_session_id.is_empty() {
        None
    } else {
        Some(provider_session_id)
    };
    session.working_dir = header
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    for line in lines {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let timestamp = parse_rfc3339_json(value.get("timestamp"));
        if timestamp.is_some() {
            updated_at = timestamp;
        }
        match value.get("type").and_then(|v| v.as_str()) {
            Some("model_change") => {
                provider_key = value
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or(provider_key);
                model = value
                    .get("modelId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or(model);
            }
            Some("message") => {
                let Some(message) = value.get("message") else {
                    continue;
                };
                let role = match message.get("role").and_then(|v| v.as_str()) {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    _ => continue,
                };
                let text = extract_text_from_json_value(
                    message.get("content").unwrap_or(&serde_json::Value::Null),
                );
                if title.is_none() && role == Role::User && !text.trim().is_empty() {
                    title = Some(truncate_title(&text));
                }
                if model.is_none() {
                    model = message
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                append_text_message(&mut session, role, text, timestamp);
            }
            _ => {}
        }
    }

    session.title = title.or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| format!("Pi session {}", stem))
    });
    session.provider_key = provider_key;
    session.model = model;
    finalize_imported_session(session, created_at, updated_at)
}

pub(super) fn find_opencode_session_file(session_id: &str) -> Result<PathBuf> {
    let root = crate::storage::user_home_path(".local/share/opencode/storage/session")?;
    for path in collect_files_recursive(&root, "json") {
        let Ok(value) = serde_json::from_reader::<_, serde_json::Value>(File::open(&path)?) else {
            continue;
        };
        if value.get("id").and_then(|v| v.as_str()) == Some(session_id) {
            return Ok(path);
        }
    }
    anyhow::bail!("OpenCode session {} not found", session_id)
}

pub fn import_opencode_session(session_id: &str) -> Result<Session> {
    let session_path = find_opencode_session_file(session_id)?;
    import_opencode_session_from_path(&session_path, Some(session_id))
}

pub fn import_opencode_session_from_path(
    session_path: &Path,
    session_id_hint: Option<&str>,
) -> Result<Session> {
    let value: serde_json::Value = serde_json::from_reader(File::open(session_path)?)?;
    let session_id = value
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
        .or(session_id_hint)
        .ok_or_else(|| {
            anyhow::anyhow!("OpenCode session id missing in {}", session_path.display())
        })?;
    let created_at = value
        .get("time")
        .and_then(|time| time.get("created"))
        .and_then(|v| v.as_i64())
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .unwrap_or_else(Utc::now);
    let mut updated_at = value
        .get("time")
        .and_then(|time| time.get("updated"))
        .and_then(|v| v.as_i64())
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or(Some(created_at));
    let mut session = Session::create_with_id(imported_opencode_session_id(session_id), None, None);
    session.provider_session_id = Some(session_id.to_string());
    session.provider_key = Some("opencode".to_string());
    session.working_dir = value
        .get("directory")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    session.title = value
        .get("title")
        .and_then(|v| v.as_str())
        .map(truncate_title);

    let messages_root = crate::storage::user_home_path(format!(
        ".local/share/opencode/storage/message/{}",
        session_id
    ))?;
    let parts_base = crate::storage::user_home_path(".local/share/opencode/storage/part")?;
    let mut messages: Vec<(Option<DateTime<Utc>>, Role, String)> = Vec::new();
    let mut model: Option<String> = None;
    let mut provider_key = session.provider_key.clone();

    if messages_root.exists() {
        for msg_path in collect_files_recursive(&messages_root, "json") {
            let Ok(msg_value) =
                serde_json::from_reader::<_, serde_json::Value>(File::open(&msg_path)?)
            else {
                continue;
            };
            let role = match msg_value.get("role").and_then(|v| v.as_str()) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            // Modern OpenCode (Go storage) stores message body text in
            // storage/part/<messageID>/*.json; fall back to legacy inline
            // content/summary for older stores.
            let text = msg_value
                .get("id")
                .and_then(|v| v.as_str())
                .map(|id| extract_opencode_part_text(&parts_base, id, true))
                .filter(|text| !text.trim().is_empty())
                .or_else(|| {
                    msg_value
                        .get("content")
                        .map(extract_text_from_json_value)
                        .filter(|text| !text.trim().is_empty())
                })
                .or_else(|| msg_value.get("summary").map(extract_text_from_json_value))
                .unwrap_or_default();
            if model.is_none() {
                model = msg_value
                    .get("modelID")
                    .or_else(|| msg_value.get("model").and_then(|m| m.get("modelID")))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            if provider_key.as_deref() == Some("opencode") {
                provider_key = msg_value
                    .get("providerID")
                    .or_else(|| msg_value.get("model").and_then(|m| m.get("providerID")))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or(provider_key);
            }
            let timestamp = msg_value
                .get("time")
                .and_then(|time| time.get("created"))
                .and_then(|v| v.as_i64())
                .and_then(DateTime::<Utc>::from_timestamp_millis);
            if timestamp.is_some() {
                updated_at = timestamp;
            }
            messages.push((timestamp, role, text));
        }
    }

    messages.sort_by_key(|(timestamp, _, _)| *timestamp);
    for (timestamp, role, text) in messages {
        append_text_message(&mut session, role, text, timestamp);
    }

    if session.title.is_none() {
        session.title = Some(format!("OpenCode session {}", session_id));
    }
    session.provider_key = provider_key;
    session.model = model;
    finalize_imported_session(session, created_at, updated_at)
}

/// Locate a Cursor agent transcript file for the given session id.
///
/// Cursor stores transcripts at
/// `~/.cursor/projects/<project>/agent-transcripts/<session-id>/<session-id>.jsonl`,
/// so the session id is the file stem. We scan the project tree for a matching
/// stem rather than guessing the project dir.
pub(super) fn find_cursor_session_file(session_id: &str) -> Result<PathBuf> {
    let root = crate::storage::user_home_path(".cursor/projects")?;
    for path in collect_files_recursive(&root, "jsonl") {
        if cursor_session_id_from_path(&path) == session_id {
            return Ok(path);
        }
    }
    anyhow::bail!("Cursor session {} not found", session_id)
}

pub fn import_cursor_session(session_id: &str) -> Result<Session> {
    let path = find_cursor_session_file(session_id)?;
    import_cursor_session_from_path(&path, Some(session_id))
}

pub fn import_cursor_session_from_path(
    session_path: &Path,
    session_id_hint: Option<&str>,
) -> Result<Session> {
    let session_id = session_id_hint
        .map(|id| id.to_string())
        .unwrap_or_else(|| cursor_session_id_from_path(session_path));
    let created_at =
        jcode_import_core::file_modified_datetime(session_path).unwrap_or_else(Utc::now);

    let mut session = Session::create_with_id(imported_cursor_session_id(&session_id), None, None);
    session.provider_session_id = Some(session_id.clone());
    session.provider_key = Some("cursor".to_string());
    session.working_dir = cursor_cwd_from_transcript_path(session_path);

    let file = File::open(session_path)?;
    let reader = BufReader::new(file);
    let mut title: Option<String> = None;
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let role = match value
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
        {
            "user" | "human" => Role::User,
            "assistant" | "model" => Role::Assistant,
            _ => continue,
        };
        let content = value
            .get("message")
            .and_then(|message| message.get("content"))
            .or_else(|| value.get("content"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let text = extract_external_text_from_json(&content, true);
        if text.trim().is_empty() {
            continue;
        }
        if title.is_none() && role == Role::User {
            title = Some(truncate_title_text(&text, 72));
        }
        append_text_message(&mut session, role, text, None);
    }

    session.title = title.or_else(|| Some(format!("Cursor session {}", session_id)));
    finalize_imported_session(session, created_at, Some(created_at))
}
