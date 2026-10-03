#[tokio::test]
async fn genuine_registered_context_executor_validates_before_native_auth_or_network(){
    use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
    use maho_test_support::{faux::FauxScript,faux_session::FauxSession};
    let home=tempfile::tempdir().expect("temporary home");
    let session=FauxSession::new(FauxScript{name:"registered-search".into(),prompt:"search".into(),responses:vec![]})
        .with_native_extension(maho_ext_host::loader::NativeExtensionFactory{path:"builtin:websearch".into(),source_info:maho_ext_api::SourceInfo{source:"builtin".into(),..Default::default()},extension:Box::new(maho_ext_websearch::index::WebsearchExtension{home:home.path().into(),provider_native_bypass:std::sync::Arc::new(|_|false)})})
        .with_native_responses(vec![faux_assistant_message(faux_tool_call("web_search",serde_json::from_value(serde_json::json!({"query":"native context","allowed_domains":["example.com"],"blocked_domains":["example.org"]})).expect("tool arguments"),Some("search-context")),FauxAssistantMessageOptions{stop_reason:Some(maho_ai::types::StopReason::ToolUse),..Default::default()}),faux_assistant_message("done",FauxAssistantMessageOptions::default())]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.expect("native session timeout").expect("native session");
    let tool=result["messages"].as_array().expect("messages").iter().find(|message|message["role"]=="toolResult").expect("registered tool result");
    assert_eq!(tool["isError"],false,"{tool}");
    assert!(tool["content"][0]["text"].as_str().expect("tool text").contains("allowed_domains"),"{tool}");
}
