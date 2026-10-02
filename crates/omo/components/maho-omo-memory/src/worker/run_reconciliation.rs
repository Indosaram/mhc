use std::{path::Path,future::Future,pin::Pin};
use super::{run_finalization_types::{RunFinalizationContext,ReservationRunResult},reservation_run_ledger::{ReservationRunLedger,parse_reservation_run_ledger},run_artifacts::{RunOutcome,read_run_json,run_outcome_matches_ledger},run_liveness::RunProcessVerdict};
pub type ReconcileFuture<'a>=Pin<Box<dyn Future<Output=Result<(),String>>+'a>>;
pub trait ReflectionRecoveryPort {
    fn hostname(&self)->String;
    fn pid_liveness(&self,pid:u32)->memory_core::locks::ProcessLiveness;
    fn process_start(&self,pid:u32)->Option<String>;
    fn classify(&self,pid:Option<u64>,start:Option<Option<&str>>)->RunProcessVerdict;
    fn wait_outcome<'a>(&'a self,path:&'a Path,deadline:f64)->ReconcileFuture<'a>;
    fn wait_until(&self,deadline:f64)->ReconcileFuture<'_>;
    fn signal_group(&self,pid:u64,signal:&str)->Result<(),String>;
    fn fail(&self,dir:&Path,ledger:&ReservationRunLedger,timed_out:bool)->Result<Option<ReservationRunResult>,String>;
    fn abandon(&self,dir:&Path,ledger:&ReservationRunLedger)->Result<Option<ReservationRunResult>,String>;
}
pub type RecoveryTerminalGate<'a>=dyn Fn(&Path,&str,&mut dyn FnMut()->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String>+'a;
pub struct NativeReflectionRecovery<'a>{pub context:&'a RunFinalizationContext<'a>,pub terminal_gate:&'a RecoveryTerminalGate<'a>}
impl ReflectionRecoveryPort for NativeReflectionRecovery<'_> {
    fn hostname(&self)->String{memory_core::support::host::hostname()}
    fn pid_liveness(&self,pid:u32)->memory_core::locks::ProcessLiveness{memory_core::locks::get_pid_liveness(pid)}
    fn process_start(&self,pid:u32)->Option<String>{memory_core::locks::get_process_start_identity(pid)}
    fn classify(&self,pid:Option<u64>,start:Option<Option<&str>>)->RunProcessVerdict{super::run_liveness::classify_run_process(pid.and_then(|pid|u32::try_from(pid).ok()),start)}
    fn wait_outcome<'a>(&'a self,path:&'a Path,deadline:f64)->ReconcileFuture<'a>{Box::pin(async move{super::run_sentinel::wait_for_run_sentinel(path,deadline,||(self.context.now_ms)() as f64,None).await;Ok(())})}
    fn wait_until(&self,deadline:f64)->ReconcileFuture<'_>{Box::pin(async move{super::run_liveness::wait_until(deadline,||(self.context.now_ms)() as f64).await;Ok(())})}
    fn signal_group(&self,pid:u64,signal:&str)->Result<(),String>{super::supervisor_process_identity::signal_supervisor_process_group(u32::try_from(pid).map_err(|error|error.to_string())?,signal,&Default::default()).map_err(|error|error.to_string())}
    fn fail(&self,dir:&Path,ledger:&ReservationRunLedger,timed_out:bool)->Result<Option<ReservationRunResult>,String>{super::run_finalization::fail_reservation_run(self.context,dir,ledger,timed_out,None,|operation|(self.terminal_gate)(dir,ledger.run_id(),operation))}
    fn abandon(&self,dir:&Path,ledger:&ReservationRunLedger)->Result<Option<ReservationRunResult>,String>{super::run_finalization::abandon_reservation_run(self.context,dir,ledger,|pid,start|self.classify(pid,start),|operation|(self.terminal_gate)(dir,ledger.run_id(),operation))}
}
fn matching(path:&Path,ledger:&ReservationRunLedger)->Result<bool,String> {
    if !path.exists(){return Ok(false);}
    let outcome:RunOutcome=read_run_json(path).map_err(|error|error.to_string())?;
    let attempt=ledger.value()["attempt"].as_u64().map(|attempt|u32::try_from(attempt).map_err(|error|error.to_string())).transpose()?;
    Ok(run_outcome_matches_ledger(attempt,&outcome))
}
pub async fn reconcile_run(context:&RunFinalizationContext<'_>,dir:&Path,ledger:ReservationRunLedger,recovery:&dyn ReflectionRecoveryPort)->Result<Option<ReservationRunResult>,String> {
    let outcome=dir.join("outcome.json");
    if matching(&outcome,&ledger)?{return super::run_finalization::finalize_recorded_outcome(context,dir,&ledger);}
    let launching=|ledger:&ReservationRunLedger|ledger.value()["launching"]==true&&(context.now_ms)() as f64<=ledger.value()["hardDeadlineAt"].as_f64().unwrap_or(f64::NAN);
    if launching(&ledger){return Ok(None);}
    let classify=|ledger:&ReservationRunLedger,pid:&str,start:&str|recovery.classify(ledger.value()[pid].as_u64(),ledger.value().get(start).map(serde_json::Value::as_str));
    let supervisor=classify(&ledger,"pid","processStart");
    let ledger=if matches!(supervisor,RunProcessVerdict::Alive|RunProcessVerdict::Unknown) {
        recovery.wait_outcome(&outcome,ledger.value()["deadlineAt"].as_f64().ok_or("Missing deadlineAt")?).await?;
        let refreshed=parse_reservation_run_ledger(read_run_json(&dir.join("ledger.json")).map_err(|error|error.to_string())?).map_err(|error|error.to_string())?;
        if matching(&outcome,&refreshed)?{return super::run_finalization::finalize_recorded_outcome(context,dir,&refreshed);}
        if launching(&refreshed){return Ok(None);}
        match classify(&refreshed,"pid","processStart") {
            RunProcessVerdict::Unknown|RunProcessVerdict::Absent=>return recovery.abandon(dir,&refreshed),
            RunProcessVerdict::Alive=>return Ok(None),
            RunProcessVerdict::Dead=>refreshed,
        }
    }else{ledger};
    let mut child=classify(&ledger,"childPid","childProcessStart");
    match child {RunProcessVerdict::Unknown=>return recovery.abandon(dir,&ledger),RunProcessVerdict::Dead|RunProcessVerdict::Absent=>return recovery.fail(dir,&ledger,false),RunProcessVerdict::Alive=>{}}
    recovery.wait_until(ledger.value()["hardDeadlineAt"].as_f64().ok_or("Missing hardDeadlineAt")?).await?;
    child=classify(&ledger,"childPid","childProcessStart");
    if child==RunProcessVerdict::Dead{return recovery.fail(dir,&ledger,false);}
    if child==RunProcessVerdict::Unknown{return recovery.abandon(dir,&ledger);}
    if let Some(pid)=ledger.value()["childPid"].as_u64(){recovery.signal_group(pid,"SIGTERM")?;}
    recovery.wait_until(ledger.value()["deadlineAt"].as_f64().ok_or("Missing deadlineAt")?).await?;
    child=classify(&ledger,"childPid","childProcessStart");
    if child==RunProcessVerdict::Unknown{return recovery.abandon(dir,&ledger);}
    if child==RunProcessVerdict::Alive&&let Some(pid)=ledger.value()["childPid"].as_u64(){recovery.signal_group(pid,"SIGKILL")?;child=classify(&ledger,"childPid","childProcessStart");}
    if child==RunProcessVerdict::Dead{recovery.fail(dir,&ledger,true)}else{Ok(None)}
}
pub async fn reconcile_reflection_runs(context:&RunFinalizationContext<'_>,recovery:&dyn ReflectionRecoveryPort)->Result<Vec<(String,String)>,String> {
    let mut results=vec![];
    if let Some(result)=reconcile_prelaunch(context,recovery)?{results.push(result);}
    let dir=context.identity.paths.reflection.join("runs");
    let entries=match std::fs::read_dir(&dir){Ok(entries)=>entries,Err(error)if error.kind()==std::io::ErrorKind::NotFound=>return Ok(results),Err(error)=>return Err(error.to_string())};
    let mut paths=vec![];for entry in entries{let entry=entry.map_err(|error|error.to_string())?;if entry.file_type().map_err(|error|error.to_string())?.is_dir(){paths.push(entry.path());}}
    paths.sort();
    for path in paths {
        if path.join("final.json").exists()||path.join("abandoned.json").exists()||!path.join("ledger.json").exists(){continue;}
        let ledger=parse_reservation_run_ledger(read_run_json(&path.join("ledger.json")).map_err(|error|error.to_string())?).map_err(|error|error.to_string())?;
        if let Some(result)=reconcile_run(context,&path,ledger,recovery).await?{results.push((result.run_id,result.outcome));}
    }
    Ok(results)
}
pub fn reconcile_prelaunch(context:&RunFinalizationContext<'_>,recovery:&dyn ReflectionRecoveryPort)->Result<Option<(String,String)>,String> {
    let Some(active)=context.reservation.read_state()?.active else{return Ok(None);};
    let (Some(reserved),Some(pid),Some(host))=(&active.reserved_at,active.launcher_pid,&active.launcher_hostname)else{return Ok(None);};
    let dir=context.identity.paths.reflection.join("runs").join(&active.run_id);
    if dir.join("ledger.json").exists(){return Ok(None);}
    let prelaunch=dir.join("prelaunch.json");if dir.exists()&&!prelaunch.exists(){return Ok(None);}
    let within_grace=chrono::DateTime::parse_from_rfc3339(reserved).is_ok_and(|reserved|(context.now_ms)()-reserved.timestamp_millis()<=60000);
    if within_grace||host!=&recovery.hostname(){return Ok(None);}
    let liveness=recovery.pid_liveness(pid);
    let dead=liveness==memory_core::locks::ProcessLiveness::Dead||(liveness==memory_core::locks::ProcessLiveness::Alive&&active.launcher_process_start.as_ref().is_some_and(|recorded|recovery.process_start(pid).is_some_and(|actual|recorded!=&actual)));
    if !dead{return Ok(None);}
    if prelaunch.exists() {
        let value=super::run_artifacts::parse_run_prelaunch_artifact(read_run_json(&prelaunch).map_err(|error|error.to_string())?).map_err(|error|error.to_string())?;
        if value.run_id!=active.run_id{return Err("Reflection prelaunch run id does not match reservation".into());}
        let repo=memory_core::git::GitMemoryRepo::open(&context.identity.paths.repo,&context.identity.id).map_err(|error|error.to_string())?;
        let exec=memory_core::git::exec::create_git_exec(Default::default());
        let cleanup=memory_core::reflection::discard_reflection_worktree(&repo,Path::new(&value.worktree_dir),&value.worktree_branch,exec.as_ref());
        if !cleanup.worktree_removed||!cleanup.branch_removed{return Ok(None);}
        std::fs::remove_dir_all(&dir).map_err(|error|error.to_string())?;
    }
    let transition=context.reservation.complete(&active.run_id,memory_core::reflection::ReflectionOutcome::Failed)?;
    if let (Some(run),Some(launch))=(&transition.launch,context.launch){launch(run);}
    Ok(Some((active.run_id,"failed".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Reservation;
    impl super::super::runner_types::ReflectionReservationPort for Reservation {
        fn read_state(&self)->Result<memory_core::reflection::ReservationState,String>{Ok(Default::default())}
        fn complete(&self,_:&str,_:memory_core::reflection::ReflectionOutcome)->Result<memory_core::reflection::CompletionResult,String>{panic!("no reservation completion")}
    }
    struct Recovery {verdicts:std::cell::RefCell<std::collections::VecDeque<RunProcessVerdict>>,calls:std::cell::RefCell<Vec<String>>}
    impl ReflectionRecoveryPort for Recovery {
        fn hostname(&self)->String{"host".into()}
        fn pid_liveness(&self,_:u32)->memory_core::locks::ProcessLiveness{panic!("no active prelaunch")}
        fn process_start(&self,_:u32)->Option<String>{panic!("no active prelaunch")}
        fn classify(&self,_:Option<u64>,_:Option<Option<&str>>)->RunProcessVerdict{self.verdicts.borrow_mut().pop_front().expect("scripted liveness")}
        fn wait_outcome<'a>(&'a self,_:&'a Path,deadline:f64)->ReconcileFuture<'a>{self.calls.borrow_mut().push(format!("outcome:{deadline}"));Box::pin(async{Ok(())})}
        fn wait_until(&self,deadline:f64)->ReconcileFuture<'_>{self.calls.borrow_mut().push(format!("wait:{deadline}"));Box::pin(async{Ok(())})}
        fn signal_group(&self,pid:u64,signal:&str)->Result<(),String>{self.calls.borrow_mut().push(format!("signal:{pid}:{signal}"));Ok(())}
        fn fail(&self,_:&Path,_:&ReservationRunLedger,timed_out:bool)->Result<Option<ReservationRunResult>,String>{self.calls.borrow_mut().push(format!("fail:{timed_out}"));Ok(None)}
        fn abandon(&self,_:&Path,_:&ReservationRunLedger)->Result<Option<ReservationRunResult>,String>{self.calls.borrow_mut().push("abandon".into());Ok(None)}
    }
    fn ledger(root:&Path)->ReservationRunLedger {
        let value=serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"missing","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"missing/.git","gitFileSnapshot":"gitdir: missing","commonConfigPath":"config","commonConfigSnapshot":null,"pid":42,"processStart":"supervisor","childPid":43,"childProcessStart":"child"});
        super::super::run_artifacts::write_run_json_atomic(&root.join("ledger.json"),&value,0o600).unwrap();parse_reservation_run_ledger(value).unwrap()
    }
    #[tokio::test]
    async fn dead_supervisor_live_child_receives_ordered_deadlines_and_signals() {
        let root=tempfile::tempdir().unwrap();let identity=memory_core::identity::resolve::MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
        let context=RunFinalizationContext{identity:&identity,reservation:&Reservation,launch:None,now_ms:&||0};
        let recovery=Recovery{verdicts:std::cell::RefCell::new([RunProcessVerdict::Dead,RunProcessVerdict::Alive,RunProcessVerdict::Alive,RunProcessVerdict::Alive,RunProcessVerdict::Dead].into()),calls:Default::default()};
        reconcile_run(&context,root.path(),ledger(root.path()),&recovery).await.unwrap();
        assert_eq!(*recovery.calls.borrow(),["wait:100","signal:43:SIGTERM","wait:105","signal:43:SIGKILL","fail:true"]);
    }
    #[tokio::test]
    async fn unknown_supervisor_waits_then_abandons_without_signaling() {
        let root=tempfile::tempdir().unwrap();let identity=memory_core::identity::resolve::MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
        let context=RunFinalizationContext{identity:&identity,reservation:&Reservation,launch:None,now_ms:&||0};
        let recovery=Recovery{verdicts:std::cell::RefCell::new([RunProcessVerdict::Unknown,RunProcessVerdict::Unknown].into()),calls:Default::default()};
        reconcile_run(&context,root.path(),ledger(root.path()),&recovery).await.unwrap();assert_eq!(*recovery.calls.borrow(),["outcome:105","abandon"]);
    }
}
