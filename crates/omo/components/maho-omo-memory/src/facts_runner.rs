use std::{future::Future,path::Path,sync::{Arc,atomic::{AtomicBool,Ordering}}};
use memory_core::{identity::resolve::MemoryIdentity,facts::{queue::{FactsQueue,FactsQueueOptions,FactsAbortSignal},FactsPayload,failures_store::{FactsFailureStore,FactsFailureStoreOptions}}};
use crate::{facts_runner_types::FactsLaunchResult,worker::resolve_model::ReflectionModelResolution};
pub struct FactsExtractorRunner{pub identity:MemoryIdentity,pub queue:FactsQueue,pub failures:FactsFailureStore,active:AtomicBool,now:Arc<dyn Fn()->i64+Send+Sync>}
struct ActiveLaunch<'a>(&'a AtomicBool);impl Drop for ActiveLaunch<'_>{fn drop(&mut self){self.0.store(false,Ordering::Release);}}
pub struct FactsAttemptInput<'a>{pub resolve_model:&'a (dyn Fn()->Result<ReflectionModelResolution,String>+Sync),pub deadline_ms:i64,pub termination_grace_ms:i64}
pub type FactsAttemptWork=std::pin::Pin<Box<dyn Future<Output=Result<FactsLaunchResult,String>>+Send>>;
pub type NativeFactsSandbox=Arc<dyn Fn(crate::worker::spawn_types::FactsSpawnArgs)->Result<crate::worker::spawn_types::FactsSpawnArgs,String>+Send+Sync>;
pub struct NativeFactsAttemptOptions{
    pub resolve_model:Arc<dyn Fn()->Result<ReflectionModelResolution,String>+Send+Sync>,
    pub env:std::collections::BTreeMap<String,String>,
    pub config_sources:Vec<crate::worker::model_preflight::ConfigSource>,
    pub launch:crate::worker::model_preflight::Launcher,
    pub supervisor_command:std::path::PathBuf,pub supervisor_args:Vec<String>,
    pub deadline_ms:i64,pub termination_grace_ms:i64,pub max_output_bytes:usize,
    pub people:memory_core::facts::person_routing::FactsPeopleRouting,
    pub sandbox:Option<NativeFactsSandbox>,
    pub warn:Arc<dyn Fn(&str)+Send+Sync>,
}
pub struct NativeFactsExtractorPort{
    pub runner:Arc<FactsExtractorRunner>,
    pub attempt:Arc<dyn Fn(Arc<FactsExtractorRunner>,Option<FactsAbortSignal>)->FactsAttemptWork+Send+Sync>,
    pub people:memory_core::facts::person_routing::FactsPeopleRouting,
    pub warn:Arc<dyn Fn(&str)+Send+Sync>,
}
impl crate::facts_wiring::FactsExtractorPort for NativeFactsExtractorPort{
    fn launch_pending(&mut self,signal:Option<&FactsAbortSignal>)->crate::facts_wiring::FactsExtractorWork{
        let runner=self.runner.clone();let attempt=self.attempt.clone();let signal=signal.cloned();
        Box::pin(async move{runner.launch_pending(signal.as_ref(),||attempt(runner.clone(),signal.clone())).await.map(|_|())})
    }
    fn reconcile_pending(&mut self,signal:Option<&FactsAbortSignal>)->crate::facts_wiring::FactsExtractorWork{
        let runner=self.runner.clone();let attempt=self.attempt.clone();let signal=signal.cloned();let people=self.people;let warn=self.warn.clone();
        Box::pin(async move{
            if runner.reconcile_runs(&people,&mut |message|warn(message))?{return Ok(());}
            runner.launch_pending(signal.as_ref(),||attempt(runner.clone(),signal.clone())).await.map(|_|())
        })
    }
}
impl FactsExtractorRunner{
    pub fn native_extractor(self:Arc<Self>,options:NativeFactsAttemptOptions)->NativeFactsExtractorPort{
        let people=options.people;let warn=options.warn.clone();let options=Arc::new(options);
        let cache=Arc::new(tokio::sync::Mutex::new(crate::worker::model_preflight::ModelPreflight::default()));
        NativeFactsExtractorPort{runner:self,people,warn,attempt:Arc::new(move|runner,signal|{
            let options=options.clone();let cache=cache.clone();
            Box::pin(async move{
                let mut warn=|message:&str|(options.warn)(message);
                runner.launch_pending_once(FactsAttemptInput{resolve_model:options.resolve_model.as_ref(),deadline_ms:options.deadline_ms,termination_grace_ms:options.termination_grace_ms},signal.as_ref(),||runner.reconcile_runs(&options.people,&mut |message|(options.warn)(message)),|dir,payload,batch,resolution|{
                    let runner=runner.clone();let options=options.clone();let cache=cache.clone();
                    async move{
                        let run_id=dir.file_name().ok_or("Facts run directory has no name")?.to_string_lossy().into_owned();
                        let ledger:crate::facts_runner_types::FactsRunLedger=crate::worker::run_artifacts::read_run_json(&dir.join("ledger.json")).map_err(|error|error.to_string())?;
                        let mut cache=cache.lock().await;
                        runner.execute_child(&mut cache,crate::worker::facts_child_launch::FactsChildLaunchInput{
                            sandbox:options.sandbox.as_deref().map(|sandbox|sandbox as &(dyn Fn(crate::worker::spawn_types::FactsSpawnArgs)->Result<crate::worker::spawn_types::FactsSpawnArgs,String>+Sync)),
                            run_id:&run_id,run_dir:&dir,payload:&payload,resolution:&resolution,env:&options.env,config_sources:&options.config_sources,launch:&options.launch,hard_deadline_at:chrono::Utc::now().timestamp_millis() as f64+options.deadline_ms as f64,
                            options:crate::worker::spawn_supervisor::FactsChildOptions{termination_grace_ms:options.termination_grace_ms as f64,max_output_bytes:options.max_output_bytes,supervisor_command:&options.supervisor_command,supervisor_args:&options.supervisor_args,batch_id:&batch,queued:&ledger.queued,launched_at:chrono::DateTime::parse_from_rfc3339(&ledger.started_at).map_err(|error|error.to_string())?.timestamp_millis()},
                        },&options.people,&mut |message|(options.warn)(message)).await
                    }
                },&mut warn).await
            })
        })}
    }
    pub fn new(identity:MemoryIdentity,now:Arc<dyn Fn()->i64+Send+Sync>)->Self{let queue=FactsQueue::new(FactsQueueOptions{identity_paths:identity.paths.clone(),now:Some(now.clone()),on_publish:None});let failures=FactsFailureStore::new(FactsFailureStoreOptions{identity_paths:identity.paths.clone(),now:Some(now.clone()),lock_wait_ms:None});Self{identity,queue,failures,active:AtomicBool::new(false),now}}
    pub async fn launch_pending<F:Future<Output=Result<FactsLaunchResult,String>>>(&self,signal:Option<&FactsAbortSignal>,attempt:impl FnMut()->F)->Result<FactsLaunchResult,String>{
        if signal.is_some_and(FactsAbortSignal::is_aborted){return Ok(FactsLaunchResult::Skipped);}
        if self.active.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err(){return Ok(FactsLaunchResult::Active);}
        let _active=ActiveLaunch(&self.active);crate::facts_drain::drain_facts_launches(attempt,||signal.is_some_and(FactsAbortSignal::is_aborted)).await
    }
    pub async fn launch_pending_once<F:Future<Output=Result<FactsLaunchResult,String>>>(
        &self,input:FactsAttemptInput<'_>,signal:Option<&FactsAbortSignal>,reconcile:impl FnOnce()->Result<bool,String>,
        execute:impl FnOnce(std::path::PathBuf,FactsPayload,String,ReflectionModelResolution)->F,warn:&mut (dyn FnMut(&str)+Send),
    )->Result<FactsLaunchResult,String>{
        let aborted=||signal.is_some_and(FactsAbortSignal::is_aborted);
        if aborted(){return Ok(FactsLaunchResult::Skipped);}
        if reconcile()?{return Ok(FactsLaunchResult::Active);}
        let pending=self.queue.list_pending().map_err(|error|error.to_string())?;
        if aborted(){return Ok(FactsLaunchResult::Skipped);}
        if pending.is_empty(){return Ok(FactsLaunchResult::Empty);}
        let failures=match crate::facts_launch_selection::read_launchable_failures(&self.failures,|message,error,_|warn(&format!("{message}: {error}"))){crate::facts_launch_selection::FactsFailuresRead::Read{failures}=>failures,_=>return Ok(FactsLaunchResult::Skipped)};
        let now=(self.now)();let instant=chrono::DateTime::from_timestamp_millis(now).ok_or("Invalid facts clock")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
        let selected=memory_core::facts::select_launchable(&pending,Some(&failures),&instant).selected;if selected.is_empty(){return Ok(FactsLaunchResult::Empty);}
        let resolution=(input.resolve_model)()?;
        if let ReflectionModelResolution::CategoryUnavailable{cause,..}=&resolution{let mut io=FactsIo{queue:&self.queue,instant:&instant,warn};let terminal=crate::facts_terminal_writes::FactsTerminalWrites{failures:&self.failures,io:&mut io};terminal.preflight_fail(&crate::facts_failure_recording::queue_entry_targets(&selected),&crate::facts_failure_recording::preflight_failure_id(None),memory_core::facts::FactsFailureReason::QuickCategoryUnavailable,cause).map_err(|error|error.to_string())?;return Ok(FactsLaunchResult::Skipped);}
        crate::engine_session::prepare_memory_engine_session(&self.identity.id,&self.identity.paths,Default::default()).map_err(|error|error.to_string())?;
        let people=crate::facts_people_payload::read_facts_people_payload(&self.identity.paths.repo);
        let envelope=memory_core::facts::FactsPayloadEnvelope{version:1,identity:self.identity.id.clone(),today:instant[..10].into(),known_people:people.known_people,primary_human:people.primary_human};
        let capped=memory_core::facts::select_capped_facts_batch(&memory_core::facts::CappedFactsBatchInput{entries:selected.clone(),envelope:envelope.clone(),now:instant.clone(),max_bytes:None,starvation_ms:None});
        {
            let mut io=FactsIo{queue:&self.queue,instant:&instant,warn};let mut terminal=crate::facts_terminal_writes::FactsTerminalWrites{failures:&self.failures,io:&mut io};
            if crate::facts_oversize::classify_oversize_payload(&mut terminal,&crate::facts_oversize::OversizeClassificationInput{envelope:&envelope,oversized:&capped.oversized,pending:&selected,envelope_oversized:capped.envelope_oversized,create_failure_id:None,max_bytes:None},&mut |_,_|{}).map_err(|error|error.to_string())?{return Ok(FactsLaunchResult::Skipped);}
        }
        if capped.selected.is_empty(){return Ok(FactsLaunchResult::Empty);}
        let batch=memory_core::support::random::random_uuid();let launched_at=(self.now)();
        if aborted(){return Ok(FactsLaunchResult::Skipped);}
        let dir=crate::facts_run_storage::reserve_facts_run_dir(&crate::facts_run_storage::ReserveFactsRunDirOptions{facts_dir:&self.identity.paths.facts,locks_dir:&self.identity.paths.locks,entries:&capped.selected,batch_id:&batch,launched_at,deadline_ms:Some(input.deadline_ms),termination_grace_ms:Some(input.termination_grace_ms),lock_wait_ms:None}).map_err(|error|error.to_string())?;
        let Some(dir)=dir else{return Ok(FactsLaunchResult::Active);};if aborted(){return Ok(FactsLaunchResult::Skipped);}
        let run_id=dir.file_name().ok_or("Facts run directory has no name")?.to_string_lossy().into_owned();
        let targets=crate::facts_failure_recording::queue_entry_targets(&capped.selected);
        let result=match execute(dir.clone(),envelope.to_payload(capped.selected),batch.clone(),resolution).await{
            Ok(result)=>result,
            Err(error)=>{let mut io=FactsIo{queue:&self.queue,instant:&instant,warn};let mut terminal=crate::facts_terminal_writes::FactsTerminalWrites{failures:&self.failures,io:&mut io};terminal.fail(&crate::facts_terminal_writes::FactsFailureWrite{run_dir:&dir,run_id:&run_id,batch_id:&batch,targets:&targets,reason:memory_core::facts::FactsFailureReason::ChildExit,detail:&error,outcome:None}).map_err(|error|error.to_string())?;FactsLaunchResult::Failed{run_id}},
        };
        self.prune(warn);Ok(result)
    }
    pub fn finalize(&self,dir:&Path,people:&memory_core::facts::person_routing::FactsPeopleRouting,warn:&mut dyn FnMut(&str))->Result<FactsLaunchResult,String>{
        use memory_core::locks::{create_lock_record,CreateLockRecordOptions,AcquireLockOptions,with_lock,run_finalization_lock_path,memory_writer_lock_path,WithLockError};
        let ledger:crate::facts_runner_types::FactsRunLedger=crate::worker::run_artifacts::read_run_json(&dir.join("ledger.json")).map_err(|error|error.to_string())?;
        let record=create_lock_record("facts-finalize",CreateLockRecordOptions{run_id:Some(ledger.run_id.clone())}).map_err(|error|error.to_string())?;
        let path=run_finalization_lock_path(&self.identity.paths.locks,&ledger.run_id).map_err(|error|error.to_string())?;
        let result=with_lock(&path,&record,&AcquireLockOptions{wait_timeout_ms:Some(2000),..Default::default()},||{
            if dir.join("final.json").exists(){let record=crate::worker::run_artifacts::read_run_json(&dir.join("final.json")).map_err(|error|error.to_string())?;return Ok(crate::facts_run_storage::final_result(&record));}
            if dir.join("abandoned.json").exists(){return Ok(FactsLaunchResult::Failed{run_id:ledger.run_id.clone()});}
            let repo=memory_core::git::GitMemoryRepo::open(&self.identity.paths.repo,&self.identity.id).map_err(|error|error.to_string())?;
            let now=chrono::DateTime::from_timestamp_millis((self.now)()).ok_or("Invalid facts clock")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
            let mut io=FactsIo{queue:&self.queue,instant:&now,warn};let mut terminal=crate::facts_terminal_writes::FactsTerminalWrites{failures:&self.failures,io:&mut io};
            crate::facts_run_finalize::finalize_claimed_facts_run(dir,&repo,&ledger,&mut terminal,&mut |records|{
                crate::facts_batch_apply::apply_facts_with_retries(|attempt|{
                    let record=create_lock_record("memory-write",CreateLockRecordOptions{run_id:Some(format!("facts-{attempt}"))}).map_err(|error|WithLockError::User(format!("{error}")))?;
                    with_lock(&memory_writer_lock_path(&self.identity.paths.locks),&record,&AcquireLockOptions{wait_timeout_ms:Some(2000),..Default::default()},||crate::facts_batch_apply::apply_claimed(dir,&ledger,&repo,records,people,&self.identity.id).map_err(|error|format!("{error:?}")))
                },|_,delay|std::thread::sleep(std::time::Duration::from_millis(delay)),||{
                    let id=memory_core::support::random::random_id();let value=id.bytes().take(8).fold(0u32,|value,byte|value*16+u32::from(if byte<=b'9'{byte-b'0'}else{byte-b'a'+10}));f64::from(value)/(f64::from(u32::MAX)+1.0)
                }).map_err(|error|error.to_string())
            }).map_err(|error|format!("{error:?}"))
        }).map_err(|error|error.to_string())?;
        self.prune(warn);Ok(result)
    }
    pub async fn execute_child(&self,cache:&mut crate::worker::model_preflight::ModelPreflight,input:crate::worker::facts_child_launch::FactsChildLaunchInput<'_>,people:&memory_core::facts::person_routing::FactsPeopleRouting,warn:&mut (dyn FnMut(&str)+Send))->Result<FactsLaunchResult,String>{
        let dir=input.run_dir;let run=input.run_id;let batch=input.options.batch_id;let targets=crate::facts_failure_recording::queue_entry_targets(&input.payload.entries);
        let child=match crate::worker::facts_child_launch::launch_facts_model_chain(cache,input,|message|warn(message)).await{
            Ok(child)=>child,
            Err(error)=>{
                let instant=chrono::DateTime::from_timestamp_millis((self.now)()).ok_or("Invalid facts clock")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
                let mut io=FactsIo{queue:&self.queue,instant:&instant,warn};
                crate::facts_terminal_writes::FactsTerminalWrites{failures:&self.failures,io:&mut io}.fail(&crate::facts_terminal_writes::FactsFailureWrite{run_dir:dir,run_id:run,batch_id:batch,targets:&targets,reason:memory_core::facts::FactsFailureReason::ChildExit,detail:&error,outcome:None}).map_err(|error|error.to_string())?;
                return Ok(FactsLaunchResult::Failed{run_id:run.into()});
            }
        };
        if child.child.timed_out||child.child.code!=Some(0){
            let instant=chrono::DateTime::from_timestamp_millis((self.now)()).ok_or("Invalid facts clock")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
            let mut io=FactsIo{queue:&self.queue,instant:&instant,warn};
            crate::facts_terminal_writes::FactsTerminalWrites{failures:&self.failures,io:&mut io}.fail(&crate::facts_terminal_writes::FactsFailureWrite{run_dir:dir,run_id:run,batch_id:batch,targets:&targets,reason:if child.child.timed_out{memory_core::facts::FactsFailureReason::DeadlineExceeded}else{memory_core::facts::FactsFailureReason::ChildExit},detail:if child.child.stderr.trim().is_empty(){"facts child failed"}else{child.child.stderr.trim()},outcome:None}).map_err(|error|error.to_string())?;
            return Ok(FactsLaunchResult::Failed{run_id:run.into()});
        }
        self.finalize(dir,people,warn)
    }
    pub fn prune(&self,warn:&mut dyn FnMut(&str)){
        if let Err(error)=crate::facts_run_prune::prune_terminal_facts_runs(&crate::facts_run_prune::PruneTerminalFactsRunsOptions{facts_dir:&self.identity.paths.facts,locks_dir:&self.identity.paths.locks,keep_last:None,max_total_bytes:None},&mut |_|{},&mut |_,_|{}){warn(&format!("facts run retention pruning failed: {error}"));}
    }
    pub fn reconcile_runs(&self,people:&memory_core::facts::person_routing::FactsPeopleRouting,warn:&mut dyn FnMut(&str))->Result<bool,String>{
        sweep_runner_artifacts(&self.identity.paths.facts,warn);
        let active=crate::facts_run_reconcile::reconcile_facts_runs(&self.identity.paths.facts,&mut FactsRecovery{runner:self,people,warn}).map_err(|error|format!("{error:?}"))?;
        self.prune(warn);Ok(active)
    }
}
struct FactsRecovery<'a>{runner:&'a FactsExtractorRunner,people:&'a memory_core::facts::person_routing::FactsPeopleRouting,warn:&'a mut dyn FnMut(&str)}
impl crate::facts_run_reconcile::FactsReconciliationPort for FactsRecovery<'_>{
    type Error=String;
    fn now_ms(&self)->f64{(self.runner.now)() as f64}
    fn finalize(&mut self,dir:&Path)->Result<(),String>{self.runner.finalize(dir,self.people,self.warn).map(|_|())}
    fn fail(&mut self,dir:&Path,ledger:&crate::facts_runner_types::FactsRunLedger,detail:&str)->Result<(),String>{
        let instant=chrono::DateTime::from_timestamp_millis((self.runner.now)()).ok_or("Invalid facts clock")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
        let mut io=FactsIo{queue:&self.runner.queue,instant:&instant,warn:self.warn};
        crate::facts_terminal_writes::FactsTerminalWrites{failures:&self.runner.failures,io:&mut io}.fail(&crate::facts_terminal_writes::FactsFailureWrite{run_dir:dir,run_id:&ledger.run_id,batch_id:&ledger.batch_id,targets:&crate::facts_failure_recording::ledger_targets(&ledger.queued),reason:memory_core::facts::FactsFailureReason::ChildExit,detail,outcome:None}).map_err(|error|error.to_string())
    }
    fn abandon(&mut self,dir:&Path,ledger:&crate::facts_runner_types::FactsRunLedger)->Result<(),String>{
        let instant=chrono::DateTime::from_timestamp_millis((self.runner.now)()).ok_or("Invalid facts clock")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
        let mut io=FactsIo{queue:&self.runner.queue,instant:&instant,warn:self.warn};crate::facts_terminal_writes::FactsTerminalWrites{failures:&self.runner.failures,io:&mut io}.abandon(dir,ledger).map_err(|error|error.to_string())
    }
    fn warn(&mut self,run:&str,error:&String){(self.warn)(&format!("facts run reconcile failed: {run}: {error}"));}
}
struct FactsIo<'a>{queue:&'a FactsQueue,instant:&'a str,warn:&'a mut dyn FnMut(&str)}
impl crate::facts_terminal_writes::FactsTerminalIo for FactsIo<'_>{fn now(&self)->String{self.instant.into()}fn mark_consumed(&mut self,entries:&[memory_core::facts::FactsQueueEntry])->std::io::Result<()>{self.queue.mark_consumed(entries).map_err(std::io::Error::other)}fn warn(&mut self,message:&str,detail:&str){(self.warn)(&format!("{message}: {detail}"));}}
pub fn sweep_runner_artifacts(facts:&Path,warn:&mut dyn FnMut(&str)){crate::facts_run_cleanup::sweep_terminal_facts_runs(facts,&mut crate::facts_run_cleanup::remove_run_artifact,&mut |message,path,error|warn(&format!("{message}: {}: {error}",path.display())));}
#[cfg(test)]mod tests{
    use super::*;
    #[tokio::test]async fn reservation_clock_is_sampled_after_model_resolution(){
        use memory_core::journal::entries::{TranscriptEntry,TextTranscriptEntry};
        let root=tempfile::tempdir().unwrap();let clock=Arc::new(std::sync::atomic::AtomicI64::new(0));let sampled=clock.clone();
        let mut runner=runner(root.path());runner.now=Arc::new(move||sampled.load(Ordering::SeqCst));
        runner.queue.enqueue(memory_core::facts::queue::FactsEnqueueRequest{identity:"agent".into(),session_id:"session".into(),conversation_id:"conversation".into(),signal:None,entries:vec![TranscriptEntry::Text(TextTranscriptEntry::new("user","Question","1970-01-01T00:00:00.000Z","m:user","m")),TranscriptEntry::Text(TextTranscriptEntry::new("assistant","Answer","1970-01-01T00:00:00.000Z","m:assistant","m"))]}).unwrap();
        let resolve=||{clock.store(42_000,Ordering::SeqCst);Ok(ReflectionModelResolution::Resolved{category:"quick".into(),model:"p/m".into(),thinking:None,source:None,fallbacks:vec![]})};
        runner.launch_pending_once(FactsAttemptInput{resolve_model:&resolve,deadline_ms:900000,termination_grace_ms:5000},None,||Ok(false),|dir,_,_,_|async move{
            let ledger:crate::facts_runner_types::FactsRunLedger=crate::worker::run_artifacts::read_run_json(&dir.join("ledger.json")).unwrap();
            assert_eq!(ledger.started_at,"1970-01-01T00:00:42.000Z");assert_eq!(ledger.hard_deadline_at,942_000.0);assert_eq!(ledger.deadline_at,947_000.0);
            Ok(FactsLaunchResult::Empty)
        },&mut |message|panic!("{message}")).await.unwrap();
    }
    #[tokio::test]async fn native_extractor_adapter_preserves_shared_active_latch_and_cancellation(){
        use crate::facts_wiring::FactsExtractorPort;
        let root=tempfile::tempdir().unwrap();let runner=Arc::new(runner(root.path()));
        let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));let seen=calls.clone();
        let mut port=NativeFactsExtractorPort{runner:runner.clone(),attempt:Arc::new(move|_,signal|{assert!(signal.is_none_or(|signal|!signal.is_aborted()));seen.fetch_add(1,Ordering::SeqCst);Box::pin(async{Ok(FactsLaunchResult::Empty)})}),people:memory_core::facts::person_routing::FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200},warn:Arc::new(|error|panic!("{error}"))};
        port.reconcile_pending(None).await.unwrap();assert_eq!(calls.load(Ordering::SeqCst),1);
        let signal=FactsAbortSignal::new();signal.abort();port.launch_pending(Some(&signal)).await.unwrap();assert_eq!(calls.load(Ordering::SeqCst),1);
        runner.active.store(true,Ordering::SeqCst);port.launch_pending(None).await.unwrap();assert_eq!(calls.load(Ordering::SeqCst),1);runner.active.store(false,Ordering::SeqCst);
        assert!(!runner.identity.paths.repo.exists());
    }
    #[tokio::test]async fn assembled_native_attempt_preserves_empty_and_unavailable_preflight(){
        use crate::facts_wiring::FactsExtractorPort;
        use memory_core::journal::entries::{TranscriptEntry,TextTranscriptEntry};
        let root=tempfile::tempdir().unwrap();let runner=Arc::new(runner(root.path()));
        let resolutions=Arc::new(std::sync::atomic::AtomicUsize::new(0));let calls=resolutions.clone();
        let mut port=runner.clone().native_extractor(NativeFactsAttemptOptions{
            resolve_model:Arc::new(move||{calls.fetch_add(1,Ordering::SeqCst);Ok(ReflectionModelResolution::CategoryUnavailable{category:"quick".into(),cause:"no_registry",attempted_chain:None,missing_providers:None})}),
            env:Default::default(),config_sources:vec![],launch:crate::worker::model_preflight::Launcher{command:"missing-child-must-not-run".into(),prefix_args:vec![]},supervisor_command:"missing-supervisor-must-not-run".into(),supervisor_args:vec![],deadline_ms:900000,termination_grace_ms:5000,max_output_bytes:1024,people:memory_core::facts::person_routing::FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200},sandbox:None,warn:Arc::new(|error|panic!("{error}")),
        });
        port.launch_pending(None).await.unwrap();assert!(!runner.identity.paths.repo.exists());assert_eq!(resolutions.load(Ordering::SeqCst),0);
        runner.queue.enqueue(memory_core::facts::queue::FactsEnqueueRequest{identity:"agent".into(),session_id:"session".into(),conversation_id:"conversation".into(),signal:None,entries:vec![TranscriptEntry::Text(TextTranscriptEntry::new("user","Question","1970-01-01T00:00:00.000Z","m:user","m")),TranscriptEntry::Text(TextTranscriptEntry::new("assistant","Answer","1970-01-01T00:00:00.000Z","m:assistant","m"))]}).unwrap();
        port.launch_pending(None).await.unwrap();assert_eq!(runner.queue.list_pending().unwrap().len(),1);
        assert!(!runner.identity.paths.repo.exists());assert!(!runner.identity.paths.facts.join("runs").exists());
        let failures=runner.failures.read_failures().unwrap();assert_eq!(failures.entries.len(),1);assert_eq!(resolutions.load(Ordering::SeqCst),1);
        port.launch_pending(None).await.unwrap();assert_eq!(resolutions.load(Ordering::SeqCst),1);
    }
    #[test]fn abandoned_sentinel_replays_without_outcome_or_payload(){
        let root=tempfile::tempdir().unwrap();let runner=runner(root.path());
        let dir=crate::facts_run_storage::reserve_facts_run_dir(&crate::facts_run_storage::ReserveFactsRunDirOptions{facts_dir:&runner.identity.paths.facts,locks_dir:&runner.identity.paths.locks,entries:&[],batch_id:"c04edbea-90ea-4c67-9c22-1a9beb207535",launched_at:0,deadline_ms:None,termination_grace_ms:None,lock_wait_ms:None}).unwrap().unwrap();
        std::fs::write(dir.join("abandoned.json"),"{}\n").unwrap();let people=memory_core::facts::person_routing::FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200};
        let result=runner.finalize(&dir,&people,&mut |message|panic!("{message}")).unwrap();assert_eq!(result,FactsLaunchResult::Failed{run_id:dir.file_name().unwrap().to_string_lossy().into_owned()});assert!(!runner.identity.paths.repo.exists());assert!(!dir.join("outcome.json").exists());
    }
    #[test]fn real_extraction_commits_under_writer_lock_and_replays_receipt(){
        let root=tempfile::tempdir().unwrap();let runner=runner(root.path());
        crate::engine_session::prepare_memory_engine_session(&runner.identity.id,&runner.identity.paths,Default::default()).unwrap();
        let dir=crate::facts_run_storage::reserve_facts_run_dir(&crate::facts_run_storage::ReserveFactsRunDirOptions{facts_dir:&runner.identity.paths.facts,locks_dir:&runner.identity.paths.locks,entries:&[],batch_id:"c04edbea-90ea-4c67-9c22-1a9beb207535",launched_at:0,deadline_ms:None,termination_grace_ms:None,lock_wait_ms:None}).unwrap().unwrap();
        let id=dir.file_name().unwrap().to_string_lossy();
        crate::worker::run_artifacts::write_run_json_atomic(&dir.join("facts-payload.json"),&FactsPayload{version:1,identity:"agent".into(),today:"1970-01-01".into(),known_people:vec![],primary_human:memory_core::facts::FactsPrimaryHuman{slug:"human".into(),aliases:vec![]},entries:vec![]},0o600).unwrap();
        crate::worker::run_artifacts::write_run_json_atomic(&dir.join("outcome.json"),&crate::worker::run_artifacts::RunOutcome{version:1,run_id:id.into_owned(),attempt:None,finished_at:"1970-01-01T00:00:00Z".into(),child_exit:crate::worker::run_artifacts::ChildExit{code:Some(0),signal:None},timed_out:false},0o600).unwrap();
        std::fs::write(dir.join("extraction.jsonl"),r#"{"scope":"project","text":"Use durable journals","date":"1970-01-01"}"#).unwrap();
        let people=memory_core::facts::person_routing::FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200};
        let first=runner.finalize(&dir,&people,&mut |message|panic!("{message}")).unwrap();assert!(matches!(first,FactsLaunchResult::Committed{..}));
        assert_eq!(runner.finalize(&dir,&people,&mut |message|panic!("{message}")).unwrap(),first);
        assert!(!memory_core::locks::memory_writer_lock_path(&runner.identity.paths.locks).exists());assert!(!dir.join("facts-payload.json").exists());
    }
    fn runner(root:&Path)->FactsExtractorRunner{FactsExtractorRunner::new(MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root,"agent")},Arc::new(||0))}
    #[tokio::test]async fn launch_latch_spans_drain_and_releases_after_error(){let root=tempfile::tempdir().unwrap();let runner=runner(root.path());let result=runner.launch_pending(None,||async{assert_eq!(runner.launch_pending(None,||async{panic!("active")}).await.unwrap(),FactsLaunchResult::Active);Err("launch".into())}).await;assert_eq!(result,Err("launch".into()));assert_eq!(runner.launch_pending(None,||std::future::ready(Ok(FactsLaunchResult::Empty))).await.unwrap(),FactsLaunchResult::Empty);}
    #[tokio::test]async fn empty_queue_does_not_initialize_repository(){let root=tempfile::tempdir().unwrap();let runner=runner(root.path());assert_eq!(runner.launch_pending_once(FactsAttemptInput{resolve_model:&||panic!("empty queue must not resolve model"),deadline_ms:900000,termination_grace_ms:5000},None,||Ok(false),|_,_,_,_|async{panic!("empty")},&mut |_|panic!("warning")).await.unwrap(),FactsLaunchResult::Empty);assert!(!runner.identity.paths.repo.exists());}
}
