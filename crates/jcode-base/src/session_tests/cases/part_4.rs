#[test]
fn test_compacted_history_window_counts_renderable_messages_not_hidden_reminders() {
    let mut session = Session::create_with_id(
        "session_render_compacted_history_hidden_budget_test".to_string(),
        None,
        Some("render compacted history hidden budget test".to_string()),
    );

    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "older visible prompt".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "<system-reminder>hidden reminder one</system-reminder>".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "previous visible assistant response".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "<system-reminder>hidden reminder two</system-reminder>".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "current prompt".to_string(),
            cache_control: None,
        }],
    );
    session.compaction = Some(StoredCompactionState {
        summary_text: "older compacted context".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 4,
        original_turn_count: 4,
        compacted_count: 4,
    });

    let (rendered, _images, info) = render_messages_and_images_with_compacted_history(&session, 1);
    let info = info.expect("compacted info");

    // Hidden system reminders are never counted as renderable messages, so the
    // small prefix (2 renderable, 1 turn) is shown in full rather than truncated.
    assert_eq!(info.total_messages, 2);
    assert_eq!(info.visible_messages, 2);
    assert_eq!(info.remaining_messages, 0);
    assert_eq!(info.hidden_user_prompts, 0);
    assert_eq!(rendered.len(), 4);
    assert!(rendered[0].content.contains("showing all 2"));
    assert_eq!(rendered[1].role, "user");
    assert_eq!(rendered[1].content, "older visible prompt");
    assert_eq!(rendered[2].role, "assistant");
    assert_eq!(rendered[2].content, "previous visible assistant response");
    assert_eq!(rendered[3].content, "current prompt");
    assert!(
        rendered
            .iter()
            .all(|msg| !msg.content.contains("hidden reminder"))
    );
}

#[test]
fn test_render_messages_and_images_share_tool_resolution_and_labels() {
    let mut session = Session::create_with_id(
        "session_render_bundle_test".to_string(),
        None,
        Some("render bundle test".to_string()),
    );

    session.add_message(
        Role::Assistant,
        vec![
            ContentBlock::ToolUse {
                id: "tool_img_1".to_string(),
                name: "view_image".to_string(),
                input: serde_json::json!({"file_path": "/tmp/screenshot.png"}),
                thought_signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: "tool_img_1".to_string(),
                content: "rendered image".to_string(),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "abcd".to_string(),
            },
            ContentBlock::Text {
                text: "[Attached image associated with the preceding tool result: screenshot.png]"
                    .to_string(),
                cache_control: None,
            },
        ],
    );

    let (rendered, images) = render_messages_and_images(&session);
    // The `[Attached image associated with the preceding tool result: ...]`
    // text block is synthetic image metadata, not a visible message. It must be
    // folded into the image label and never rendered as a (user) message,
    // otherwise it leaks out as a bogus "last prompt".
    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].role, "tool");
    assert_eq!(rendered[0].content, "rendered image");
    assert!(
        !rendered
            .iter()
            .any(|m| m.content.contains("Attached image associated")),
        "attached-image label must not render as its own message"
    );
    assert_eq!(
        rendered[0]
            .tool_data
            .as_ref()
            .map(|tool| tool.name.as_str()),
        Some("view_image")
    );

    assert_eq!(images.len(), 1);
    assert_eq!(images[0].label.as_deref(), Some("screenshot.png"));
    assert_eq!(images[0].media_type, "image/png");
    assert_eq!(
        images[0].source,
        RenderedImageSource::ToolResult {
            tool_name: "view_image".to_string(),
        }
    );
}

