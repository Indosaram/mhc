use std::sync::Arc;
use maho_ext_api::{Extension,ExtensionApi,EventKind,ExtensionEvent,EventResult};
use crate::controller::{NativeLoopController,LoopStoreReference};
pub struct LoopExtension {
    pub reference:LoopStoreReference,
    pub now:Arc<dyn Fn()->f64+Send+Sync>,
    pub ids:crate::ids::LoopIdFactory,
    pub home:String,
    pub on_controller_ready:Option<Arc<dyn Fn(Arc<NativeLoopController>)+Send+Sync>>,
}
impl LoopExtension {
    pub fn new(reference:LoopStoreReference)->Self {
        let now:Arc<dyn Fn()->f64+Send+Sync>=Arc::new(||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0,|duration|duration.as_millis() as f64));
        Self { reference,ids:crate::ids::default_ids(now.clone()),now,home:std::env::var("HOME").unwrap_or_default(),on_controller_ready:None }
    }
}
impl Extension for LoopExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let actions=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        let controller_slot=Arc::new(std::sync::OnceLock::<std::sync::Weak<NativeLoopController>>::new());
        let slot=controller_slot.clone();
        let on_fire=Arc::new(move |id:String| {
            let Some(controller)=slot.get().and_then(std::sync::Weak::upgrade) else { return; };
            tokio::spawn(async move { controller.timer_fire(&id).await; });
        });
        let controller=Arc::new(NativeLoopController::new(actions,self.reference.clone(),self.now.clone(),self.ids.clone(),self.home.clone(),on_fire));
        let _=controller_slot.set(Arc::downgrade(&controller));
        if let Some(ready)=&self.on_controller_ready { ready(controller.clone()); }
        crate::command_registration::register_loop_command(api,controller.clone());
        crate::tools::register_loop_tools(api,controller.clone());
        for kind in [EventKind::SessionStart,EventKind::Input,EventKind::SessionCompact,EventKind::AgentEnd,EventKind::AgentSettled,EventKind::SessionAbort,EventKind::SessionShutdown] {
            let controller=controller.clone();
            api.on(kind,Arc::new(move |event,context| { let controller=controller.clone(); Box::pin(async move {
                if matches!(event,ExtensionEvent::SessionStart(_)) { controller.session_start(context).await?; }
                else { controller.event(event).await?; }
                Ok(EventResult::None)
            }) }));
        }
    }
}
