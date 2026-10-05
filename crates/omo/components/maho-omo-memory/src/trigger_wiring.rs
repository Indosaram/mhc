use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use maho_ext_api::{EventKind, EventResult, ExtensionApi, ExtensionEvent, SessionCompactEvent};
use memory_core::reflection::{ReflectionEvent, ReflectionRequest, ReservationResult, TriggerConfig};
use crate::context::MemoryPendingLedger;

pub trait ReflectionTriggerEngine: Send + Sync {
    fn evaluate(&self, conversation: &str, event: ReflectionEvent) -> Result<Option<ReservationResult>, String>;
}
impl ReflectionTriggerEngine for memory_core::reflection::ReflectionReservationStore {
    fn evaluate(&self, conversation: &str, event: ReflectionEvent) -> Result<Option<ReservationResult>, String> { memory_core::reflection::ReflectionReservationStore::evaluate(self, conversation, event).map_err(|error| error.to_string()) }
}
pub struct ReflectionTriggerSession {
    pub conversation_id: String,
    pub ledger: Arc<Mutex<MemoryPendingLedger>>,
    pub engine: Arc<dyn ReflectionTriggerEngine>,
    pub enabled: bool,
}
pub type ResolveTriggerSession = Arc<dyn Fn(Option<&str>) -> Option<ReflectionTriggerSession> + Send + Sync>;
#[derive(Clone)]
pub struct ReflectionTriggerWiringOptions {
    pub resolve_session: ResolveTriggerSession,
    pub on_launch: Arc<dyn Fn(ReflectionRequest) -> Result<(), String> + Send + Sync>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}
pub struct ReflectionTriggerWiring { options: ReflectionTriggerWiringOptions, outcomes: Mutex<BTreeMap<String, (bool, bool)>> }
pub fn create_reflection_trigger_wiring(options: ReflectionTriggerWiringOptions) -> Arc<ReflectionTriggerWiring> {
    Arc::new(ReflectionTriggerWiring { options, outcomes:Mutex::new(BTreeMap::new()) })
}
impl ReflectionTriggerWiring {
    fn evaluate_and_launch(&self, session: &ReflectionTriggerSession, event: ReflectionEvent) -> Result<(), String> {
        if let Some(result) = session.engine.evaluate(&session.conversation_id, event)? && result.status == "active" && let Err(error) = (self.options.on_launch)(result.run.request) {
            (self.options.warn)(&format!("omo-senpi memory reflection launch failed: {error}"));
        }
        Ok(())
    }
    pub fn handle_event(&self, event: &ExtensionEvent, session_id: &str) {
        let Some(session) = (self.options.resolve_session)(Some(session_id)) else { return; };
        match event {
            ExtensionEvent::AgentEnd { aborted, will_retry, .. } => { self.outcomes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(session.conversation_id, (aborted.unwrap_or(false), will_retry.unwrap_or(false))); }
            ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted { .. }) => { session.ledger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending_compaction = true; }
            ExtensionEvent::AgentSettled => {
                let outcome = self.outcomes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&session.conversation_id);
                if outcome != Some((false, false)) { return; }
                let mut ledger = session.ledger.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !session.enabled { ledger.pending_compaction = false; return; }
                let evaluate = || {
                    if ledger.pending_compaction { session.engine.evaluate(&session.conversation_id, ReflectionEvent::CompactionAccepted)?; ledger.pending_compaction = false; }
                    self.evaluate_and_launch(&session, ReflectionEvent::Settled { success:true })
                };
                let mut evaluate = evaluate;
                if let Err(error) = evaluate() { (self.options.warn)(&format!("omo-senpi memory reflection trigger failed: {error}")); }
            }
            _ => {}
        }
    }
    pub fn register(self: &Arc<Self>, api: &mut ExtensionApi) {
        for event in [EventKind::AgentEnd, EventKind::AgentSettled, EventKind::SessionCompact] {
            let wiring = Arc::clone(self);
            api.on(event, Arc::new(move |event, context| {
                wiring.handle_event(event, context.session_manager.session_id());
                Box::pin(async { Ok(EventResult::None) })
            }));
        }
    }
    pub fn request_manual_reflection(&self, focus: Option<String>, recent_n: Option<usize>, conversation_ids: Option<Vec<String>>) {
        let Some(session) = (self.options.resolve_session)(None).filter(|session| session.enabled) else { return; };
        if let Err(error) = self.evaluate_and_launch(&session, ReflectionEvent::Manual { focus, recent_n, conversation_ids }) { (self.options.warn)(&format!("omo-senpi memory reflection trigger failed: {error}")); }
    }
}