#[test]
fn reasoning_trace_survives_session_save_and_load() -> Result<()> {
    let _env_lock = lock_env();
    let temp_home = tempfile::Builder::new()
        .prefix("jcode-reasoning-persist-test-")
        .tempdir()
        .map_err(|e| anyhow!(e))?;
    let _home = EnvVarGuard::set("JCODE_HOME", temp_home.path().as_os_str());

    let session_id = "session_reasoning_trace_roundtrip";
    let mut session = Session::create_with_id(session_id.to_string(), None, None);
    session.append_stored_message(StoredMessage {
        id: "msg_assistant".to_string(),
        role: Role::Assistant,
        content: vec![
            ContentBlock::ReasoningTrace {
                text: "step 1: consider the run loop ordering".to_string(),
            },
            ContentBlock::Text {
                text: "Here is my answer.".to_string(),
                cache_control: None,
            },
        ],
        display_role: None,
        timestamp: Some(Utc::now()),
        tool_duration_ms: None,
        token_usage: None,
    });
    session.save()?;

    // The reasoning must be persisted to the on-disk transcript, not just held
    // in memory, so it can be recalled/debugged after a restart.
    let raw = std::fs::read_to_string(session_path(session_id)?)?;
    assert!(
        raw.contains("reasoning_trace"),
        "transcript should serialize reasoning_trace block"
    );
    assert!(raw.contains("step 1: consider the run loop ordering"));

    let loaded = Session::load(session_id)?;
    let assistant = loaded
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .ok_or_else(|| anyhow!("assistant message missing after reload"))?;
    let has_trace = assistant.content.iter().any(|b| {
        matches!(
            b,
            ContentBlock::ReasoningTrace { text }
                if text == "step 1: consider the run loop ordering"
        )
    });
    assert!(has_trace, "ReasoningTrace must survive save/load roundtrip");
    Ok(())
}

#[test]
fn test_render_images_anchors_tool_and_user_images() {
    let mut session = Session::create_with_id(
        "session_render_image_anchor_test".to_string(),
        None,
        Some("image anchor test".to_string()),
    );

    // Prompt 0 with a pasted image.
    session.add_message(
        Role::User,
        vec![
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "user-image-data".to_string(),
            },
            ContentBlock::Text {
                text: "look at this".to_string(),
                cache_control: None,
            },
        ],
    );
    // Assistant calls a tool.
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool-call-1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "shot.png"}),
            thought_signature: None,
        }],
    );
    // Tool result with an attached image.
    session.add_message(
        Role::User,
        vec![
            ContentBlock::ToolResult {
                tool_use_id: "tool-call-1".to_string(),
                content: "read image".to_string(),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "tool-image-data".to_string(),
            },
        ],
    );

    let (_, images) = render_messages_and_images(&session);
    assert_eq!(images.len(), 2);
    assert_eq!(
        images[0].anchor,
        Some(RenderedImageAnchor::UserPrompt { ordinal: 0 }),
        "pasted user image should anchor to its prompt"
    );
    assert_eq!(
        images[1].anchor,
        Some(RenderedImageAnchor::ToolCall {
            id: "tool-call-1".to_string()
        }),
        "tool image should anchor to its tool call"
    );
}

#[test]
fn test_render_images_attached_label_message_does_not_shift_prompt_ordinals() {
    let mut session = Session::create_with_id(
        "session_render_image_label_ordinal_test".to_string(),
        None,
        Some("image label ordinal test".to_string()),
    );

    // Tool flow that produces a labeled image: the synthetic label text message
    // must not count as a user prompt for anchoring.
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool-call-2".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "shot.png"}),
            thought_signature: None,
        }],
    );
    session.add_message(
        Role::User,
        vec![
            ContentBlock::ToolResult {
                tool_use_id: "tool-call-2".to_string(),
                content: "read image".to_string(),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "tool-image-data".to_string(),
            },
            ContentBlock::Text {
                text: "[Attached image associated with the preceding tool result: shot.png]"
                    .to_string(),
                cache_control: None,
            },
        ],
    );
    // A real follow-up prompt with an image: must be ordinal 0 (first prompt).
    session.add_message(
        Role::User,
        vec![
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "second-user-image".to_string(),
            },
            ContentBlock::Text {
                text: "and this one".to_string(),
                cache_control: None,
            },
        ],
    );

    let (_, images) = render_messages_and_images(&session);
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].label.as_deref(), Some("shot.png"));
    assert_eq!(
        images[1].anchor,
        Some(RenderedImageAnchor::UserPrompt { ordinal: 0 }),
        "label-only messages must not consume prompt ordinals"
    );
}

