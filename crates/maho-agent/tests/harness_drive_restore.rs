use std::sync::Arc;
use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::runtime::restore::{restore_lane, restore_session};
use maho_agent::harness::session::memory::MemoryStorage;
use maho_agent::harness::session::session::{StorageBackedSession, StorageBackedSessionOptions};
use maho_agent::harness::session::types::{Session, SessionReader, SessionMetadata, Write};
use maho_agent::harness::session::values::*;
use serde_json::{Value, json};

fn session() -> StorageBackedSession { StorageBackedSession::new(SessionMetadata { id: "restore".into(), created_at: 1, storage_version: 1, cwd: None, parent_session_id: None, legacy_parent_session_path: None }, Arc::new(MemoryStorage::new(Default::default())), StorageBackedSessionOptions::default()) }
async fn commit(session: &dyn Session, writes: Vec<Write>) { let mutation = session.begin_mutation(&BACKGROUND_CONTEXT).await.expect("fixture mutation"); mutation.commit(writes, &BACKGROUND_CONTEXT).await.expect("fixture commit"); mutation.end(&BACKGROUND_CONTEXT).await; }
fn configuration() -> Value { json!({"model":{"provider":"test","modelId":"model"},"thinkingLevel":"off","activeToolNames":[]}) }
fn scope(at: &str) -> Value { json!({"at":at,"control":{"status":"running"},"settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"},"latestAssistantEntryId":null}) }
fn checkpoint() -> Value { let mut s = scope("checkpoint"); s["continuation"] = json!({"kind":"need_assistant","overflowRecoveryUsed":false}); s["triggerEntryId"] = json!("missing-trigger"); s }
async fn seed(session: &dyn Session) { commit(session, vec![Write::Value(set_value(&branch_tip("main"), Value::Null)), Write::Value(set_value(&lane_config("main"), configuration())), Write::Value(set_value(&lane_state("main"), json!({"currentOperationId":null,"lastOperationId":null,"inbox":[]})))]).await; }
async fn operation(session: &dyn Session, meta: Value, state: Value) { commit(session, vec![Write::Value(set_value(&operation_meta("op"), meta)), Write::Value(set_value(&operation_state("op"), state)), Write::Value(set_value(&lane_state("main"), json!({"currentOperationId":"op","lastOperationId":null,"inbox":[]})))]).await; }
fn meta(intent: Value) -> Value { json!({"operationId":"op","lane":"main","sourceTipId":null,"startedAt":1,"intent":intent}) }

