use std::{collections::BTreeMap,sync::{Arc,Mutex}};
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionFailure,ExtensionFuture};
use crate::{index::*,runtime::LoopRuntime,scheduler::{DueResult,ScheduleWakeupInput,ScheduleWakeupResult},types::*};

pub type LoopStoreReference=Arc<dyn Fn(&ExtensionContext)->LoopStoreRef+Send+Sync>;
struct Session {
    runtime:LoopRuntime,context:ExtensionContext,reference:LoopStoreRef,
    timers:NodeTimerPort,ticker:crate::status::LoopStatusTicker,named:bool,
}
struct Snapshot { state:LoopState,failure:Option<LoopStoreFailure>,errors:std::collections::BTreeSet<LoopId>,target:Option<LoopId> }
pub struct NativeLoopController {
    session:tokio::sync::Mutex<Option<Session>>,snapshot:Mutex<Snapshot>,
    api:Arc<ExtensionApi>,reference:LoopStoreReference,now:Arc<dyn Fn()->f64+Send+Sync>,
    ids:crate::ids::LoopIdFactory,home:String,on_fire:Arc<dyn Fn(LoopId)+Send+Sync>,
}
impl NativeLoopController {
    pub fn new(api:Arc<ExtensionApi>,reference:LoopStoreReference,now:Arc<dyn Fn()->f64+Send+Sync>,ids:crate::ids::LoopIdFactory,home:String,on_fire:Arc<dyn Fn(LoopId)+Send+Sync>)->Self {
        Self { session:tokio::sync::Mutex::new(None),snapshot:Mutex::new(Snapshot { state:crate::store::empty_loop_state(""),failure:None,errors:Default::default(),target:None }),api,reference,now,ids,home,on_fire }
    }
    fn publish(&self,session:&Session) {
        let failure=session.runtime.store_failure.as_ref().map(|failure|LoopStoreFailure { end_reason:failure.end_reason,message:failure.message.clone(),loop_ids:failure.loop_ids.clone() });
        *self.snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Snapshot { state:session.runtime.snapshot(),failure,errors:session.runtime.ended_with_error.clone(),target:session.runtime.attribution.target().map(str::to_owned) };
    }
    async fn refresh(&self,session:&mut Session)->Result<(),ExtensionFailure> {
        self.publish(session);
        if let Some(failure)=&session.runtime.store_failure {
            for worker in session.timers.cancel_all() { match worker.await { Ok(())=>(),Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(ExtensionFailure::new(error.to_string())) } }
            session.ticker.dispose().await?;
            session.context.ui.notify(&format!("/loop state could not be used and the affected loops were ended: {}",failure.message),maho_ext_api::NotificationType::Error);
            return Ok(());
        }
        crate::activation::sync_schedule_wakeup_activation(&self.api,&session.runtime.scheduler.state)?;
        for worker in session.runtime.sync_timers(&mut session.timers,(self.now)(),self.on_fire.clone()) { match worker.await { Ok(())=>(),Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(ExtensionFailure::new(error.to_string())) } }
        let state=session.runtime.snapshot();
        if state.entries.values().all(|entry|match entry { CronEntry::Fixed { lifecycle,.. }|CronEntry::Dynamic { lifecycle,.. }=>lifecycle.phase()==LoopPhase::Ended }) { if session.ticker.running() { session.ticker.dispose().await?; } }
        else { session.ticker.sync(state).await?; }
        Ok(())
    }
    async fn persist(&self,session:&mut Session)->Result<(),ExtensionFailure> {
        // Store failures are terminal in memory, as upstream: the failed store cannot
        // record its own failure, but no timer or subsequent delivery may survive it.
        let _=session.runtime.persist(&session.reference).await;
        self.refresh(session).await
    }
    pub async fn session_start(&self,context:&ExtensionContext)->Result<(),ExtensionFailure> {
        let mut owner=self.session.lock().await;
        if let Some(old)=owner.as_mut() {
            for worker in old.timers.cancel_all() { match worker.await { Ok(())=>(),Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(ExtensionFailure::new(error.to_string())) } }
            old.ticker.dispose().await?;
        }
        let reference=(self.reference)(context);
        let initial=crate::store::read_loop_state(&reference).await;
        let mut runtime=LoopRuntime::new(context.session_manager.session_id(),initial.as_ref().ok().cloned().flatten(),&std::env::vars().collect::<BTreeMap<_,_>>());
        if let Err(error)=initial { runtime.store_failure=Some(LoopStoreFailure { end_reason:LoopEndReason::Error,message:error.to_string(),loop_ids:Vec::new() }); }
        let ui=context.ui.clone();
        let ticker=crate::status::LoopStatusTicker::new(Arc::new(move |key,text| { ui.set_status(key,text); Ok(()) }),self.now.clone());
        let mut session=Session { runtime,context:context.clone(),reference,timers:NodeTimerPort::new(),ticker,named:false };
        if session.runtime.store_failure.is_some() { self.refresh(&mut session).await?; *owner=Some(session); return Ok(()); }
        let restored=session.runtime.scheduler.restore((self.now)(),&[],|| (self.ids)("delivery"));
        self.persist(&mut session).await?;
        for tick in restored.recovery_ticks { self.dispatch(&mut session,&tick).await?; }
        if !restored.expired_loop_ids.is_empty() { context.ui.notify(&format!("Loop expired after 7 days: {}",restored.expired_loop_ids.join(", ")),maho_ext_api::NotificationType::Info); }
        *owner=Some(session); Ok(())
    }
    async fn dispatch(&self,session:&mut Session,tick:&crate::scheduler::LoopTick)->Result<(),ExtensionFailure> {
        let result=session.runtime.dispatch_tick(&self.api,&session.context,&session.reference,tick,&self.home).await;
        if session.runtime.store_failure.is_none() { result?; }
        self.refresh(session).await
    }
    async fn due(&self,session:&mut Session,id:&str)->Result<(),ExtensionFailure> {
        let busy=!session.context.is_idle()||session.context.has_pending_messages()?;
        let result=session.runtime.due(id,(self.now)(),busy,(self.ids)("delivery"));
        self.persist(session).await?;
        match result {
            DueResult::Dispatch(tick)=>self.dispatch(session,&tick).await?,
            DueResult::Expire=>session.context.ui.notify("Loop expired after 7 days and is no longer armed.",maho_ext_api::NotificationType::Info),
            DueResult::Coalesce=>{},
        }
        Ok(())
    }
    pub async fn timer_fire(&self,id:&str) {
        if let Err(error)=self.fire_due(id).await {
            let owner=self.session.lock().await;
            if let Some(session)=owner.as_ref() { session.context.ui.notify(&format!("Loop tick failed: {}",error.message),maho_ext_api::NotificationType::Error); }
        }
    }
    async fn created(&self,session:&mut Session,outcome:&LoopCreateOutcome,name:&str)->Result<(),ExtensionFailure> {
        if let LoopCreateOutcome::Created(created)=outcome {
            session.runtime.delivery_states.insert(created.loop_id.clone(),Default::default());
            self.persist(session).await?;
            if !session.named { session.named=true; if self.api.get_session_name()?.is_none() { self.api.set_session_name(format!("loop: {name}").trim())?; } }
            self.due(session,&created.loop_id).await?;
        }
        Ok(())
    }
    pub async fn event(&self,event:&maho_ext_api::ExtensionEvent)->Result<(),ExtensionFailure> {
        use maho_ext_api::{ExtensionEvent,AbortSource,InputSource,SessionCompactEvent};
        let mut owner=self.session.lock().await;
        let Some(session)=owner.as_mut() else { return Ok(()); };
        match event {
            ExtensionEvent::Input(input)=>{ if input.source!=InputSource::Extension { session.runtime.attribution.clear(); self.publish(session); } return Ok(()); },
            ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted { .. })=>session.runtime.accepted_compaction(),
            ExtensionEvent::AgentEnd { will_retry:Some(true),.. }=>return Ok(()),
            ExtensionEvent::SessionAbort|ExtensionEvent::AgentEnd { aborted:Some(true),abort_source:Some(AbortSource::User),.. }=>{
                let LoopRuntime { scheduler,attribution,.. }=&mut session.runtime;
                if attribution.pause(scheduler,(self.now)()).is_empty() { return Ok(()); }
                self.persist(session).await?;
                session.context.ui.notify("loop paused - /loop resume or /loop stop",maho_ext_api::NotificationType::Info); return Ok(());
            },
            ExtensionEvent::AgentEnd { aborted,.. }=>{ self.settle(session,if *aborted==Some(true) { crate::scheduler::TickOutcome::Error } else { crate::scheduler::TickOutcome::Completed }).await?; return Ok(()); },
            ExtensionEvent::AgentSettled=>{
                self.settle(session,crate::scheduler::TickOutcome::Completed).await?;
                let deferred=std::mem::take(&mut session.runtime.deferred_dispatches);
                for tick in deferred { self.dispatch(session,&tick).await?; }
                return self.refresh(session).await;
            },
            ExtensionEvent::SessionShutdown(_)=>{
                session.runtime.shutdown((self.now)()); self.persist(session).await?;
                for worker in session.timers.cancel_all() { match worker.await { Ok(())=>(),Err(error) if error.is_cancelled()=>(),Err(error)=>return Err(ExtensionFailure::new(error.to_string())) } }
                session.ticker.dispose().await?; *owner=None; return Ok(());
            },_=>return Ok(()),
        }
        self.persist(session).await
    }
    async fn settle(&self,session:&mut Session,outcome:crate::scheduler::TickOutcome)->Result<(),ExtensionFailure> {
        let Some(result)=session.runtime.settled(outcome,(self.now)(),(self.ids)("delivery"),(self.ids)("wakeup")) else { return Ok(()); };
        self.persist(session).await?;
        if let Some(keepalive)=result.keepalive {
            if let crate::scheduler::KeepaliveResult::Ended(reason)=keepalive { session.context.ui.notify(if reason==LoopEndReason::KeepaliveExhausted { "Loop ended: the model stopped scheduling wakeups." } else { "Loop expired after 7 days and is no longer armed." },maho_ext_api::NotificationType::Info); }
        } else if let crate::scheduler::SettledResult::Due(DueResult::Dispatch(tick))=result.settled { self.dispatch(session,&tick).await?; }
        Ok(())
    }
}
fn no_session()->ExtensionFailure { ExtensionFailure::new("loop extension has no active session") }
impl crate::tools::ScheduleWakeupSchedulerPort for NativeLoopController {
    fn get_wakeup_target(&self)->Option<crate::tools::ScheduleWakeupTarget> {
        let snapshot=self.snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id=snapshot.target.as_ref()?;
        let (kind,lifecycle)=match snapshot.state.entries.get(id)? { CronEntry::Fixed { lifecycle,.. }=>(LoopKind::Fixed,lifecycle),CronEntry::Dynamic { lifecycle,.. }=>(LoopKind::Dynamic,lifecycle) };
        (lifecycle.phase()!=LoopPhase::Ended).then(||crate::tools::ScheduleWakeupTarget { kind,loop_id:id.clone() })
    }
    fn schedule_wakeup(&self,request:crate::tools::ScheduleWakeupRequest)->ExtensionFuture<'_,crate::tools::ScheduleWakeupOutcome> { Box::pin(async move {
        let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?;
        let request=ScheduleWakeupInput { loop_id:request.loop_id,requested_delay_seconds:request.requested_delay_seconds,delay_seconds:request.delay_seconds,reason:request.reason,prompt:request.prompt,noop:request.noop };
        let result=session.runtime.scheduler.on_schedule_wakeup(request,(self.ids)("wakeup"),(self.now)());
        let outcome=match result {
            ScheduleWakeupResult::Scheduled { wakeup_id,replaced_wakeup_id,due_at,noop_streak }=>crate::tools::ScheduleWakeupOutcome { wakeup_id,replaced_wakeup_id,due_at,noop_streak },
            result=>return Err(ExtensionFailure::new(format!("schedule_wakeup rejected: {}",match result { ScheduleWakeupResult::UnknownLoop=>"unknown_loop",ScheduleWakeupResult::NotDynamic=>"not_dynamic",ScheduleWakeupResult::Ended=>"ended",ScheduleWakeupResult::Expired=>"expired",ScheduleWakeupResult::Scheduled { .. }=>unreachable!() }))),
        };
        session.runtime.attribution.resolve(); self.persist(session).await?; Ok(outcome)
    }) }
    fn stop_dynamic_loop(&self,request:crate::tools::StopDynamicLoopRequest)->ExtensionFuture<'_,crate::tools::StopDynamicLoopOutcome> { Box::pin(async move {
        let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?;
        session.runtime.scheduler.stop(&request.loop_id,"model-stop",(self.now)()); session.runtime.attribution.resolve(); self.persist(session).await?;
        Ok(crate::tools::StopDynamicLoopOutcome { ended_at:(self.now)() })
    }) }
}
impl LoopController for NativeLoopController {
    fn start_fixed(&self,request:StartFixedRequest)->ExtensionFuture<'_,LoopCreateOutcome> { Box::pin(async move {
        let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?; let name=request.prompt.clone();
        let result=crate::creation::create_fixed(&mut session.runtime.scheduler,request,(self.ids)("loop"),(self.now)()); self.created(session,&result,&name).await?; Ok(result)
    }) }
    fn start_dynamic(&self,request:StartDynamicRequest)->ExtensionFuture<'_,LoopCreateOutcome> { Box::pin(async move {
        let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?; let name=request.prompt.clone();
        let result=crate::creation::create_dynamic(&mut session.runtime.scheduler,request,(self.ids)("loop"),(self.now)()); self.created(session,&result,&name).await?; Ok(result)
    }) }
    fn start_bare(&self,request:StartBareRequest)->ExtensionFuture<'_,LoopCreateOutcome> { Box::pin(async move {
        let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?;
        let file=crate::loopfile::resolve_loop_file(&session.context.cwd.to_string_lossy(),&self.home,&crate::loopfile::NodeFs).ok().flatten().is_some();
        let trimmed=request.original_args.trim(); let name=if trimmed.is_empty() { "/loop".into() } else { format!("/loop {trimmed}") };
        let result=crate::creation::create_bare(&mut session.runtime.scheduler,request,(self.ids)("loop"),(self.now)(),file); self.created(session,&result,&name).await?; Ok(result)
    }) }
    fn fire_due(&self,loop_id:&str)->ExtensionFuture<'_,()> { let id=loop_id.to_owned(); Box::pin(async move { let mut owner=self.session.lock().await; self.due(owner.as_mut().ok_or_else(no_session)?,&id).await }) }
    fn schedule_wakeup(&self,request:ScheduleWakeupInput)->ExtensionFuture<'_,()> { Box::pin(async move { let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?; if matches!(session.runtime.scheduler.on_schedule_wakeup(request,(self.ids)("wakeup"),(self.now)()),ScheduleWakeupResult::Scheduled { .. }) { session.runtime.attribution.resolve(); } self.persist(session).await }) }
    fn stop(&self,target:&str,detail:&str)->ExtensionFuture<'_,Vec<LoopId>> { let target=target.to_owned(); let detail=detail.to_owned(); Box::pin(async move { let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?; let affected=session.runtime.scheduler.stop(&target,&detail,(self.now)()); self.persist(session).await?; Ok(affected) }) }
    fn pause(&self,target:&str)->ExtensionFuture<'_,Vec<LoopId>> { let target=target.to_owned(); Box::pin(async move { let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?; let affected=session.runtime.scheduler.pause(&target,(self.now)()); self.persist(session).await?; Ok(affected) }) }
    fn resume(&self,target:&str)->ExtensionFuture<'_,Vec<LoopId>> { let target=target.to_owned(); Box::pin(async move { let mut owner=self.session.lock().await; let session=owner.as_mut().ok_or_else(no_session)?; let affected=session.runtime.scheduler.resume(&target,(self.now)()); self.persist(session).await?; Ok(affected) }) }
    fn get_state(&self)->LoopState { self.snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).state.clone() }
    fn status_line(&self)->Option<String> { crate::status::format_loop_status(&self.get_state(),(self.now)()) }
    fn last_store_failure(&self)->Option<LoopStoreFailure> { self.snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).failure.as_ref().map(|failure|LoopStoreFailure { end_reason:failure.end_reason,message:failure.message.clone(),loop_ids:failure.loop_ids.clone() }) }
    fn is_ended_with_error(&self,id:&str)->bool { self.snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).errors.contains(id) }
}
