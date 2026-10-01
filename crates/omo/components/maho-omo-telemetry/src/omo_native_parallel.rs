use std::{collections::HashMap,sync::{Arc,Mutex}};
use serde_json::{Value,json};
use maho_ext_api::{BusSubscription,EventKind,EventResult,ExtensionApi,ExtensionEvent};
use crate::{eval_cell_correlation::EvalCellCorrelation,eval_classifier::is_eval_tool_name,omo_native_eval::{EvalExecutionRollup,EvalExecutionParseResult,parse_eval_execution_event,add_eval_execution_rollup,SENPI_EVAL_EXECUTION_EVENT},wave_assembler::{WaveAssembly,assemble_waves}};
pub struct ParallelSessionSnapshot {pub assembly:WaveAssembly,pub eval_execution:EvalExecutionRollup,pub eval_execution_event_bus_available:bool,pub measured_turn_duration_ms_total:f64}
#[derive(Default)]
struct SessionState {eval_execution:EvalExecutionRollup,observations:Vec<Value>,turn_start_ms:Option<f64>,measured_turn_duration_ms_total:f64}
#[derive(Default)]
pub struct ParallelTelemetryRegistry {sessions:HashMap<String,SessionState>,eval_cells:EvalCellCorrelation,event_bus_available:bool}
impl ParallelTelemetryRegistry {
    pub fn record(&mut self,id:&str,observation:Value) {
        let start=observation.get("kind").and_then(Value::as_str)==Some("start");
        let state=if start {Some(self.sessions.entry(id.into()).or_default())} else {self.sessions.get_mut(id)};
        if let Some(state)=state {state.observations.push(observation.clone());}
        if start && let (Some(name),Some(cell))=(observation.get("toolName").and_then(Value::as_str),observation.get("toolCallId").and_then(Value::as_str)) && is_eval_tool_name(name) {self.eval_cells.track(id,cell);}
    }
    pub fn record_eval_execution(&mut self,payload:&Value) {let parsed=parse_eval_execution_event(payload);let (cell,rollup)=match parsed {EvalExecutionParseResult::Ignored=>return,EvalExecutionParseResult::Accepted {cell_id,rollup}=>(cell_id,rollup),EvalExecutionParseResult::Rejected {cell_id}=>(cell_id,EvalExecutionRollup {rejected_count:1,..Default::default()})};let Some(id)=self.eval_cells.consume(&cell) else {return;};if let Some(state)=self.sessions.get_mut(&id) {state.eval_execution=add_eval_execution_rollup(&state.eval_execution,&rollup);}}
    pub fn set_eval_execution_event_bus_available(&mut self,available:bool) {self.event_bus_available=available;}
    pub fn start_turn(&mut self,id:&str,at:f64) {self.sessions.entry(id.into()).or_default().turn_start_ms=Some(at);}
    pub fn end_turn(&mut self,id:&str,at:f64) {if let Some(state)=self.sessions.get_mut(id) && let Some(start)=state.turn_start_ms.take() {let elapsed=at-start;if elapsed>0.0 {state.measured_turn_duration_ms_total+=elapsed;}}}
    pub fn snapshot(&self,id:&str) -> Option<ParallelSessionSnapshot> {let state=self.sessions.get(id)?;Some(ParallelSessionSnapshot {assembly:assemble_waves(&state.observations),eval_execution:state.eval_execution.clone(),eval_execution_event_bus_available:self.event_bus_available,measured_turn_duration_ms_total:state.measured_turn_duration_ms_total})}
    pub fn clear(&mut self,id:&str) {self.sessions.remove(id);self.eval_cells.clear_session(id);}
    pub fn size(&self) -> usize {self.sessions.len()}
}
pub fn register_omo_native_parallel_telemetry(api:&mut ExtensionApi,registry:Arc<Mutex<ParallelTelemetryRegistry>>,now:Arc<dyn Fn()->f64+Send+Sync>) -> BusSubscription {
    registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_eval_execution_event_bus_available(true);
    let captured=Arc::clone(&registry);
    let subscription=api.events.on(SENPI_EVAL_EXECUTION_EVENT,Arc::new(move |v|captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).record_eval_execution(v)));
    for kind in [EventKind::ToolExecutionStart,EventKind::ToolExecutionEnd,EventKind::TurnStart,EventKind::TurnEnd,EventKind::SessionShutdown] {
        let registry=Arc::clone(&registry);let now=Arc::clone(&now);
        api.on(kind,Arc::new(move |event,ctx| {let at=now();let registry=Arc::clone(&registry);Box::pin(async move {
            let id=ctx.session_manager.session_id();if id.is_empty() {return Ok(EventResult::None);}
            let mut registry=registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match event {
                ExtensionEvent::ToolExecutionStart {tool_call_id,tool_name,..}|ExtensionEvent::ToolExecutionEnd {tool_call_id,tool_name,..} if !tool_call_id.is_empty() && !tool_name.is_empty() =>registry.record(id,json!({"kind":if kind==EventKind::ToolExecutionStart {"start"} else {"end"},"toolCallId":tool_call_id,"toolName":tool_name,"atMs":at})),
                ExtensionEvent::TurnStart {timestamp,..}=>{let at=timestamp.to_string().parse::<f64>().map_err(|e|maho_ext_api::ExtensionFailure::new(e.to_string()))?;registry.start_turn(id,at);},
                ExtensionEvent::TurnEnd {..}=>registry.end_turn(id,at),
                ExtensionEvent::SessionShutdown(_)=>registry.clear(id),
                _=>{},
            }
            Ok(EventResult::None)
        })}));
    }
    subscription
}
#[cfg(test)]
mod tests {
    use super::*;
    fn obs(kind:&str,cell:&str,name:&str,at:f64)->Value {json!({"kind":kind,"toolCallId":cell,"toolName":name,"atMs":at})}
    fn pair(r:&mut ParallelTelemetryRegistry,id:&str,cell:&str,a:f64,b:f64) {r.record(id,obs("start",cell,"bash",a));r.record(id,obs("end",cell,"bash",b));}
    #[test] fn overlap() {let mut r=ParallelTelemetryRegistry::default();pair(&mut r,"s","a",1000.0,1500.0);pair(&mut r,"s","b",1100.0,1600.0);pair(&mut r,"s","c",1200.0,1400.0);let s=r.snapshot("s").unwrap();assert_eq!(s.assembly.waves.len(),1);assert!((s.assembly.waves[0].span_ms-600.0).abs()<f64::EPSILON);}
    #[test] fn retains_identity_only() {let mut r=ParallelTelemetryRegistry::default();pair(&mut r,"s","a",1000.0,1500.0);let s=r.snapshot("s").unwrap();assert_eq!(s.assembly.waves[0].calls[0].tool_call_id,"a");assert_eq!(s.assembly.waves[0].calls[0].tool_name,"bash");}
    #[test] fn sequential() {let mut r=ParallelTelemetryRegistry::default();pair(&mut r,"s","a",1000.0,1100.0);pair(&mut r,"s","b",1200.0,1300.0);assert_eq!(r.snapshot("s").unwrap().assembly.waves.len(),2);}
    #[test] fn turn_accumulates() {let mut r=ParallelTelemetryRegistry::default();r.start_turn("s",2000.0);r.end_turn("s",2750.0);r.start_turn("s",3000.0);r.end_turn("s",3250.0);assert!((r.snapshot("s").unwrap().measured_turn_duration_ms_total-1000.0).abs()<f64::EPSILON);}
    #[test] fn orphan_turn_end() {let mut r=ParallelTelemetryRegistry::default();r.end_turn("s",9999.0);assert_eq!(r.size(),0);}
    #[test] fn negative_turn() {let mut r=ParallelTelemetryRegistry::default();r.start_turn("s",5000.0);r.end_turn("s",4000.0);assert!(r.snapshot("s").unwrap().measured_turn_duration_ms_total.abs()<f64::EPSILON);}
    #[test] fn supplied_clock() {let mut r=ParallelTelemetryRegistry::default();pair(&mut r,"s","a",42.0,84.0);assert!((r.snapshot("s").unwrap().assembly.waves[0].span_ms-42.0).abs()<f64::EPSILON);}
    #[test] fn sessions_isolated() {let mut r=ParallelTelemetryRegistry::default();pair(&mut r,"a","a",1000.0,1200.0);pair(&mut r,"b","b",1050.0,1300.0);assert_eq!(r.snapshot("a").unwrap().assembly.waves[0].calls[0].tool_call_id,"a");assert_eq!(r.snapshot("b").unwrap().assembly.waves[0].calls[0].tool_call_id,"b");}
    #[test] fn shutdown_clears() {let mut r=ParallelTelemetryRegistry::default();pair(&mut r,"s","a",1.0,2.0);r.clear("s");assert_eq!(r.size(),0);assert!(r.snapshot("s").is_none());}
    #[test] fn shutdown_only_owner() {let mut r=ParallelTelemetryRegistry::default();pair(&mut r,"a","a",1.0,2.0);pair(&mut r,"b","b",1.0,2.0);r.clear("a");assert_eq!(r.size(),1);assert_eq!(r.snapshot("b").unwrap().assembly.counters.paired_calls,1);}
    #[test] fn repeated_shutdown() {let mut r=ParallelTelemetryRegistry::default();r.record("s",obs("start","a","bash",1.0));r.clear("s");r.record("s",obs("end","a","bash",2.0));r.clear("s");assert_eq!(r.size(),0);}
    #[test] fn late_end_no_resurrection() {let mut r=ParallelTelemetryRegistry::default();pair(&mut r,"s","a",1.0,2.0);r.clear("s");r.record("s",obs("end","a","bash",3.0));r.end_turn("s",3.0);assert_eq!(r.size(),0);}
    #[test] fn malformed_eval_ignored() {let mut r=ParallelTelemetryRegistry::default();r.record_eval_execution(&Value::Null);assert_eq!(r.size(),0);}
    #[test] fn unresolved_session_ignored() {let r=ParallelTelemetryRegistry::default();assert!(r.snapshot("unknown").is_none());}
    #[test] fn incomplete_start() {let mut r=ParallelTelemetryRegistry::default();r.record("s",obs("start","a","bash",1.0));let s=r.snapshot("s").unwrap();assert_eq!(s.assembly.counters.incomplete,1);assert_eq!(s.assembly.counters.paired_calls,0);}
}
