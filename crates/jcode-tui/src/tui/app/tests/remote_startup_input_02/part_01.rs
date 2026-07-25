#[test]
fn test_model_picker_copilot_selection_prefixes_model() {
    let mut app = create_test_app();
    configure_test_remote_models_with_copilot(&mut app);

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");

    // Find grok-code-fast-1 (which should only be a copilot route)
    let grok_idx = picker
        .entries
        .iter()
        .position(|m| m.name == "grok-code-fast-1")
        .expect("grok-code-fast-1 should be in picker");

    // Navigate to it and select
    let filtered_pos = picker
        .filtered
        .iter()
        .position(|&i| i == grok_idx)
        .expect("grok-code-fast-1 should be in filtered list");

    // Set the selected position to grok's position
    app.inline_interactive_state.as_mut().unwrap().selected = filtered_pos;

    // Press Enter to select
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    // In remote mode, selection should produce a pending_model_switch with copilot: prefix
    if let Some(ref spec) = app.pending_model_switch {
        assert!(
            spec.starts_with("copilot:"),
            "copilot model should be prefixed with 'copilot:', got: {}",
            spec
        );
    }
    // Picker should be closed
    assert!(app.inline_interactive_state.is_none());
}

#[test]
fn test_model_picker_cursor_models_have_cursor_route() {
    let mut app = create_test_app();
    configure_test_remote_models_with_cursor(&mut app);

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");

    let composer_entry = picker
        .entries
        .iter()
        .find(|m| m.name == "composer-2-fast")
        .expect("composer-2-fast should be in picker");

    assert!(
        composer_entry
            .options
            .iter()
            .any(|r| r.api_method == "cursor"),
        "composer-2-fast should have a cursor route, got: {:?}",
        composer_entry.options
    );
}

#[test]
fn test_model_picker_cursor_selection_prefixes_model() {
    let mut app = create_test_app();
    configure_test_remote_models_with_cursor(&mut app);

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");

    let composer_idx = picker
        .entries
        .iter()
        .position(|m| m.name == "composer-2-fast")
        .expect("composer-2-fast should be in picker");

    let filtered_pos = picker
        .filtered
        .iter()
        .position(|&i| i == composer_idx)
        .expect("composer-2-fast should be in filtered list");

    app.inline_interactive_state.as_mut().unwrap().selected = filtered_pos;

    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    assert_eq!(
        app.pending_model_switch.as_deref(),
        Some("cursor:composer-2-fast")
    );
    assert!(app.inline_interactive_state.is_none());
}

#[test]
fn test_model_picker_bedrock_selection_prefixes_model() {
    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_available_entries = vec!["amazon.nova-pro-v1:0".to_string()];
    app.remote_model_options = vec![crate::provider::ModelRoute {
        model: "amazon.nova-pro-v1:0".to_string(),
        provider: "AWS Bedrock".to_string(),
        api_method: "bedrock".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
    }];

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");
    let model_idx = picker
        .entries
        .iter()
        .position(|m| m.name == "amazon.nova-pro-v1:0")
        .expect("Bedrock model should be in picker");
    let filtered_pos = picker
        .filtered
        .iter()
        .position(|&i| i == model_idx)
        .expect("Bedrock model should be in filtered list");

    app.inline_interactive_state.as_mut().unwrap().selected = filtered_pos;
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    assert_eq!(
        app.pending_model_switch.as_deref(),
        Some("bedrock:amazon.nova-pro-v1:0")
    );
    assert!(app.inline_interactive_state.is_none());
}

