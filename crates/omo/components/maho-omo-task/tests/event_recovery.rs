mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_omo_task::{event_bridge::{EventBridgeDeps,wire_event_bridge},runtime_context::TaskRuntimeContext,session_transition_bridge::SessionTransitionBridge,residency_registry::ManagerResidencyRegistry,status_ui::{TaskStatusUi,StatusUiTimers},task_rpc_bridge::wire_task_rpc_bridge,lead_poller_lifecycle::{LeadPollerLifecycleDeps,create_lead_poller_lifecycle}};
use senpi_task::{manager::{create_task_manager,types::{ManagedRunner,ManagedRunnerResult,ManagedStartSpec,ManagedRunners,TaskManagerOptions,ResolvedChildPlan}},store::{StateDirConfig,TaskRecordStore},completion::{ParentNotifier,ParentNotifierMessage,CompletionNotifierDeps,create_completion_notifier,ParentState},host::HostError,lifecycle::{create_task_lifecycle,context::LifecycleDeps,settings::TaskSettings},team::{runtime_config::{TeamTaskBounds,to_team_core_config},messaging::lead_poller_types::{LeadInjectionSink,LeadInjection}}};
struct NoLaunch; impl ManagedRunner for NoLaunch { fn start(&self,_:&ManagedStartSpec)->ManagedRunnerResult { panic!("no launch") } }
struct Parent; impl ParentNotifier for Parent { fn enqueue_with_callbacks(&self,_:&ParentNotifierMessage,callbacks:senpi_task::completion::DeliveryCallbacks)->Result<(),HostError> { callbacks.delivered(); Ok(()) } }
struct Timers; impl StatusUiTimers for Timers { fn set(&self,_:Box<dyn FnOnce()+Send>,_:u64)->u64 { 1 } fn clear(&self,_:u64) {} }
struct Sink; impl LeadInjectionSink for Sink { fn enqueue(&self,_:LeadInjection) {} }
struct Fixture { api:ExtensionApi,events:Arc<Mutex<Vec<String>>>,runtime:Arc<Mutex<TaskRuntimeContext>>,store:TaskRecordStore,_root:tempfile::TempDir }
fn fixture(mailbox_fails:bool)->Fixture {
    let root=tempfile::tempdir().expect("root"); let store=TaskRecordStore::new(&StateDirConfig { project_dir:root.path().into(),task_state_dir:None });
    let manager=Arc::new(create_task_manager(TaskManagerOptions::new(store.clone(),ManagedRunners { in_process:Arc::new(NoLaunch),process:Arc::new(NoLaunch) },Arc::new(|_| Ok(ResolvedChildPlan { model:"faux/faux".into(),..Default::default() })),root.path().to_string_lossy())));
    let registry=manager.clone(); let mut lifecycle_deps=LifecycleDeps::new(Arc::new(store.clone()),Arc::new(ManagerResidencyRegistry { get_manager:Arc::new(move || (*registry).clone()) }),TaskSettings::from_resolved(&serde_json::json!({}))); lifecycle_deps.now=Some(Arc::new(|| 1000));
    let lifecycle=Arc::new(create_task_lifecycle(lifecycle_deps)); let notifier=create_completion_notifier(CompletionNotifierDeps::new(Arc::new(Parent),Arc::new(store.clone()))); let runtime=Arc::new(Mutex::new(TaskRuntimeContext::new(root.path().into()))); let events=Arc::new(Mutex::new(Vec::new())); let mut api=support::api();
    let session=runtime.clone(); let emitted=events.clone(); let rpc=wire_task_rpc_bridge(&mut api,manager.clone(),Arc::new(move || session.lock().expect("runtime").session_id().map(str::to_owned)),store.state_dir().to_string_lossy().into_owned(),Arc::new(move |_,_| emitted.lock().expect("events").push("rpc".into()))).expect("rpc");
    let status=TaskStatusUi::new(manager.clone(),runtime.clone(),Arc::new(Timers),Arc::new(|| 1000),Arc::new(|| None));
    let config=to_team_core_config(&TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 },root.path().to_str().expect("path")).expect("config"); let path=root.path().to_path_buf();
    let pollers=create_lead_poller_lifecycle(LeadPollerLifecycleDeps { list_teams:Arc::new(|| Ok(vec![])),session_id:Arc::new(|| Some("session".into())),session_file:Arc::new(|| None),parent_state:Arc::new(|| ParentState::Idle),config,runtime_dir:Arc::new(move |id| path.join(id)),delivery_journal:None,append_event:Arc::new(|_,_| {}),sink:Arc::new(Sink),factory:None,timers:Arc::new(Timers),on_error:Arc::new(|error| panic!("{error}")) });
    let start=events.clone(); let mailbox=events.clone(); let shutdown=events.clone(); let acknowledge=events.clone(); let warning=events.clone(); let unsubscribe=events.clone(); let liveness=events.clone();
    wire_event_bridge(&mut api,Arc::new(EventBridgeDeps { runtime:runtime.clone(),manager,lifecycle,notifier:notifier.clone(),transitions:Mutex::new(SessionTransitionBridge::new(runtime.clone(),notifier)),status_ui:status,task_rpc:rpc,lead_pollers:pollers,notify_liveness:Arc::new(move |record| liveness.lock().expect("events").push(format!("liveness:{}",record.task_id))),resumption_start:Arc::new(move || { start.lock().expect("events").push("start".into()); Ok(()) }),reconcile_mailbox:Arc::new(move || { mailbox.lock().expect("events").push("mailbox".into()); if mailbox_fails { Err("fixture".into()) } else { Ok(()) } }),resumption_shutdown:Arc::new(move || { shutdown.lock().expect("events").push("shutdown".into()); Ok(()) }),acknowledge_liveness:Arc::new(move || { acknowledge.lock().expect("events").push("ack".into()); Ok(()) }),on_warning:Arc::new(move |_| warning.lock().expect("events").push("warning".into())),unsubscribe_snapshots:Mutex::new(Some(Box::new(move || unsubscribe.lock().expect("events").push("unsubscribe".into())))) }));
    Fixture { api,events,runtime,store,_root:root }
}
async fn dispatch(f:&Fixture,kind:EventKind,event:&mut ExtensionEvent) { let context=support::context(); for handler in &f.api.registered.handlers[&kind] { handler(event,&context).await.expect("dispatch"); } }
#[tokio::test]
async fn startup_mailbox_failure_is_best_effort_before_rpc_attach() {
    let f=fixture(true); let mut event=ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None }); dispatch(&f,EventKind::SessionStart,&mut event).await;
    assert_eq!(*f.events.lock().expect("events"),["start","mailbox","warning","rpc"]);
}
#[tokio::test]
async fn shutdown_unsubscribes_before_resumption_and_clears_ui() {
    let f=fixture(false); let mut event=ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None }); dispatch(&f,EventKind::SessionShutdown,&mut event).await;
    assert_eq!(*f.events.lock().expect("events"),["unsubscribe","shutdown"]); assert!(f.runtime.lock().expect("runtime").ui().is_none()); assert_eq!(f.runtime.lock().expect("runtime").parent_state(),ParentState::SessionShutdown);
}
#[tokio::test]
async fn session_switch_clears_ui_and_marks_transition() {
    let f=fixture(false); let mut event=ExtensionEvent::SessionBeforeSwitch { reason:SessionReason::New,target_session_file:None }; dispatch(&f,EventKind::SessionBeforeSwitch,&mut event).await;
    assert!(f.runtime.lock().expect("runtime").ui().is_none()); assert_eq!(f.runtime.lock().expect("runtime").parent_state(),ParentState::SessionSwitching); assert!(f.events.lock().expect("events").is_empty());
}
#[tokio::test]
async fn session_start_releases_switch_transition_before_resumption() {
    let f=fixture(false); let mut switching=ExtensionEvent::SessionBeforeSwitch { reason:SessionReason::New,target_session_file:None }; dispatch(&f,EventKind::SessionBeforeSwitch,&mut switching).await;
    let mut start=ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::New,initial_model_provenance:None,previous_session_file:None }); dispatch(&f,EventKind::SessionStart,&mut start).await;
    assert_eq!(f.runtime.lock().expect("runtime").parent_state(),ParentState::Idle); assert_eq!(*f.events.lock().expect("events"),["start","mailbox","rpc"]);
}
#[tokio::test]
async fn agent_end_acknowledges_liveness_with_captured_context() {
    let f=fixture(false); let mut event=ExtensionEvent::AgentEnd { messages:vec![],aborted:None,will_retry:None,abort_source:None }; dispatch(&f,EventKind::AgentEnd,&mut event).await;
    assert_eq!(*f.events.lock().expect("events"),["ack"]); assert_eq!(f.runtime.lock().expect("runtime").session_id(),Some("session"));
}
#[tokio::test]
async fn persisted_terminal_is_reobserved_before_mailbox_and_rpc_on_restart() {
    let f=fixture(false); let mut record=senpi_task::state::create_task_record(senpi_task::state::TaskRecordInput { parent_session_id:"session".into(),root_session_id:"session".into(),..Default::default() },Some(1)).expect("record"); record.status=senpi_task::state::TaskStatus::Completed; record.created_at="1970-01-01T00:00:01.000Z".into(); record.updated_at=record.created_at.clone(); f.store.save(&record).expect("save");
    let mut event=ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None }); dispatch(&f,EventKind::SessionStart,&mut event).await;
    assert_eq!(*f.events.lock().expect("events"),[format!("liveness:{}",record.task_id),"start".into(),"mailbox".into(),"rpc".into()]);
}
