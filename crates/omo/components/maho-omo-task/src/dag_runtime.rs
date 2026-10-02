use std::sync::Arc;
use maho_ext_api::{ExtensionApi,ExtensionFailure,EventKind,EventResult};
pub trait DagRuntimeLifecycle:Send+Sync {
    fn attach(&self)->Result<(),ExtensionFailure>;
    fn detach(&self);
    fn pause_for_shutdown(&self)->Result<(),ExtensionFailure>;
    fn dispose(&self);
}
pub fn wire_dag_lifecycle(api:&mut ExtensionApi,runtime:Arc<dyn DagRuntimeLifecycle>,wire_task_lifecycle:impl FnOnce(&mut ExtensionApi)) {
    let pause=runtime.clone();
    api.on(EventKind::SessionShutdown,Arc::new(move |_,_| { let pause=pause.clone(); Box::pin(async move { pause.pause_for_shutdown()?; Ok(EventResult::None) }) }));
    wire_task_lifecycle(api);
    let attach=runtime.clone();
    api.on(EventKind::SessionStart,Arc::new(move |_,_| { let attach=attach.clone(); Box::pin(async move { attach.attach()?; Ok(EventResult::None) }) }));
    let detach=runtime.clone();
    api.on(EventKind::SessionBeforeSwitch,Arc::new(move |_,_| { let detach=detach.clone(); Box::pin(async move { detach.detach(); Ok(EventResult::None) }) }));
    api.on(EventKind::SessionShutdown,Arc::new(move |_,_| { let runtime=runtime.clone(); Box::pin(async move { runtime.dispose(); Ok(EventResult::None) }) }));
}
