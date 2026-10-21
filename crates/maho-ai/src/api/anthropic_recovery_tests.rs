use super::*;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn haiku_55_uses_adaptive_xhigh_without_disabled_thinking() -> anyhow::Result<()> {
    let mut model = crate::models_generated::MODELS
        .get("anthropic")
        .and_then(|models| models.values().next())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing Anthropic fixture"))?;
    model.id = "claude-haiku-5-5".into();
    model.compat = None;
    model.thinking_level_map = None;
    assert!(supports_adaptive_thinking(&model));
    assert_eq!(
        map_thinking_level_to_effort(&model, Some(ThinkingLevel::Xhigh)),
        "xhigh"
    );
    let mut options = StreamOptions::default();
    options.extra.insert("thinkingEnabled".into(), json!(false));
    let params = build_params(&model, &Context::default(), false, &options, "text")
        .map_err(anyhow::Error::msg)?;
    assert_ne!(
        params.get("thinking").and_then(|value| value.get("type")),
        Some(&json!("disabled"))
    );
    Ok(())
}

#[tokio::test]
async fn expired_search_retries_and_remembers_only_for_its_session() -> anyhow::Result<()> {
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = captured.clone();
    let app = axum::Router::new().route("/v1/messages", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
        let requests = requests.clone();
        async move {
            let native = body["messages"][0]["content"][0]["type"] == "web_search_tool_result";
            requests.lock().unwrap_or_else(|p| p.into_inner()).push(body);
            if native {
                (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error":{"type":"invalid_request_error","message":"messages.0.content.0: Invalid `encrypted_content` in `search_result` block"}})))
            } else {
                (axum::http::StatusCode::OK, axum::Json(json!({})))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let mut model = crate::models_generated::MODELS
        .get("anthropic")
        .and_then(|models| models.values().next())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing Anthropic fixture"))?;
    model.base_url = format!("http://{address}");
    let mut options = StreamOptions::default();
    options.request.on_payload = Some(Arc::new(|payload, _, _| {
        let mut payload = payload.clone();
        payload["messages"] = json!([{"role":"assistant","content":[{"type":"web_search_tool_result","tool_use_id":"expired","content":[]}]}]);
        Some(payload)
    }));
    let context = Context::default();
    let client = create_client(&model, &context, &options, Some("key"), None, None);
    let replay = Mutex::new("text");
    let omitted_tools = Mutex::new(0);
    let key = unsigned_thinking_fallback_key(&model, Some("expiry-test"));
    LazyLock::force(&UNSIGNED_THINKING_CLEANUP);
    for _ in 0..2 {
        tokio::time::timeout(
            Duration::from_secs(5),
            attempt_request(
                &model,
                &context,
                &client,
                &options,
                Some("key"),
                RequestReplay {
                    unsigned_thinking: &replay,
                    fallback_key: key.as_deref(),
                    omitted_tools: &omitted_tools,
                },
            ),
        )
        .await?
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    }
    assert_eq!(captured.lock().unwrap_or_else(|p| p.into_inner()).len(), 3);
    let prefix = format!("{}\0", key.as_deref().unwrap_or_default());
    assert!(
        EXPIRED_WEB_SEARCH_REPLAYS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&format!("{prefix}expired"))
    );
    crate::session_resources::cleanup_session_resources(Some("expiry-test"))?;
    assert!(
        !EXPIRED_WEB_SEARCH_REPLAYS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|key| key.starts_with(&prefix))
    );
    server.abort();
    Ok(())
}
