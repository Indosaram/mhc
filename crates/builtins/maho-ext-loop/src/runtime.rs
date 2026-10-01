use std::collections::{BTreeMap,BTreeSet};
use crate::{scheduler::{LoopScheduler,LoopTick,DueResult,TickOutcome},types::*,attribution::LoopAttribution,index::LoopStoreFailure};
pub struct LoopRuntime {
    pub scheduler:LoopScheduler,pub attribution:LoopAttribution,
    pub deferred_dispatches:Vec<LoopTick>,pub delivery_states:BTreeMap<LoopId,SentinelDeliveryState>,
    pub ended_with_error:BTreeSet<LoopId>,pub store_failure:Option<LoopStoreFailure>,
    store_touched:bool,
}
impl LoopRuntime {
    pub fn sync_timers(&self,timers:&mut crate::index::NodeTimerPort,now:f64,on_fire:std::sync::Arc<dyn Fn(LoopId)+Send+Sync>)->Vec<tokio::task::JoinHandle<()>> {
        let mut retired=Vec::new();
        for key in timers.keys() { if !self.scheduler.armed_timers.contains_key(&key)&&let Some(handle)=timers.cancel(&key) { retired.push(handle); } }
        for (id,due) in &self.scheduler.armed_timers {
            let callback=on_fire.clone(); let fired=id.clone();
            if let Some(handle)=timers.arm(id,*due,now,move ||callback(fired)) { retired.push(handle); }
        }
        retired
    }
    pub fn new(session_id:&str,initial:Option<LoopState>,env:&BTreeMap<String,String>)->Self {
        let store_touched=initial.is_some();
        Self { scheduler:LoopScheduler::new(session_id,initial,env),attribution:Default::default(),deferred_dispatches:Vec::new(),delivery_states:BTreeMap::new(),ended_with_error:BTreeSet::new(),store_failure:None,store_touched }
    }
    pub fn snapshot(&self)->LoopState {
        let mut state=self.scheduler.state.clone();
        for (id,delivery) in &self.delivery_states {
            if let Some(entry)=state.entries.get_mut(id) { match entry { CronEntry::Fixed { fields,.. }|CronEntry::Dynamic { fields,.. }=>fields.sentinel_delivery=delivery.clone() } }
        }
        state
    }
    pub async fn persist(&mut self,reference:&LoopStoreRef)->Result<(),crate::store::LoopStoreError> {
        let state=self.snapshot(); if !self.store_touched&&state.entries.is_empty() { return Ok(()); }
        self.store_touched=true;
        if let Err(error)=crate::store::write_loop_state(reference,&state).await {
            self.scheduler.armed_timers.clear(); self.ended_with_error.extend(state.entries.keys().cloned());
            self.store_failure=Some(LoopStoreFailure { end_reason:LoopEndReason::Error,message:error.to_string(),loop_ids:state.entries.keys().cloned().collect() }); return Err(error);
        }
        Ok(())
    }
    pub fn prepare_tick(&mut self,tick:&LoopTick,file:crate::tick_prompt::LoopFileSnapshot,busy:bool,commands:&[maho_ext_api::SlashCommandInfo])->Option<crate::index::PreparedLoopTick> {
        if self.ended_with_error.contains(&tick.loop_id) { return None; }
        let entry=self.scheduler.state.entries.get(&tick.loop_id)?;
        let delivery=self.delivery_states.get(&tick.loop_id).cloned().unwrap_or_default();
        let prepared=crate::index::prepare_loop_tick(entry,tick,delivery,file,busy,commands)?;
        self.delivery_states.insert(tick.loop_id.clone(),prepared.delivery_state.clone());
        if prepared.defer { self.deferred_dispatches.push(tick.clone()); return None; }
        self.attribution.dispatched(tick); Some(prepared)
    }
    pub fn due(&mut self,id:&str,now:f64,busy:bool,delivery:DeliveryId)->DueResult { self.scheduler.on_due(id,now,busy,delivery) }
    pub async fn dispatch_tick(&mut self,api:&maho_ext_api::ExtensionApi,ctx:&maho_ext_api::ExtensionContext,reference:&LoopStoreRef,tick:&LoopTick,home:&str)->Result<(),maho_ext_api::ExtensionFailure> {
        let file=match self.scheduler.state.entries.get(&tick.loop_id) {
            Some(CronEntry::Fixed { fields,.. }|CronEntry::Dynamic { fields,.. }) if matches!(fields.payload,LoopPayload::Sentinel { .. })=>{
                match crate::loopfile::resolve_loop_file(&ctx.cwd.to_string_lossy(),home,&crate::loopfile::NodeFs).ok().flatten() {
                    Some(file)=>crate::tick_prompt::LoopFileSnapshot::Present { path:file.path.to_string_lossy().into_owned(),content:file.content,mtime_ms:file.fingerprint.mtime_ms,size:file.fingerprint.size as f64,content_hash:file.fingerprint.content_hash },None=>crate::tick_prompt::LoopFileSnapshot::Absent,
                }
            },_=>crate::tick_prompt::LoopFileSnapshot::Absent,
        };
        let busy=!ctx.is_idle()||ctx.has_pending_messages()?;
        let commands=api.get_commands()?;
        let Some(prepared)=self.prepare_tick(tick,file,busy,&commands) else { return Ok(()); };
        crate::activation::sync_schedule_wakeup_activation(api,&self.scheduler.state)?;
        crate::index::deliver_loop_tick(api,prepared,busy)?;
        self.persist(reference).await.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))
    }
    pub fn settled(&mut self,outcome:TickOutcome,now:f64,next_delivery:DeliveryId,wakeup:WakeupId)->Option<crate::attribution::AttributedSettlement> { self.attribution.settle(&mut self.scheduler,outcome,now,next_delivery,wakeup) }
    pub fn restore_anchors(&mut self,entries:&[maho_ext_api::SessionEntry]) {
        for (id,entry) in &self.scheduler.state.entries {
            let fields=match entry { CronEntry::Fixed { fields,.. }|CronEntry::Dynamic { fields,.. }=>fields };
            self.delivery_states.insert(id.clone(),crate::anchors::restored_delivery_state(&fields.sentinel_delivery,entries,id));
        }
    }
    pub fn accepted_compaction(&mut self) {
        for (id,entry) in &self.scheduler.state.entries {
            let (fields,lifecycle)=match entry { CronEntry::Fixed { fields,lifecycle,.. }|CronEntry::Dynamic { fields,lifecycle,.. }=>(fields,lifecycle) };
            if lifecycle.phase!=LoopPhase::Ended { self.delivery_states.entry(id.clone()).or_insert_with(||fields.sentinel_delivery.clone()).force_full_delivery=true; }
        }
    }
    pub fn shutdown(&mut self,now:f64) { self.scheduler.on_shutdown(now); self.deferred_dispatches.clear(); self.attribution.clear(); }
}
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test(start_paused=true)] async fn scheduler_timer_reconciliation_fires_and_shutdown_cancels() {
        let mut runtime=LoopRuntime::new("s",None,&Default::default()); let mut timers=crate::index::NodeTimerPort::new();
        runtime.scheduler.armed_timers.insert("a".into(),1000.0);
        let (send,mut receive)=tokio::sync::mpsc::unbounded_channel(); let callback=std::sync::Arc::new(move |id| { send.send(id).unwrap(); });
        assert!(runtime.sync_timers(&mut timers,0.0,callback.clone()).is_empty());
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(2),receive.recv()).await.unwrap().unwrap(),"a");
        runtime.scheduler.armed_timers.insert("a".into(),2000.0);
        for worker in runtime.sync_timers(&mut timers,1000.0,callback.clone()) { worker.await.unwrap(); }
        runtime.shutdown(1000.0);
        for worker in runtime.sync_timers(&mut timers,1000.0,callback) { assert!(worker.await.unwrap_err().is_cancelled()); }
        assert!(receive.recv().await.is_none());
    }
    #[tokio::test] async fn prepared_delivery_overlay_persists_and_shutdown_suspends() {
        let dir=tempfile::tempdir().unwrap(); let reference=LoopStoreRef { base_dir:dir.path().into(),session_id:"s".into() }; let mut runtime=LoopRuntime::new("s",None,&Default::default());
        crate::creation::create_bare(&mut runtime.scheduler,crate::index::StartBareRequest { original_args:String::new(),interval:None },"a".into(),0.0,false);
        let DueResult::Dispatch(tick)=runtime.due("a",0.0,false,"d".into()) else { panic!("expected dispatch") };
        assert!(runtime.prepare_tick(&tick,crate::tick_prompt::LoopFileSnapshot::Absent,false,&[]).is_some());
        runtime.persist(&reference).await.unwrap(); let saved=crate::store::read_loop_state(&reference).await.unwrap().unwrap();
        let CronEntry::Dynamic { fields,.. }=&saved.entries["a"] else { panic!("expected dynamic") }; assert!(fields.sentinel_delivery.autonomous_preamble_delivered);
        runtime.shutdown(1.0); runtime.persist(&reference).await.unwrap();
        let saved=crate::store::read_loop_state(&reference).await.unwrap().unwrap(); let CronEntry::Dynamic { lifecycle,.. }=&saved.entries["a"] else { panic!("expected dynamic") }; assert_eq!(lifecycle.phase,LoopPhase::Suspended); assert_eq!(lifecycle.end_reason,None);
    }
    #[tokio::test] async fn unused_runtime_does_not_create_sidecar() {
        let dir=tempfile::tempdir().unwrap(); let reference=LoopStoreRef { base_dir:dir.path().into(),session_id:"s".into() }; let mut runtime=LoopRuntime::new("s",None,&Default::default()); runtime.persist(&reference).await.unwrap(); assert!(crate::store::read_loop_state(&reference).await.unwrap().is_none());
    }
    #[tokio::test] async fn write_failure_disarms_and_blocks_delivery() {
        let dir=tempfile::tempdir().unwrap(); let path=dir.path().join("file"); std::fs::write(&path,"not a directory").unwrap(); let reference=LoopStoreRef { base_dir:path,session_id:"s".into() }; let mut runtime=LoopRuntime::new("s",None,&Default::default());
        crate::creation::create_dynamic(&mut runtime.scheduler,crate::index::StartDynamicRequest { original_args:"work".into(),prompt:"work".into() },"a".into(),0.0);
        assert!(runtime.persist(&reference).await.is_err()); assert!(runtime.scheduler.armed_timers.is_empty()); assert!(runtime.ended_with_error.contains("a")); assert!(runtime.store_failure.is_some());
    }
}
