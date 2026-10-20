use super::*;
use crate::types::{ConstrainedSampling, ConstrainedSamplingConfig, JsonSchemaStrictness};

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
    let converted =
        convert_tools(&[tool.clone()], false, false, true, None, false).expect("prefer falls back");
    assert!(converted[0].get("strict").is_none());
    let required = Tool {
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema {
                strict: JsonSchemaStrictness::Require,
            },
        )),
        ..tool
    };
    assert!(convert_tools(&[required], false, false, true, None, false).is_err());
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
    let converted = convert_tools(&[tool], false, false, true, None, false).expect("strict");
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
