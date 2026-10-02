use std::sync::Mutex;
use maho_omo_task::dag_wake::{DagWake,DagWakeRun,DagWakeInjection,DagWakeCoordinator,DagWakeFailure};
use senpi_task::{completion::ParentState,dag::types::DagNodeCounts};
#[derive(Default)] struct Coordinator { injections:Mutex<Vec<DagWakeInjection>>,calls:Mutex<Vec<&'static str>> }
impl DagWakeCoordinator for Coordinator {
    fn enqueue(&self,injection:DagWakeInjection) { self.injections.lock().expect("injections").push(injection); self.calls.lock().expect("calls").push("enqueue"); }
    fn schedule_flush(&self) { self.calls.lock().expect("calls").push("schedule"); }
    fn flush_soon(&self) { self.calls.lock().expect("calls").push("soon"); }
}
fn run(id:&str)->DagWakeRun<'_> { DagWakeRun { run_id:id,name:"release",parent_session_id:"parent" } }
fn counts()->DagNodeCounts { DagNodeCounts { total:4,completed:3,failed:1,..Default::default() } }
#[test] fn idle_failure_delivers_structured_counts_without_streaming_flush() { let mut wake=DagWake::default(); let coordinator=Coordinator::default(); wake.on_run_event(run("run"),"dag.run.failed",Some(counts()),Some(DagWakeFailure { code:"task_error",message:"compile failed",node_id:Some("build") }),ParentState::Idle,&coordinator); assert_eq!(*coordinator.calls.lock().expect("calls"),["enqueue","soon"]); let injections=coordinator.injections.lock().expect("injections"); assert_eq!(injections[0].details["firstFailure"],serde_json::json!({"code":"task_error","message":"compile failed","nodeId":"build"})); assert_eq!(injections[0].details["counts"]["total"],4); assert!(!injections[0].display); }
#[test] fn streaming_terminal_schedules_coordinator_flush() { let mut wake=DagWake::default(); let coordinator=Coordinator::default(); wake.on_run_event(run("run"),"dag.run.completed",Some(counts()),None,ParentState::Streaming,&coordinator); assert_eq!(*coordinator.calls.lock().expect("calls"),["enqueue","schedule"]); }
#[test] fn nonterminal_or_missing_counts_never_wakes() { let mut wake=DagWake::default(); let coordinator=Coordinator::default(); wake.on_run_event(run("run"),"dag.run.started",Some(counts()),None,ParentState::Idle,&coordinator); wake.on_run_event(run("run"),"dag.run.completed",None,None,ParentState::Idle,&coordinator); assert!(coordinator.calls.lock().expect("calls").is_empty()); }
#[test] fn every_session_transition_buffers_terminal_until_matching_start_once() {
    for state in [ParentState::Compacting,ParentState::SessionSwitching,ParentState::SessionShutdown] {
        let mut wake=DagWake::default(); let coordinator=Coordinator::default(); wake.on_run_event(run("run"),"dag.run.completed",Some(counts()),None,state,&coordinator);
        assert!(coordinator.calls.lock().expect("calls").is_empty()); assert_eq!(wake.buffered_count("parent"),1); wake.on_session_start(None,&coordinator); wake.on_session_start(Some("foreign"),&coordinator); assert!(coordinator.calls.lock().expect("calls").is_empty());
        wake.on_session_start(Some("parent"),&coordinator); wake.on_session_start(Some("parent"),&coordinator); assert_eq!(*coordinator.calls.lock().expect("calls"),["enqueue","soon"]); assert_eq!(coordinator.injections.lock().expect("injections").len(),1); assert_eq!(wake.buffered_count("parent"),0);
    }
}
#[test] fn transition_buffer_keeps_first_insertion_order_and_latest_payload() { let mut wake=DagWake::default(); let coordinator=Coordinator::default(); for (id,status) in [("z","dag.run.completed"),("a","dag.run.failed"),("z","dag.run.cancelled")] { wake.on_run_event(run(id),status,Some(counts()),None,ParentState::SessionSwitching,&coordinator); } assert_eq!(wake.buffered_count("parent"),2); wake.on_session_start(Some("foreign"),&coordinator); assert!(coordinator.injections.lock().expect("injections").is_empty()); wake.on_session_start(Some("parent"),&coordinator); let injections=coordinator.injections.lock().expect("injections"); assert_eq!(injections.iter().map(|injection| injection.details["runId"].as_str().expect("id")).collect::<Vec<_>>(),["z","a"]); assert_eq!(injections[0].details["status"],"cancelled"); assert_eq!(wake.buffered_count("parent"),0); }
