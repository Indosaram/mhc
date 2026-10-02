use std::sync::Arc;
use maho_ext_api::{Extension,ExtensionApi,EventKind,EventResult};
use crate::{accounting_hooks::GoalStoreReference,runtime::GoalRuntime};
pub struct GoalExtension { pub reference:GoalStoreReference,pub now:Arc<dyn Fn()->f64+Send+Sync> }
impl GoalExtension {
    pub fn new(reference:GoalStoreReference)->Self { Self { reference,now:Arc::new(||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0,|duration|duration.as_millis() as f64)) } }
}
impl Extension for GoalExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let runtime=Arc::new(GoalRuntime::new(self.reference.clone(),self.now.clone()));
        for kind in [EventKind::SessionStart,EventKind::AgentStart,EventKind::MessageEnd,EventKind::AgentEnd,EventKind::Input,EventKind::InputDisposition,EventKind::SessionAbort,EventKind::SessionShutdown] {
            let runtime=runtime.clone();
            api.on(kind,Arc::new(move |event,context| { let runtime=runtime.clone(); Box::pin(async move { runtime.event(event,context).await?; Ok(EventResult::None) }) }));
        }
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn registered_factory_hooks_share_accounting_and_persist_user_abort() {
        use maho_ext_api::*;
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() }; let stored=reference.clone();
        crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let clock=Arc::new(std::sync::atomic::AtomicU64::new(0)); let reading=clock.clone();
        let extension=GoalExtension { reference:Arc::new(move |_|stored.clone()),now:Arc::new(move ||reading.load(std::sync::atomic::Ordering::SeqCst) as f64) };
        let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default()); extension.register(&mut api);
        let context=crate::test_context::context();
        api.registered.handlers[&EventKind::AgentStart][0](&mut ExtensionEvent::AgentStart,&context).await.unwrap();
        clock.store(1500,std::sync::atomic::Ordering::SeqCst);
        api.registered.handlers[&EventKind::SessionAbort][0](&mut ExtensionEvent::SessionAbort,&context).await.unwrap();
        let blocked=crate::store::read_goal(&reference).unwrap().unwrap(); assert_eq!(blocked.status,crate::types::GoalStatus::Blocked); assert_eq!(blocked.time_used_seconds,2.0);
        let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None });
        api.registered.handlers[&EventKind::SessionShutdown][0](&mut shutdown,&context).await.unwrap();
    }
}