#[test]
fn fork_notice_is_model_visible_but_hidden_from_transcript() {
    let mut session = Session::create(None, None);
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "original request".to_string(),
            cache_control: None,
        }],
    );

    session.append_fork_notice("session_parent_abc", "otter");

    let notice = session.messages.last().expect("fork notice appended");
    assert_eq!(notice.role, Role::User);
    assert_eq!(notice.display_role, Some(StoredDisplayRole::System));
    let text = notice.content_preview();
    assert!(text.contains("<system-reminder>"));
    assert!(text.contains("forked"));
    assert!(text.contains("session_parent_abc"));
    assert!(text.contains("otter"));

    // Model-visible: included in the provider message list.
    let provider_messages = session.messages_for_provider_uncached();
    assert!(
        provider_messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::Text { text, .. } if text.contains("forked")
                )
            })
        }),
        "fork notice must reach the model"
    );

    // Transcript-hidden: not rendered as a visible user message.
    let (rendered, _) = render_messages_and_images(&session);
    assert!(
        !rendered
            .iter()
            .any(|message| message.role == "user" && message.content.contains("forked")),
        "fork notice must not render as a visible user message"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn streaming_guard_creates_visible_macos_sleep_assertion() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());

    let reason = "Jcode streaming model response";
    {
        let _streaming = StreamingGuard::new("session_power");

        let output = std::process::Command::new("pmset")
            .args(["-g", "assertions"])
            .output()
            .expect("pmset -g assertions should run on macOS");
        assert!(output.status.success(), "pmset should succeed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(reason),
            "pmset output should show the streaming assertion; output was:\n{stdout}"
        );
    }

    let output = std::process::Command::new("pmset")
        .args(["-g", "assertions"])
        .output()
        .expect("pmset -g assertions should run on macOS");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(reason),
        "streaming assertion should be released after guard drop; output was:\n{stdout}"
    );
}

/// Issue #432: `/rewind N` must interpret N against the same numbered list the
/// TUI shows, even in tool-heavy sessions where stored user-role tool-result
/// messages vastly outnumber real prompts.
#[test]
fn test_rewind_targets_match_rendered_transcript_numbering() {
    let mut session = Session::create_with_id(
        "session_rewind_numbering_test".to_string(),
        None,
        Some("rewind numbering".to_string()),
    );

    // Turn 1: prompt, assistant tool call, tool result, assistant answer.
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "prompt-1".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "tool_1".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
            thought_signature: None,
        }],
    );
    // Tool results are stored as user-role messages; the old index mapping
    // counted them as rewind targets even though the UI never numbers them.
    session.add_message(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "file-a file-b".to_string(),
            is_error: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "answer-1".to_string(),
            cache_control: None,
        }],
    );

    // Turn 2: prompt + answer.
    session.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "prompt-2".to_string(),
            cache_control: None,
        }],
    );
    session.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "answer-2".to_string(),
            cache_control: None,
        }],
    );

    // The numbered /rewind list shows user/assistant transcript entries only:
    // 1 prompt-1, 2 answer-1, 3 prompt-2, 4 answer-2.
    let rendered_targets: Vec<String> = render_messages(&session)
        .into_iter()
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
        .map(|m| m.content)
        .collect();
    assert_eq!(
        rendered_targets,
        ["prompt-1", "answer-1", "prompt-2", "answer-2"]
    );

    let targets = session.rewind_target_stored_indices();
    assert_eq!(session.rewind_target_count(), 4);
    assert_eq!(targets.len(), 4);

    // Rewinding to entry 3 ("prompt-2") must keep everything through the
    // stored prompt-2 message (stored index 4) and archive answer-2.
    assert_eq!(targets[2], 4);
    let mut rewound = session.clone();
    let canonical_ids = rewound
        .messages
        .iter()
        .map(|message| message.id.clone())
        .collect::<Vec<_>>();
    rewound
        .rewind_active_branch_through(targets[2])
        .expect("rewind active branch");
    assert_eq!(
        rewound
            .messages
            .iter()
            .map(|message| message.id.clone())
            .collect::<Vec<_>>(),
        canonical_ids,
        "rewind must not delete canonical raw messages"
    );
    let remaining: Vec<String> = render_messages(&rewound)
        .into_iter()
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
        .map(|m| m.content)
        .collect();
    assert_eq!(remaining, ["prompt-1", "answer-1", "prompt-2"]);

    // The old stored-message mapping counted the tool result as target 3,
    // which would have chopped the transcript mid-turn (the #432 bug).
    assert_eq!(
        session.stored_len_for_visible_conversation_message(3),
        Some(3),
        "sanity: raw stored counting diverges, which is why rewind must not use it"
    );
}

