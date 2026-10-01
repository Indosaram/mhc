use std::sync::{Arc,Mutex};
use maho_ai::types::BoxFuture;
use maho_ai::utils::assistant_message_frame::AssistantMessageFrame;
use maho_agent::harness::context::{Context,BACKGROUND_CONTEXT,with_abort_signal};
use maho_agent::harness::events::HarnessEvent;
use maho_agent::harness::agent_harness::{DriveOptions,DriveOutcome};
use maho_agent::harness::runtime::types::{Drive,LaneState,RuntimeLane};
use maho_agent::harness::runtime::restore::decode_operation_state;
use maho_agent::harness::runtime::progress::{open_frame_progress,open_tool_progress,read_assistant_frames};
use maho_agent::harness::session::memory::MemoryStorage;
use maho_agent::harness::session::session::{StorageBackedSession,StorageBackedSessionOptions};
use maho_agent::harness::session::types::*;
use maho_agent::harness::session::values::*;
use serde_json::json;

struct Lane { session: StorageBackedSession, state: Mutex<LaneState> }
impl RuntimeLane for Lane {
    fn name(&self)->&str {"main"}
    fn session(&self)->&dyn Session {&self.session}
    fn state(&self)->LaneState {self.state.lock().expect("valid progress fixture").clone()}
    fn publish_state(&self,state:LaneState){*self.state.lock().expect("valid progress fixture")=state;}
    fn emit<'a>(&'a self,_:Vec<HarnessEvent>,_:&'a Context)->BoxFuture<'a,()> {Box::pin(async {})}
}
fn fixture(at:&str)->(Arc<Lane>,Drive){fixture_with_storage(at,Arc::new(MemoryStorage::new(Default::default())))}
fn fixture_with_storage(at:&str,storage:Arc<dyn Storage>)->(Arc<Lane>,Drive){
    let configuration=json!({"model":{"provider":"test","modelId":"model"},"thinkingLevel":"off","activeToolNames":[]});
    let mut state=json!({"at":at,"control":{"status":"running"},"settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"},"latestAssistantEntryId":null});
    if at=="tools" {state["batch"]=json!({"assistantEntryId":"assistant","configuration":configuration,"turnId":"turn","calls":[{"status":"effect_pending","sourceIndex":0,"resultEntryId":"result","replay":"safe"}]});}
    else {state["generationContext"]=json!({"stepId":"step","triggerEntryId":"trigger","configuration":configuration,"streamOptions":{},"retryPolicy":{"maxAttempts":2,"baseDelayMs":1,"maxAgentDelayMs":30000},"overflowRecoveryUsed":false});state["attempt"]=json!(1);state["responseEntryId"]=json!("response");state["usageId"]=json!("usage");state["intendedOutputLimit"]=json!(100);state["contextWindow"]=json!(1000);}
    let lane=Arc::new(Lane {session:StorageBackedSession::new(SessionMetadata {id:"progress".into(),created_at:1,storage_version:1,cwd:None,parent_session_id:None,legacy_parent_session_path:None},storage,StorageBackedSessionOptions::default()),state:Mutex::new(LaneState {tip_id:None,configuration:serde_json::from_value(configuration).expect("valid progress fixture"),inbox:vec![],last_operation_id:None,operation:Some(Operation {meta:OperationMeta {operation_id:"op".into(),lane:"main".into(),source_tip_id:None,started_at:1,intent:OperationIntent::Run {prompt_entry_ids:vec![]}},state:decode_operation_state(state).expect("valid progress fixture")})})});
    (lane,Drive::new(&DriveOptions {operation_id:"op".into(),wait_for_retry:None,poll_deferred:None},&BACKGROUND_CONTEXT))
}
fn frame(text:&str)->AssistantMessageFrame {serde_json::from_value(json!({"type":"text_delta","contentIndex":0,"delta":text})).expect("valid progress fixture")}
#[tokio::test]
async fn shared_completion_settles_once_and_hides_owner_controls(){let (_,drive)=fixture("assistant.effect_pending");let outcome=DriveOutcome::WaitingRetry {operation_id:"op".into(),not_before:10};drive.settle(outcome.clone());drive.fail("late failure".into());assert_eq!(drive.completion.wait().await.unwrap(),outcome);let (_,failed)=fixture("assistant.effect_pending");failed.fail("drive failed".into());assert_eq!(failed.completion.wait().await.unwrap_err(),"drive failed");}
#[tokio::test]
async fn strips_invocation_cancellation_and_owns_policy_gate(){let controller=maho_ai::utils::abort::AbortController::new();let drive=Drive::new(&DriveOptions {operation_id:"op".into(),wait_for_retry:Some(true),poll_deferred:Some(true)},&with_abort_signal(controller.signal(),&BACKGROUND_CONTEXT));assert!(drive.context.abort_signal().is_none());assert!(drive.wait_for_retry);assert_eq!(*drive.deferred_permits.lock().unwrap(),1);drive.close_gate("closed".into());assert!(drive.gate.signal().aborted());assert!(drive.gate.admit(||()).is_err());assert_eq!(drive.completion.wait().await.unwrap_err(),"closed");}
#[tokio::test]
async fn enqueues_frames_in_order_and_seals_admission(){let(lane,drive)=fixture("assistant.effect_pending");let progress=open_frame_progress(lane.clone(),&drive,"response");progress.write(frame("a"));progress.write(frame("b"));progress.seal();progress.write(frame("late"));progress.drain().await.unwrap();assert_eq!(read_assistant_frames(&lane.session,"op","response",&BACKGROUND_CONTEXT).await.unwrap(),vec![frame("a"),frame("b")]);}
#[tokio::test]
async fn declines_queued_frame_after_projection_leaves_phase(){let(lane,drive)=fixture("assistant.effect_pending");let mutation=lane.session.begin_mutation(&BACKGROUND_CONTEXT).await.unwrap();let progress=open_frame_progress(lane.clone(),&drive,"response");progress.write(frame("late"));let mut state=lane.state();state.operation.as_mut().unwrap().state=decode_operation_state(json!({"at":"checkpoint","control":{"status":"running"},"settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"},"latestAssistantEntryId":null,"continuation":{"kind":"may_finish","includeFinalAssistant":true},"triggerEntryId":"response"})).unwrap();lane.publish_state(state);mutation.end(&BACKGROUND_CONTEXT).await;progress.drain().await.unwrap();assert!(lane.session.read_list(&pending_assistant_frames("op","response"),None,&BACKGROUND_CONTEXT).await.unwrap().is_empty());}
#[tokio::test]
async fn declines_queued_frame_after_terminal_publication(){let(lane,drive)=fixture("assistant.effect_pending");let mutation=lane.session.begin_mutation(&BACKGROUND_CONTEXT).await.unwrap();let progress=open_frame_progress(lane.clone(),&drive,"response");progress.write(frame("late"));let mut state=lane.state();state.operation=None;lane.publish_state(state);mutation.end(&BACKGROUND_CONTEXT).await;progress.drain().await.unwrap();assert!(lane.session.read_list(&pending_assistant_frames("op","response"),None,&BACKGROUND_CONTEXT).await.unwrap().is_empty());}
#[tokio::test]
async fn replaces_tool_checkpoints_in_invocation_order(){let(lane,drive)=fixture("tools");let progress=open_tool_progress(lane.clone(),&drive,"turn",0,"result");progress.write(maho_agent::types::AgentToolResult::text("first"));progress.write(maho_agent::types::AgentToolResult::text("second"));progress.drain().await.unwrap();assert_eq!(lane.session.get_value(&pending_tool_output("op","result"),&BACKGROUND_CONTEXT).await.unwrap().unwrap().value["content"],json!([{"type":"text","text":"second"}]));progress.seal();progress.drain().await.unwrap();}
#[tokio::test]
async fn drain_propagates_commit_failure(){let storage=Arc::new(maho_agent::harness::session::testing::gating_storage::GatingStorage::new(Arc::new(MemoryStorage::new(Default::default()))));let(lane,drive)=fixture_with_storage("assistant.effect_pending",storage.clone());storage.arm();let progress=open_frame_progress(lane.clone(),&drive,"response");progress.write(frame("lost"));tokio::time::timeout(std::time::Duration::from_secs(5),storage.wait_pending(1)).await.expect("commit admission").expect("pending commit");storage.discard();let error=progress.drain().await.expect_err("discarded frame commit");assert_eq!(error.message,"commit discarded");assert!(lane.session.read_list(&pending_assistant_frames("op","response"),None,&BACKGROUND_CONTEXT).await.expect("read frames").is_empty());progress.seal();assert_eq!(progress.drain().await.expect_err("retained rejection"),error);}
