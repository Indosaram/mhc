use maho_omo_task::dag_runtime::dag_activity_payload;
use senpi_task::{progress::{create_child_progress,ChildProgressTarget},shared::ManagedChildEvent};
use serde_json::json;
#[test] fn accepted_child_tool_event_projects_exact_unsequenced_dag_activity() {
    let mut progress=create_child_progress("st_00000001",ChildProgressTarget::default(),1000,|| 1000);
    assert!(progress.accept(&ManagedChildEvent { event_type:"tool_execution_start".into(),tool_name:Some("read".into()),tool_call_id:Some("call".into()),args:Some(json!({"path":"src/lib.rs"})),..Default::default() }));
    let details=progress.details(); let payload=dag_activity_payload("run","node","st_00000001","1970-01-01T00:00:01.000Z",&details);
    assert_eq!(payload["schemaVersion"],1); assert_eq!(payload["runId"],"run"); assert_eq!(payload["nodeId"],"node"); assert_eq!(payload["taskId"],"st_00000001"); assert_eq!(payload["activity"],details.progress.activity); assert_eq!(payload["currentTool"],details.current_tool.expect("tool")); assert_eq!(payload["turns"],json!(details.turns)); assert_eq!(payload["toolCalls"],json!(details.tool_calls.expect("calls"))); assert!(payload.get("seq").is_none()); assert!(payload.get("lastAssistantLine").is_none());
}
