use std::{collections::BTreeMap,sync::{Arc,Mutex},future::Future,pin::Pin};
use maho_ext_api::{ExtensionApi,ExtensionContext,EventKind,EventResult};
use memory_core::reflection::DreamOrigin;
pub use crate::dream_trigger_gates::*;
pub use crate::dream_trigger_fire::{ManualDreamRequest,DreamFireOutcome};
pub type DreamLaunch=Arc<dyn Fn(String,DreamOrigin,ManualDreamRequest,Option<maho_ext_api::AbortSignal>)->Pin<Box<dyn Future<Output=Result<DreamFireOutcome,String>>+Send>>+Send+Sync>;
pub type DreamSessionResolver=Arc<dyn Fn(&ExtensionContext)->Option<(String,String)>+Send+Sync>;
pub struct DreamTriggerOptions {
    pub resolve_session:DreamSessionResolver,
    pub resolve_active_session:Arc<dyn Fn()->Option<String>+Send+Sync>,
    pub resolve_settings:Arc<dyn Fn(&str)->DreamTriggerSettings+Send+Sync>,
    pub launch:DreamLaunch,pub warn:Arc<dyn Fn(&str)+Send+Sync>,
}
pub struct DreamTriggerWiring {options:Arc<DreamTriggerOptions>,timers:Arc<Mutex<BTreeMap<String,maho_ext_api::AbortSignal>>>,in_flight:Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>}
struct DreamShutdownEvaluator(Arc<DreamTriggerWiring>);
struct AbortDreamOnReturn(maho_ext_api::AbortSignal);
impl Drop for AbortDreamOnReturn { fn drop(&mut self) { self.0.abort(); } }
impl crate::shutdown_drain::ShutdownEvaluator for DreamShutdownEvaluator {
    fn evaluate<'a>(&'a mut self,input:crate::shutdown_drain::ShutdownEvaluatorInput<'a>)->crate::shutdown_drain::ShutdownWork<'a>{
        Box::pin(async move{
            if input.signal.aborted(){return Ok(());}
            let signal=maho_ext_api::AbortSignal::default();let _abort=AbortDreamOnReturn(signal.clone());
            self.0.shutdown_evaluate(input.session_id,input.deadline_at,signal).await
        })
    }
}
impl DreamTriggerWiring {
    pub fn new(options:DreamTriggerOptions)->Self{Self{options:Arc::new(options),timers:Default::default(),in_flight:Default::default()}}
    pub fn shutdown_evaluator(self:&Arc<Self>)->Box<dyn crate::shutdown_drain::ShutdownEvaluator>{Box::new(DreamShutdownEvaluator(self.clone()))}
    pub fn register(&self,api:&mut ExtensionApi) {
        for kind in [EventKind::AgentSettled,EventKind::Input,EventKind::AgentStart,EventKind::SessionCompact,EventKind::SessionShutdown,EventKind::SessionAbort] {
            let options=self.options.clone();let timers=self.timers.clone();let in_flight=self.in_flight.clone();
            api.on(kind,Arc::new(move |_,context| {
                if let Some((conversation,identity))=(options.resolve_session)(context) {
                    let mut timer_state=timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if let Some(signal)=timer_state.remove(&conversation){signal.abort();}
                    let delay=(options.resolve_settings)(&identity).idle_minutes;
                    if kind==EventKind::AgentSettled&&delay>0.0 {
                        let signal=maho_ext_api::AbortSignal::default();timer_state.insert(conversation.clone(),signal.clone());
                        let context=context.clone();let options=options.clone();let timers=timers.clone();
                        let in_flight=in_flight.clone();
                        tokio::spawn(async move {
                            tokio::select! {_=signal.cancelled()=>return,_=tokio::time::sleep(std::time::Duration::from_secs_f64(delay*60.0))=>{}}
                            timers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&conversation);
                            if !context.is_idle()||!matches!(context.has_pending_messages(),Ok(false)){return;}
                            let Some((current,_))=(options.resolve_session)(&context)else{return;};if current!=conversation{return;}
                            let task=tokio::spawn(async move {if let Err(error)=(options.launch)(conversation,DreamOrigin::Idle,ManualDreamRequest::default(),None).await{(options.warn)(&error);}});
                            in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(task);
                        });
                    }
                }
                Box::pin(async{Ok(EventResult::None)})
            }));
        }
    }
    pub async fn request_manual_dream(&self,request:ManualDreamRequest)->Result<Option<DreamFireOutcome>,String> {
        let Some(session)=(self.options.resolve_active_session)()else{return Ok(None);};
        (self.options.launch)(session,DreamOrigin::Manual,request,None).await.map(Some)
    }
    pub async fn shutdown_evaluate(&self,session:&str,deadline:f64,signal:maho_ext_api::AbortSignal)->Result<(),String> {
        if signal.is_aborted(){return Ok(());}
        (self.options.launch)(session.into(),DreamOrigin::Shutdown,ManualDreamRequest{deadline_at:Some(deadline),..Default::default()},Some(signal)).await.map(|_|())
    }
    pub async fn when_idle(&self) {
        loop {let tasks=std::mem::take(&mut *self.in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner));if tasks.is_empty(){return;}for task in tasks{let _=task.await;}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wiring(active:bool,captured:Arc<Mutex<Vec<(String,DreamOrigin,ManualDreamRequest)>>>)->DreamTriggerWiring {
        DreamTriggerWiring::new(DreamTriggerOptions {
            resolve_session:Arc::new(|_|None),resolve_active_session:Arc::new(move ||active.then(||"session".into())),resolve_settings:Arc::new(|_|Default::default()),
            launch:Arc::new(move |session,origin,request,_|{captured.lock().unwrap().push((session,origin,request));Box::pin(async{Ok(DreamFireOutcome::Fired{run_id:"run".into(),status:"active".into()})})}),warn:Arc::new(|error|panic!("{error}")),
        })
    }
    #[tokio::test]
    async fn manual_preserves_focus_sources_and_target() {
        let captured=Arc::new(Mutex::new(Vec::new()));let wiring=wiring(true,captured.clone());
        assert!(wiring.request_manual_dream(ManualDreamRequest{focus:Some("focus".into()),conversation_ids:Some(vec!["one".into()]),target_doc:Some("notes/fact.md".into()),deadline_at:None}).await.unwrap().is_some());
        let captured=captured.lock().unwrap();assert_eq!(captured.len(),1);assert_eq!(captured[0].1,DreamOrigin::Manual);assert_eq!(captured[0].2.focus.as_deref(),Some("focus"));assert_eq!(captured[0].2.target_doc.as_deref(),Some("notes/fact.md"));
    }
    #[tokio::test]
    async fn absent_active_session_never_launches() {
        let captured=Arc::new(Mutex::new(Vec::new()));assert!(wiring(false,captured.clone()).request_manual_dream(Default::default()).await.unwrap().is_none());assert!(captured.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn aborted_shutdown_does_not_start_and_live_shutdown_preserves_budget() {
        let captured=Arc::new(Mutex::new(Vec::new()));let wiring=wiring(true,captured.clone());let aborted=maho_ext_api::AbortSignal::default();aborted.abort();
        wiring.shutdown_evaluate("session",50.0,aborted).await.unwrap();assert!(captured.lock().unwrap().is_empty());
        wiring.shutdown_evaluate("session",50.0,Default::default()).await.unwrap();let captured=captured.lock().unwrap();assert_eq!(captured[0].1,DreamOrigin::Shutdown);assert_eq!(captured[0].2.deadline_at,Some(50.0));
    }
    #[test]
    fn native_registration_uses_source_timer_events_only() {
        let root=tempfile::tempdir().unwrap();let mut api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",root.path().into(),Default::default()),Default::default(),Default::default(),Default::default());
        wiring(false,Default::default()).register(&mut api);assert_eq!(api.registered.handlers.len(),6);assert!(api.registered.handlers.contains_key(&EventKind::SessionAbort));assert!(api.registered.handlers.contains_key(&EventKind::AgentSettled));
    }
    #[tokio::test]
    async fn idle_wait_does_not_wait_for_armed_timers() {
        let wiring=wiring(false,Default::default());wiring.when_idle().await;
    }
    #[tokio::test]
    async fn dropping_shutdown_evaluation_aborts_the_launched_dream_signal(){
        let captured=Arc::new(Mutex::new(None));let seen=captured.clone();
        let wiring=Arc::new(DreamTriggerWiring::new(DreamTriggerOptions{
            resolve_session:Arc::new(|_|None),resolve_active_session:Arc::new(||None),resolve_settings:Arc::new(|_|Default::default()),
            launch:Arc::new(move|session,origin,request,signal|{
                assert_eq!(session,"session");assert_eq!(origin,DreamOrigin::Shutdown);assert_eq!(request.deadline_at,Some(1500.0));
                *seen.lock().unwrap()=signal;Box::pin(std::future::pending())
            }),warn:Arc::new(|error|panic!("{error}")),
        }));
        let mut evaluator=wiring.shutdown_evaluator();
        let mut work=evaluator.evaluate(crate::shutdown_drain::ShutdownEvaluatorInput{reason:crate::shutdown_drain::ShutdownReason::Quit,session_id:"session",deadline_at:1500.0,signal:Default::default()});
        assert!(std::future::poll_fn(|context|std::task::Poll::Ready(work.as_mut().poll(context).is_pending())).await);
        let signal=captured.lock().unwrap().clone().unwrap();assert!(!signal.is_aborted());
        drop(work);assert!(signal.is_aborted());
    }
}
