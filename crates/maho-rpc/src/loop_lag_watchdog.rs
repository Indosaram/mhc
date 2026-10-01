use std::collections::HashMap;
use crate::{session_attribution::{SessionActivityRegistry,SessionAttribution},loop_blocked_time::LoopBlockedTime,host_lifecycle::parse_idle_exit_ms};
pub const LOOP_LAG_WARN_MS_ENV:&str="SENPI_RPC_LOOP_LAG_WARN_MS";
pub const LOOP_LAG_ERROR_MS_ENV:&str="SENPI_RPC_LOOP_LAG_ERROR_MS";
pub const DEFAULT_LOOP_LAG_WARN_MS:f64=500.;
pub const DEFAULT_LOOP_LAG_ERROR_MS:f64=5000.;
pub const LOOP_LAG_TICK_MS:f64=200.;
pub const LOOP_LAG_WARN_INTERVAL_MS:f64=10000.;
pub struct LoopLagWatchdog{warn_ms:f64,error_ms:f64,expected_tick_at:Option<f64>,activity_mark:u64,last_warn_at:Option<f64>}
#[derive(Debug,Default)]pub struct LagSample{pub record:Option<serde_json::Value>,pub log:Option<String>}
fn describe(attribution:Option<&SessionAttribution>)->String{let Some(attribution)=attribution.filter(|a|a.session_id.is_some()||a.tool.is_some())else{return "no attributed session".into();};let tool=attribution.tool.as_deref().filter(|tool|!tool.is_empty()).map(|tool|format!(" tool={tool}")).unwrap_or_default();format!("sessionId={}{tool}",attribution.session_id.as_deref().unwrap_or("unknown"))}
impl LoopLagWatchdog{
    pub fn new(env:&HashMap<String,String>)->Self{Self{warn_ms:parse_idle_exit_ms(env.get(LOOP_LAG_WARN_MS_ENV).map(String::as_str)).unwrap_or(DEFAULT_LOOP_LAG_WARN_MS),error_ms:parse_idle_exit_ms(env.get(LOOP_LAG_ERROR_MS_ENV).map(String::as_str)).unwrap_or(DEFAULT_LOOP_LAG_ERROR_MS),expected_tick_at:None,activity_mark:0,last_warn_at:None}}
    pub fn start(&mut self,now:f64,registry:&SessionActivityRegistry){if self.expected_tick_at.is_none(){self.expected_tick_at=Some(now+LOOP_LAG_TICK_MS);self.activity_mark=registry.mark();}}
    pub fn stop(&mut self){self.expected_tick_at=None;}
    pub fn tick(&mut self,now:f64,registry:&SessionActivityRegistry,blocked:&mut LoopBlockedTime)->LagSample{
        let expected=self.expected_tick_at.unwrap_or(now);let previous=self.activity_mark;self.expected_tick_at=Some(now+LOOP_LAG_TICK_MS);self.activity_mark=registry.mark();let drift=(now-expected+0.5).floor();blocked.record_loop_blocked_ms(drift);let mut sample=LagSample::default();if drift<=self.warn_ms{return sample;}let attribution=registry.since(previous);
        if drift>self.error_ms{
            let mut record=serde_json::json!({"type":"host_stalled","driftMs":drift});
            if let Some(attribution)=&attribution{
                if let Some(session)=&attribution.session_id{record["sessionId"]=session.clone().into();}
                if let Some(tool)=&attribution.tool{record["tool"]=tool.clone().into();}
            }
            sample.record=Some(record);
        }
        if self.last_warn_at.is_some_and(|last|now-last<LOOP_LAG_WARN_INTERVAL_MS){return sample;}self.last_warn_at=Some(now);sample.log=Some(format!("senpi rpc host stall: event loop blocked {drift}ms ({})\n",describe(attribution.as_ref())));sample
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn records_drift_and_attributes_errors_even_when_warning_throttled(){let registry=SessionActivityRegistry::default();let mut blocked=LoopBlockedTime::default();let mut watchdog=LoopLagWatchdog::new(&HashMap::new());watchdog.start(0.,&registry);let _span=registry.open_span(SessionAttribution{session_id:Some("s".into()),tool:Some("bash".into())},None);let sample=watchdog.tick(6000.,&registry,&mut blocked);assert_eq!(sample.record.unwrap()["sessionId"],"s");assert!(sample.log.is_some());let sample=watchdog.tick(12000.,&registry,&mut blocked);assert!(sample.record.is_some());assert!(sample.log.is_none());assert_eq!(blocked.loop_blocked_mark(),11600.);}
    #[test]fn first_manual_tick_only_sets_baseline(){let registry=SessionActivityRegistry::default();let mut blocked=LoopBlockedTime::default();let mut watchdog=LoopLagWatchdog::new(&HashMap::new());assert!(watchdog.tick(99999.,&registry,&mut blocked).log.is_none());assert_eq!(blocked.loop_blocked_mark(),0.);}
}