#[test]
fn rewind_persists_canonical_raw_history_and_appends_a_new_active_branch() {
    let _lock = lock_env();
    let temp = tempfile::TempDir::new().expect("temp home");
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let mut session = Session::create_with_id(
        "session_non_destructive_rewind".to_string(),
        None,
        Some("rewind durability".to_string()),
    );
    for text in ["first", "second", "archived-third"] {
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        );
    }
    session.save().expect("save canonical transcript");
    let canonical_ids = session
        .messages
        .iter()
        .map(|message| message.id.clone())
        .collect::<Vec<_>>();

    session
        .rewind_active_branch_through(1)
        .expect("archive active suffix");
    session.save().expect("persist active branch frontier");

    let mut loaded = Session::load(&session.id).expect("reload rewound session");
    assert_eq!(
        loaded
            .messages
            .iter()
            .map(|message| message.id.clone())
            .collect::<Vec<_>>(),
        canonical_ids,
        "raw canonical history must survive rewind and reload"
    );
    assert_eq!(loaded.archived_message_ids, vec![canonical_ids[2].clone()]);
    assert_eq!(loaded.messages_for_provider_uncached().len(), 2);
    assert!(loaded.messages.iter().any(|message| {
        message.content.iter().any(
            |block| matches!(block, ContentBlock::Text { text, .. } if text == "archived-third"),
        )
    }));

    loaded.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "new-branch".to_string(),
            cache_control: None,
        }],
    );
    loaded.save().expect("append new branch");
    let reloaded = Session::load(&loaded.id).expect("reload new branch");
    assert_eq!(reloaded.messages.len(), 4);
    let active_text = reloaded
        .messages_for_provider_uncached()
        .into_iter()
        .flat_map(|message| message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(active_text, ["first", "second", "new-branch"]);
}

#[test]
fn lifecycle_context_continuity_keeps_archive_frontier_and_graphless_compaction() -> Result<()> {
    let _lock = lock_env();
    let temp = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let mut parent = Session::create_with_id("continuity_parent".to_string(), None, None);
    seed_context_source(&mut parent);
    parent.add_message(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "archived lifecycle branch".to_string(),
            cache_control: None,
        }],
    );
    parent.rewind_active_branch_through(0)?;
    parent.compaction = Some(StoredCompactionState {
        summary_text: "rolling projection".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    });

    let mut child = Session::create_with_id(
        "continuity_child".to_string(),
        Some(parent.id.clone()),
        None,
    );
    child.inherit_context_continuity_from(&parent)?;
    child.save()?;
    let loaded = Session::load(&child.id)?;

    assert_eq!(
        stored_messages_sha256(&loaded.messages)?,
        stored_messages_sha256(&parent.messages)?
    );
    assert_eq!(loaded.archived_message_ids, parent.archived_message_ids);
    assert_eq!(loaded.compaction, parent.compaction);
    assert_eq!(loaded.exact_runtime_identity, parent.exact_runtime_identity);
    assert!(loaded.context_nodes.is_empty());
    assert!(loaded.context_frontier.is_none());
    assert!(
        loaded
            .messages
            .iter()
            .any(|message| { message.content_preview() == "archived lifecycle branch" })
    );
    assert!(
        loaded
            .messages_for_provider_uncached()
            .iter()
            .all(|message| {
                message.content.iter().all(|block| {
            !matches!(block, ContentBlock::Text { text, .. } if text == "archived lifecycle branch")
        })
            })
    );
    Ok(())
}

