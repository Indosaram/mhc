#[tokio::test]
async fn genuine_registered_context_executor_validates_before_native_auth_or_network() {
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, faux_assistant_message, faux_tool_call,
    };
    use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
    let home = tempfile::tempdir().expect("temporary home");
    let session=FauxSession::new(FauxScript{name:"registered-search".into(),prompt:"search".into(),responses:vec![]})
        .with_native_extension(maho_ext_host::loader::NativeExtensionFactory{path:"builtin:websearch".into(),source_info:maho_ext_api::SourceInfo{source:"builtin".into(),..Default::default()},extension:Box::new(maho_ext_websearch::index::WebsearchExtension{home:home.path().into(),provider_native_bypass:std::sync::Arc::new(|_|false)})})
        .with_native_responses(vec![faux_assistant_message(faux_tool_call("web_search",serde_json::from_value(serde_json::json!({"query":"native context","allowed_domains":["example.com"],"blocked_domains":["example.org"]})).expect("tool arguments"),Some("search-context")),FauxAssistantMessageOptions{stop_reason:Some(maho_ai::types::StopReason::ToolUse),..Default::default()}),faux_assistant_message("done",FauxAssistantMessageOptions::default())]);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await
        .expect("native session timeout")
        .expect("native session");
    let tool = result["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["role"] == "toolResult")
        .expect("registered tool result");
    assert_eq!(tool["isError"], true, "{tool}");
    assert!(
        tool["content"][0]["text"]
            .as_str()
            .expect("tool text")
            .contains("allowed_domains"),
        "{tool}"
    );
}

#[tokio::test]
async fn registered_function_search_recovers_from_native_bypass_for_both_providers() {
    use maho_ext_host::loader::NativeExtensionFactory;
    use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
    use serde_json::json;
    for (provider, api, endpoint) in [
        ("openai", "openai-responses", "https://api.openai.com/v1"),
        (
            "anthropic",
            "anthropic-messages",
            "https://api.anthropic.com",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let session = FauxSession::new(FauxScript {
            name: "search-fallback".into(),
            prompt: "search".into(),
            responses: vec![],
        })
        .with_native_extension(NativeExtensionFactory {
            path: "builtin:websearch".into(),
            source_info: Default::default(),
            extension: Box::new(maho_ext_websearch::WebsearchExtension {
                home: home.path().into(),
                provider_native_bypass: std::sync::Arc::new(|model| {
                    maho_ext_openai_web_search::supports_native_openai_web_search(model)
                        || maho_ext_anthropic_web_search::supports_native_anthropic_web_search(
                            model,
                        )
                }),
            }),
        })
        .with_native_extension(NativeExtensionFactory {
            path: "builtin:anthropic-web-search".into(),
            source_info: Default::default(),
            extension: Box::new(maho_ext_anthropic_web_search::AnthropicWebSearch),
        })
        .with_native_extension(NativeExtensionFactory {
            path: "builtin:openai-web-search".into(),
            source_info: Default::default(),
            extension: Box::new(maho_ext_openai_web_search::OpenAiWebSearch),
        })
        .run_native_handle()
        .await
        .unwrap();
        let model:maho_ai::types::Model = serde_json::from_value(json!({"id":"test", "name":"Test", "api":api, "provider":provider, "baseUrl":endpoint, "reasoning":false, "input":["text"], "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}, "contextWindow":200000, "maxTokens":16384})).unwrap();
        session
            .configure_provider_key(provider, "fixture-key")
            .unwrap();
        session.set_model(model.clone()).await.unwrap();
        let tool = if provider == "openai" {
            json!({"type":"function","name":"web_search","parameters":{"type":"object"}})
        } else {
            json!({"name":"web_search","input_schema":{"type":"object"}})
        };
        let payload = json!({"tools":[tool.clone()]});
        let initial = session
            .emit_before_provider_request(payload.clone(), None)
            .await
            .unwrap();
        assert_ne!(initial["tools"], payload["tools"]);
        std::fs::create_dir_all(session.cwd().join(".senpi")).unwrap();
        std::fs::write(session.cwd().join(".senpi/websearch.json"), "invalid").unwrap();

        let result = session
            .execute_tool("web_search", json!({"query":"current sources"}))
            .await
            .unwrap();
        assert_eq!(result.details["reason"], "invalid_config");
        assert_eq!(result.is_error, Some(true));
        let fallback = session
            .emit_before_provider_request(payload.clone(), None)
            .await
            .unwrap();
        assert_eq!(fallback, payload);

        let mut other = model.clone();
        other.id = "other".into();
        let other_request = session
            .emit_before_provider_request(payload.clone(), Some(other.clone()))
            .await
            .unwrap();
        assert_ne!(other_request["tools"], payload["tools"]);
        session.set_model(other).await.unwrap();
        session.set_model(model).await.unwrap();
        assert_eq!(
            session
                .emit_before_provider_request(payload.clone(), None)
                .await
                .unwrap(),
            payload
        );
        session.close().await;
    }
}

#[tokio::test]
async fn native_failure_message_restores_standalone_search_without_duplicate_answer() {
    use maho_ai::{
        providers::faux::{FauxAssistantMessageOptions, faux_assistant_message},
        types::{ContentBlock, ProviderNativeContent},
    };
    use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
    use serde_json::json;
    let home = tempfile::tempdir().unwrap();
    let mut response =
        faux_assistant_message("completed answer", FauxAssistantMessageOptions::default());
    response.content.push(ContentBlock::ProviderNative(ProviderNativeContent {
        subtype:"web_search_tool_result".into(), raw:json!({"type":"web_search_tool_result", "content":{"type":"web_search_tool_result_error","error_code":"unavailable"}}),
    }));
    let session = FauxSession::new(FauxScript {
        name: "native-search-failure".into(),
        prompt: "search".into(),
        responses: vec![],
    })
    .with_native_extension(maho_ext_host::loader::NativeExtensionFactory {
        path: "builtin:websearch".into(),
        source_info: Default::default(),
        extension: Box::new(maho_ext_websearch::WebsearchExtension {
            home: home.path().into(),
            provider_native_bypass: std::sync::Arc::new(|_| true),
        }),
    })
    .with_native_responses(vec![response])
    .run_native_handle()
    .await
    .unwrap();
    std::fs::create_dir_all(session.cwd().join(".senpi")).unwrap();
    std::fs::write(session.cwd().join(".senpi/websearch.json"), "invalid").unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        session.prompt("search".into()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(session.provider_calls().len(), 1);
    let result = session
        .execute_tool("web_search", json!({"query":"sources"}))
        .await
        .unwrap();
    assert_eq!(result.details["reason"], "invalid_config");
    assert_eq!(result.is_error, Some(true));
    session.close().await;
}
