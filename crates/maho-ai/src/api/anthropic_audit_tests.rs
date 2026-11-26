use super::*;
use crate::types::{ConstrainedSampling, ConstrainedSamplingConfig, JsonSchemaStrictness};

#[tokio::test]
async fn streams_refuse_a_claude_oauth_api_key_before_building_the_request() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut model = crate::providers::all::get_builtin_models("anthropic")
        .into_iter()
        .next()
        .expect("Anthropic model");
    model.base_url = "http://127.0.0.1:0".into();
    let token = ["sk-ant", "oat01", "test-only"].join("-");
    let context = Context::default();
    let mut options = SimpleStreamOptions::default();
    options.stream.request.api_key = Some(token.clone());
    options.stream.request.max_retries = Some(0);
    let built = std::sync::Arc::new(AtomicBool::new(false));
    let observed = built.clone();
    options.stream.request.on_payload = Some(std::sync::Arc::new(move |_, _, _| {
        observed.store(true, Ordering::Relaxed);
        None
    }));
    let error = assert_not_claude_oauth_token(&model, Some(&token)).expect_err("typed refusal");
    assert_eq!(
        ClaudeOAuthTokenRefusedError::CODE,
        "claude_oauth_token_refused"
    );
    assert_eq!(error.provider, "anthropic");
    assert_eq!(error.model_id, model.id);

    for stream in [
        stream_simple(&model, &context, Some(options.clone())),
        stream(&model, &context, Some(options.stream)),
    ] {
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("refusal must settle")
            .expect("error result");
        assert_eq!(message.stop_reason, StopReason::Error);
        let detail = message.error_message.expect("refusal detail");
        assert_eq!(detail, error.to_string());
        assert!(!detail.contains(&token));
    }
    assert!(!built.load(Ordering::Relaxed));
}

#[test]
fn api_key_requests_keep_their_own_prompt_identity_and_tool_names() {
    let mut model = crate::providers::all::get_builtin_models("anthropic")
        .into_iter()
        .next()
        .expect("Anthropic model");
    // The upstream fixture has no native deferred-tool metadata.
    model.compat = None;
    let tools = ["read", "ask_user_question"].map(|name| Tool {
        name: name.into(),
        description: String::new(),
        parameters: json!({"type":"object","properties":{}}),
        output_schema: None,
        freeform: None,
        constrained_sampling: None,
    });
    let context = Context {
        system_prompt: Some("System prompt.".into()),
        tools: Some(tools.into()),
        ..Default::default()
    };
    let options = StreamOptions::default();
    let key = "test-api-key";
    let client = create_client(&model, &context, &options, Some(key), None, None);
    let headers = request_headers(&client, &model, Some(key));
    let body = build_params(&model, &context, &options, "text").expect("API key payload");

    assert_eq!(headers["x-api-key"], key);
    assert!(!headers.contains_key("authorization"));
    assert!(!headers.contains_key("x-app"));
    assert!(
        !headers["user-agent"]
            .to_str()
            .unwrap()
            .starts_with("claude-cli/")
    );
    let betas = ["claude-code-20250219", "oauth-2025-04-20"];
    for beta in betas {
        assert!(!headers.get("anthropic-beta").is_some_and(|value| {
            value
                .to_str()
                .unwrap()
                .split(',')
                .any(|value| value == beta)
        }));
        assert!(
            !body
                .get("betas")
                .and_then(Value::as_array)
                .is_some_and(|values| { values.iter().any(|value| value.as_str() == Some(beta)) })
        );
    }
    let system = body["system"].as_array().expect("system blocks");
    assert_eq!(system.len(), 1);
    assert_eq!(system[0]["text"], "System prompt.");
    let names = body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["read", "ask_user_question"]);
}

#[tokio::test]
async fn warmer_honors_already_aborted_request() -> anyhow::Result<()> {
    let mut model = crate::models_generated::MODELS
        .get("anthropic")
        .and_then(|models| models.values().next())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing model"))?;
    model.base_url = "https://api.anthropic.com".into();
    let controller = AbortController::new();
    controller.abort(None);
    let mut options = StreamOptions::default();
    options.request.signal = Some(controller.signal());
    let result = crate::api::warm_prompt_cache::warm_prompt_cache(
        &model,
        &Context::default(),
        Some(options),
        &|_, _, _| Ok(json!({})),
    )
    .await;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn raw_event_hook_runs_before_message_normalization() -> anyhow::Result<()> {
    let app = axum::Router::new().route("/v1/messages", axum::routing::post(|| async {
        ([("content-type", "text/event-stream")],
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"observed\",\"usage\":{}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut model = crate::models_generated::MODELS
        .get("anthropic")
        .and_then(|models| models.values().next())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing model"))?;
    model.base_url = format!("http://{address}");
    let observed = std::sync::Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let mut options = StreamOptions::default();
    options.request.api_key = Some("key".into());
    options.async_on_provider_stream_event = Some(std::sync::Arc::new(move |event, _| {
        captured
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
        Box::pin(async { Err("observer stopped".into()) })
    }));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream(&model, &Context::default(), Some(options)).collect(),
    )
    .await;
    server.abort();
    let events = result??;
    assert_eq!(
        observed.lock().unwrap_or_else(|error| error.into_inner())[0]["type"],
        "message_start"
    );
    let error = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Error { error, .. } => Some(error),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("missing observer error"))?;
    assert!(error.response_id.is_none());
    Ok(())
}

#[test]
fn strict_policy_checks_nested_schema_nodes() {
    let tool = Tool {
        name: "lookup".into(),
        description: String::new(),
        parameters: json!({"type":"object","properties":{"nested":{"type":"array",
            "items":{"anyOf":[{"type":"integer","minimum":0},{"type":"null"}]}}}}),
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrictness::Prefer,
            },
        )),
        output_schema: None,
        freeform: None,
    };
    let converted = convert_tools(std::slice::from_ref(&tool), false, true, None, false)
        .expect("prefer falls back");
    assert!(converted[0].get("strict").is_none());
    let required = Tool {
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrictness::Require,
            },
        )),
        ..tool
    };
    assert!(convert_tools(&[required], false, true, None, false).is_err());
}

#[test]
fn strict_policy_accepts_supported_array_and_format_constraints() {
    let tool = Tool {
        name: "lookup".into(),
        description: String::new(),
        parameters: json!({"type":"object","properties":{"dates":{"type":"array",
            "minItems":1,"items":{"type":"string","format":"date-time"}}}}),
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrictness::Require,
            },
        )),
        output_schema: None,
        freeform: None,
    };
    let converted = convert_tools(&[tool], false, true, None, false).expect("strict");
    assert_eq!(converted[0]["strict"], true);
    assert_eq!(converted[0]["input_schema"]["additionalProperties"], false);
}

#[test]
fn glm_server_result_replay_requires_a_paired_use() {
    let result = json!({"type":"tool_result","tool_use_id":"server1","content":[]});
    let mut pairing = ProviderNativeToolPairing::default();
    assert!(is_replayable_anthropic_provider_native_block(&result));
    assert!(is_anthropic_web_search_replay_block(&result));
    assert!(is_unpaired_provider_native_tool_block(&result, &pairing));
    pairing.valid_result_ids.insert("server1".into());
    assert!(!is_unpaired_provider_native_tool_block(&result, &pairing));
}
