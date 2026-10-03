mod support;
use std::{collections::BTreeMap, sync::{Arc, Mutex, Condvar, mpsc}, time::Duration};
use maho_ext_api::*;
use maho_omo_task::{component::TaskComponent, engine::{compose_task_engine, ComposeTaskEngineDeps}, dag_engine::TaskDagEngine};
use senpi_task::{host::HostError, manager::{ManagedChildHandle, ManagedChildListener, Unsubscribe, types::{ManagedRunner, ManagedRunnerResult, ManagedStartSpec, ManagedRunners}}, runners::RunnerOutcome};
use serde_json::json;

struct Child { id:String, listeners:Arc<Mutex<BTreeMap<u64,ManagedChildListener>>>, done:Mutex<Option<RunnerOutcome>>, signal:Condvar }
impl Child {
    fn emit(&self) { let listeners=self.listeners.lock().expect("listeners").values().cloned().collect::<Vec<_>>(); for listener in listeners { listener(&senpi_task::shared::ManagedChildEvent { event_type:"tool_execution_start".into(),tool_name:Some("read".into()),args:Some(json!({"path":"rebind"})),..Default::default() }); } }
    fn complete(&self) { *self.done.lock().expect("done")=Some(RunnerOutcome::completed("node completed")); self.signal.notify_all(); }
}
impl ManagedChildHandle for Child {
    fn task_id(&self)->&str { &self.id }
    fn session_id(&self)->Option<String> { Some("child".into()) }
    fn pid(&self)->Option<i64> { None }
    fn steer(&self,_:&str)->Result<(),HostError> { Ok(()) }
    fn follow_up(&self,_:&str)->Result<(),HostError> { Ok(()) }
    fn abort(&self)->Result<(),HostError> { *self.done.lock().expect("done")=Some(RunnerOutcome::Cancelled); self.signal.notify_all(); Ok(()) }
    fn subscribe(&self,listener:ManagedChildListener)->Unsubscribe { let mut listeners=self.listeners.lock().expect("listeners"); let id=listeners.keys().next_back().copied().unwrap_or(0)+1; listeners.insert(id,listener); let listeners=self.listeners.clone(); Box::new(move || { listeners.lock().expect("listeners").remove(&id); }) }
    fn wait_for_outcome(&self)->RunnerOutcome { let (mut done,timeout)=self.signal.wait_timeout_while(self.done.lock().expect("done"),Duration::from_secs(10),|done| done.is_none()).expect("settlement"); assert!(!timeout.timed_out()); done.take().expect("outcome") }
    fn last_assistant_text(&self)->Option<String> { None }
    fn dispose(&self)->Result<(),HostError> { self.listeners.lock().expect("listeners").clear(); Ok(()) }
}
struct Runner(mpsc::Sender<Arc<Child>>);
struct Cleanup {
    component:Arc<TaskComponent>, scheduler:Arc<senpi_task::dag::scheduler::DagSchedulerContext>, run:String,
    worker:Option<std::thread::JoinHandle<Result<senpi_task::dag::types::DagRunRecordV1,senpi_task::dag::store::DagStoreError>>>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(worker)=self.worker.take() {
            if let Err(error)=self.scheduler.cancel(&self.run,Some("fixture cleanup")) { eprintln!("lifecycle fixture cancel failed: {error}"); }
            match worker.join() { Ok(Ok(_))=>{},Ok(Err(error))=>eprintln!("lifecycle fixture scheduler failed: {error}"),Err(_)=>eprintln!("lifecycle fixture scheduler panicked") }
        }
        self.component.dispose();
        for entry in self.component.engine.manager.list(&senpi_task::manager::types::ListScope::All) { self.component.engine.manager.forget(&entry.record.task_id); }
    }
}
impl ManagedRunner for Runner { fn start(&self,spec:&ManagedStartSpec)->ManagedRunnerResult { let child=Arc::new(Child { id:spec.task_id.clone(),listeners:Arc::default(),done:Mutex::new(None),signal:Condvar::new() }); self.0.send(child.clone()).expect("child"); Ok(child) } }
struct Actions;
struct ForeignSession;
impl ToolSessionManager for ForeignSession { fn session_id(&self)->&str { "foreign" } fn session_file(&self)->Option<&std::path::Path> { None } }
impl SessionManager for ForeignSession { fn get_entries(&self)->Vec<SessionEntry> { vec![] } fn get_branch(&self)->Vec<SessionEntry> { vec![] } fn get_leaf_id(&self)->Option<String> { None } fn get_session_name(&self)->Option<String> { None } }
impl ExtensionActions for Actions {
    fn send_message(&self,_:CustomMessage,_:SendMessageOptions)->Result<(),ExtensionFailure> { Ok(()) }
    fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> { Ok(()) }
    fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure> { Ok(()) }
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(vec![]) }
}
async fn dispatch(api:&ExtensionApi,kind:EventKind,event:&mut ExtensionEvent,ctx:&ExtensionContext) { for handler in &api.registered.handlers[&kind] { handler(event,ctx).await.expect("registered lifecycle"); } }
fn start_event()->ExtensionEvent { ExtensionEvent::SessionStart(SessionStartEvent { reason:SessionReason::Resume,initial_model_provenance:None,previous_session_file:None }) }
async fn reload_veto(api:&ExtensionApi,ctx:&ExtensionContext)->bool {
    let mut event=ExtensionEvent::SessionBeforeReload;
    for handler in &api.registered.handlers[&EventKind::SessionBeforeReload] {
        if matches!(handler(&mut event,ctx).await.expect("reload handler"),EventResult::SessionBefore(SessionBeforeEventResult { cancel:Some(true),.. })) { return true; }
    }
    false
}
#[derive(Default)]
struct Timers { next:Mutex<u64>, callbacks:Mutex<BTreeMap<u64,(u64,Box<dyn FnOnce()+Send>)>> }
impl maho_omo_task::status_ui::StatusUiTimers for Timers {
    fn set(&self,callback:Box<dyn FnOnce()+Send>,ms:u64)->u64 { let mut next=self.next.lock().expect("next"); *next+=1; self.callbacks.lock().expect("timers").insert(*next,(ms,callback)); *next }
    fn clear(&self,id:u64) { self.callbacks.lock().expect("timers").remove(&id); }
}
impl Timers {
    fn fire(&self,ms:u64) { let callbacks={ let mut timers=self.callbacks.lock().expect("timers"); let ids=timers.iter().filter(|(_, (delay,_))| *delay==ms).map(|(id,_)| *id).collect::<Vec<_>>(); ids.into_iter().filter_map(|id| timers.remove(&id)).collect::<Vec<_>>() }; for (_,callback) in callbacks { callback(); } }
    fn count(&self)->usize { self.callbacks.lock().expect("timers").len() }
}

