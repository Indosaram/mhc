use std::sync::Arc;
use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::runtime::drive::terminal::{operation_cleanup_writes, operation_result_record_at};
use maho_agent::harness::runtime::restore::decode_operation_state;
use maho_agent::harness::session::memory::MemoryStorage;
use maho_agent::harness::session::session::{StorageBackedSession,StorageBackedSessionOptions};
use maho_agent::harness::session::types::*;
use maho_agent::harness::session::values::*;
use serde_json::{Value,json};

fn session() -> StorageBackedSession { StorageBackedSession::new(SessionMetadata { id:"terminal".into(),created_at:1,storage_version:1,cwd:None,parent_session_id:None,legacy_parent_session_path:None },Arc::new(MemoryStorage::new(Default::default())),StorageBackedSessionOptions::default()) }
fn scope(at: &str) -> Value { json!({"at":at,"control":{"status":"running"},"settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"},"latestAssistantEntryId":null}) }
async fn seed(s: &dyn Session) { for address in [operation_meta("op"),operation_state("op"),operation_tool_args("op","step",0),operation_tool_memo("op","invocation","memo"),operation_preparation("op","task"),pending_tool_output("op","invocation")] { s.set_value(&address,json!({"value":true}),&BACKGROUND_CONTEXT).await.expect("seed operation leftover"); } }
fn address(write: &Write) -> String { match write { Write::Value(v) => format!("value:{}:{}",v.namespace(),v.key()),Write::List(v) => format!("list:{}:{}",v.namespace(),v.key()),_=>panic!("unexpected write") } }
fn expected() -> Vec<String> { ["value:pi.op.meta:op","value:pi.op.state:op","value:pi.op.tool_args:op:step:0","value:pi.op.tool_memo:op:invocation:memo","value:pi.op.preparation:op:task","value:pi.pending.tool_output:op:invocation"].map(str::to_owned).to_vec() }
#[tokio::test]
async fn deletes_all_owned_families_live_frames_but_preserves_inbox() {
    let s=session();seed(&s).await; for id in ["steer","follow","write","next"] {s.set_value(&pending_entry(id),json!({"id":id}),&BACKGROUND_CONTEXT).await.unwrap();}
    let mut state=scope("assistant.effect_pending");state["generationContext"]=json!({"stepId":"step","triggerEntryId":"trigger","configuration":{"model":{"provider":"test","modelId":"model"},"thinkingLevel":"off","activeToolNames":[]},"streamOptions":{},"retryPolicy":{"maxAttempts":2,"baseDelayMs":1,"maxAgentDelayMs":30000},"overflowRecoveryUsed":false});state["attempt"]=json!(1);state["responseEntryId"]=json!("response");state["usageId"]=json!("usage");state["intendedOutputLimit"]=json!(100);state["contextWindow"]=json!(1000);
    s.append_list(&pending_assistant_frames("op","response"),json!({"partial":true}),&BACKGROUND_CONTEXT).await.unwrap();
    let writes=operation_cleanup_writes(&s,"op",&decode_operation_state(state).unwrap(),&BACKGROUND_CONTEXT).await.unwrap();let mut want=expected();want.push("list:pi.pending.assistant_frame:op:response".into());assert_eq!(writes.iter().map(address).collect::<Vec<_>>(),want);
    let mutation=s.begin_mutation(&BACKGROUND_CONTEXT).await.unwrap();mutation.commit(writes,&BACKGROUND_CONTEXT).await.unwrap();mutation.end(&BACKGROUND_CONTEXT).await;
    for id in ["steer","follow","write","next"] {assert!(s.get_value(&pending_entry(id),&BACKGROUND_CONTEXT).await.unwrap().is_some());}assert!(s.read_list(&pending_assistant_frames("op","response"),None,&BACKGROUND_CONTEXT).await.unwrap().is_empty());
}
#[tokio::test]
async fn deletes_staged_tool_outcomes_but_not_completed_results() {let s=session();seed(&s).await;let mut state=scope("tools");state["batch"]=json!({"assistantEntryId":"assistant","turnId":"turn","configuration":{"model":{"provider":"test","modelId":"model"},"thinkingLevel":"off","activeToolNames":[]},"calls":[{"status":"outcome_ready","sourceIndex":0,"resultEntryId":"staged","terminate":false},{"status":"completed","sourceIndex":1,"resultEntryId":"placed","terminate":false}]});let writes=operation_cleanup_writes(&s,"op",&decode_operation_state(state).unwrap(),&BACKGROUND_CONTEXT).await.unwrap();let addresses=writes.iter().map(address).collect::<Vec<_>>();assert!(addresses.contains(&"value:pi.pending.entry:staged".into()));assert!(!addresses.contains(&"value:pi.pending.entry:placed".into()));}
async fn structural_cleanup(mut state: Value) {let s=session();seed(&s).await;if state["at"]=="summary.deciding" {state["task"]=json!({"taskId":"task","reason":"manual","boundary":{"kind":"finish"}});}else {state["targetId"]=Value::Null;}let writes=operation_cleanup_writes(&s,"op",&decode_operation_state(state).expect("structural fixture"),&BACKGROUND_CONTEXT).await.expect("structural cleanup");assert_eq!(writes.iter().map(address).collect::<Vec<_>>(),expected());}
#[tokio::test] async fn deletes_leftover_compaction_families() {structural_cleanup(scope("summary.deciding")).await;}
#[tokio::test] async fn deletes_leftover_navigation_families() {structural_cleanup(scope("navigation.ready_to_commit")).await;}
fn metadata() -> OperationMeta {OperationMeta {operation_id:"op".into(),lane:"main".into(),source_tip_id:Some("source".into()),started_at:10,intent:OperationIntent::Run {prompt_entry_ids:vec!["prompt".into()]}}}
#[test] fn constructs_flat_immutable_terminal_observation() {let error=OperationError {code:"provider".into(),message:"failed".into(),details:None};let record=operation_result_record_at(&metadata(),TerminalStatus::Failed,Some("tip".into()),Some(error.clone()),20).unwrap();assert_eq!(record,OperationResultRecord {operation_id:"op".into(),kind:OperationIntentKind::Run,status:TerminalStatus::Failed,error:Some(error),from_tip_id:Some("source".into()),tip_id:Some("tip".into()),started_at:10,ended_at:20});}
#[test] fn rejects_error_on_non_failed_and_missing_error_on_failed() {let error=OperationError {code:"x".into(),message:"x".into(),details:None};for (status,error) in [(TerminalStatus::Completed,Some(error)),(TerminalStatus::Failed,None)] {assert_eq!(operation_result_record_at(&metadata(),status,Some("tip".into()),error,20).unwrap_err().message,"Only a failed operation result may carry an error");}}
