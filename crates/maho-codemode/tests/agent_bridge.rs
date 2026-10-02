use maho_codemode::bridges::{agent_bridge::*, output_bridge::OutputExecuteTool, schema_bridge::EvalSchemaToolInfo};
use maho_ext_api::{AgentToolResult, ExecuteToolFuture, ExecuteToolOptions};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Fixture { result: AgentToolResult, available: bool, calls: Mutex<Vec<Value>> }
impl OutputExecuteTool for Fixture {
    fn is_tool_available(&self, _: &str) -> Option<bool> { Some(self.available) }
    fn execute_tool<'a>(&'a self, _: &'a str, params: Value, options: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            self.calls.lock().expect("fixture call recorder poisoned").push(params);
            if let Some(update) = options.on_update { update(self.result.clone()); }
            Ok(self.result.clone())
        })
    }
}
fn fixture(text: &str, details: Value) -> Fixture {
    let mut result = AgentToolResult::text(text); result.details = details;
    Fixture { result, available: true, calls: Mutex::new(vec![]) }
}

#[tokio::test]
async fn isolation_cache_is_shared_by_executor_not_cell() {
    let executor:Arc<dyn OutputExecuteTool>=Arc::new(fixture("ok",json!({})));
    let bridge=AgentBridge::for_executor(&executor);
    let next=AgentBridge::for_executor(&executor);
    assert!(Arc::ptr_eq(&bridge,&next));
    let other:Arc<dyn OutputExecuteTool>=Arc::new(fixture("ok",json!({})));
    assert!(!Arc::ptr_eq(&bridge,&AgentBridge::for_executor(&other)));
    let owner=Arc::downgrade(&executor);
    drop(executor);
    assert!(owner.upgrade().is_none(),"capability cache must not retain executor");
}
async fn invoke(args: Value, fixture: &Fixture) -> Result<Value, AgentBridgeError> {
    AgentBridge::default().run(&args, AgentBridgeOptions { call_id: "call", task_tool_name: "task", executor: fixture, tools: None, execute_options: ExecuteToolOptions::default(), emit_status: None }).await
}
#[tokio::test] async fn schema_mapping_and_json_parsing() {
    let fixture = fixture("{\"answer\":42}", json!({}));
    let value = invoke(json!({"prompt":"solve","agent":"reviewer","model":"slow","label":"lane","schema":{"type":"object"}}), &fixture).await.unwrap();
    assert_eq!(value["data"], json!({"answer":42}));
    assert_eq!(fixture.calls.lock().unwrap()[0], json!({"prompt":"solve\n\nRespond ONLY with JSON matching this JSON-Schema:\n{\"type\":\"object\"}","subagent_type":"reviewer","model":"slow","name":"lane","run_in_background":false}));
}
#[tokio::test] async fn invalid_arguments_never_execute() {
    let fixture = fixture("unused", json!({}));
    for args in [json!({}),json!({"prompt":""}),json!({"prompt":"x","extra":true})] { assert!(matches!(invoke(args,&fixture).await, Err(AgentBridgeError::Arguments(_)))); }
    assert!(fixture.calls.lock().unwrap().is_empty());
}
#[tokio::test] async fn unavailable_never_executes() {
    let mut fixture = fixture("unused", json!({})); fixture.available = false;
    assert!(matches!(invoke(json!({"prompt":"x"}), &fixture).await,Err(AgentBridgeError::Unavailable(_))));
    assert!(fixture.calls.lock().unwrap().is_empty());
}
#[tokio::test] async fn valid_handle_comes_from_details_not_prose() {
    let fixture = fixture("st_deadbeef",json!({"task_id":"st_123abc","run_epoch":3}));
    let value = invoke(json!({"prompt":"x","handle":true}),&fixture).await.unwrap();
    assert_eq!(value["handle"], "agent://st_123abc"); assert_eq!(value["run_epoch"],3);
}
#[tokio::test] async fn invalid_handle_and_task_error_rejected() {
    for details in [json!({}),json!({"task_id":"st_AB","run_epoch":0}),json!({"task_id":"st_ab","run_epoch":-1}),json!({"task_id":"st_ab","run_epoch":0,"isError":true})] {
        assert_eq!(invoke(json!({"prompt":"x","handle":true}), &fixture("st_ab",details)).await.unwrap_err().code(),Some("invalid_task_handle"));
    }
}
#[tokio::test] async fn unapplied_foreground_isolation_fails_but_handle_preserves_it() {
    let fixture = fixture("{}",json!({"task_id":"st_ab","run_epoch":0,"isolation":{"changes_applied":false,"patch_path":"/artifact/task.patch"}}));
    assert_eq!(invoke(json!({"prompt":"x"}), &fixture).await.unwrap_err().code(),Some("isolation_not_applied"));
    assert_eq!(invoke(json!({"prompt":"x","handle":true}), &fixture).await.unwrap()["details"]["isolation"]["changes_applied"],false);
}
#[tokio::test] async fn advertised_isolation_and_progress_forwarded() {
    let fixture = fixture("ok",json!({"task_id":"st_ab","status":"completed","subagent_type":"reviewer"}));
    let tools = vec![EvalSchemaToolInfo { name:"task".into(), description:None, parameters:Some(json!({"properties":{"isolated":{}}})) }];
    let events = Arc::new(Mutex::new(Vec::new())); let seen = Arc::clone(&events);
    AgentBridge::default().run(&json!({"prompt":"x","isolated":true,"apply":false,"merge":true}), AgentBridgeOptions { call_id:"call", task_tool_name:"task", executor:&fixture, tools:Some(&tools),execute_options:ExecuteToolOptions::default(),emit_status:Some(Arc::new(move |event| seen.lock().unwrap().push(event))) }).await.unwrap();
    assert_eq!(fixture.calls.lock().unwrap()[0]["merge"],"branch");
    assert_eq!(events.lock().unwrap()[0],json!({"op":"agent","id":"st_ab","status":"completed","agent":"reviewer"}));
}
#[tokio::test] async fn malformed_json_is_data_not_exception() {
    assert!(invoke(json!({"prompt":"x","schema":{}}),&fixture("not json",json!({}))).await.unwrap()["parseError"].is_string());
}
