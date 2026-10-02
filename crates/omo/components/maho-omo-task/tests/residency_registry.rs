use std::{sync::{Arc,Mutex,Condvar,atomic::{AtomicUsize,Ordering}},time::Duration};
use maho_omo_task::residency_registry::ManagerResidencyRegistry;
use senpi_task::{host::HostError,lifecycle::{ResidencyRegistry,ResidentKind},manager::{ManagedChildHandle,ManagedChildListener,Unsubscribe,create_task_manager,types::{ManagedRunner,ManagedRunnerResult,ManagedStartSpec,ManagedRunners,TaskManagerOptions,ManagerStartSpec,ResolvedChildPlan,StartResult}},runners::RunnerOutcome,store::{TaskRecordStore,StateDirConfig}};
struct Child { id:String,terminable:bool,terminated:AtomicUsize,aborted:AtomicUsize,settled:Mutex<bool>,signal:Condvar }
impl Child { fn settle(&self) { *self.settled.lock().expect("settled")=true; self.signal.notify_all(); } }
impl ManagedChildHandle for Child {
    fn task_id(&self)->&str { &self.id } fn session_id(&self)->Option<String> { Some("child".into()) } fn pid(&self)->Option<i64> { Some(4321) }
    fn steer(&self,_:&str)->Result<(),HostError> { panic!("not steer") } fn follow_up(&self,_:&str)->Result<(),HostError> { panic!("not follow up") }
    fn abort(&self)->Result<(),HostError> { self.aborted.fetch_add(1,Ordering::SeqCst); self.settle(); Ok(()) }
    fn subscribe(&self,_:ManagedChildListener)->Unsubscribe { Box::new(|| {}) }
    fn wait_for_outcome(&self)->RunnerOutcome { let (_settled,timeout)=self.signal.wait_timeout_while(self.settled.lock().expect("settled"),Duration::from_secs(10),|settled| !*settled).expect("signal"); assert!(!timeout.timed_out(),"fixture unsettled"); RunnerOutcome::Cancelled }
    fn last_assistant_text(&self)->Option<String> { None } fn dispose(&self)->Result<(),HostError> { Ok(()) }
    fn has_terminate(&self)->bool { self.terminable } fn terminate(&self)->Result<(),HostError> { self.terminated.fetch_add(1,Ordering::SeqCst); self.settle(); Ok(()) }
}
struct Runner { terminable:bool,child:Arc<Mutex<Option<Arc<Child>>>> }
impl ManagedRunner for Runner { fn start(&self,spec:&ManagedStartSpec)->ManagedRunnerResult { let child=Arc::new(Child { id:spec.task_id.clone(),terminable:self.terminable,terminated:AtomicUsize::new(0),aborted:AtomicUsize::new(0),settled:Mutex::new(false),signal:Condvar::new() }); *self.child.lock().expect("child")=Some(child.clone()); Ok(child) } }
#[test] fn rpc_resident_termination_never_aborts_and_missing_port_rejects() {
    for terminable in [true,false] {
        let root=tempfile::tempdir().expect("root"); let store=TaskRecordStore::new(&StateDirConfig { project_dir:root.path().into(),task_state_dir:None }); let slot=Arc::new(Mutex::new(None)); let runner=Arc::new(Runner { terminable,child:slot.clone() }); let manager=create_task_manager(TaskManagerOptions::new(store,ManagedRunners { in_process:runner.clone(),process:runner },Arc::new(|_| Ok(ResolvedChildPlan { model:"faux/faux".into(),..Default::default() })),root.path().to_string_lossy()));
        let StartResult::Started(task)=manager.start(&ManagerStartSpec { parent_session_id:"parent".into(),prompt:"work".into(),run_in_background:true,..Default::default() }) else { panic!("start") }; let current=manager.clone(); let registry=ManagerResidencyRegistry { get_manager:Arc::new(move || current.clone()) }; let resident=registry.get(&task.task_id).expect("resident"); assert_eq!(resident.kind(),ResidentKind::Rpc); assert_eq!(registry.entries().len(),1); let child=slot.lock().expect("child").clone().expect("child");
        assert_eq!(resident.terminate().is_ok(),terminable); assert_eq!(child.terminated.load(Ordering::SeqCst),usize::from(terminable)); assert_eq!(child.aborted.load(Ordering::SeqCst),0); child.settle(); manager.wait_for(&task.task_id,None,Some(Duration::from_secs(10))).expect("settled manager"); registry.forget(&task.task_id); assert!(registry.entries().is_empty());
    }
}