#[test]
fn test_model_picker_bedrock_arn_selection_prefixes_model() {
    let mut app = create_test_app();
    app.is_remote = true;
    let model = "arn:aws:bedrock:us-east-2:302154194530:inference-profile/us.deepseek.r1-v1:0";
    app.remote_available_entries = vec![model.to_string()];
    app.remote_model_options = vec![crate::provider::ModelRoute {
        model: model.to_string(),
        provider: "AWS Bedrock".to_string(),
        api_method: "bedrock".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
    }];

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");
    let model_idx = picker
        .entries
        .iter()
        .position(|m| m.name == model)
        .expect("Bedrock ARN should be in picker");
    let filtered_pos = picker
        .filtered
        .iter()
        .position(|&i| i == model_idx)
        .expect("Bedrock ARN should be in filtered list");

    app.inline_interactive_state.as_mut().unwrap().selected = filtered_pos;
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    let expected = format!("bedrock:{model}");
    assert_eq!(app.pending_model_switch.as_deref(), Some(expected.as_str()));
    assert!(app.inline_interactive_state.is_none());
}

#[test]
fn test_remote_fallback_bedrock_arn_does_not_create_openrouter_route() {
    let mut app = create_test_app();
    app.is_remote = true;
    let model = "arn:aws:bedrock:us-east-2:302154194530:inference-profile/us.deepseek.r1-v1:0";
    app.remote_available_entries = vec![model.to_string()];
    app.remote_model_options.clear();

    let routes = app.build_remote_model_routes_fallback();

    assert!(routes.iter().any(|route| {
        route.model == model && route.api_method == "bedrock" && route.provider == "AWS Bedrock"
    }));
    assert!(
        !routes
            .iter()
            .any(|route| route.model == model && route.api_method == "openrouter")
    );
}

#[test]
fn test_remote_hydrated_catalog_restores_missing_direct_bedrock_route() {
    with_temp_jcode_home(|| {
        let previous_enable = std::env::var_os("JCODE_BEDROCK_ENABLE");
        crate::env::set_var("JCODE_BEDROCK_ENABLE", "1");
        crate::auth::AuthStatus::invalidate_cache();

        let model = "amazon.nova-pro-v1:0";
        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_provider_name = Some("OpenAI".to_string());
        app.remote_available_entries = vec![model.to_string()];
        app.remote_model_options = vec![crate::provider::ModelRoute {
            model: model.to_string(),
            provider: "OpenAI".to_string(),
            api_method: "remote-catalog".to_string(),
            available: true,
            detail: "compacted route snapshot".to_string(),
            cheapness: None,
        }];

        app.open_model_picker();

        match previous_enable {
            Some(value) => crate::env::set_var("JCODE_BEDROCK_ENABLE", value),
            None => crate::env::remove_var("JCODE_BEDROCK_ENABLE"),
        }
        crate::auth::AuthStatus::invalidate_cache();

        assert!(app.remote_model_options.iter().any(|route| {
            route.model == model
                && route.provider == "AWS Bedrock"
                && route.api_method == "bedrock"
                && route.available
        }));
        assert!(app.remote_model_options.iter().any(|route| {
            route.model == model
                && route.provider == "OpenAI"
                && route.api_method == "remote-catalog"
        }));
    });
}

#[test]
fn test_remote_current_fpt_live_model_uses_fpt_route_not_copilot_without_cache() {
    with_temp_jcode_home(|| {
        crate::env::set_var("FPT_API_KEY", "test-fpt-key");

        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_provider_name = Some("FPT AI Marketplace".to_string());
        app.remote_available_entries = vec!["GLM-5.1".to_string()];
        app.remote_model_options.clear();

        let routes = app.build_remote_model_routes_fallback();

        assert!(
            routes.iter().any(|route| {
                route.model == "GLM-5.1"
                    && route.provider == "FPT AI Marketplace"
                    && route.api_method == "openai-compatible:fpt"
            }),
            "FPT current-provider live model should use FPT route, got {routes:?}"
        );
        assert!(
            !routes
                .iter()
                .any(|route| route.model == "GLM-5.1" && route.api_method == "copilot"),
            "FPT current-provider live model must not be guessed as Copilot: {routes:?}"
        );

        crate::env::remove_var("FPT_API_KEY");
    });
}