#[test]
fn invalid_archive_metadata_fails_closed_without_deleting_canonical_history() -> Result<()> {
    let _lock = lock_env();
    let temp = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let id = "session_invalid_archive_metadata";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    for text in ["canonical one", "canonical two"] {
        session.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        );
    }
    session.save()?;

    let path = session_path(id)?;
    let mut snapshot: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    snapshot["archived_message_ids"] = serde_json::json!(["nonexistent-message-id"]);
    std::fs::write(&path, serde_json::to_vec_pretty(&snapshot)?)?;

    let mut loaded = Session::load(id)?;
    assert_eq!(loaded.messages.len(), 2);
    assert_eq!(loaded.archived_message_ids.len(), 2);
    assert!(loaded.active_stored_messages().is_empty());
    loaded.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "safe new branch".to_string(),
            cache_control: None,
        }],
    );
    loaded.save()?;

    let reloaded = Session::load(id)?;
    assert_eq!(reloaded.messages.len(), 3);
    assert_eq!(reloaded.active_stored_messages().len(), 1);
    assert_eq!(
        reloaded.active_stored_messages()[0].content_preview(),
        "safe new branch"
    );
    Ok(())
}

#[test]
fn provider_image_suppression_survives_restart_without_mutating_canonical_messages() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut session =
        Session::create_with_id("provider_image_projection_restart".to_string(), None, None);
    let big = "a".repeat(8 * 1024 * 1024);
    for idx in 0..3 {
        session.add_message(
            Role::User,
            vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: format!("{idx}{big}"),
            }],
        );
    }
    session.save()?;
    let durable_revision = session.persistence_revision;
    let canonical_before = serde_json::to_vec(&session.messages)?;

    let suppressed = session
        .suppress_oversized_images_for_provider(crate::compaction::PAYLOAD_IMAGE_CHAR_BUDGET);
    assert_eq!(suppressed, 2);
    assert_eq!(
        session.suppress_oversized_images_for_provider(crate::compaction::PAYLOAD_IMAGE_CHAR_BUDGET),
        0,
        "an unchanged projection budget must not report another recovery"
    );
    assert_eq!(
        session.persistence_revision, durable_revision,
        "projection changes must not impersonate a completed durable write"
    );
    assert_eq!(serde_json::to_vec(&session.messages)?, canonical_before);
    assert!(matches!(
        session.messages[0].content[0],
        ContentBlock::Image { .. }
    ));
    let provider = session.messages_for_provider().to_vec();
    assert!(matches!(provider[0].content[0], ContentBlock::Text { .. }));
    assert!(matches!(provider[1].content[0], ContentBlock::Text { .. }));
    assert!(matches!(provider[2].content[0], ContentBlock::Image { .. }));

    session.save()?;
    let mut reloaded = Session::load("provider_image_projection_restart")?;
    assert_eq!(serde_json::to_vec(&reloaded.messages)?, canonical_before);
    let reloaded_provider = reloaded.messages_for_provider().to_vec();
    assert!(matches!(
        reloaded_provider[0].content[0],
        ContentBlock::Text { .. }
    ));
    assert!(matches!(
        reloaded_provider[2].content[0],
        ContentBlock::Image { .. }
    ));
    // Once the projected cache exists, later canonical appends must reapply the
    // global budget rather than appending unsuppressed images to that cache.
    for idx in 3..5 {
        reloaded.add_message(
            Role::User,
            vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: format!("{idx}{big}"),
            }],
        );
    }
    let appended_provider = reloaded.messages_for_provider().to_vec();
    assert_eq!(
        appended_provider
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| matches!(block, ContentBlock::Image { .. }))
            .count(),
        1
    );
    assert!(reloaded
        .messages
        .iter()
        .all(|message| matches!(message.content[0], ContentBlock::Image { .. })));
    reloaded.save()?;
    let mut remote = Session::load_for_remote_startup("provider_image_projection_restart")?;
    assert_eq!(
        remote
            .messages_for_provider()
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| matches!(block, ContentBlock::Image { .. }))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn provider_tool_use_suppression_survives_restart_and_rollback_without_mutating_canonical_messages()
-> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let mut session =
        Session::create_with_id("provider_tool_projection_restart".to_string(), None, None);
    let assistant_id = session.add_message(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: "toolu_truncated".to_string(),
            name: "bash".to_string(),
            input: serde_json::Value::Null,
            thought_signature: Some("exact-original-signature".to_string()),
        }],
    );
    session.save()?;
    let durable_revision = session.persistence_revision;
    let canonical_before = serde_json::to_vec(&session.messages)?;

    session.suppress_tool_use_blocks_for_provider(&assistant_id);
    assert_eq!(
        session.persistence_revision, durable_revision,
        "projection changes must remain pending until save succeeds"
    );
    assert_eq!(serde_json::to_vec(&session.messages)?, canonical_before);
    assert!(matches!(
        session.messages[0].content[0],
        ContentBlock::ToolUse { .. }
    ));
    assert!(session.messages_for_provider()[0].content.is_empty());
    session.save()?;

    let mut reloaded = Session::load("provider_tool_projection_restart")?;
    assert_eq!(serde_json::to_vec(&reloaded.messages)?, canonical_before);
    assert!(reloaded.messages_for_provider()[0].content.is_empty());
    let mut remote = Session::load_for_remote_startup("provider_tool_projection_restart")?;
    assert!(remote.messages_for_provider()[0].content.is_empty());
    assert_eq!(serde_json::to_vec(&remote.messages)?, canonical_before);

    reloaded.archived_message_ids.push(assistant_id.clone());
    assert!(reloaded.messages_for_provider_uncached().is_empty());
    reloaded.archived_message_ids.clear();
    assert_eq!(serde_json::to_vec(&reloaded.messages)?, canonical_before);
    assert!(
        reloaded.messages_for_provider_uncached()[0]
            .content
            .is_empty()
    );
    Ok(())
}