pub struct ResolvedTriggerConfig { pub enabled: bool, pub config: TriggerConfig }
pub fn resolve_reflection_trigger_config(settings: &serde_json::Value, agent: Option<&str>) -> Result<ResolvedTriggerConfig, String> {
    let reflection = match agent { Some(agent) => crate::reflection_settings::resolve_agent_reflection_settings(Some(settings), agent)?, None => settings["reflection"].clone() };
    Ok(ResolvedTriggerConfig { enabled:reflection["enabled"].as_bool().ok_or("Missing reflection enabled")?, config:TriggerConfig { step_count:reflection["trigger"]["step_count"].as_u64().map(|value| usize::try_from(value).map_err(|error| error.to_string())).transpose()?, on_compaction:reflection["trigger"]["on_compaction"].as_bool() } })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Engine(Arc<Mutex<Vec<ReflectionEvent>>>);
    impl ReflectionTriggerEngine for Engine { fn evaluate(&self, _: &str, event:ReflectionEvent) -> Result<Option<ReservationResult>,String> { self.0.lock().unwrap().push(event); Ok(None) } }
    type Fixture = (Arc<ReflectionTriggerWiring>, Arc<Mutex<Vec<ReflectionEvent>>>, Arc<Mutex<MemoryPendingLedger>>);
    fn fixture(enabled:bool) -> Fixture {
        let events=Arc::new(Mutex::new(vec![])); let ledger=Arc::new(Mutex::new(MemoryPendingLedger::default()));
        let engine=Arc::new(Engine(events.clone())); let resolved_ledger=ledger.clone();
        let wiring=create_reflection_trigger_wiring(ReflectionTriggerWiringOptions { resolve_session:Arc::new(move |_| Some(ReflectionTriggerSession { conversation_id:"conversation".into(), ledger:resolved_ledger.clone(), engine:engine.clone(), enabled })), on_launch:Arc::new(|_| panic!("no reservation")), warn:Arc::new(|error| panic!("{error}")) });
        (wiring,events,ledger)
    }
    fn end(aborted:bool,retry:bool) -> ExtensionEvent { ExtensionEvent::AgentEnd { messages:vec![], aborted:Some(aborted), will_retry:Some(retry), abort_source:None } }
    #[test] fn real_reservation_launches_only_active_then_collapses_pending() {
        use memory_core::{identity::resolve::MemoryIdentity, journal::{store::{TranscriptJournal, TranscriptJournalOptions}, entries::{TranscriptEntry, TextTranscriptEntry}}, reflection::{ReflectionReservationStore, ReflectionReservationStoreOptions}};
        let root=tempfile::tempdir().unwrap(); let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");
        let journal=TranscriptJournal::new(TranscriptJournalOptions::new(paths.transcripts.join("conversation")));
        journal.append(&[TranscriptEntry::Text(TextTranscriptEntry::new("user","Question","2026-01-01T00:00:00.000Z","m:user","m")),TranscriptEntry::Text(TextTranscriptEntry::new("assistant","Answer","2026-01-01T00:00:00.000Z","m:assistant","m"))]).unwrap();
        let transcripts=paths.transcripts.clone(); let sequence=Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let store=Arc::new(ReflectionReservationStore::new(ReflectionReservationStoreOptions { identity:MemoryIdentity { id:"agent".into(), safe_slug:"agent".into(), paths }, config:TriggerConfig { step_count:Some(1), on_compaction:Some(false) }, get_journal:Arc::new(move |id| Ok(TranscriptJournal::new(TranscriptJournalOptions::new(transcripts.join(id))))), create_run_id:Some(Arc::new(move || format!("run-{}",sequence.fetch_add(1,std::sync::atomic::Ordering::SeqCst)))), now_iso:None, launcher_identity:None }));
        let engine=store.clone(); let ledger=Arc::new(Mutex::new(MemoryPendingLedger::default())); let launches=Arc::new(Mutex::new(vec![])); let observed=launches.clone();
        let wiring=create_reflection_trigger_wiring(ReflectionTriggerWiringOptions { resolve_session:Arc::new(move |_| Some(ReflectionTriggerSession { conversation_id:"conversation".into(), ledger:ledger.clone(), engine:engine.clone(), enabled:true })), on_launch:Arc::new(move |request| { observed.lock().unwrap().push(request); Ok(()) }), warn:Arc::new(|error| panic!("{error}")) });
        for _ in 0..3 { wiring.handle_event(&end(false,false),"session"); wiring.handle_event(&ExtensionEvent::AgentSettled,"session"); }
        assert_eq!(launches.lock().unwrap().len(),1); assert_eq!(launches.lock().unwrap()[0].trigger,memory_core::reflection::ReflectionTrigger::StepCount);
        wiring.request_manual_reflection(Some("first".into()),None,None);
        wiring.request_manual_reflection(Some("second".into()),None,None);
        assert_eq!(launches.lock().unwrap().len(),1);
        let state=store.read_state().unwrap(); assert!(state.active.is_some()); assert!(state.pending.is_some());
    }
    #[test] fn registration_contains_only_the_three_lifecycle_events() { let (wiring,_,_)=fixture(true); let mut api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default()); wiring.register(&mut api); assert_eq!(api.registered.handlers.len(),3); for kind in [EventKind::AgentEnd,EventKind::AgentSettled,EventKind::SessionCompact] { assert_eq!(api.registered.handlers[&kind].len(),1); } assert!(api.registered.tools.is_empty()); assert!(api.registered.commands.is_empty()); }
    #[test] fn successful_settle_consumes_compaction_before_evaluation() { let (wiring,events,ledger)=fixture(true); ledger.lock().unwrap().pending_compaction=true; wiring.handle_event(&end(false,false),"session"); wiring.handle_event(&ExtensionEvent::AgentSettled,"session"); assert_eq!(*events.lock().unwrap(),[ReflectionEvent::CompactionAccepted,ReflectionEvent::Settled { success:true }]); assert!(!ledger.lock().unwrap().pending_compaction); wiring.handle_event(&ExtensionEvent::AgentSettled,"session"); assert_eq!(events.lock().unwrap().len(),2); }
    #[test] fn aborted_retry_and_missing_outcome_never_evaluate() { for (aborted,retry) in [(true,false),(false,true)] { let (wiring,events,ledger)=fixture(true); ledger.lock().unwrap().pending_compaction=true; wiring.handle_event(&end(aborted,retry),"session"); wiring.handle_event(&ExtensionEvent::AgentSettled,"session"); assert!(events.lock().unwrap().is_empty()); assert!(ledger.lock().unwrap().pending_compaction); } let (wiring,events,_)=fixture(true); wiring.handle_event(&ExtensionEvent::AgentSettled,"session"); assert!(events.lock().unwrap().is_empty()); }
    #[test] fn disabled_clears_compaction_without_reserving() { let (wiring,events,ledger)=fixture(false); ledger.lock().unwrap().pending_compaction=true; wiring.handle_event(&end(false,false),"session"); wiring.handle_event(&ExtensionEvent::AgentSettled,"session"); wiring.request_manual_reflection(None,None,None); assert!(events.lock().unwrap().is_empty()); assert!(!ledger.lock().unwrap().pending_compaction); }
    #[test] fn manual_preserves_requested_focus_and_conversations() { let (wiring,events,_)=fixture(true); wiring.request_manual_reflection(Some("focus".into()),Some(3),Some(vec!["other".into()])); assert_eq!(*events.lock().unwrap(),[ReflectionEvent::Manual { focus:Some("focus".into()), recent_n:Some(3), conversation_ids:Some(vec!["other".into()]) }]); }
    #[test] fn imported_defaults_and_agent_override_resolve() { let mut settings=crate::reflection_settings::resolve_memory_settings(None).unwrap(); settings["agents"]["agent"]=serde_json::json!({"reflection":{"enabled":false,"trigger":{"step_count":0}}}); let resolved=resolve_reflection_trigger_config(&settings,Some("agent")).unwrap(); assert!(!resolved.enabled); assert_eq!(resolved.config.step_count,Some(0)); assert_eq!(resolved.config.on_compaction,Some(true)); }
}
