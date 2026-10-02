use std::collections::BTreeMap;
use crate::{store::empty_loop_state,types::*};
pub const LOOP_EXPIRY_MS:f64=604_800_000.0;
pub const MAX_ACTIVE_LOOPS:usize=5;
pub const DEFAULT_KEEPALIVE_SECONDS:f64=1200.0;
pub const MIN_KEEPALIVE_SECONDS:f64=60.0;
pub const MAX_KEEPALIVE_SECONDS:f64=3600.0;
pub const DEFAULT_MAX_TICKS:f64=2000.0;
pub const KEEPALIVE_SECONDS_ENV:&str="SENPI_LOOP_KEEPALIVE_SECONDS";
pub const MAX_TICKS_ENV:&str="SENPI_LOOP_MAX_TICKS";
pub struct CreateDynamicRequest { pub original_args:String,pub reentry_prompt:String,pub payload:LoopPayload }
pub struct CreateFixedRequest { pub base:CreateDynamicRequest,pub requested_interval:RequestedInterval,pub effective_interval:EffectiveInterval,pub cron_expression:String,pub interval_ms:f64 }
#[derive(Clone,Debug,PartialEq)]
pub enum CreateResult { Created { loop_id:LoopId,superseded_loop_id:Option<LoopId> },Cap { active_loop_ids:Vec<LoopId> } }
#[derive(Clone,Debug,PartialEq)]
pub struct LoopTick { pub loop_id:LoopId,pub delivery_id:DeliveryId,pub scheduled_for_at:f64,pub coalesced:bool }
#[derive(Clone,Debug,PartialEq)]
pub enum DueResult { Dispatch(LoopTick),Coalesce,Expire }
pub enum TickOutcome { Completed,Error,Retrying }
#[derive(Clone,Debug,PartialEq)]
pub enum SettledResult { Due(DueResult),Idle,InProgress,Ended(LoopEndReason),Ignored }
pub struct ScheduleWakeupInput { pub loop_id:LoopId,pub delay_seconds:f64,pub requested_delay_seconds:f64,pub reason:String,pub prompt:String,pub noop:bool }
#[derive(Clone,Debug,PartialEq)]
pub enum ScheduleWakeupResult { Scheduled { wakeup_id:WakeupId,replaced_wakeup_id:Option<WakeupId>,due_at:f64,noop_streak:f64 },UnknownLoop,NotDynamic,Ended,Expired }
#[derive(Clone,Debug,PartialEq)]
pub enum KeepaliveResult { Armed { wakeup_id:WakeupId,delay_seconds:f64,due_at:f64 },Ended(LoopEndReason),Ignored }
#[derive(Default)]
pub struct RestoreResult { pub recovery_ticks:Vec<LoopTick>,pub expired_loop_ids:Vec<LoopId>,pub rearmed_loop_ids:Vec<LoopId>,pub still_paused_loop_ids:Vec<LoopId> }
fn fields(entry:&CronEntry)->&LoopEntryFields { match entry { CronEntry::Fixed { fields,.. }|CronEntry::Dynamic { fields,.. }=>fields } }
fn fields_mut(entry:&mut CronEntry)->&mut LoopEntryFields { match entry { CronEntry::Fixed { fields,.. }|CronEntry::Dynamic { fields,.. }=>fields } }
fn lifecycle(entry:&CronEntry)->&LoopLifecycle { match entry { CronEntry::Fixed { lifecycle,.. }|CronEntry::Dynamic { lifecycle,.. }=>lifecycle } }
fn lifecycle_mut(entry:&mut CronEntry)->&mut LoopLifecycle { match entry { CronEntry::Fixed { lifecycle,.. }|CronEntry::Dynamic { lifecycle,.. }=>lifecycle } }
fn phase(entry:&mut CronEntry,next:LoopPhase) { *lifecycle_mut(entry)=LoopLifecycle { phase:next,ended_at:None,end_reason:None,end_detail:None }; }
fn int_env(env:&BTreeMap<String,String>,name:&str,fallback:f64,min:f64,max:f64)->f64 { env.get(name).filter(|raw|!maho_ai::utils::js::trim(raw).is_empty()).map(|raw|maho_ai::utils::js::string_to_number(raw)).filter(|value|value.is_finite() && value.fract()==0.0).map_or(fallback,|value|value.clamp(min,max)) }
pub struct LoopScheduler { pub state:LoopState,pub mutation_count:u64,pub max_ticks:f64,pub keepalive_seconds:f64,pub armed_timers:BTreeMap<LoopId,f64>,in_flight_deliveries:BTreeMap<LoopId,DeliveryId>,order:Vec<LoopId> }
impl LoopScheduler {
    pub fn new(session_id:&str,initial:Option<LoopState>,env:&BTreeMap<String,String>)->Self { let state=initial.unwrap_or_else(||empty_loop_state(session_id)); let order=state.entries.keys().cloned().collect(); Self { state,mutation_count:0,max_ticks:int_env(env,MAX_TICKS_ENV,DEFAULT_MAX_TICKS,1.0,9_007_199_254_740_991.0),keepalive_seconds:int_env(env,KEEPALIVE_SECONDS_ENV,DEFAULT_KEEPALIVE_SECONDS,MIN_KEEPALIVE_SECONDS,MAX_KEEPALIVE_SECONDS),armed_timers:BTreeMap::new(),in_flight_deliveries:BTreeMap::new(),order } }
    fn commit(&mut self,now:f64) { self.state.updated_at=now; self.mutation_count+=1; }
    fn active_ids(&self)->Vec<LoopId> { self.order.iter().filter(|id|self.state.entries.get(*id).is_some_and(|entry|lifecycle(entry).phase!=LoopPhase::Ended)).cloned().collect() }
    fn end(&mut self,id:&str,now:f64,reason:LoopEndReason,detail:Option<String>) { self.armed_timers.remove(id); self.in_flight_deliveries.remove(id); if let Some(entry)=self.state.entries.get_mut(id) { *lifecycle_mut(entry)=LoopLifecycle { phase:LoopPhase::Ended,ended_at:Some(now),end_reason:Some(reason),end_detail:detail }; if let CronEntry::Dynamic { pending_wakeup,.. }=entry { *pending_wakeup=None; } } }
    fn arm(&mut self,id:&str,now:f64)->Option<LoopEndReason> {
        let entry=self.state.entries.get(id)?; let expiry=fields(entry).expires_at; let due=match entry { CronEntry::Fixed { next_fire_at,.. }=>Some(*next_fire_at),CronEntry::Dynamic { pending_wakeup,.. }=>pending_wakeup.as_ref().map(|wake|wake.due_at) };
        if now>=expiry || due.is_some_and(|due|due>=expiry) { self.end(id,now,LoopEndReason::Expired,None); return Some(LoopEndReason::Expired); }
        if let Some(due)=due { self.armed_timers.insert(id.into(),due); } else { self.armed_timers.remove(id); }
        None
    }
    fn base(now:f64,id:LoopId,request:CreateDynamicRequest)->LoopEntryFields { LoopEntryFields { id,original_args:request.original_args,reentry_prompt:request.reentry_prompt,payload:request.payload,created_at:now,last_fired_at:None,expires_at:now+LOOP_EXPIRY_MS,last_scheduled_for_at:None,coalesced_fire_pending:false,queued_for_at:None,noop_streak:0.0,tick_count:0.0,sentinel_delivery:Default::default(),wake_sources:vec![] } }
    pub fn current_delivery_id(&self,id:&str)->Option<&str> { self.in_flight_deliveries.get(id).map(String::as_str) }
    pub fn create_fixed(&mut self,request:CreateFixedRequest,id:LoopId,now:f64)->CreateResult {
        let active=self.active_ids(); if active.len()>=MAX_ACTIVE_LOOPS { return CreateResult::Cap { active_loop_ids:active }; }
        let entry=CronEntry::Fixed { fields:Self::base(now,id.clone(),request.base),lifecycle:LoopLifecycle { phase:LoopPhase::Waiting,ended_at:None,end_reason:None,end_detail:None },requested_interval:request.requested_interval,effective_interval:request.effective_interval,cron_expression:request.cron_expression,next_fire_at:now+request.interval_ms,interval_ms:request.interval_ms };
        self.state.entries.insert(id.clone(),entry); self.order.push(id.clone()); self.arm(&id,now); self.commit(now); CreateResult::Created { loop_id:id,superseded_loop_id:None }
    }
    pub fn create_dynamic(&mut self,request:CreateDynamicRequest,id:LoopId,now:f64)->CreateResult {
        let previous=self.state.active_dynamic_id.clone().filter(|id|self.state.entries.get(id).is_some_and(|entry|lifecycle(entry).phase!=LoopPhase::Ended)); let active=self.active_ids(); if active.iter().filter(|id|Some(*id)!=previous.as_ref()).count()>=MAX_ACTIVE_LOOPS { return CreateResult::Cap { active_loop_ids:active }; }
        if let Some(previous)=&previous { self.end(previous,now,LoopEndReason::Stopped,Some("superseded".into())); }
        self.state.entries.insert(id.clone(),CronEntry::Dynamic { fields:Self::base(now,id.clone(),request),lifecycle:LoopLifecycle { phase:LoopPhase::Waiting,ended_at:None,end_reason:None,end_detail:None },pending_wakeup:None,keepalive_credit:1 }); self.order.push(id.clone()); self.state.active_dynamic_id=Some(id.clone()); self.commit(now); CreateResult::Created { loop_id:id,superseded_loop_id:previous }
    }
    pub fn on_due(&mut self,id:&str,now:f64,busy:bool,delivery_id:DeliveryId)->DueResult {
        let Some(entry)=self.state.entries.get(id) else { return DueResult::Coalesce; };
        if now>=fields(entry).expires_at { if lifecycle(entry).phase==LoopPhase::Ended { return DueResult::Coalesce; } self.end(id,now,LoopEndReason::Expired,None); self.commit(now); return DueResult::Expire; }
        if matches!(lifecycle(entry).phase,LoopPhase::Ended|LoopPhase::Suspended) || fields(entry).tick_count>=self.max_ticks { return DueResult::Coalesce; }
        let in_flight=matches!(lifecycle(entry).phase,LoopPhase::Queued|LoopPhase::Running); let coalesced=fields(entry).coalesced_fire_pending;
        if let Some(entry)=self.state.entries.get_mut(id) {
            if in_flight { fields_mut(entry).coalesced_fire_pending=true; } else {
                phase(entry,if busy { LoopPhase::Queued } else { LoopPhase::Running }); let base=fields_mut(entry); base.last_fired_at=Some(now); base.last_scheduled_for_at=Some(now); base.queued_for_at=Some(now); base.coalesced_fire_pending=false; base.tick_count+=1.0;
                if let CronEntry::Dynamic { pending_wakeup,.. }=entry { *pending_wakeup=None; }
            }
            if let CronEntry::Fixed { next_fire_at,interval_ms,.. }=entry { *next_fire_at=now+*interval_ms; }
        }
        let fixed=matches!(self.state.entries.get(id),Some(CronEntry::Fixed { .. })); let expired=if fixed || in_flight { self.arm(id,now).is_some() } else { self.armed_timers.remove(id); false }; self.commit(now);
        if in_flight { return DueResult::Coalesce; }
        if !expired { self.in_flight_deliveries.insert(id.into(),delivery_id.clone()); }
        DueResult::Dispatch(LoopTick { loop_id:id.into(),delivery_id,scheduled_for_at:now,coalesced })
    }
    pub fn on_tick_settled(&mut self,id:&str,delivery:&str,outcome:TickOutcome,now:f64,next_delivery:DeliveryId)->SettledResult {
        let Some(entry)=self.state.entries.get(id) else { return SettledResult::Ignored; };
        if matches!(outcome,TickOutcome::Retrying) { return SettledResult::InProgress; }
        if self.current_delivery_id(id)!=Some(delivery) { return SettledResult::Ignored; }
        let phase_now=lifecycle(entry).phase; let expired=now>=fields(entry).expires_at; let exhausted=fields(entry).tick_count>=self.max_ticks; let coalesced=fields(entry).coalesced_fire_pending;
        self.in_flight_deliveries.remove(id);
        if phase_now==LoopPhase::Ended { return SettledResult::Ignored; }
        if phase_now==LoopPhase::Suspended { return SettledResult::Idle; }
        if expired || exhausted { let reason=if expired { LoopEndReason::Expired } else { LoopEndReason::TickBudgetExhausted }; self.end(id,now,reason,None); self.commit(now); return SettledResult::Ended(reason); }
        if let Some(entry)=self.state.entries.get_mut(id) { phase(entry,LoopPhase::Waiting); fields_mut(entry).queued_for_at=None; if coalesced { fields_mut(entry).coalesced_fire_pending=false; } }
        self.commit(now); if coalesced { return SettledResult::Due(self.on_due(id,now,false,next_delivery)); }
        if matches!(self.state.entries.get(id),Some(CronEntry::Fixed { .. })) && let Some(reason)=self.arm(id,now) { return SettledResult::Ended(reason); } SettledResult::Idle
    }
    pub fn on_schedule_wakeup(&mut self,request:ScheduleWakeupInput,wakeup_id:WakeupId,now:f64)->ScheduleWakeupResult {
        let Some(entry)=self.state.entries.get(&request.loop_id) else { return ScheduleWakeupResult::UnknownLoop; }; let CronEntry::Dynamic { fields,lifecycle,pending_wakeup,.. }=entry else { return ScheduleWakeupResult::NotDynamic; };
        if lifecycle.phase==LoopPhase::Ended { return ScheduleWakeupResult::Ended; }
        if now>=fields.expires_at { self.end(&request.loop_id,now,LoopEndReason::Expired,None); self.commit(now); return ScheduleWakeupResult::Expired; }
        let replaced=pending_wakeup.as_ref().map(|wake|wake.id.clone()); let due_at=now+request.delay_seconds*1000.0; let noop_streak=if request.noop { fields.noop_streak+1.0 } else { 0.0 };
        if let Some(entry)=self.state.entries.get_mut(&request.loop_id) { phase(entry,LoopPhase::Waiting); if let CronEntry::Dynamic { fields,pending_wakeup,keepalive_credit,.. }=entry { fields.queued_for_at=None; fields.noop_streak=noop_streak; *keepalive_credit=1; *pending_wakeup=Some(PendingWakeup { id:wakeup_id.clone(),loop_id:request.loop_id.clone(),kind:DynamicKind::Dynamic,source:WakeupSource::Model,requested_delay_seconds:request.requested_delay_seconds,delay_seconds:request.delay_seconds,due_at,reason:request.reason,prompt:request.prompt,noop:request.noop,created_at:now }); } }
        self.arm(&request.loop_id,now); self.commit(now); ScheduleWakeupResult::Scheduled { wakeup_id,replaced_wakeup_id:replaced,due_at,noop_streak }
    }
    pub fn on_turn_ended_without_schedule(&mut self,id:&str,wakeup_id:WakeupId,now:f64)->KeepaliveResult {
        let Some(CronEntry::Dynamic { fields,lifecycle,keepalive_credit,.. })=self.state.entries.get(id) else { return KeepaliveResult::Ignored; };
        if matches!(lifecycle.phase,LoopPhase::Ended|LoopPhase::Suspended) { return KeepaliveResult::Ignored; }
        if now>=fields.expires_at || *keepalive_credit==0 { let reason=if now>=fields.expires_at { LoopEndReason::Expired } else { LoopEndReason::KeepaliveExhausted }; self.end(id,now,reason,None); self.commit(now); return KeepaliveResult::Ended(reason); }
        let due_at=now+self.keepalive_seconds*1000.0;
        if let Some(entry)=self.state.entries.get_mut(id) { phase(entry,LoopPhase::Waiting); if let CronEntry::Dynamic { fields,pending_wakeup,keepalive_credit,.. }=entry { fields.queued_for_at=None; *keepalive_credit=0; *pending_wakeup=Some(PendingWakeup { id:wakeup_id.clone(),loop_id:id.into(),kind:DynamicKind::Dynamic,source:WakeupSource::Keepalive,requested_delay_seconds:self.keepalive_seconds,delay_seconds:self.keepalive_seconds,due_at,reason:"keepalive armed (model did not reschedule)".into(),prompt:fields.reentry_prompt.clone(),noop:false,created_at:now }); } }
        let expired=self.arm(id,now); self.commit(now); expired.map_or(KeepaliveResult::Armed { wakeup_id,delay_seconds:self.keepalive_seconds,due_at },KeepaliveResult::Ended)
    }
    fn targets(&self,target:&str)->Vec<LoopId> { if target=="all" { self.order.clone() } else { vec![target.into()] } }
    pub fn pause(&mut self,target:&str,now:f64)->Vec<LoopId> { let affected=self.targets(target).into_iter().filter(|id|self.state.entries.get(id).is_some_and(|entry|!matches!(lifecycle(entry).phase,LoopPhase::Ended|LoopPhase::Suspended))).collect::<Vec<_>>(); for id in &affected { self.armed_timers.remove(id); self.in_flight_deliveries.remove(id); if let Some(entry)=self.state.entries.get_mut(id) { phase(entry,LoopPhase::Suspended); fields_mut(entry).queued_for_at=None; } } if !affected.is_empty() { self.commit(now); } affected }
    pub fn resume(&mut self,target:&str,now:f64)->Vec<LoopId> { let affected=self.targets(target).into_iter().filter(|id|self.state.entries.get(id).is_some_and(|entry|lifecycle(entry).phase==LoopPhase::Suspended)).collect::<Vec<_>>(); for id in &affected { if let Some(entry)=self.state.entries.get_mut(id) { phase(entry,LoopPhase::Waiting); fields_mut(entry).queued_for_at=None; match entry { CronEntry::Fixed { next_fire_at,interval_ms,.. }=>*next_fire_at=now+*interval_ms,CronEntry::Dynamic { pending_wakeup,.. }=>if let Some(wake)=pending_wakeup { wake.due_at=wake.due_at.max(now); } } } self.arm(id,now); } if !affected.is_empty() { self.commit(now); } affected }
    pub fn stop(&mut self,target:&str,detail:&str,now:f64)->Vec<LoopId> { let affected=self.targets(target).into_iter().filter(|id|self.state.entries.get(id).is_some_and(|entry|lifecycle(entry).phase!=LoopPhase::Ended)).collect::<Vec<_>>(); for id in &affected { self.end(id,now,LoopEndReason::Stopped,Some(detail.into())); if self.state.active_dynamic_id.as_ref()==Some(id) { self.state.active_dynamic_id=None; } } if !affected.is_empty() { self.commit(now); } affected }
    pub fn on_shutdown(&mut self,now:f64)->Vec<LoopId> { self.armed_timers.clear(); let affected=self.pause("all",now); self.in_flight_deliveries.clear(); affected }
    pub fn restore(&mut self,now:f64,user_paused:&[LoopId],mut delivery_id:impl FnMut()->DeliveryId)->RestoreResult {
        let mut result=RestoreResult::default(); self.armed_timers.clear();
        for id in self.order.clone() {
            let Some(entry)=self.state.entries.get(&id) else { continue; };
            if lifecycle(entry).phase==LoopPhase::Ended || self.in_flight_deliveries.contains_key(&id) { continue; }
            if user_paused.contains(&id) { if let Some(entry)=self.state.entries.get_mut(&id) { phase(entry,LoopPhase::Suspended); fields_mut(entry).queued_for_at=None; } result.still_paused_loop_ids.push(id); continue; }
            if now>=fields(entry).expires_at { self.end(&id,now,LoopEndReason::Expired,None); result.expired_loop_ids.push(id); continue; }
            let due=match entry { CronEntry::Fixed { next_fire_at,.. }=>Some(*next_fire_at),CronEntry::Dynamic { pending_wakeup,.. }=>pending_wakeup.as_ref().map(|wake|wake.due_at) };
            let overdue=due.is_some_and(|due|due<=now); let remnant=matches!(lifecycle(entry).phase,LoopPhase::Starting|LoopPhase::Queued|LoopPhase::Running);
            let recover=(overdue || remnant || fields(entry).coalesced_fire_pending) && fields(entry).tick_count<self.max_ticks;
            if let Some(entry)=self.state.entries.get_mut(&id) { phase(entry,LoopPhase::Waiting); let base=fields_mut(entry); base.queued_for_at=None; base.coalesced_fire_pending=false; match entry { CronEntry::Fixed { next_fire_at,interval_ms,.. }=>*next_fire_at=now+*interval_ms,CronEntry::Dynamic { pending_wakeup,.. }=>if overdue { *pending_wakeup=None; } } }
            if self.arm(&id,now).is_some() { result.expired_loop_ids.push(id); continue; }
            if recover { result.recovery_ticks.push(LoopTick { loop_id:id.clone(),delivery_id:delivery_id(),scheduled_for_at:now,coalesced:true }); }
            result.rearmed_loop_ids.push(id);
        }
        self.commit(now);
        if !result.recovery_ticks.is_empty() {
            for tick in &result.recovery_ticks { if let Some(entry)=self.state.entries.get_mut(&tick.loop_id) { if lifecycle(entry).phase==LoopPhase::Ended { continue; } phase(entry,LoopPhase::Queued); let base=fields_mut(entry); base.last_fired_at=Some(now); base.last_scheduled_for_at=Some(now); base.queued_for_at=Some(now); base.tick_count+=1.0; self.in_flight_deliveries.insert(tick.loop_id.clone(),tick.delivery_id.clone()); } }
            self.commit(now);
        }
        result
    }
}
#[cfg(test)] mod tests {
    use super::*;
    fn dynamic()->CreateDynamicRequest { CreateDynamicRequest { original_args:String::new(),reentry_prompt:"/loop".into(),payload:LoopPayload::Sentinel { sentinel:LoopSentinel::Autonomous } } }
    fn scheduler()->LoopScheduler { LoopScheduler::new("test",None,&BTreeMap::new()) }
    #[test] fn environment_uses_javascript_number_coercion_and_clamps() {
        for (raw,expected) in [("0x80",128.0),("0b10000000",128.0),("0o200",128.0),("\u{feff}120\u{feff}",120.0),("1e2",100.0),("-4",60.0),("4000",3600.0),("",1200.0),("1.5",1200.0),("Infinity",1200.0),("garbage",1200.0)] {
            let env=BTreeMap::from([(KEEPALIVE_SECONDS_ENV.into(),raw.into())]);
            assert_eq!(LoopScheduler::new("test",None,&env).keepalive_seconds,expected,"{raw}");
        }
    }
    #[test] fn dynamic_supersession_is_one_atomic_revision() { let mut scheduler=scheduler(); scheduler.create_dynamic(dynamic(),"one".into(),0.0); let result=scheduler.create_dynamic(dynamic(),"two".into(),1.0); assert_eq!(result,CreateResult::Created { loop_id:"two".into(),superseded_loop_id:Some("one".into()) }); assert_eq!(scheduler.mutation_count,2); assert_eq!(lifecycle(&scheduler.state.entries["one"]).end_reason,Some(LoopEndReason::Stopped)); }
    #[test] fn due_coalesces_in_flight_ticks() { let mut scheduler=scheduler(); scheduler.create_dynamic(dynamic(),"one".into(),0.0); let first=scheduler.on_due("one",1.0,false,"first".into()); assert!(matches!(first,DueResult::Dispatch(_))); assert_eq!(scheduler.on_due("one",2.0,false,"second".into()),DueResult::Coalesce); assert_eq!(scheduler.current_delivery_id("one"),Some("first")); let settled=scheduler.on_tick_settled("one","first",TickOutcome::Completed,3.0,"next".into()); assert!(matches!(settled,SettledResult::Due(DueResult::Dispatch(_)))); assert_eq!(fields(&scheduler.state.entries["one"]).tick_count,2.0); }
    #[test] fn keepalive_is_two_strike_and_pause_does_not_consume_credit() { let mut scheduler=scheduler(); scheduler.create_dynamic(dynamic(),"one".into(),0.0); scheduler.pause("one",1.0); assert_eq!(scheduler.on_turn_ended_without_schedule("one","wake".into(),2.0),KeepaliveResult::Ignored); scheduler.resume("one",3.0); assert!(matches!(scheduler.on_turn_ended_without_schedule("one","wake".into(),4.0),KeepaliveResult::Armed { .. })); assert_eq!(scheduler.on_turn_ended_without_schedule("one","wake2".into(),5.0),KeepaliveResult::Ended(LoopEndReason::KeepaliveExhausted)); }
    #[test] fn expiry_never_extends_when_model_schedules() { let mut scheduler=scheduler(); scheduler.create_dynamic(dynamic(),"one".into(),0.0); scheduler.on_schedule_wakeup(ScheduleWakeupInput { loop_id:"one".into(),delay_seconds:60.0,requested_delay_seconds:60.0,reason:"wait".into(),prompt:"check".into(),noop:false },"wake".into(),LOOP_EXPIRY_MS-1.0); assert_eq!(lifecycle(&scheduler.state.entries["one"]).end_reason,Some(LoopEndReason::Expired)); assert!(scheduler.armed_timers.is_empty()); }
    #[test] fn shutdown_suspends_without_terminal_reason() { let mut scheduler=scheduler(); scheduler.create_dynamic(dynamic(),"one".into(),0.0); scheduler.on_shutdown(1.0); let lifecycle=lifecycle(&scheduler.state.entries["one"]); assert_eq!(lifecycle.phase,LoopPhase::Suspended); assert!(lifecycle.end_reason.is_none()); }
    #[test] fn retrying_tick_keeps_delivery_slot() { let mut scheduler=scheduler(); scheduler.create_dynamic(dynamic(),"one".into(),0.0); scheduler.on_due("one",1.0,false,"delivery".into()); let result=scheduler.on_tick_settled("one","delivery",TickOutcome::Retrying,2.0,"unused".into()); assert_eq!(result,SettledResult::InProgress); assert_eq!(scheduler.current_delivery_id("one"),Some("delivery")); }
    #[test] fn crash_remnant_recovers_exactly_one_delivery() { let mut first=scheduler(); first.create_dynamic(dynamic(),"one".into(),0.0); first.on_due("one",1.0,false,"lost".into()); let mut resumed=LoopScheduler::new("test",Some(first.state),&BTreeMap::new()); let result=resumed.restore(2.0,&[],||"recovered".into()); assert_eq!(result.recovery_ticks.len(),1); assert_eq!(resumed.current_delivery_id("one"),Some("recovered")); assert_eq!(fields(&resumed.state.entries["one"]).tick_count,2.0); }
    #[test] fn restore_never_duplicates_live_delivery() { let mut scheduler=scheduler(); scheduler.create_dynamic(dynamic(),"one".into(),0.0); scheduler.on_due("one",1.0,false,"live".into()); let result=scheduler.restore(2.0,&[],||"duplicate".into()); assert!(result.recovery_ticks.is_empty()); assert_eq!(scheduler.current_delivery_id("one"),Some("live")); }
    #[test] fn explicit_pause_survives_restore() { let mut scheduler=scheduler(); scheduler.create_dynamic(dynamic(),"one".into(),0.0); let result=scheduler.restore(2.0,&["one".into()],||"unused".into()); assert_eq!(result.still_paused_loop_ids,["one"]); assert_eq!(lifecycle(&scheduler.state.entries["one"]).phase,LoopPhase::Suspended); }
}
#[cfg(test)]
#[path="scheduler_parity_tests.rs"]
mod parity_tests;
