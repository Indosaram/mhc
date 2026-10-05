use std::sync::Arc;
use maho_ext_api::{ExtensionApi,ExtensionEvent,EventKind,EventResult,ExtensionFailure};
use crate::{accounting_hooks::GoalStoreReference,direct_input_lifecycle::{GoalDirectInputLifecycle,DirectInputGoalChange},monitor_continuation::MonitorAwareGoalContinuation,index::GoalTurnAccounting};
pub type DirectInputGoalChanged=Arc<dyn for<'a> Fn(&'a maho_ext_api::ExtensionContext,&'a crate::types::Goal,bool)->maho_ext_api::ExtensionFuture<'a,()>+Send+Sync>;
pub struct GoalInputHookDeps {
    pub lifecycle:Arc<tokio::sync::Mutex<GoalDirectInputLifecycle>>,pub monitor:Arc<std::sync::Mutex<MonitorAwareGoalContinuation>>,
    pub accounting:Arc<tokio::sync::Mutex<GoalTurnAccounting>>,pub reference:GoalStoreReference,
    pub now:Arc<dyn Fn()->f64+Send+Sync>,pub changed:DirectInputGoalChanged,
}
pub fn register_goal_input_hooks(api:&mut ExtensionApi,deps:Arc<GoalInputHookDeps>) {
    for kind in [EventKind::Input,EventKind::InputDisposition] {
        let deps=deps.clone();
        api.on(kind,Arc::new(move |event,ctx| { let deps=deps.clone(); Box::pin(async move {
            let reference=(deps.reference)(ctx); let now=(deps.now)(); let mut lifecycle=deps.lifecycle.lock().await;
            match event {
                ExtensionEvent::Input(input)=>{
                    let mut monitor=deps.monitor.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                    lifecycle.on_input(input,&reference,|id|monitor.hold_direct_input(id,now)).map_err(failure)?;
                },
                ExtensionEvent::InputDisposition { input_id,disposition }=>{
                    let monitor=deps.monitor.clone();
                    let change=lifecycle.on_disposition(input_id,*disposition,&reference,(now/1000.0).floor() as u64,move |id,accepted|monitor.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resolve_direct_input(id,accepted,now)).await.map_err(failure)?;
                    if let Some(change)=change {
                        let (goal,resume)=match change { DirectInputGoalChange::Reactivated(goal)=>(goal,false),DirectInputGoalChange::Active { goal,resume_suppressed_load }=>(goal,resume_suppressed_load) };
                        deps.accounting.lock().await.begin(&goal,now); (deps.changed)(ctx,&goal,resume).await?;
                    }
                },_=>{},
            }
            Ok(EventResult::None)
        }) }));
    }
}
fn failure(error:crate::errors::GoalError)->ExtensionFailure { ExtensionFailure::new(error.to_string()) }
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn registered_accepted_input_reactivates_and_releases_held_timer() {
        use maho_ext_api::*;
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        crate::store::update_goal(&reference,&crate::types::GoalUpdate { status:Some(crate::types::GoalStatus::Blocked),reason:Some("continuation cap reached".into()),..Default::default() },crate::types::GoalUpdateSource::Model,1).await.unwrap();
        let monitor=Arc::new(std::sync::Mutex::new(MonitorAwareGoalContinuation::default())); monitor.lock().unwrap().arm_timer(crate::wait_progress::GoalWaitKind::Monitor,1000.0,1000.0,false,0.0);
        let accounting=Arc::new(tokio::sync::Mutex::new(GoalTurnAccounting::default())); let changed=Arc::new(std::sync::atomic::AtomicBool::new(false)); let captured=changed.clone(); let stored=reference.clone();
        let deps=Arc::new(GoalInputHookDeps { lifecycle:Arc::new(tokio::sync::Mutex::new(GoalDirectInputLifecycle::default())),monitor:monitor.clone(),accounting:accounting.clone(),reference:Arc::new(move |_|stored.clone()),now:Arc::new(||2000.0),changed:Arc::new(move |_,goal,resume| { assert_eq!(goal.status,crate::types::GoalStatus::Active); assert!(!resume); captured.store(true,std::sync::atomic::Ordering::SeqCst); Box::pin(async { Ok(()) }) }) });
        let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default()); register_goal_input_hooks(&mut api,deps);
        let ctx=crate::test_context::context(); let mut input=ExtensionEvent::Input(InputEvent { input_id:"i".into(),text:"continue".into(),images:None,source:InputSource::Interactive,streaming_behavior:None });
        api.registered.handlers[&EventKind::Input][0](&mut input,&ctx).await.unwrap(); assert!(monitor.lock().unwrap().held_timer.is_some());
        let mut admitted=ExtensionEvent::InputDisposition { input_id:"i".into(),disposition:InputDisposition::Started }; api.registered.handlers[&EventKind::InputDisposition][0](&mut admitted,&ctx).await.unwrap();
        assert!(changed.load(std::sync::atomic::Ordering::SeqCst)); assert!(monitor.lock().unwrap().held_timer.is_none()); assert!(accounting.lock().await.window.is_some()); assert_eq!(crate::store::read_goal(&reference).unwrap().unwrap().status,crate::types::GoalStatus::Active);
    }
}
