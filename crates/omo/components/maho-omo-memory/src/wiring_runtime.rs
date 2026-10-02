use std::collections::BTreeMap;
use crate::{context::MemoryIdentityContext,identity_runtime::{MemoryIdentityRuntime,create_identity_runtime},journal_wiring::MemoryJournalWiring,facts_wiring::{MemoryFactsWiring,MemoryFactsWiringOptions,create_memory_facts_wiring}};
#[derive(Default)]pub struct MemoryRuntimeWiring{
    pub contexts:BTreeMap<String,MemoryIdentityContext>,runtimes:BTreeMap<String,MemoryIdentityRuntime>,
    journals:BTreeMap<String,MemoryJournalWiring>,facts:BTreeMap<String,MemoryFactsWiring>,
}
pub struct RuntimeDreamSession<'a>{pub session_id:String,pub runtime:&'a MemoryIdentityRuntime,pub launch:&'a mut dyn FnMut(memory_core::reflection::ReservedRun)->Result<(),String>}
#[derive(Debug)]pub enum RuntimeDreamError{Io(std::io::Error),Selector(crate::dream_selector::DreamSelectorError),Journal(memory_core::journal::store::JournalError),Reservation(memory_core::reflection::reservation::ReservationError),Launch(String)}
impl From<std::io::Error> for RuntimeDreamError{fn from(error:std::io::Error)->Self{Self::Io(error)}}
impl From<crate::dream_selector::DreamSelectorError> for RuntimeDreamError{fn from(error:crate::dream_selector::DreamSelectorError)->Self{Self::Selector(error)}}
impl crate::dream_trigger_fire::DreamTriggerSession for RuntimeDreamSession<'_>{
    type Error=RuntimeDreamError;
    fn conversation_id(&self)->&str{&self.session_id}
    fn paths(&self)->&memory_core::identity::layout::MemoryIdentityPaths{&self.runtime.identity.identity_paths}
    fn capture_snapshot(&mut self,conversation:&str)->Result<Option<memory_core::journal::cursor::ReflectionSnapshot>,Self::Error>{memory_core::journal::store::TranscriptJournal::new(memory_core::journal::store::TranscriptJournalOptions::new(self.runtime.identity.identity_paths.transcripts.join(conversation))).capture_reflection_snapshot(None).map_err(RuntimeDreamError::Journal)}
    fn try_reserve(&mut self,request:memory_core::reflection::ReflectionRequest)->Result<memory_core::reflection::ReservationResult,Self::Error>{self.runtime.store.try_reserve(request).map_err(RuntimeDreamError::Reservation)}
    fn launch(&mut self,run:memory_core::reflection::ReservedRun)->Result<(),Self::Error>{(self.launch)(run).map_err(RuntimeDreamError::Launch)}
}
impl MemoryRuntimeWiring{
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
    pub fn dream_session_by_id<'a>(&'a mut self,session:&str,settings:impl FnOnce()->Result<serde_json::Value,String>,launch:&'a mut dyn FnMut(memory_core::reflection::ReservedRun)->Result<(),String>)->Result<Option<RuntimeDreamSession<'a>>,String>{
        let Some(identity)=self.resolve_context(session).cloned()else{return Ok(None);};let runtime=self.runtime_for(&identity,settings)?;
        Ok(Some(RuntimeDreamSession{session_id:session.into(),runtime,launch}))
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn dream_runtime_captures_real_journal_and_launches_reserved_run(){
        let root=tempfile::tempdir().unwrap();let identity=identity(root.path());let journal=memory_core::journal::store::TranscriptJournal::new(memory_core::journal::store::TranscriptJournalOptions::new(identity.identity_paths.transcripts.join("session")));
        journal.append(&[memory_core::journal::entries::TranscriptEntry::Text(memory_core::journal::entries::TextTranscriptEntry::new("user","question","1970-01-01T00:00:00Z","m:user","m")),memory_core::journal::entries::TranscriptEntry::Text(memory_core::journal::entries::TextTranscriptEntry::new("assistant","answer","1970-01-01T00:00:00Z","m:assistant","m"))]).unwrap();
        let mut wiring=MemoryRuntimeWiring::default();wiring.contexts.insert("session".into(),identity.clone());let mut launched=vec![];{
        let mut launch=|run:memory_core::reflection::ReservedRun|{launched.push(run);Ok(())};
        let mut session=wiring.dream_session_by_id("session",||crate::reflection_settings::resolve_memory_settings(None),&mut launch).unwrap().unwrap();
        let result=crate::dream_trigger_fire::fire_dream(&mut session,memory_core::reflection::DreamOrigin::Manual,&Default::default(),&crate::dream_trigger_fire::ManualDreamRequest{conversation_ids:Some(vec!["session".into()]),..Default::default()},&||0.0,&||false,&mut |error|panic!("{error:?}")).unwrap();
        assert!(matches!(result,crate::dream_trigger_fire::DreamFireOutcome::Fired{ref status,..} if status=="active"));}
        assert_eq!(launched.len(),1);assert_eq!(launched[0].request.trigger,memory_core::reflection::ReflectionTrigger::Dream);assert_eq!(launched[0].request.conversation_ids,["session"]);assert!(!identity.repo_path().exists());
    }
    fn identity(root:&std::path::Path)->MemoryIdentityContext{MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root,"agent"),crate::binding::create_memory_binding("agent","repo",0.0))}
    #[test]fn runtime_settings_are_read_only_on_first_identity_access(){let root=tempfile::tempdir().unwrap();let identity=identity(root.path());let mut wiring=MemoryRuntimeWiring::default();let first=wiring.runtime_for(&identity,||crate::reflection_settings::resolve_memory_settings(None)).unwrap().store.clone();let second=wiring.runtime_for(&identity,||panic!("cached runtime must not load config")).unwrap().store.clone();assert!(std::sync::Arc::ptr_eq(&first,&second));assert!(!identity.repo_path().exists());}
    #[test]fn journal_cache_replays_delta_without_duplicate_entries(){let root=tempfile::tempdir().unwrap();let identity=identity(root.path());let mut wiring=MemoryRuntimeWiring::default();let entries=vec![serde_json::json!({"type":"message","id":"user","message":{"role":"user","content":"hello"}})];assert_eq!(wiring.journal_wiring_for(&identity).reconcile_session(Some("session"),&entries,|_,_|panic!("contention")).unwrap().appended,1);assert_eq!(wiring.journal_wiring_for(&identity).reconcile_session(Some("session"),&entries,|_,_|panic!("contention")).unwrap().appended,0);}
}
