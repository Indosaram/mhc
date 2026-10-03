use maho_ext_api::{ExtensionFailure,ToolInfo};
struct Catalog;
impl maho_ext_api::ExtensionActions for Catalog {
    fn send_message(&self,_:maho_ext_api::CustomMessage,_:maho_ext_api::SendMessageOptions)->Result<(),ExtensionFailure>{panic!("not used")}
    fn send_user_message(&self,_:maho_ext_api::UserMessageContent,_:maho_ext_api::SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("not used")}
    fn append_entry(&self,_:&str,_:Option<serde_json::Value>)->Result<(),ExtensionFailure>{panic!("not used")}
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![ToolInfo {name:"read_docs".into(),label:"Read documentation".into(),description:"Read documentation and API references".into(),parameters:serde_json::json!({"type":"object"}),prompt_guidelines:None,source_info:maho_ext_api::SourceInfo {path:"builtin:docs".into(),..Default::default()},exposure:maho_ext_api::ToolExposure::Search,search_text:None,search_keywords:vec!["documentation".into()],search_group:Some("docs".into()),allow_lazy_activation:true}])}
}
struct Search;
impl maho_ext_api::Extension for Search {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi){
        maho_ext_tool_search::index::ToolSearchExtension {actions:std::sync::Arc::new(Catalog),mcp_native_enabled:std::sync::Arc::new(||false)}.register(api);
    }
}
#[tokio::test]
async fn native_session_searches_live_catalog_without_activating_matches() {
    use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
    use maho_test_support::{faux::FauxScript,faux_session::FauxSession};
    let session=FauxSession::new(FauxScript {name:"tool-search-native".into(),prompt:"search documentation".into(),responses:vec![]})
        .with_native_extension(maho_ext_host::loader::NativeExtensionFactory {path:"builtin:tool-search".into(),source_info:maho_ext_api::SourceInfo {source:"builtin".into(),..Default::default()},extension:Box::new(Search)})
        .with_native_responses(vec![faux_assistant_message(faux_tool_call("tool_search",serde_json::from_value(serde_json::json!({"query":"documentation","source":"extension","group":"docs"})).unwrap(),Some("search-call")),FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::ToolUse),timestamp:Some(0),..Default::default()}),faux_assistant_message("done",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()})]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let tool=result["messages"].as_array().unwrap().iter().find(|message|message["role"]=="toolResult").unwrap();
    assert_eq!(tool["isError"],false);assert_eq!(tool["details"]["matched"],serde_json::json!(["read_docs"]));assert_eq!(tool["details"]["query"],"documentation");
}