#[test]
fn derive_session_provider_key_prefers_runtime_identity_over_transport() {
    let _lock = lock_env();
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_PROVIDER", "azure-openai");
    let _namespace = EnvVarGuard::set("JCODE_OPENROUTER_CACHE_NAMESPACE", "azure-cache");
    let _active = EnvVarGuard::set("JCODE_ACTIVE_PROVIDER", "openrouter");

    assert_eq!(
        derive_session_provider_key("openrouter").as_deref(),
        Some("azure-openai")
    );
}

#[test]
fn production_graph_commit_fails_closed_until_unconfirmed_append_is_reloaded() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = crate::id::new_id("context_graph_unconfirmed_append");
    let mut session = Session::create_with_id(id.clone(), None, None);
    seed_context_source(&mut session);
    session.save()?;

    let first = canonical_context_transaction(&session, "unconfirmed-first");
    let first_projection = StoredCompactionState {
        summary_text: "[LCM context node 1 level 0]\nsummary-unconfirmed-first".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 1,
        original_turn_count: 1,
        compacted_count: 1,
    };
    let journal_path = session_journal_path(&id)?;
    let post_append_failure = crate::storage::inject_post_append_failure(journal_path.clone());
    let confirmation_failure = crate::storage::inject_confirmation_failure(journal_path.clone());

    let err = session
        .commit_context_graph_transaction_with_compaction(first, Some(first_projection.clone()))
        .unwrap_err();
    assert!(format!("{err:#}").contains("durability could not be confirmed"));
    // A complete line may be visible, but the live caller must not publish it.
    assert!(session.context_nodes.is_empty());
    assert!(session.context_frontier.is_none());
    assert!(session.compaction.is_none());
    assert_eq!(std::fs::read_to_string(&journal_path)?.lines().count(), 1);

    // A different operation on the stale writer cannot occupy the same journal
    // sequence. The durable-base check forces reconciliation before any retry.
    let second = canonical_context_transaction(&session, "must-not-replace-first");
    let stale_err = session
        .commit_context_graph_transaction_with_compaction(second, Some(first_projection.clone()))
        .unwrap_err();
    assert!(stale_err.to_string().contains("stale session writer rejected"));
    assert_eq!(std::fs::read_to_string(&journal_path)?.lines().count(), 1);

    drop(confirmation_failure);
    drop(post_append_failure);
    // Simulated crash/reload deterministically reconciles the one visible
    // operation, including its paired provider projection.
    let loaded = Session::load(&id)?;
    assert_eq!(loaded.context_nodes.len(), 1);
    assert_eq!(loaded.last_context_op_id.as_deref(), Some("unconfirmed-first"));
    assert_eq!(loaded.compaction, Some(first_projection));
    Ok(())
}

