use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use maho_ai::providers::faux::{FauxAssistantMessageOptions,faux_assistant_message,faux_tool_call};
use maho_ext_api::*;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript,faux_session::FauxSession};
use serde_json::json;

struct ReadTool(Arc<AtomicUsize>);
impl Extension for ReadTool {
    fn register(&self,api:&mut ExtensionApi) {
        let calls=self.0.clone();
        api.register_tool(ToolDefinition::new("read","fixture read",json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),Arc::new(move |_| {
            let calls=calls.clone(); Box::pin(async move { calls.fetch_add(1,Ordering::SeqCst); Ok(ToolResult::text("fixture")) })
        })));
    }
}
#[tokio::test]
async fn real_faux_session_vetoes_identical_calls_after_two_notices() {
    let calls=Arc::new(AtomicUsize::new(0));
    let mut responses=(0..7).map(|index|faux_assistant_message(faux_tool_call("read",serde_json::from_value(json!({"path":"same"})).unwrap(),Some(&format!("call-{index}"))),FauxAssistantMessageOptions { stop_reason:Some(maho_ai::types::StopReason::ToolUse),timestamp:Some(0),..Default::default() })).collect::<Vec<_>>();
    responses.push(faux_assistant_message("done",FauxAssistantMessageOptions { timestamp:Some(0),..Default::default() }));
    let session=FauxSession::new(FauxScript { name:"guard-native-veto".into(),prompt:"read".into(),responses:Vec::new() })
        .with_native_extension(NativeExtensionFactory { path:"<read>".into(),source_info:Default::default(),extension:Box::new(ReadTool(calls.clone())) })
        .with_native_extension(NativeExtensionFactory { path:"<loop-guard>".into(),source_info:Default::default(),extension:Box::new(maho_ext_loop_guard::index::LoopGuardExtension) })
        .with_native_responses(responses);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst),6);
    let messages=result["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message|message["role"]=="toolResult"&&message["isError"]==true));
    assert!(messages.iter().any(|message|message["customType"]==maho_ext_loop_guard::notice::LOOP_GUARD_NOTICE_CUSTOM_TYPE));
}