#[test]
fn test_remote_fallback_claude_model_gets_api_key_route_without_oauth() {
    // A newly released Claude model can reach the picker via the names-only
    // catalog fallback (oversized route frames are downgraded to model names).
    // With only ANTHROPIC_API_KEY configured, the fallback must synthesize a
    // claude-api route; previously it only ever emitted claude-oauth routes.
    with_temp_jcode_home(|| {
        crate::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-key");
        crate::auth::AuthStatus::invalidate_cache();

        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_available_entries = vec!["claude-fable-5".to_string()];
        app.remote_model_options.clear();

        let routes = app.build_remote_model_routes_fallback();

        assert!(
            routes.iter().any(|route| {
                route.model == "claude-fable-5"
                    && route.provider == "Anthropic"
                    && route.api_method == "claude-api"
                    && route.available
            }),
            "claude model with only an API key should get a claude-api fallback route, got {routes:?}"
        );

        crate::env::remove_var("ANTHROPIC_API_KEY");
        crate::auth::AuthStatus::invalidate_cache();
    });
}

#[test]
fn test_remote_cached_oauth_only_claude_route_gains_api_key_route_in_picker() {
    struct AnthropicApiKeyGuard(Option<String>);

    impl Drop for AnthropicApiKeyGuard {
        fn drop(&mut self) {
            if let Some(value) = self.0.take() {
                crate::env::set_var("ANTHROPIC_API_KEY", value);
            } else {
                crate::env::remove_var("ANTHROPIC_API_KEY");
            }
            crate::auth::AuthStatus::invalidate_cache();
        }
    }

    // A stale persisted catalog can carry an OAuth-only route for a newly
    // released Claude model. When an Anthropic API key is configured, opening
    // the picker must add the claude-api route instead of trusting the stale
    // single-route cache forever.
    with_temp_jcode_home(|| {
        let _api_key_guard =
            AnthropicApiKeyGuard(std::env::var("ANTHROPIC_API_KEY").ok());
        crate::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-key");
        crate::auth::AuthStatus::invalidate_cache();

        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_available_entries = vec!["claude-fable-5".to_string()];
        app.remote_model_options = vec![crate::provider::ModelRoute {
            model: "claude-fable-5".to_string(),
            provider: "Anthropic".to_string(),
            api_method: "claude-oauth".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }];

        app.open_model_picker();

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("model picker should be open");
        let fable_entries = picker
            .entries
            .iter()
            .filter(|entry| {
                entry.name == "claude-fable-5"
                    || entry.name.starts_with("claude-fable-5 (")
            })
            .collect::<Vec<_>>();
        assert!(!fable_entries.is_empty(), "fable should be in the picker");
        assert!(
            fable_entries.iter().any(|entry| entry.options.iter().any(
                |option| option.api_method == "claude-api" && option.available
            )),
            "stale oauth-only cached route should be augmented with claude-api, got {:?}",
            fable_entries
        );

    });
}

