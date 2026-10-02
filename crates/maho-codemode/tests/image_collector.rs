use std::sync::Arc;
use base64::Engine;
use maho_codemode::tool::{image::*, image_resize::*};
struct Sdk;
impl EvalImageSdk for Sdk {
    fn resize_image<'a>(&'a self, _:Vec<u8>, _: &'a str, _:Option<usize>) -> ImageFuture<'a, Option<ResizedImage>> {Box::pin(async {Err("unexpected image resize".into())})}
    fn convert_to_png<'a>(&'a self, _: &'a str, _: &'a str) -> ImageFuture<'a, Option<EvalImageContent>> {Box::pin(async {Err("unexpected conversion".into())})}
}
fn collector() -> EvalOutputCollector {EvalOutputCollector::new(EvalOutputOptions {artifact_path:None,head_bytes:20480,max_columns:768,provider:None,api:None,image_sdk:Arc::new(Sdk)})}
#[tokio::test]
async fn markdown_and_json_display_are_retained_and_invalid_json_fails() {
    let mut collector=collector();
    collector.push("stdout\n").await.unwrap();
    let encode=|text:&str|base64::engine::general_purpose::STANDARD.encode(text);
    collector.display("text/markdown",&encode("# heading")).await.unwrap();
    collector.display("application/json",&encode("{\"answer\":42}")).await.unwrap();
    assert!(collector.display("application/json",&encode("{")).await.is_err());
    let result=collector.finish().await.unwrap();
    assert!(result.has_markdown);
    assert_eq!(result.json_outputs,vec![serde_json::json!({"answer":42})]);
    assert!(result.output.contains("stdout"));
    assert!(result.images.is_empty());
}
#[tokio::test]
async fn json_retention_is_capped_per_cell() {
    let mut collector=collector();
    for _ in 0..66 {collector.display("application/json","NDI=").await.unwrap();}
    let result=collector.finish().await.unwrap();
    assert_eq!(result.json_outputs.len(),64);
    assert!(result.output.contains("2 JSON output(s) elided"));
}

#[tokio::test]
async fn truncation_metadata_distinguishes_columns_from_bytes() {
    for (columns,length,reason) in [(8,20,"columns"),(0,maho_codemode::output::streaming_output::DEFAULT_MAX_BYTES+10,"bytes")] {
        let mut collector=EvalOutputCollector::new(EvalOutputOptions {artifact_path:None,head_bytes:0,max_columns:columns,provider:None,api:None,image_sdk:Arc::new(Sdk)});
        collector.push(&format!("{}\n","x".repeat(length))).await.unwrap();
        let result=collector.finish().await.unwrap();
        let meta=result.meta.unwrap();
        assert_eq!(meta["truncatedBy"],reason);
        if columns>0 {assert_eq!(meta["maxColumns"],8);assert!(meta.get("maxBytes").is_none());}
        else {assert_eq!(meta["maxBytes"],maho_codemode::output::streaming_output::DEFAULT_MAX_BYTES);}
    }
}

#[tokio::test]
async fn result_builder_finalizes_machine_details() {
    use maho_codemode::tool::{cell_runtime::{CellState, CellResultBuilder}, types::{EvalToolInput, EvalLanguage}};
    let state = CellState {
        input:EvalToolInput {language:EvalLanguage::Py,code:"print(42)".into(),summary:"compute result".into(),action:None,timeout:None,on_timeout:None,reset:None},
        runtime:None,started_at:0.0,run_started_at:Some(1.0),queued_behind:None,on_update:None,
        tool_calls:vec![],tool_call_metrics:vec![],status_events:vec![],active:true,output:String::new(),phase:None,error:None,duration_ms:0.0,status:"running".into(),
    };
    let mut builder = CellResultBuilder::new(state, EvalOutputOptions {artifact_path:None,head_bytes:20480,max_columns:768,provider:None,api:None,image_sdk:Arc::new(Sdk)});
    builder.push("42\n").await.unwrap();
    let result = builder.finalize(&serde_json::json!({"type":"result","ok":true,"durationMs":12})).await.unwrap();
    assert_eq!(result.details["cells"][0]["status"], "complete");
    assert_eq!(result.details["cells"][0]["output"], "42");
    assert_eq!(result.details["durationMs"],12.0);
    assert!(result.details.get("isError").is_none());
    assert_eq!(result.details["toolCallCount"],0);
}

#[tokio::test]
async fn cell_handler_dispatches_native_tools_and_blocks_recursive_eval() {
    use maho_codemode::tool::{cell_runtime::{CellState, CellResultBuilder}, cell_handler::{CellHandler,CellBridgeRuntime},types::{EvalToolInput,EvalLanguage}};
    struct Executor;
    impl maho_codemode::bridges::output_bridge::OutputExecuteTool for Executor {
        fn execute_tool<'a>(&'a self, name:&'a str, _:serde_json::Value, _:maho_ext_api::ExecuteToolOptions) -> maho_ext_api::ExecuteToolFuture<'a> {
            Box::pin(async move {assert_eq!(name,"echo");Ok(maho_ext_api::AgentToolResult::text("reply"))})
        }
    }
    let state=CellState {input:EvalToolInput {language:EvalLanguage::Js,code:"tool.echo({})".into(),summary:"dispatch tool".into(),action:None,timeout:None,on_timeout:None,reset:None},runtime:None,started_at:0.0,run_started_at:None,queued_behind:None,on_update:None,tool_calls:vec![],tool_call_metrics:vec![],status_events:vec![],active:true,output:String::new(),phase:None,error:None,duration_ms:0.0,status:"running".into()};
    let builder=CellResultBuilder::new(state,EvalOutputOptions {artifact_path:None,head_bytes:20480,max_columns:768,provider:None,api:None,image_sdk:Arc::new(Sdk)});
    let (sender,mut replies)=tokio::sync::mpsc::unbounded_channel();
    let controller=maho_ai::utils::abort::AbortController::new();
    let mut handler=CellHandler::new(builder,CellBridgeRuntime {executor:Arc::new(Executor),tools:None,complete:None,settings:maho_codemode::config::settings::CodemodeSettings::default(),signal:controller.signal(),deliver_reply:Arc::new(move |reply|{sender.send(reply).expect("reply receiver");})});
    handler.handle(&serde_json::json!({"type":"tool-call","toolName":"echo","callId":"call","args":{}})).await.unwrap();
    let reply=replies.recv().await.unwrap();
    assert_eq!(reply["value"]["text"],"reply");
    assert_eq!(handler.builder.state.tool_calls.len(),1);
    assert_eq!(handler.builder.state.tool_call_metrics[0].ok,Some(true));
    handler.handle(&serde_json::json!({"type":"tool-call","toolName":"eval","callId":"recursive","args":{}})).await.unwrap();
    assert_eq!(replies.recv().await.unwrap()["ok"],false);
    assert_eq!(handler.builder.state.tool_calls[1]["ok"],false);
}