#[tokio::test]
async fn restores_idle_latest_operation_without_reading_result() { let s = session(); seed(&s).await; s.set_value(&lane_state("main"), json!({"currentOperationId":null,"lastOperationId":"last","inbox":[]}), &BACKGROUND_CONTEXT).await.unwrap(); let state = restore_lane(&s,"main",&BACKGROUND_CONTEXT).await.unwrap(); assert_eq!(state.last_operation_id.as_deref(),Some("last")); assert!(state.operation.is_none()); }
#[tokio::test]
async fn restores_open_operation_without_interpreting_payloads() { let s = session(); seed(&s).await; operation(&s, meta(json!({"kind":"run","promptEntryIds":[]})), checkpoint()).await; let op = restore_lane(&s,"main",&BACKGROUND_CONTEXT).await.unwrap().operation.unwrap(); assert_eq!(serde_json::to_value(op.state).unwrap(),checkpoint()); }
#[tokio::test]
async fn validates_identity_lane_and_intent() { for corruption in ["identity","lane","kind"] { let s = session(); seed(&s).await; let mut m = meta(json!({"kind":"run","promptEntryIds":[]})); match corruption { "identity" => m["operationId"]=json!("different"), "lane" => m["lane"]=json!("worker"), _ => m["intent"]=json!({"kind":"navigation","targetId":null,"summarize":false}) }; operation(&s,m,checkpoint()).await; assert!(restore_lane(&s,"main",&BACKGROUND_CONTEXT).await.is_err()); } }
#[tokio::test]
async fn accepts_exact_family_neutral_reachability_matrix() {
    let run = json!({"kind":"run","promptEntryIds":[]}); let compact = json!({"kind":"compaction"});
    let summary = |boundary: Value| { let mut state=scope("summary.deciding"); state["task"]=json!({"taskId":"task","boundary":boundary}); state };
    let resume = summary(json!({"kind":"resume_checkpoint","resumeAfter":{"continuation":{"kind":"need_assistant","overflowRecoveryUsed":false},"triggerEntryId":"trigger"}}));
    let finish = summary(json!({"kind":"finish"})); let navigation = summary(json!({"kind":"commit_navigation","targetId":"target"}));
    let mut direct=scope("navigation.ready_to_commit"); direct["targetId"]=Value::Null;
    let mut target_direct=scope("navigation.ready_to_commit");target_direct["targetId"]=json!("target");
    let mut different_direct=scope("navigation.ready_to_commit");different_direct["targetId"]=json!("different");
    let cases=vec![(run.clone(),checkpoint(),true),(run.clone(),resume.clone(),true),(run.clone(),finish.clone(),false),(run,navigation.clone(),false),(compact.clone(),finish.clone(),true),(compact.clone(),resume.clone(),false),(compact.clone(),navigation.clone(),false),(compact,checkpoint(),false),(json!({"kind":"navigation","targetId":null,"summarize":false}),direct,true),(json!({"kind":"navigation","targetId":"target","summarize":false}),different_direct,false),(json!({"kind":"navigation","targetId":"target","summarize":false}),navigation.clone(),false),(json!({"kind":"navigation","targetId":"target","summarize":true}),navigation.clone(),true),(json!({"kind":"navigation","targetId":"different","summarize":true}),navigation,false),(json!({"kind":"navigation","targetId":"target","summarize":true}),target_direct,false),(json!({"kind":"navigation","targetId":"target","summarize":true}),finish,false),(json!({"kind":"navigation","targetId":"target","summarize":true}),resume,false),(json!({"kind":"navigation","targetId":"target","summarize":false}),checkpoint(),false)];
    for (intent,state,accepted) in cases { let s=session(); seed(&s).await; operation(&s,meta(intent),state.clone()).await; let restored=restore_lane(&s,"main",&BACKGROUND_CONTEXT).await; assert_eq!(restored.is_ok(),accepted,"{state}"); if accepted { assert_eq!(serde_json::to_value(restored.unwrap().operation.unwrap().state).unwrap(),state); } }
}
async fn missing_lane_value(address: ValueAddress) { let s=session(); seed(&s).await; s.delete_value(&address.0,&BACKGROUND_CONTEXT).await.expect("delete fixture value"); assert!(restore_lane(&s,"main",&BACKGROUND_CONTEXT).await.expect_err("missing lane value").message.contains(address.1)); }
struct ValueAddress(maho_agent::harness::session::values::Value, &'static str);
#[tokio::test] async fn requires_branch_tip() { missing_lane_value(ValueAddress(branch_tip("main"),"missing branch.tip")).await; }
#[tokio::test] async fn requires_lane_config() { missing_lane_value(ValueAddress(lane_config("main"),"missing lane.config")).await; }
#[tokio::test] async fn requires_lane_state() { missing_lane_value(ValueAddress(lane_state("main"),"missing lane.state")).await; }
async fn missing_operation_value(address: maho_agent::harness::session::values::Value, expected: &str) { let s=session(); seed(&s).await; operation(&s,meta(json!({"kind":"run","promptEntryIds":[]})),checkpoint()).await; s.delete_value(&address,&BACKGROUND_CONTEXT).await.expect("delete operation value"); assert!(restore_lane(&s,"main",&BACKGROUND_CONTEXT).await.expect_err("missing operation value").message.contains(expected)); }
#[tokio::test] async fn requires_current_operation_meta() { missing_operation_value(operation_meta("op"),"missing op.meta").await; }
#[tokio::test] async fn requires_current_operation_state() { missing_operation_value(operation_state("op"),"missing op.state").await; }
#[tokio::test] async fn restores_every_configured_lane_without_writing() { let s=session(); seed(&s).await; commit(&s,vec![Write::Value(set_value(&branch_tip("worker"),Value::Null)),Write::Value(set_value(&lane_config("worker"),configuration())),Write::Value(set_value(&lane_state("worker"),json!({"currentOperationId":null,"lastOperationId":null,"inbox":[]})))]).await; let before=s.get_stats(&BACKGROUND_CONTEXT).await.unwrap(); let lanes=restore_session(&s,&BACKGROUND_CONTEXT).await.unwrap(); assert_eq!(lanes.keys().map(String::as_str).collect::<Vec<_>>(),["main","worker"]); assert_eq!(s.get_stats(&BACKGROUND_CONTEXT).await.unwrap(),before); }
#[tokio::test] async fn empty_inventory_allowed_but_values_without_branch_rejected() { let s=session(); assert!(restore_session(&s,&BACKGROUND_CONTEXT).await.unwrap().is_empty()); seed(&s).await; s.delete_value(&branch_tip("main"),&BACKGROUND_CONTEXT).await.unwrap(); assert_eq!(restore_session(&s,&BACKGROUND_CONTEXT).await.unwrap_err().message,"Lane \"main\" is missing branch.tip"); }