#[test]
fn test_remote_jcode_subscription_catalog_is_not_augmented_with_local_auth_routes() {
    with_temp_jcode_home(|| {
        let previous_anthropic_key = std::env::var_os("ANTHROPIC_API_KEY");
        crate::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-key");
        crate::auth::AuthStatus::invalidate_cache();

        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_provider_name =
            Some(crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME.to_string());
        app.remote_available_entries = vec![
            "claude-opus-4-8".to_string(),
            "gpt-5.5".to_string(),
            "gpt-5.6-sol".to_string(),
        ];
        app.remote_model_options = vec![crate::provider::ModelRoute {
            model: "claude-opus-4-8".to_string(),
            provider: "Anthropic".to_string(),
            api_method: "claude-api".to_string(),
            available: true,
            detail: "stale cached route".to_string(),
            cheapness: None,
        }];

        app.open_model_picker();

        match previous_anthropic_key {
            Some(value) => crate::env::set_var("ANTHROPIC_API_KEY", value),
            None => crate::env::remove_var("ANTHROPIC_API_KEY"),
        }
        crate::auth::AuthStatus::invalidate_cache();

        let expected = crate::subscription_catalog::curated_models()
            .iter()
            .filter(|model| {
                crate::subscription_catalog::JcodeTier::Plus.allows(model.min_tier)
            })
            .map(|model| model.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(app.remote_model_options.len(), expected.len());
        assert!(app.remote_model_options.iter().all(|route| {
            route.provider == crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME
                && route.api_method == crate::subscription_catalog::JCODE_ROUTE_API_METHOD
                && route.available
        }));
        assert_eq!(
            app.remote_model_options
                .iter()
                .map(|route| route.model.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            expected
        );
    });
}

#[test]
fn test_remote_mixed_catalog_keeps_jcode_subscription_separate_from_other_providers() {
    ensure_test_jcode_home_if_unset();
    clear_persisted_test_ui_state();
    crate::tui::ui::clear_test_render_state_for_tests();

    let mut app = create_test_app();
    app.is_remote = true;
    app.remote_provider_name = Some("Claude".to_string());
    app.remote_available_entries = vec![
        "claude-fable-5".to_string(),
        "claude-opus-4-8".to_string(),
        "gpt-5.5".to_string(),
        "gpt-5.6-sol".to_string(),
        "deepseek/deepseek-v4-pro".to_string(),
    ];
    app.remote_model_options = vec![
        crate::provider::ModelRoute {
            model: "claude-fable-5".to_string(),
            provider: "Anthropic".to_string(),
            api_method: "claude-oauth".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        },
        crate::provider::ModelRoute {
            model: "claude-opus-4-8".to_string(),
            provider: crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME.to_string(),
            api_method: crate::subscription_catalog::JCODE_ROUTE_API_METHOD.to_string(),
            available: true,
            detail: "managed subscription route".to_string(),
            cheapness: None,
        },
        crate::provider::ModelRoute {
            model: "gpt-5.5".to_string(),
            provider: crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME.to_string(),
            api_method: crate::subscription_catalog::JCODE_ROUTE_API_METHOD.to_string(),
            available: true,
            detail: "managed subscription route".to_string(),
            cheapness: None,
        },
        crate::provider::ModelRoute {
            model: "gpt-5.6-sol".to_string(),
            provider: crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME.to_string(),
            api_method: crate::subscription_catalog::JCODE_ROUTE_API_METHOD.to_string(),
            available: true,
            detail: "managed subscription route".to_string(),
            cheapness: None,
        },
        crate::provider::ModelRoute {
            model: "deepseek/deepseek-v4-pro".to_string(),
            provider: "auto".to_string(),
            api_method: "openrouter".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        },
    ];

    app.open_model_picker();

    assert_eq!(app.remote_model_options.len(), 5);
    let jcode_routes = app
        .remote_model_options
        .iter()
        .filter(|route| {
            route.provider == crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME
                && route.api_method == crate::subscription_catalog::JCODE_ROUTE_API_METHOD
        })
        .collect::<Vec<_>>();
    assert_eq!(jcode_routes.len(), 3);
    assert_eq!(
        jcode_routes
            .iter()
            .map(|route| route.model.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["claude-opus-4-8", "gpt-5.5", "gpt-5.6-sol",])
    );
    assert!(app.remote_model_options.iter().any(|route| {
        route.model == "claude-fable-5"
            && route.provider == "Anthropic"
            && route.api_method == "claude-oauth"
    }));
    assert!(app.remote_model_options.iter().any(|route| {
        route.model == "deepseek/deepseek-v4-pro"
            && route.provider == "auto"
            && route.api_method == "openrouter"
    }));
    assert!(app.remote_model_options.iter().all(|route| {
        route.provider != crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME
            || matches!(
                route.model.as_str(),
                "claude-opus-4-8" | "gpt-5.5" | "gpt-5.6-sol"
            )
    }));
}

#[test]
fn test_remote_hydrated_catalog_adds_entitled_jcode_subscription_routes() {
    with_temp_jcode_home(|| {
        let previous_key = std::env::var_os(crate::subscription_catalog::JCODE_API_KEY_ENV);
        let previous_tier = std::env::var_os(crate::subscription_catalog::JCODE_TIER_ENV);
        crate::env::set_var(
            crate::subscription_catalog::JCODE_API_KEY_ENV,
            "jcode_test_subscription_key",
        );
        crate::env::set_var(crate::subscription_catalog::JCODE_TIER_ENV, "plus");

        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_provider_name = Some("OpenAI".to_string());
        app.remote_available_entries = vec![
            "claude-fable-5".to_string(),
            "claude-opus-4-8".to_string(),
            "gpt-5.5".to_string(),
            "gpt-5.6-sol".to_string(),
            "deepseek/deepseek-v4-pro".to_string(),
        ];
        app.remote_model_options = vec![
            crate::provider::ModelRoute {
                model: "claude-opus-4-8".to_string(),
                provider: "Anthropic".to_string(),
                api_method: "claude-api".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            },
            crate::provider::ModelRoute {
                model: "gpt-5.5".to_string(),
                provider: "OpenAI".to_string(),
                api_method: "openai-oauth".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            },
            crate::provider::ModelRoute {
                model: "gpt-5.6-sol".to_string(),
                provider: "OpenAI".to_string(),
                api_method: "openai-oauth".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            },
            crate::provider::ModelRoute {
                model: "deepseek/deepseek-v4-pro".to_string(),
                provider: "auto".to_string(),
                api_method: "openrouter".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            },
        ];

        app.open_model_picker();

        match previous_key {
            Some(value) => {
                crate::env::set_var(crate::subscription_catalog::JCODE_API_KEY_ENV, value)
            }
            None => crate::env::remove_var(crate::subscription_catalog::JCODE_API_KEY_ENV),
        }
        match previous_tier {
            Some(value) => crate::env::set_var(crate::subscription_catalog::JCODE_TIER_ENV, value),
            None => crate::env::remove_var(crate::subscription_catalog::JCODE_TIER_ENV),
        }

        let jcode_routes = app
            .remote_model_options
            .iter()
            .filter(|route| {
                route.provider == crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME
                    && route.api_method == crate::subscription_catalog::JCODE_ROUTE_API_METHOD
            })
            .collect::<Vec<_>>();
        let expected = crate::subscription_catalog::curated_models()
            .iter()
            .filter(|model| {
                crate::subscription_catalog::JcodeTier::Plus.allows(model.min_tier)
            })
            .map(|model| model.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(jcode_routes.len(), expected.len());
        assert_eq!(
            jcode_routes
                .iter()
                .map(|route| route.model.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            expected
        );
        assert!(app.remote_model_options.iter().any(|route| {
            route.model == "claude-opus-4-8"
                && route.provider == "Anthropic"
                && route.api_method == "claude-api"
        }));
        assert!(app.remote_model_options.iter().any(|route| {
            route.model == "deepseek/deepseek-v4-pro"
                && route.provider == "auto"
                && route.api_method == "openrouter"
        }));
        assert!(app.remote_model_options.iter().all(|route| {
            route.provider != crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME
                || crate::subscription_catalog::find_curated_model(&route.model)
                    .is_some_and(|model| {
                        crate::subscription_catalog::JcodeTier::Plus.allows(model.min_tier)
                    })
        }));
    });
}

#[test]
fn test_remote_non_jcode_catalog_repairs_poisoned_all_jcode_routes() {
    with_temp_jcode_home(|| {
        let previous_tier = std::env::var_os(crate::subscription_catalog::JCODE_TIER_ENV);
        crate::env::set_var(crate::subscription_catalog::JCODE_TIER_ENV, "plus");

        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_provider_name = Some("OpenAI".to_string());
        app.remote_available_entries = vec![
            "claude-fable-5".to_string(),
            "claude-opus-4-8".to_string(),
            "gpt-5.5".to_string(),
            "gpt-5.6-sol".to_string(),
            "deepseek/deepseek-v4-pro".to_string(),
        ];
        app.remote_model_options = app
            .remote_available_entries
            .iter()
            .map(|model| crate::provider::ModelRoute {
                model: model.clone(),
                provider: crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME.to_string(),
                api_method: crate::subscription_catalog::JCODE_ROUTE_API_METHOD.to_string(),
                available: true,
                detail: "poisoned version 1 cache".to_string(),
                cheapness: None,
            })
            .collect();

        app.open_model_picker();

        match previous_tier {
            Some(value) => crate::env::set_var(crate::subscription_catalog::JCODE_TIER_ENV, value),
            None => crate::env::remove_var(crate::subscription_catalog::JCODE_TIER_ENV),
        }

        let jcode_routes = app
            .remote_model_options
            .iter()
            .filter(|route| {
                route.provider == crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME
                    && route.api_method == crate::subscription_catalog::JCODE_ROUTE_API_METHOD
            })
            .collect::<Vec<_>>();
        assert_eq!(jcode_routes.len(), 3);
        assert_eq!(
            jcode_routes
                .iter()
                .map(|route| route.model.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "claude-opus-4-8",
                "gpt-5.5",
                "gpt-5.6-sol",
            ])
        );
        assert!(app.remote_model_options.iter().any(|route| {
            route.model == "deepseek/deepseek-v4-pro"
                && route.provider != crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME
        }));
        assert!(app.remote_model_options.iter().all(|route| {
            route.provider != crate::subscription_catalog::JCODE_PROVIDER_DISPLAY_NAME
                || matches!(
                    route.model.as_str(),
                    "claude-opus-4-8" | "gpt-5.5" | "gpt-5.6-sol"
                )
        }));
    });
}

#[test]
fn test_model_picker_ctrl_b_bedrock_selection_saves_bedrock_default() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_available_entries = vec!["amazon.nova-pro-v1:0".to_string()];
        app.remote_model_options = vec![crate::provider::ModelRoute {
            model: "amazon.nova-pro-v1:0".to_string(),
            provider: "AWS Bedrock".to_string(),
            api_method: "bedrock".to_string(),
            available: true,
            detail: String::new(),
            cheapness: None,
        }];

        app.open_model_picker();

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("model picker should be open");
        let model_idx = picker
            .entries
            .iter()
            .position(|m| m.name == "amazon.nova-pro-v1:0")
            .expect("Bedrock model should be in picker");
        let filtered_pos = picker
            .filtered
            .iter()
            .position(|&i| i == model_idx)
            .expect("Bedrock model should be in filtered list");
        app.inline_interactive_state.as_mut().unwrap().selected = filtered_pos;

        // Ctrl+O replaced Ctrl+B so the picker no longer steals tmux's prefix.
        app.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL)
            .unwrap();

        let cfg = crate::config::Config::load();
        assert_eq!(
            cfg.provider.default_model.as_deref(),
            Some("bedrock:amazon.nova-pro-v1:0")
        );
        assert_eq!(cfg.provider.default_provider.as_deref(), Some("bedrock"));
    });
}

