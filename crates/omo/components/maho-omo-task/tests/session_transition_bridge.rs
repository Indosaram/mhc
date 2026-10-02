use std::sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}};
use maho_omo_task::{runtime_context::TaskRuntimeContext,session_transition_bridge::SessionTransitionBridge};
use senpi_task::{completion::{ParentNotifier,ParentNotifierMessage,CompletionNotifierDeps,create_completion_notifier,CompletionRequest,TransitionReason,FlushResult,ParentState},host::HostError,store::{TaskRecordStore,StateDirConfig},state::{TaskRecordInput,TaskStatus,create_task_record}};
struct Parent(Arc<AtomicUsize>); impl ParentNotifier for Parent { fn enqueue(&self,_:&ParentNotifierMessage)->Result<(),HostError> { self.0.fetch_add(1,Ordering::SeqCst); Ok(()) } }
#[test]
fn buffered_completion_flushes_only_to_same_session() {
    for (reason,replacement,expected_count) in [(TransitionReason::SessionSwitching,"session-a",1),(TransitionReason::SessionSwitching,"session-b",0),(TransitionReason::Compacting,"session-a",1)] {
        let root=tempfile::tempdir().expect("root"); let store=TaskRecordStore::new(&StateDirConfig { project_dir:root.path().into(),task_state_dir:None }); let mut record=create_task_record(TaskRecordInput { parent_session_id:"session-a".into(),root_session_id:"session-a".into(),..Default::default() },Some(1)).expect("record"); record.status=TaskStatus::Completed; record.final_response=Some("done".into()); store.save(&record).expect("save");
        let delivered=Arc::new(AtomicUsize::new(0)); let notifier=create_completion_notifier(CompletionNotifierDeps::new(Arc::new(Parent(delivered.clone())),Arc::new(store.clone()))); let runtime=Arc::new(Mutex::new(TaskRuntimeContext::new(root.path().into()))); let mut bridge=SessionTransitionBridge::new(runtime.clone(),notifier.clone());
        bridge.mark(reason,Some("session-a")); notifier.notify_terminal(&CompletionRequest { record:record.clone(),parent_state:runtime.lock().expect("runtime").parent_state(),run_in_background:true,tokens:None }).expect("buffer"); assert_eq!(delivered.load(Ordering::SeqCst),0);
        let result=bridge.resolve(Some(replacement)).expect("resolve"); assert_eq!(result,Some(if expected_count==1 { FlushResult::Flushed(1) } else { FlushResult::Dropped(1) })); assert_eq!(delivered.load(Ordering::SeqCst),expected_count); assert_eq!(runtime.lock().expect("runtime").parent_state(),ParentState::Idle); assert_eq!(store.load(&record.task_id).expect("load").expect("record").notification.notified_epoch,if expected_count==1 { 0 } else { -1 });
    }
}
#[test]
fn transition_marking_preserves_reason_and_empty_release_is_idempotent() {
    let root=tempfile::tempdir().expect("root"); let store=TaskRecordStore::new(&StateDirConfig { project_dir:root.path().into(),task_state_dir:None }); let notifier=create_completion_notifier(CompletionNotifierDeps::new(Arc::new(Parent(Arc::new(AtomicUsize::new(0)))),Arc::new(store))); let runtime=Arc::new(Mutex::new(TaskRuntimeContext::new(root.path().into()))); let mut bridge=SessionTransitionBridge::new(runtime.clone(),notifier);
    for (reason,state) in [(TransitionReason::Compacting,ParentState::Compacting),(TransitionReason::SessionSwitching,ParentState::SessionSwitching),(TransitionReason::SessionShutdown,ParentState::SessionShutdown)] { bridge.mark(reason,Some("session")); assert_eq!(runtime.lock().expect("runtime").parent_state(),state); assert_eq!(bridge.resolve(Some("session")).expect("release"),Some(FlushResult::Empty)); assert_eq!(bridge.resolve(Some("session")).expect("repeat"),None); }
}
