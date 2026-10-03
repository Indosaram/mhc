use maho_codemode::bridges::{reserved_dispatch::*, agent_bridge::AgentBridge, output_bridge::OutputExecuteTool, schema_bridge::EvalSchemaToolInfo};
use maho_ext_api::{AgentToolResult, ExecuteToolFuture, ExecuteToolOptions};
use serde_json::{Value,json};
struct Fixture;
impl OutputExecuteTool for Fixture {
    fn execute_tool<'a>(&'a self, _: &'a str, _: Value, _: ExecuteToolOptions) -> ExecuteToolFuture<'a> { Box::pin(async { Ok(AgentToolResult::text("transcript")) }) }
}
async fn run(name: &str, args: Value, tools: Option<&[EvalSchemaToolInfo]>) -> Result<Value, ReservedDispatchError> {
    run_reserved_tool(name,ReservedDispatchContext { call_id:"call",args:&args,executor:&Fixture,task_tool_name:"task",task_output_tool_name:"task_output",tools,execute_options:ExecuteToolOptions::default(),emit_status:None,agent_bridge:&AgentBridge::default() }).await
}
#[tokio::test] async fn routes_all_reserved_tools() {
    assert_eq!(run("__agent__",json!({"prompt":"x"}),None).await.unwrap(),json!({"text":"transcript"}));
    assert_eq!(run("__output__",json!({"ids":["st_ab"]}),None).await.unwrap(),json!("transcript"));
    assert_eq!(run("__schema__",json!({}),Some(&[])).await.unwrap(),json!({"tools":[]}));
}
#[tokio::test] async fn unavailable_catalog_and_nonreserved_name_are_errors() {
    assert!(matches!(run("__schema__",json!({}),None).await,Err(ReservedDispatchError::SchemaUnavailable)));
    assert!(matches!(run("read",json!({}),None).await,Err(ReservedDispatchError::NotReserved(_))));
    assert!(is_reserved_tool_name("__output__")); assert!(!is_reserved_tool_name("read"));
}
