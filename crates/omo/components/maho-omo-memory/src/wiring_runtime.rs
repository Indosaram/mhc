use std::collections::BTreeMap;
use crate::{context::MemoryIdentityContext,identity_runtime::{MemoryIdentityRuntime,create_identity_runtime},journal_wiring::MemoryJournalWiring,facts_wiring::{MemoryFactsWiring,MemoryFactsWiringOptions,create_memory_facts_wiring}};
#[derive(Default)]pub struct MemoryRuntimeWiring{
    pub contexts:BTreeMap<String,MemoryIdentityContext>,runtimes:BTreeMap<String,MemoryIdentityRuntime>,
    journals:BTreeMap<String,MemoryJournalWiring>,facts:BTreeMap<String,MemoryFactsWiring>,
    trigger_ledgers:BTreeMap<String,std::sync::Arc<std::sync::Mutex<crate::context::MemoryPendingLedger>>>,
}
pub type RuntimeReflectionLaunch=std::sync::Arc<dyn Fn(memory_core::reflection::ReservedRun)->Result<(),String>+Send+Sync>;
pub struct RuntimeDreamInput<'a>{pub session:&'a str,pub origin:memory_core::reflection::DreamOrigin,pub request:&'a crate::dream_trigger_fire::ManualDreamRequest,pub settings:&'a serde_json::Value,pub now:&'a dyn Fn()->f64,pub aborted:&'a dyn Fn()->bool,pub warn:&'a mut dyn FnMut(&str)}
struct RuntimeTriggerEngine{store:std::sync::Arc<memory_core::reflection::ReflectionReservationStore>,launch:RuntimeReflectionLaunch}
impl crate::trigger_wiring::ReflectionTriggerEngine for RuntimeTriggerEngine{
    fn evaluate(&self,conversation:&str,event:memory_core::reflection::ReflectionEvent)->Result<Option<memory_core::reflection::ReservationResult>,String>{
        let result=self.store.evaluate(conversation,event).map_err(|error|error.to_string())?;
        if let Some(result)=&result&&result.status=="active"{(self.launch)(result.run.clone())?;}
        Ok(result)
    }
}
pub struct RuntimeDreamSession<'a>{pub session_id:String,pub runtime:&'a MemoryIdentityRuntime,pub launch:&'a mut dyn FnMut(memory_core::reflection::ReservedRun)->Result<(),String>,pub aborted:&'a dyn Fn()->bool}
#[derive(Debug)]pub enum RuntimeDreamError{Io(std::io::Error),Selector(crate::dream_selector::DreamSelectorError),Journal(memory_core::journal::store::JournalError),Reservation(memory_core::reflection::reservation::ReservationError),Launch(String)}
impl From<std::io::Error> for RuntimeDreamError{fn from(error:std::io::Error)->Self{Self::Io(error)}}
impl From<crate::dream_selector::DreamSelectorError> for RuntimeDreamError{fn from(error:crate::dream_selector::DreamSelectorError)->Self{Self::Selector(error)}}
impl crate::dream_trigger_fire::DreamTriggerSession for RuntimeDreamSession<'_>{
    type Error=RuntimeDreamError;
    fn conversation_id(&self)->&str{&self.session_id}
    fn paths(&self)->&memory_core::identity::layout::MemoryIdentityPaths{&self.runtime.identity.identity_paths}
    fn capture_snapshot(&mut self,conversation:&str)->Result<Option<memory_core::journal::cursor::ReflectionSnapshot>,Self::Error>{memory_core::journal::store::TranscriptJournal::new(memory_core::journal::store::TranscriptJournalOptions::new(self.runtime.identity.identity_paths.transcripts.join(conversation))).capture_reflection_snapshot(None).map_err(RuntimeDreamError::Journal)}
    fn try_reserve(&mut self,request:memory_core::reflection::ReflectionRequest)->Result<memory_core::reflection::ReservationResult,Self::Error>{self.runtime.store.try_reserve(request,Some(self.aborted)).map_err(RuntimeDreamError::Reservation)}
    fn launch(&mut self,run:memory_core::reflection::ReservedRun)->Result<(),Self::Error>{(self.launch)(run).map_err(RuntimeDreamError::Launch)}
}
impl MemoryRuntimeWiring{
    pub fn native_dream_launch(wiring:std::sync::Arc<tokio::sync::Mutex<Self>>,settings:std::sync::Arc<dyn Fn()->Result<serde_json::Value,String>+Send+Sync>,launch:RuntimeReflectionLaunch,now:std::sync::Arc<dyn Fn()->f64+Send+Sync>,warn:std::sync::Arc<dyn Fn(&str)+Send+Sync>)->crate::dream_trigger::DreamLaunch{
        std::sync::Arc::new(move|session,origin,request,signal|{
            let wiring=wiring.clone();let settings=settings.clone();let launch=launch.clone();let now=now.clone();let warn=warn.clone();
            Box::pin(async move{
                let settings=settings()?;let mut launch=|run|launch(run);
                wiring.lock().await.fire_dream_by_id(RuntimeDreamInput{session:&session,origin,request:&request,settings:&settings,now:now.as_ref(),aborted:&||signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted),warn:&mut |message|warn(message)},&mut launch)?.ok_or_else(||"memory dream session is not bound".into())
            })
        })
    }
    pub fn fire_dream_by_id(&mut self,input:RuntimeDreamInput<'_>,launch:&mut dyn FnMut(memory_core::reflection::ReservedRun)->Result<(),String>)->Result<Option<crate::dream_trigger_fire::DreamFireOutcome>,String>{
        let RuntimeDreamInput{session,origin,request,settings,now,aborted,warn}=input;
        let Some(identity)=self.resolve_context(session)else{return Ok(None);};
        let policy=crate::dream_trigger_gates::resolve_dream_trigger_settings(settings,Some(&identity.identity))?;
        let Some(mut session)=self.dream_session_by_id(session,||Ok(settings.clone()),launch,aborted)?else{return Ok(None);};
        crate::dream_trigger_fire::fire_dream(&mut session,origin,&policy,request,now,aborted,&mut |error|warn(&format!("memory dream launch failed: {error:?}"))).map(Some).map_err(|error|format!("{error:?}"))
    }
    pub fn trigger_session_by_id(&mut self,session:&str,settings:impl FnOnce()->Result<serde_json::Value,String>,launch:RuntimeReflectionLaunch)->Result<Option<crate::trigger_wiring::ReflectionTriggerSession>,String>{
        let Some(identity)=self.resolve_context(session).cloned()else{return Ok(None);};let settings=settings()?;
        let enabled=crate::trigger_wiring::resolve_reflection_trigger_config(&settings,Some(&identity.identity))?.enabled;
        let store=self.runtime_for(&identity,||Ok(settings))?.store.clone();
        let ledger=self.trigger_ledgers.entry(session.into()).or_insert_with(||std::sync::Arc::new(std::sync::Mutex::new(identity.ledger))).clone();
        Ok(Some(crate::trigger_wiring::ReflectionTriggerSession{conversation_id:session.into(),ledger,enabled,engine:std::sync::Arc::new(RuntimeTriggerEngine{store,launch})}))
    }
    pub fn resolve_context(&self,session:&str)->Option<&MemoryIdentityContext>{self.contexts.get(session)}
    pub fn existing_facts_wiring(&mut self,identity:&str)->Option<&mut MemoryFactsWiring>{self.facts.get_mut(identity)}
    pub fn journal_wiring_for(&mut self,identity:&MemoryIdentityContext)->&mut MemoryJournalWiring{
        self.journals.entry(identity.identity.clone()).or_insert_with(||MemoryJournalWiring::new(identity.identity_paths.transcripts.clone()))
    }
    pub fn runtime_for(&mut self,identity:&MemoryIdentityContext,settings:impl FnOnce()->Result<serde_json::Value,String>)->Result<&mut MemoryIdentityRuntime,String>{
        if !self.runtimes.contains_key(&identity.identity){let runtime=create_identity_runtime(identity.clone(),&settings()?)?;self.runtimes.insert(identity.identity.clone(),runtime);}
        self.runtimes.get_mut(&identity.identity).ok_or_else(||"Identity runtime missing after construction".into())
    }
    pub fn facts_wiring_for(&mut self,identity:&MemoryIdentityContext,options:impl FnOnce()->MemoryFactsWiringOptions)->&mut MemoryFactsWiring{
        self.facts.entry(identity.identity.clone()).or_insert_with(||create_memory_facts_wiring(options()))
    }
    pub fn native_facts_wiring_for(&mut self,identity:&MemoryIdentityContext,settings:std::sync::Arc<dyn Fn()->Result<serde_json::Value,String>+Send+Sync>,attempt:crate::facts_runner::NativeFactsAttemptOptions,now:std::sync::Arc<dyn Fn()->i64+Send+Sync>)->&mut MemoryFactsWiring{
        self.facts_wiring_for(identity,||Self::native_facts_options(identity,settings,attempt,now))
    }
    pub fn native_facts_options(identity:&MemoryIdentityContext,settings:std::sync::Arc<dyn Fn()->Result<serde_json::Value,String>+Send+Sync>,attempt:crate::facts_runner::NativeFactsAttemptOptions,now:std::sync::Arc<dyn Fn()->i64+Send+Sync>)->MemoryFactsWiringOptions{
            let runner=std::sync::Arc::new(crate::facts_runner::FactsExtractorRunner::new(crate::identity_runtime::as_memory_identity(identity),now.clone()));
            let enabled_settings=settings.clone();let enabled_identity=identity.identity.clone();let debounce_identity=identity.identity.clone();let warn=attempt.warn.clone();let enabled_warn=warn.clone();let debounce_warn=warn.clone();
            MemoryFactsWiringOptions{identity:identity.identity.clone(),identity_paths:identity.identity_paths.clone(),extractor:Some(Box::new(runner.native_extractor(attempt))),now:Some(now),warn,
                facts_enabled:Box::new(move||match enabled_settings(){Ok(settings)=>settings["agents"][&enabled_identity]["facts"]["enabled"].as_bool().or_else(||settings["facts"]["enabled"].as_bool()).unwrap_or(true),Err(error)=>{enabled_warn(&error);false}}),
                debounce_settles:Box::new(move||match settings(){Ok(settings)=>settings["agents"][&debounce_identity]["facts"]["debounce_settles"].as_u64().or_else(||settings["facts"]["debounce_settles"].as_u64()).unwrap_or(4) as usize,Err(error)=>{debounce_warn(&error);4}}),
            }
    }
    pub fn dream_session_by_id<'a>(&'a mut self,session:&str,settings:impl FnOnce()->Result<serde_json::Value,String>,launch:&'a mut dyn FnMut(memory_core::reflection::ReservedRun)->Result<(),String>,aborted:&'a dyn Fn()->bool)->Result<Option<RuntimeDreamSession<'a>>,String>{
        let Some(identity)=self.resolve_context(session).cloned()else{return Ok(None);};let runtime=self.runtime_for(&identity,settings)?;
        Ok(Some(RuntimeDreamSession{session_id:session.into(),runtime,launch,aborted}))
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn native_facts_factory_reuses_identity_and_reads_live_enablement(){
        let root=tempfile::tempdir().unwrap();let identity=identity(root.path());let mut wiring=MemoryRuntimeWiring::default();
        let settings=std::sync::Arc::new(std::sync::Mutex::new(crate::reflection_settings::resolve_memory_settings(None).unwrap()));let current=settings.clone();let settings_reader:std::sync::Arc<dyn Fn()->Result<serde_json::Value,String>+Send+Sync>=std::sync::Arc::new(move||Ok(current.lock().unwrap().clone()));
        let attempt=crate::facts_runner::NativeFactsAttemptOptions{resolve_model:std::sync::Arc::new(||Ok(crate::worker::resolve_model::ReflectionModelResolution::CategoryUnavailable{category:"quick".into(),cause:"no_registry",attempted_chain:None,missing_providers:None})),env:Default::default(),config_sources:vec![],launch:crate::worker::model_preflight::Launcher{command:"must-not-run".into(),prefix_args:vec![]},supervisor_command:"must-not-run".into(),supervisor_args:vec![],deadline_ms:900000,termination_grace_ms:5000,max_output_bytes:1024,people:memory_core::facts::person_routing::FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200},sandbox:None,warn:std::sync::Arc::new(|error|panic!("{error}"))};
        wiring.native_facts_wiring_for(&identity,settings_reader,attempt,std::sync::Arc::new(||0));
        assert_eq!(wiring.facts.len(),1);assert!(!identity.repo_path().exists());
        let journal=memory_core::journal::store::TranscriptJournal::new(memory_core::journal::store::TranscriptJournalOptions::new(identity.identity_paths.transcripts.join("session")));
        journal.append(&[memory_core::journal::entries::TranscriptEntry::Text(memory_core::journal::entries::TextTranscriptEntry::new("user","question","1970-01-01T00:00:00Z","m:user","m")),memory_core::journal::entries::TranscriptEntry::Text(memory_core::journal::entries::TextTranscriptEntry::new("assistant","answer","1970-01-01T00:00:00Z","m:assistant","m"))]).unwrap();
        settings.lock().unwrap()["agents"]=serde_json::json!({"agent":{"facts":{"enabled":false}}});
        let facts=wiring.facts_wiring_for(&identity,||panic!("cached identity must not rebuild extractor"));assert!(!facts.enqueue_settled("session",None).is_enqueued());assert!(!identity.repo_path().exists());
        settings.lock().unwrap()["agents"]["agent"]["facts"]["enabled"]=true.into();assert!(facts.enqueue_settled("session",None).is_enqueued());
    }
    #[test]fn trigger_runtime_launches_winning_reservation_and_reuses_pending_ledger(){
        let root=tempfile::tempdir().unwrap();let identity=identity(root.path());let mut wiring=MemoryRuntimeWiring::default();wiring.contexts.insert("session".into(),identity.clone());
        let calls=std::sync::Arc::new(std::sync::Mutex::new(vec![]));let captured=calls.clone();let launch:RuntimeReflectionLaunch=std::sync::Arc::new(move|run|{captured.lock().unwrap().push(run);Ok(())});
        let session=wiring.trigger_session_by_id("session",||crate::reflection_settings::resolve_memory_settings(None),launch.clone()).unwrap().unwrap();
        session.ledger.lock().unwrap().pending_compaction=true;
        let again=wiring.trigger_session_by_id("session",||crate::reflection_settings::resolve_memory_settings(None),launch).unwrap().unwrap();assert!(std::sync::Arc::ptr_eq(&session.ledger,&again.ledger));assert!(again.ledger.lock().unwrap().pending_compaction);
        let reserved=session.engine.evaluate("session",memory_core::reflection::ReflectionEvent::Manual{focus:None,recent_n:None,conversation_ids:None}).unwrap().unwrap();assert_eq!(reserved.status,"active");assert_eq!(calls.lock().unwrap()[0].run_id,reserved.run.run_id);
        again.engine.evaluate("session",memory_core::reflection::ReflectionEvent::Manual{focus:None,recent_n:None,conversation_ids:None}).unwrap();assert_eq!(calls.lock().unwrap().len(),1);assert!(!identity.repo_path().exists());
    }
    #[tokio::test]async fn dream_runtime_captures_real_journal_and_launches_reserved_run(){
        let root=tempfile::tempdir().unwrap();let identity=identity(root.path());let journal=memory_core::journal::store::TranscriptJournal::new(memory_core::journal::store::TranscriptJournalOptions::new(identity.identity_paths.transcripts.join("session")));
        journal.append(&[memory_core::journal::entries::TranscriptEntry::Text(memory_core::journal::entries::TextTranscriptEntry::new("user","question","1970-01-01T00:00:00Z","m:user","m")),memory_core::journal::entries::TranscriptEntry::Text(memory_core::journal::entries::TextTranscriptEntry::new("assistant","answer","1970-01-01T00:00:00Z","m:assistant","m"))]).unwrap();
        let mut wiring=MemoryRuntimeWiring::default();wiring.contexts.insert("session".into(),identity.clone());let launched=std::sync::Arc::new(std::sync::Mutex::new(vec![]));let captured=launched.clone();
        let launch=MemoryRuntimeWiring::native_dream_launch(std::sync::Arc::new(tokio::sync::Mutex::new(wiring)),std::sync::Arc::new(||crate::reflection_settings::resolve_memory_settings(None)),std::sync::Arc::new(move|run|{captured.lock().unwrap().push(run);Ok(())}),std::sync::Arc::new(||0.0),std::sync::Arc::new(|error|panic!("{error}")));
        let request=crate::dream_trigger_fire::ManualDreamRequest{conversation_ids:Some(vec!["session".into()]),..Default::default()};
        let result=launch("session".into(),memory_core::reflection::DreamOrigin::Manual,request,None).await.unwrap();assert!(matches!(result,crate::dream_trigger_fire::DreamFireOutcome::Fired{ref status,..} if status=="active"));
        let launched=launched.lock().unwrap();assert_eq!(launched.len(),1);assert_eq!(launched[0].request.trigger,memory_core::reflection::ReflectionTrigger::Dream);assert_eq!(launched[0].request.conversation_ids,["session"]);assert!(!identity.repo_path().exists());
    }
    fn identity(root:&std::path::Path)->MemoryIdentityContext{MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root,"agent"),crate::binding::create_memory_binding("agent","repo",0.0))}
    #[test]fn runtime_settings_are_read_only_on_first_identity_access(){let root=tempfile::tempdir().unwrap();let identity=identity(root.path());let mut wiring=MemoryRuntimeWiring::default();let first=wiring.runtime_for(&identity,||crate::reflection_settings::resolve_memory_settings(None)).unwrap().store.clone();let second=wiring.runtime_for(&identity,||panic!("cached runtime must not load config")).unwrap().store.clone();assert!(std::sync::Arc::ptr_eq(&first,&second));assert!(!identity.repo_path().exists());}
    #[test]fn journal_cache_replays_delta_without_duplicate_entries(){let root=tempfile::tempdir().unwrap();let identity=identity(root.path());let mut wiring=MemoryRuntimeWiring::default();let entries=vec![serde_json::json!({"type":"message","id":"user","message":{"role":"user","content":"hello"}})];assert_eq!(wiring.journal_wiring_for(&identity).reconcile_session(Some("session"),&entries,|_,_|panic!("contention")).unwrap().appended,1);assert_eq!(wiring.journal_wiring_for(&identity).reconcile_session(Some("session"),&entries,|_,_|panic!("contention")).unwrap().appended,0);}
}