#[test]
fn derive_session_provider_key_falls_back_to_openrouter_namespace() {
    let _lock = lock_env();
    let _runtime = EnvVarGuard::remove("JCODE_RUNTIME_PROVIDER");
    let _namespace = EnvVarGuard::set("JCODE_OPENROUTER_CACHE_NAMESPACE", "azure-openai");
    let _active = EnvVarGuard::set("JCODE_ACTIVE_PROVIDER", "openrouter");

    assert_eq!(
        derive_session_provider_key("openrouter").as_deref(),
        Some("azure-openai")
    );
}

#[test]
fn derive_session_provider_key_keeps_openai_compatible_profile_namespace() {
    let _lock = lock_env();
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_PROVIDER", "openai-compatible");
    let _namespace = EnvVarGuard::set("JCODE_OPENROUTER_CACHE_NAMESPACE", "zai");
    let _active = EnvVarGuard::set("JCODE_ACTIVE_PROVIDER", "openrouter");

    assert_eq!(
        derive_session_provider_key("openrouter").as_deref(),
        Some("zai")
    );
}

#[test]
fn context_graph_invalid_snapshot_falls_back_to_legacy_state() -> Result<()> {
    let _env_lock = lock_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set("JCODE_HOME", home.path().as_os_str());
    let id = "context_graph_invalid_snapshot";
    let mut session = Session::create_with_id(id.to_string(), None, None);
    let transaction = context_transaction(id, "invalid-snapshot");
    session.context_nodes = vec![
        transaction.append_context_nodes[0].clone(),
        transaction.append_context_nodes[0].clone(),
    ];
    session.context_frontier = Some(transaction.frontier);
    std::fs::create_dir_all(session_path(id)?.parent().unwrap())?;
    std::fs::write(session_path(id)?, serde_json::to_vec(&session)?)?;

    let loaded = Session::load(id)?;
    assert!(loaded.context_nodes.is_empty());
    assert!(loaded.context_frontier.is_none());
    assert!(loaded.compaction.is_none());
    Ok(())
}

#[test]
fn test_session_exists_roundtrip() -> Result<()> {
    let tmp_dir = std::env::temp_dir().join(format!(
        "jcode-session-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow!(e))?
            .as_nanos()
    ));
    std::fs::create_dir_all(tmp_dir.join("sessions"))?;

    assert!(!session_path_in_dir(&tmp_dir, "missing-session").exists());

    let session_path = session_path_in_dir(&tmp_dir, "exists-session");
    std::fs::write(&session_path, "{}")?;
    assert!(session_path.exists());

    let random_id = format!(
        "missing-session-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| anyhow!(e))?
            .as_nanos()
    );
    assert!(!session_exists(&random_id));
    Ok(())
}
#[test]
fn completed_transition_watermark_does_not_roll_back_newer_token_generation() {
    let _lock = lock_env();
    let temp = tempfile::TempDir::new().expect("temp home");
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let mut identity = test_runtime_identity();
    identity.account_generation = Some(2);
    let mut session = Session::create(None, None);
    session.exact_runtime_identity = Some(identity.clone());
    session.provider_session_id = Some("generation-two-resume".to_string());
    session.provider_session_identity = Some(identity.clone());
    session.save().expect("save refreshed identity");

    crate::session::record_completed_account_transition(
        identity.route.runtime_key.clone(),
        identity.account_label.as_deref().unwrap(),
        identity.account_id.as_deref().unwrap(),
        1,
    )
    .expect("record older transition watermark");

    session.save().expect("save after routine credential refresh");

    assert_eq!(
        session
            .exact_runtime_identity
            .as_ref()
            .and_then(|value| value.account_generation),
        Some(2)
    );
    assert_eq!(
        session.provider_session_id.as_deref(),
        Some("generation-two-resume")
    );
    let loaded = Session::load(&session.id).expect("load refreshed identity");
    assert_eq!(
        loaded
            .exact_runtime_identity
            .as_ref()
            .and_then(|value| value.account_generation),
        Some(2)
    );
    assert_eq!(
        loaded.provider_session_id.as_deref(),
        Some("generation-two-resume")
    );
}

