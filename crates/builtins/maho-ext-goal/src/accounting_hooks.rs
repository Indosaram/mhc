use std::sync::Arc;
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionEvent,EventKind,EventResult,ExtensionFailure};
use crate::{index::GoalTurnAccounting,types::{GoalStoreRef,GoalStatus,GoalAccountingMode}};
pub type GoalStoreReference=Arc<dyn Fn(&ExtensionContext)->GoalStoreRef+Send+Sync>;
pub fn register_goal_accounting_hooks(api:&mut ExtensionApi,accounting:Arc<tokio::sync::Mutex<GoalTurnAccounting>>,reference:GoalStoreReference,now:Arc<dyn Fn()->f64+Send+Sync>) {
    for kind in [EventKind::AgentStart,EventKind::MessageEnd,EventKind::AgentEnd,EventKind::SessionAbort,EventKind::SessionShutdown] {
        let accounting=accounting.clone(); let reference=reference.clone(); let now=now.clone();
        api.on(kind,Arc::new(move |event,ctx| {
            let accounting=accounting.clone(); let reference=reference.clone(); let now=now.clone();
            Box::pin(async move {
                let mut accounting=accounting.lock().await; let milliseconds=now(); let seconds=(milliseconds/1000.0).floor() as u64;
                let reference=reference(ctx);
                match event {
                    ExtensionEvent::AgentStart=>{ let goal=crate::store::read_goal(&reference).map_err(failure)?; accounting.agent_start(goal.as_ref(),milliseconds); },
                    ExtensionEvent::MessageEnd { message }=>accounting.usage.note_message_end(message),
                    ExtensionEvent::AgentEnd { messages,aborted,abort_source,.. }=>{ accounting.agent_end(&reference,messages,*aborted==Some(true)&&*abort_source==Some(maho_ext_api::AbortSource::User),milliseconds,seconds).await.map_err(failure)?; },
                    ExtensionEvent::SessionAbort=>{
                        let goal=crate::store::read_goal(&reference).map_err(failure)?;
                        if goal.is_some_and(|goal|goal.status==GoalStatus::Active) {
                            let accounted=accounting.account(&reference,GoalAccountingMode::Active,None,milliseconds,seconds).await.map_err(failure)?;
                            if accounted.is_some_and(|goal|goal.status==GoalStatus::Active) {
                                crate::store::update_goal(&reference,&crate::types::GoalUpdate { status:Some(GoalStatus::Blocked),reason:Some("user interrupted the turn".into()),..Default::default() },crate::types::GoalUpdateSource::Model,seconds).await.map_err(failure)?; accounting.clear();
                            }
                        }
                    },
                    ExtensionEvent::SessionShutdown(_)=>{ if accounting.window.is_some() { accounting.account(&reference,GoalAccountingMode::Active,None,milliseconds,seconds).await.map_err(failure)?; } accounting.clear(); },
                    _=>{},
                }
                Ok(EventResult::None)
            })
        }));
    }
}
fn failure(error:crate::errors::GoalError)->ExtensionFailure { ExtensionFailure::new(error.to_string()) }
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn registered_hooks_account_and_block_user_aborted_goal() {
        use maho_ext_api::*;
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        crate::store::create_goal(&reference,"work",None,0).await.unwrap();
        let accounting=Arc::new(tokio::sync::Mutex::new(GoalTurnAccounting::default())); let clock=Arc::new(std::sync::atomic::AtomicU64::new(0)); let reading=clock.clone(); let stored=reference.clone();
        let mut api=ExtensionApi::new(LoadedExtension::new("goal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
        register_goal_accounting_hooks(&mut api,accounting.clone(),Arc::new(move |_|stored.clone()),Arc::new(move ||reading.load(std::sync::atomic::Ordering::SeqCst) as f64));
        let ctx=crate::test_context::context();
        api.registered.handlers[&EventKind::AgentStart][0](&mut ExtensionEvent::AgentStart,&ctx).await.unwrap();
        assert!(accounting.lock().await.turn_in_progress);
        clock.store(1500,std::sync::atomic::Ordering::SeqCst);
        let mut end=ExtensionEvent::AgentEnd { messages:Vec::new(),aborted:Some(true),will_retry:Some(false),abort_source:Some(AbortSource::User) };
        api.registered.handlers[&EventKind::AgentEnd][0](&mut end,&ctx).await.unwrap();
        let goal=crate::store::read_goal(&reference).unwrap().unwrap(); assert_eq!(goal.status,GoalStatus::Blocked); assert_eq!(goal.time_used_seconds,2.0); assert!(!accounting.lock().await.turn_in_progress);
    }
}