#[tokio::test]
async fn registered_rebind_restores_existing_live_child_subscription() {
    live_lifecycle(false,false).await;
}
#[tokio::test]
async fn registered_shutdown_clears_live_activity_and_all_bridge_timers() {
    live_lifecycle(true,false).await;
}
#[tokio::test]
async fn registered_terminal_node_releases_activity_while_peer_remains_live() {
    live_lifecycle(false,true).await;
}
async fn live_lifecycle(shutdown_live:bool,peer_live:bool) {
    let root=tempfile::tempdir().expect("root"); let (created,children)=mpsc::channel(); let runner=Arc::new(Runner(created));
    let engine=compose_task_engine(ComposeTaskEngineDeps { cwd:root.path().into(),config:json!({}),runners:ManagedRunners { in_process:runner.clone(),process:runner },actions:Arc::new(Actions),coordinator:None,resolve_registry:Arc::new(|| None) });
    let dag=Arc::new(TaskDagEngine::compose(&engine,None).expect("dag")); let mut api=support::api();
    let task_timers=Arc::new(Timers::default());
    let component=TaskComponent::register_with_status_timers(&mut api,engine,Default::default(),senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps { state_dir:senpi_task::store::StateDirConfig { project_dir:root.path().into(),task_state_dir:None },team_bounds:senpi_task::team::runtime_config::TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 },load_runtime_state:None },false,task_timers.clone()).expect("register").expect("component");
    let mut ctx=support::context(); ctx.cwd=root.path().into(); dispatch(&api,EventKind::SessionStart,&mut start_event(),&ctx).await;
    let status_timers=Arc::new(Timers::default()); let rpc_timers=Arc::new(Timers::default());
    dag.register_rpc_with_timers(&mut api,&component,status_timers.clone(),rpc_timers.clone());
    let activity=Arc::new(Mutex::new(Vec::<JsonValue>::new())); let received=activity.clone();
    let activity_subscription=api.events.on("senpi:extension-rpc-event",Arc::new(move |event| { if event["name"]=="omo.dag.activity" { received.lock().expect("activity").push(event["data"].clone()); } }));
    let mut definition=senpi_task::dag::graph::DagDefinition { key:"held".into(),name:"held".into(),nodes:vec![senpi_task::dag::graph::DagNodeInput { id:"one".into(),prompt:"hold".into(),target:senpi_task::dag::types::DagNodeTarget::SubagentType { subagent_type:"explore".into(),model:Some("faux/native".into()) },label:None,depends_on:None,task_summary:None,description:None,load_skills:None }] };
    if peer_live { let mut peer=definition.nodes[0].clone(); peer.id="two".into(); definition.nodes.push(peer); }
    let run=dag.manager.start(senpi_task::dag::manager::DagStartParams { definition,parent_session_id:"session".into(),root_session_id:"session".into() }).expect("start").snapshot.run_id;
    dispatch(&api,EventKind::SessionStart,&mut start_event(),&ctx).await;
    let (attached,attachment)=mpsc::channel(); let subscription=senpi_task::dag::journal::subscribe_dag_journal(&dag.store,&run,Arc::new(move |event| { if matches!(event.payload,senpi_task::dag::types::DagRunEventPayload::NodeTransitioned { to:senpi_task::dag::types::DagNodeState::Running,.. }) { attached.send(()).expect("attached"); } }));
    let scheduler=dag.scheduler(&component.engine,&run,"session").expect("scheduler"); let running=scheduler.clone(); let worker=std::thread::spawn(move || running.run());
    let mut cleanup=Cleanup { component:component.clone(),scheduler:scheduler.clone(),run:run.clone(),worker:Some(worker) };
    let child=children.recv_timeout(Duration::from_secs(10)).expect("child"); attachment.recv_timeout(Duration::from_secs(10)).expect("attachment");
    let peer=if peer_live { let peer=children.recv_timeout(Duration::from_secs(10)).expect("peer"); attachment.recv_timeout(Duration::from_secs(10)).expect("peer attachment"); Some(peer) } else { None };
    component.sync();
    assert!(reload_veto(&api,&ctx).await,"registered reload must veto a resident running child");
    let before=child.listeners.lock().expect("listeners").len(); child.emit(); rpc_timers.fire(150);
    assert_eq!(activity.lock().expect("activity").len(),1);
    dispatch(&api,EventKind::SessionBeforeSwitch,&mut ExtensionEvent::SessionBeforeSwitch { reason:SessionReason::Resume,target_session_file:None },&ctx).await;
    let detached=child.listeners.lock().expect("listeners").len();
    assert_eq!(status_timers.count(),0); assert_eq!(rpc_timers.count(),0);
    child.emit(); rpc_timers.fire(150); assert_eq!(activity.lock().expect("activity").len(),1,"detached child cannot emit old-session telemetry");
    let mut foreign=support::context(); foreign.cwd=root.path().into(); foreign.session_manager=Arc::new(ForeignSession);
    dispatch(&api,EventKind::SessionStart,&mut start_event(),&foreign).await;
    let foreign_listeners=child.listeners.lock().expect("listeners").len();
    child.emit(); rpc_timers.fire(150); assert_eq!(activity.lock().expect("activity").len(),1,"foreign session cannot receive the original session's child activity");
    dispatch(&api,EventKind::SessionBeforeSwitch,&mut ExtensionEvent::SessionBeforeSwitch { reason:SessionReason::Resume,target_session_file:None },&foreign).await;
    dispatch(&api,EventKind::SessionStart,&mut start_event(),&ctx).await;
    let rebound=child.listeners.lock().expect("listeners").len(); child.emit(); rpc_timers.fire(150);
    { let events=activity.lock().expect("activity"); assert_eq!(events.len(),2,"exactly one activity event per emit after rebind"); assert_eq!(events[1]["taskId"],child.id); assert_eq!(events[1]["runId"],run); }
    if let Some(peer)=&peer {
        let (terminal,transition)=mpsc::channel(); let expected=child.id.clone(); let node=dag.manager.record(&run,"session").expect("record").nodes.into_iter().find(|node| node.task_id.as_deref()==Some(expected.as_str())).expect("child node").id;
        let terminal_subscription=senpi_task::dag::journal::subscribe_dag_journal(&dag.store,&run,Arc::new(move |event| { if matches!(&event.payload,senpi_task::dag::types::DagRunEventPayload::NodeTransitioned { node_id,to:senpi_task::dag::types::DagNodeState::Completed,.. } if node_id==&node) { terminal.send(()).expect("terminal signal"); } }));
        child.complete(); transition.recv_timeout(Duration::from_secs(10)).expect("node terminal"); terminal_subscription();
        let peer_before=peer.listeners.lock().expect("peer listeners").len();
        child.emit(); rpc_timers.fire(150); assert_eq!(activity.lock().expect("activity").len(),2,"terminal node is silent while another node remains live");
        peer.emit(); rpc_timers.fire(150); assert_eq!(activity.lock().expect("activity").len(),3,"live peer retains activity");
        assert!(peer_before>0); assert!(!dag.manager.record(&run,"session").expect("live run").status.is_terminal());
    }
    let expected_activity=if peer_live { 3 } else { 2 };
    if shutdown_live {
        child.emit();
        dispatch(&api,EventKind::SessionShutdown,&mut ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None }),&ctx).await;
        assert_eq!(status_timers.count(),0,"live shutdown removes status timers");
        assert_eq!(rpc_timers.count(),0,"live shutdown removes heartbeat, snapshot and pending activity timers");
        child.emit(); rpc_timers.fire(150); assert_eq!(activity.lock().expect("activity").len(),2,"shutdown must suppress pending and subsequent child telemetry");
    }
    scheduler.cancel(&run,Some("test cleanup")).expect("cancel"); cleanup.worker.take().expect("worker").join().expect("worker").expect("run"); subscription();
    component.engine.manager.wait_for(&child.id,None,Some(Duration::from_secs(10))).expect("cancelled child settlement");
    component.sync();
    assert!(!reload_veto(&api,&ctx).await,"registered reload must allow the settled cancelled child");
    assert!(child.listeners.lock().expect("listeners").is_empty(),"cancelled manager child must release every child listener");
    child.emit(); rpc_timers.fire(150); assert_eq!(activity.lock().expect("activity").len(),expected_activity,"terminal node must not retain activity delivery");
    // Terminal runs retain journal subscriptions until detach, but pending timers
    // must settle without rearming once the run has no live node.
    rpc_timers.fire(50); rpc_timers.fire(15000);
    status_timers.fire(250); status_timers.fire(1000);
    assert_eq!(rpc_timers.count(),0,"cancelled run cannot rearm RPC timers");
    assert_eq!(status_timers.count(),0,"cancelled run cannot rearm live status timers");
    component.dispose(); for entry in component.engine.manager.list(&senpi_task::manager::types::ListScope::All) { component.engine.manager.forget(&entry.record.task_id); }
    dispatch(&api,EventKind::SessionShutdown,&mut ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None }),&ctx).await;
    assert_eq!(status_timers.count(),0); assert_eq!(rpc_timers.count(),0); assert!(child.listeners.lock().expect("listeners").is_empty());
    assert_eq!(task_timers.count(),0,"shutdown removes component status timers");
    assert_eq!(before-detached,2,"before-switch removes task RPC and DAG activity listeners"); assert_eq!(rebound,before,"rebind restores the same existing child listeners without a fresh attach event");
    assert_eq!(foreign_listeners,detached,"foreign session cannot rebind an owned child listener");
    drop(cleanup); drop(activity_subscription); drop(api); drop(dag); drop(component); drop(child); root.close().expect("cleanup");
}
