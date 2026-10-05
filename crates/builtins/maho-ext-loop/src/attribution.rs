use crate::{scheduler::{LoopScheduler,LoopTick,TickOutcome,SettledResult,KeepaliveResult},types::{LoopId,DeliveryId,CronEntry}};
/// Attribution survives extension-generated inputs and retrying turns, but never a user turn.
#[derive(Default)]
pub struct LoopAttribution { current:Option<(LoopId,DeliveryId)>,resolved:bool }
pub struct AttributedSettlement { pub settled:SettledResult,pub keepalive:Option<KeepaliveResult> }
impl LoopAttribution {
    pub fn dispatched(&mut self,tick:&LoopTick) { self.current=Some((tick.loop_id.clone(),tick.delivery_id.clone())); self.resolved=false; }
    pub fn input(&mut self,source:&str) { if source!="extension" { self.clear(); } }
    pub fn resolve(&mut self) { self.resolved=true; }
    pub fn clear(&mut self) { self.current=None; self.resolved=false; }
    pub fn target(&self)->Option<&str> { self.current.as_ref().map(|(id,_)|id.as_str()) }
    pub fn pause(&mut self,scheduler:&mut LoopScheduler,now:f64)->Vec<LoopId> {
        let target=self.target().unwrap_or("all").to_owned(); self.clear(); scheduler.pause(&target,now)
    }
    pub fn settle(&mut self,scheduler:&mut LoopScheduler,outcome:TickOutcome,now:f64,next_delivery:DeliveryId,wakeup:crate::types::WakeupId)->Option<AttributedSettlement> {
        if matches!(outcome,TickOutcome::Retrying) { return None; }
        let (id,delivery)=self.current.take()?;
        let dynamic=matches!(scheduler.state.entries.get(&id)?,CronEntry::Dynamic { .. });
        let resolved=std::mem::take(&mut self.resolved);
        let settled=scheduler.on_tick_settled(&id,&delivery,outcome,now,next_delivery);
        let keepalive=(!resolved&&dynamic).then(||scheduler.on_turn_ended_without_schedule(&id,wakeup,now));
        Some(AttributedSettlement { settled,keepalive })
    }
}
#[cfg(test)] mod tests {
    use super::*;
    use crate::{scheduler::CreateDynamicRequest,types::LoopPayload};
    fn setup()->(LoopScheduler,LoopAttribution) {
        let mut scheduler=LoopScheduler::new("s",None,&Default::default());
        scheduler.create_dynamic(CreateDynamicRequest { original_args:"check".into(),reentry_prompt:"/loop check".into(),payload:LoopPayload::Prompt { prompt:"check".into() } },"a".into(),0.0);
        let crate::scheduler::DueResult::Dispatch(tick)=scheduler.on_due("a",0.0,false,"d".into()) else { panic!("expected dispatch") };
        let mut attribution=LoopAttribution::default(); attribution.dispatched(&tick); (scheduler,attribution)
    }
    #[test] fn extension_input_retains_keepalive_attribution() {
        let (mut scheduler,mut attribution)=setup(); attribution.input("extension");
        let result=attribution.settle(&mut scheduler,TickOutcome::Completed,1.0,"next".into(),"wake".into()).unwrap();
        assert!(matches!(result.keepalive,Some(KeepaliveResult::Armed { .. }))); assert!(attribution.target().is_none());
    }
    #[test] fn user_input_cannot_charge_keepalive() {
        let (mut scheduler,mut attribution)=setup(); attribution.input("interactive");
        assert!(attribution.settle(&mut scheduler,TickOutcome::Completed,1.0,"next".into(),"wake".into()).is_none());
    }
    #[test] fn retry_keeps_delivery_and_does_not_settle() {
        let (mut scheduler,mut attribution)=setup();
        assert!(attribution.settle(&mut scheduler,TickOutcome::Retrying,1.0,"next".into(),"wake".into()).is_none());
        assert_eq!(attribution.target(),Some("a")); assert_eq!(scheduler.current_delivery_id("a"),Some("d"));
    }
    #[test] fn resolved_iteration_does_not_arm_keepalive() {
        let (mut scheduler,mut attribution)=setup(); attribution.resolve();
        assert!(attribution.settle(&mut scheduler,TickOutcome::Completed,1.0,"next".into(),"wake".into()).unwrap().keepalive.is_none());
    }
    #[test] fn abort_pauses_and_clears_delivery() {
        let (mut scheduler,mut attribution)=setup(); assert_eq!(attribution.pause(&mut scheduler,1.0),vec!["a"]);
        assert!(attribution.target().is_none()); assert!(scheduler.current_delivery_id("a").is_none()); assert!(scheduler.armed_timers.is_empty());
    }
}
