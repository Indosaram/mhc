use std::sync::Arc;
use maho_ext_api::{ExtensionApi,EventKind,ExtensionEvent,EventResult,ExtensionFailure,InputSource};
use crate::{runtime::LoopRuntime,types::LoopStoreRef,scheduler::TickOutcome};
pub type SettledLoopDelivery=Arc<dyn Fn(crate::attribution::AttributedSettlement)->maho_ext_api::ExtensionFuture<'static,()>+Send+Sync>;
pub fn register_loop_lifecycle_hooks(api:&mut ExtensionApi,runtime:Arc<tokio::sync::Mutex<LoopRuntime>>,reference:Arc<dyn Fn(&maho_ext_api::ExtensionContext)->LoopStoreRef+Send+Sync>,now:Arc<dyn Fn()->f64+Send+Sync>,ids:Arc<dyn Fn()->String+Send+Sync>,settled:SettledLoopDelivery) {
    for kind in [EventKind::Input,EventKind::SessionCompact,EventKind::AgentEnd,EventKind::SessionAbort,EventKind::SessionShutdown] {
        let runtime=runtime.clone(); let reference=reference.clone(); let now=now.clone(); let ids=ids.clone(); let settled=settled.clone();
        api.on(kind,Arc::new(move |event,ctx| { let runtime=runtime.clone(); let reference=reference.clone(); let now=now.clone(); let ids=ids.clone(); let settled=settled.clone(); Box::pin(async move {
            let mut runtime=runtime.lock().await; let now=now(); let mut outcome=None; let mut persist=false;
            match event {
                ExtensionEvent::Input(input) if input.source!=InputSource::Extension=>runtime.attribution.clear(),
                ExtensionEvent::SessionCompact(maho_ext_api::SessionCompactEvent::Accepted { .. })=>{ runtime.accepted_compaction(); persist=true; },
                ExtensionEvent::AgentEnd { aborted,abort_source,will_retry,.. }=>{
                    if *will_retry==Some(true) { return Ok(EventResult::None); }
                    if *aborted==Some(true)&&*abort_source==Some(maho_ext_api::AbortSource::User) { let LoopRuntime { scheduler,attribution,.. }=&mut *runtime; attribution.pause(scheduler,now); }
                    else { outcome=runtime.settled(if *aborted==Some(true) { TickOutcome::Error } else { TickOutcome::Completed },now,ids(),ids()); }
                    persist=true;
                },
                ExtensionEvent::SessionAbort=>{ let LoopRuntime { scheduler,attribution,.. }=&mut *runtime; attribution.pause(scheduler,now); persist=true; },
                ExtensionEvent::SessionShutdown(_)=>{ runtime.shutdown(now); persist=true; },_=>{},
            }
            if persist { runtime.persist(&reference(ctx)).await.map_err(|error|ExtensionFailure::new(error.to_string()))?; }
            drop(runtime); if let Some(outcome)=outcome { settled(outcome).await?; }
            Ok(EventResult::None)
        }) }));
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn registers_only_owned_lifecycle_kinds() {
        let mut api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("loop","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default());
        let runtime=Arc::new(tokio::sync::Mutex::new(LoopRuntime::new("s",None,&Default::default())));
        register_loop_lifecycle_hooks(&mut api,runtime,Arc::new(|_|LoopStoreRef { base_dir:"/tmp/loop".into(),session_id:"s".into() }),Arc::new(||0.0),Arc::new(||"id".into()),Arc::new(|_|Box::pin(async { Ok(()) })));
        assert_eq!(api.registered.handlers.len(),5);
        for kind in [EventKind::Input,EventKind::SessionCompact,EventKind::AgentEnd,EventKind::SessionAbort,EventKind::SessionShutdown] { assert_eq!(api.registered.handlers[&kind].len(),1); }
    }
}