#[test]
fn test_handle_key_cursor_movement() {
    let mut app = create_test_app();

    app.handle_key(KeyCode::Char('a'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('b'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('c'), KeyModifiers::empty())
        .unwrap();

    assert_eq!(app.cursor_pos(), 3);

    app.handle_key(KeyCode::Left, KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.cursor_pos(), 2);

    app.handle_key(KeyCode::Home, KeyModifiers::empty())
        .unwrap();
    assert_eq!(app.cursor_pos(), 0);

    app.handle_key(KeyCode::End, KeyModifiers::empty()).unwrap();
    assert_eq!(app.cursor_pos(), 3);
}

#[test]
fn test_handle_key_ctrl_word_movement_and_delete() {
    let mut app = create_test_app();
    app.set_input_for_test("hello world again");

    app.handle_key(KeyCode::Left, KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.cursor_pos(), "hello world ".len());

    app.handle_key(KeyCode::Left, KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.cursor_pos(), "hello ".len());

    app.handle_key(KeyCode::Right, KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.cursor_pos(), "hello world ".len());

    app.handle_key(KeyCode::Backspace, KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.input(), "hello again");
    assert_eq!(app.cursor_pos(), "hello ".len());
}

#[test]
fn test_handle_key_ctrl_backspace_csi_u_char_fallback_deletes_word() {
    let mut app = create_test_app();
    app.set_input_for_test("hello world again");

    app.handle_key(KeyCode::Char('\u{8}'), KeyModifiers::CONTROL)
        .unwrap();

    assert_eq!(app.input(), "hello world ");
    assert_eq!(app.cursor_pos(), "hello world ".len());
}

#[test]
fn test_handle_key_super_backspace_deletes_previous_word() {
    let mut app = create_test_app();
    app.set_input_for_test("hello world again");

    app.handle_key(KeyCode::Left, KeyModifiers::CONTROL)
        .unwrap();
    app.handle_key(KeyCode::Backspace, KeyModifiers::SUPER)
        .unwrap();

    // Cmd+Backspace deletes the previous word, leaving the cursor at the new boundary.
    assert_eq!(app.input(), "hello again");
    assert_eq!(app.cursor_pos(), "hello ".len());
}

#[test]
fn test_handle_key_super_delete_aliases_delete_previous_word() {
    for code in [KeyCode::Delete, KeyCode::Char('\u{7f}')] {
        let mut app = create_test_app();
        app.set_input_for_test("hello world again");

        app.handle_key(KeyCode::Left, KeyModifiers::CONTROL)
            .unwrap();
        app.handle_key(code, KeyModifiers::SUPER).unwrap();

        assert_eq!(app.input(), "hello again");
        assert_eq!(app.cursor_pos(), "hello ".len());
    }
}

#[test]
fn test_handle_key_alt_delete_aliases_delete_previous_word() {
    for code in [KeyCode::Backspace, KeyCode::Delete, KeyCode::Char('\u{7f}')] {
        let mut app = create_test_app();
        app.set_input_for_test("hello world again");

        app.handle_key(KeyCode::Left, KeyModifiers::CONTROL)
            .unwrap();
        app.handle_key(code, KeyModifiers::ALT).unwrap();

        assert_eq!(app.input(), "hello again");
        assert_eq!(app.cursor_pos(), "hello ".len());
    }
}

#[test]
fn test_remote_super_backspace_deletes_previous_word() {
    let mut app = create_test_app();
    app.set_input_for_test("hello world again");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    app.handle_key(KeyCode::Left, KeyModifiers::CONTROL)
        .unwrap();
    rt.block_on(app.handle_remote_key(KeyCode::Backspace, KeyModifiers::SUPER, &mut remote))
        .unwrap();

    assert_eq!(app.input(), "hello again");
    assert_eq!(app.cursor_pos(), "hello ".len());
}

#[test]
fn test_handle_key_ctrl_k_deletes_to_end() {
    let mut app = create_test_app();
    app.set_input_for_test("hello world again");

    app.handle_key(KeyCode::Left, KeyModifiers::CONTROL)
        .unwrap();
    app.handle_key(KeyCode::Char('k'), KeyModifiers::CONTROL)
        .unwrap();

    assert_eq!(app.input(), "hello world ");
    assert_eq!(app.cursor_pos(), "hello world ".len());
}

#[test]
fn test_handle_key_super_left_right_move_to_edges() {
    let mut app = create_test_app();
    app.set_input_for_test("hello world");

    if cfg!(target_os = "macos") {
        // On macOS, Cmd+Left/Right default to effort cycling, so the cursor
        // must NOT move; Home/End still jump to the edges.
        let before = app.cursor_pos();
        app.handle_key(KeyCode::Left, KeyModifiers::SUPER).unwrap();
        assert_eq!(app.cursor_pos(), before);

        app.handle_key(KeyCode::Home, KeyModifiers::empty()).unwrap();
        assert_eq!(app.cursor_pos(), 0);

        app.handle_key(KeyCode::End, KeyModifiers::empty()).unwrap();
        assert_eq!(app.cursor_pos(), "hello world".len());
    } else {
        app.handle_key(KeyCode::Left, KeyModifiers::SUPER).unwrap();
        assert_eq!(app.cursor_pos(), 0);

        app.handle_key(KeyCode::Right, KeyModifiers::SUPER).unwrap();
        assert_eq!(app.cursor_pos(), "hello world".len());
    }
}

#[test]
fn test_handle_key_alt_left_right_move_by_word() {
    // On non-macOS platforms Alt+Left/Right default to effort cycling, so the
    // word-move behavior only applies where Cmd+Left/Right own effort cycling.
    if !cfg!(target_os = "macos") {
        return;
    }
    let mut app = create_test_app();
    app.set_input_for_test("hello world");

    app.handle_key(KeyCode::Left, KeyModifiers::ALT).unwrap();
    assert_eq!(app.cursor_pos(), "hello ".len());

    app.handle_key(KeyCode::Left, KeyModifiers::ALT).unwrap();
    assert_eq!(app.cursor_pos(), 0);

    // Forward lands at the start of the next word, matching Alt+F.
    app.handle_key(KeyCode::Right, KeyModifiers::ALT).unwrap();
    assert_eq!(app.cursor_pos(), "hello ".len());

    app.handle_key(KeyCode::Right, KeyModifiers::ALT).unwrap();
    assert_eq!(app.cursor_pos(), "hello world".len());
}

#[test]
fn test_handle_key_super_z_undoes_input_change() {
    let mut app = create_test_app();

    app.handle_key(KeyCode::Char('a'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('b'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('z'), KeyModifiers::SUPER)
        .unwrap();

    assert_eq!(app.input(), "a");
    assert_eq!(app.cursor_pos(), 1);
}

#[test]
fn test_handle_key_ctrl_h_does_not_insert_text() {
    let mut app = create_test_app();
    app.set_input_for_test("hello");

    app.handle_key(KeyCode::Char('h'), KeyModifiers::CONTROL)
        .unwrap();

    assert_eq!(app.input(), "hello");
    assert_eq!(app.cursor_pos(), "hello".len());
}

#[test]
fn test_handle_key_escape_clears_input() {
    let mut app = create_test_app();

    app.handle_key(KeyCode::Char('t'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('e'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('s'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('t'), KeyModifiers::empty())
        .unwrap();

    assert_eq!(app.input(), "test");

    app.handle_key(KeyCode::Esc, KeyModifiers::empty()).unwrap();

    assert!(app.input().is_empty());
    assert_eq!(app.cursor_pos(), 0);
    assert_eq!(
        app.status_notice(),
        Some("Input cleared - Ctrl+Z to restore".to_string())
    );
}

#[test]
fn test_handle_key_ctrl_z_restores_escaped_input() {
    let mut app = create_test_app();

    app.handle_key(KeyCode::Char('t'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('e'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('s'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('t'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Esc, KeyModifiers::empty()).unwrap();

    app.handle_key(KeyCode::Char('z'), KeyModifiers::CONTROL)
        .unwrap();

    assert_eq!(app.input(), "test");
    assert_eq!(app.cursor_pos(), 4);
    assert_eq!(app.status_notice(), Some("↶ Input restored".to_string()));
}

#[test]
fn test_handle_key_ctrl_z_undoes_typing() {
    let mut app = create_test_app();

    app.handle_key(KeyCode::Char('a'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('b'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('c'), KeyModifiers::empty())
        .unwrap();

    app.handle_key(KeyCode::Char('z'), KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.input(), "ab");
    assert_eq!(app.cursor_pos(), 2);

    app.handle_key(KeyCode::Char('z'), KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.input(), "a");
    assert_eq!(app.cursor_pos(), 1);
}

#[test]
fn test_handle_key_ctrl_u_clears_input() {
    let mut app = create_test_app();

    app.handle_key(KeyCode::Char('t'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('e'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('s'), KeyModifiers::empty())
        .unwrap();
    app.handle_key(KeyCode::Char('t'), KeyModifiers::empty())
        .unwrap();

    app.handle_key(KeyCode::Char('u'), KeyModifiers::CONTROL)
        .unwrap();

    assert!(app.input().is_empty());
    assert_eq!(app.cursor_pos(), 0);
}
