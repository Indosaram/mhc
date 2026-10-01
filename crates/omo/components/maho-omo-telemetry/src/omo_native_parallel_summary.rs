use std::sync::{Arc,Mutex};
use serde_json::{Value,json};
use maho_ext_api::{BusSubscription,EventKind,EventResult,ExtensionApi};
use crate::{eval_classifier::{ClassifiableWave,WaveBucket,classify_wave_bucket,summarize_wave_buckets},omo_native_parallel::{ParallelSessionSnapshot,ParallelTelemetryRegistry,register_omo_native_parallel_telemetry},savings_math::{MeasurableCall,MeasurableWave,modeled_wall_clock_saved_ms,upper_bound_saved_ms,saved_round_trips}};
pub fn build_parallelism_summary(snapshot:&ParallelSessionSnapshot,hash:&str) -> Option<Value> {
    let projected:Vec<_>=snapshot.assembly.waves.iter().map(|w|ClassifiableWave {tool_names:w.calls.iter().map(|c|c.tool_name.clone()).collect(),span_ms:w.span_ms}).collect();
    let buckets=summarize_wave_buckets(&projected);
    let eval=&snapshot.eval_execution;
    if buckets.non_eval.waves_total==0 && buckets.eval_only_waves==0 && buckets.mixed_waves==0 && eval.event_count==0 {return None;}
    let waves:Vec<_>=snapshot.assembly.waves.iter().zip(&projected).filter(|(_,p)|classify_wave_bucket(p)==WaveBucket::NonEval).map(|(w,_)|MeasurableWave {calls:w.calls.iter().map(|c|MeasurableCall {start_ms:c.start_ms,end_ms:c.end_ms}).collect(),span_ms:w.span_ms,max_concurrency:f64::from(w.max_concurrency)}).collect();
    let counters=&snapshot.assembly.counters;
    Some(json!({
        "$session_id":hash,"clock_anomalies":counters.clock_anomalies,"dropped_calls":counters.dropped_calls,
        "eval_execution_detached_count":eval.detached_count,"eval_execution_event_bus_available":snapshot.eval_execution_event_bus_available,"eval_execution_event_count":eval.event_count,"eval_execution_event_rejected_count":eval.rejected_count,"eval_execution_ok_count":eval.ok_count,
        "eval_nested_tool_call_count":eval.nested_tool_call_count,"eval_nested_tool_call_error_count":eval.nested_tool_call_error_count,"eval_nested_tool_call_ok_count":eval.nested_tool_call_ok_count,"eval_nested_tool_call_pending_count":eval.nested_tool_call_pending_count,
        "eval_only_duration_ms":buckets.eval_only_duration_ms,"eval_only_waves":buckets.eval_only_waves,"eval_outer_joined_calls":buckets.eval_outer_joined_calls,"eval_tool_aggregate_truncated_execution_count":eval.truncated_execution_count,
        "incomplete_calls":counters.incomplete,"measured_eval_execution_duration_ms_sum":eval.measured_execution_duration_ms_sum,"measured_eval_nested_tool_duration_ms_sum":eval.measured_nested_tool_duration_ms_sum,"measured_turn_duration_ms_total":snapshot.measured_turn_duration_ms_total,
        "mixed_non_eval_joined_calls":buckets.mixed_non_eval_joined_calls,"mixed_waves":buckets.mixed_waves,"modeled_wallclock_saved_ms":waves.iter().map(|w|modeled_wall_clock_saved_ms(w).value_ms).sum::<f64>(),
        "non_eval_joined_calls":buckets.non_eval.joined_calls,"non_eval_saved_round_trips":saved_round_trips(&waves),"non_eval_wave_size_histogram":buckets.non_eval.wave_size_histogram,"non_eval_waves_multi":buckets.non_eval.waves_multi,"non_eval_waves_total":buckets.non_eval.waves_total,
        "schema_kind":"parallelism_v2","upper_bound_saved_ms":waves.iter().map(|w|upper_bound_saved_ms(w).value_ms).sum::<f64>()
    }))
}
pub type SummaryCapture=Arc<dyn Fn(&str,Value)+Send+Sync>;
pub fn register_omo_native_parallel_summary(api:&mut ExtensionApi,registry:Arc<Mutex<ParallelTelemetryRegistry>>,now:Arc<dyn Fn()->f64+Send+Sync>,hash:Arc<dyn Fn(&str)->String+Send+Sync>,capture:SummaryCapture) -> BusSubscription {
    let captured=Arc::clone(&registry);
    api.on(EventKind::SessionShutdown,Arc::new(move |_,ctx| {let captured=Arc::clone(&captured);let hash=Arc::clone(&hash);let capture=Arc::clone(&capture);Box::pin(async move {
        let id=ctx.session_manager.session_id();if id.is_empty() {return Ok(EventResult::None);}
        let snapshot={let mut registry=captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let snapshot=registry.snapshot(id);registry.clear(id);snapshot};
        if let Some(snapshot)=snapshot && let Some(properties)=build_parallelism_summary(&snapshot,&hash(id)) {capture("parallelism_summary",properties);}
        Ok(EventResult::None)
    })}));
    register_omo_native_parallel_telemetry(api,registry,now)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn call(r:&mut ParallelTelemetryRegistry,id:&str,name:&str,start:f64,end:f64) {r.record("s",json!({"kind":"start","toolCallId":id,"toolName":name,"atMs":start}));r.record("s",json!({"kind":"end","toolCallId":id,"toolName":name,"atMs":end}));}
    #[test] fn non_eval_summary() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"a","bash",0.0,100.0);call(&mut r,"b","read",0.0,100.0);let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();assert_eq!(p["non_eval_joined_calls"],2);assert_eq!(p["modeled_wallclock_saved_ms"],100.0);assert_eq!(p["non_eval_saved_round_trips"],1.0);}
    #[test] fn exact_schema() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"a","bash",0.0,1.0);let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();let mut keys:Vec<_>=p.as_object().unwrap().keys().map(String::as_str).collect();let mut expected:Vec<_>=crate::parallelism_schema::PARALLELISM_SUMMARY_SCHEMA.iter().map(|(k,_)|*k).collect();keys.sort_unstable();expected.sort_unstable();assert_eq!(keys,expected);}
    #[test] fn mixed_savings_excluded() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"a","eval",0.0,100.0);call(&mut r,"b","read",0.0,100.0);let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();assert_eq!(p["mixed_waves"],1);assert_eq!(p["modeled_wallclock_saved_ms"],0.0);assert_eq!(p["non_eval_waves_total"],0);}
    #[test] fn idle_and_quality_only_no_event() {let mut r=ParallelTelemetryRegistry::default();r.start_turn("s",0.0);assert!(build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").is_none());r.record("s",json!({"kind":"start","toolCallId":"x","toolName":"bash","atMs":0.0}));assert!(build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").is_none());}
    #[test] fn eval_only() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"a","eval",0.0,100.0);let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();assert_eq!(p["eval_only_waves"],1);assert_eq!(p["eval_only_duration_ms"],100.0);}
    #[test] fn quality_counters_carried() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"ok","bash",1000.0,1100.0);r.record("s",json!({"kind":"start","toolCallId":"never","toolName":"bash","atMs":1200.0}));call(&mut r,"reversed","bash",1300.0,1250.0);let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();assert_eq!(p["dropped_calls"],0);assert_eq!(p["incomplete_calls"],1);assert_eq!(p["clock_anomalies"],1);}
    #[test] fn three_buckets_isolated() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"n1","bash",1000.0,1500.0);call(&mut r,"n2","read",1100.0,1400.0);call(&mut r,"e1","eval",2000.0,2700.0);call(&mut r,"m1","eval",3000.0,3900.0);call(&mut r,"m2","bash",3100.0,3400.0);let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();assert_eq!(p["non_eval_joined_calls"],2);assert_eq!(p["modeled_wallclock_saved_ms"],300.0);assert_eq!(p["eval_only_duration_ms"],700.0);assert_eq!(p["mixed_waves"],1);}
    #[test] fn histogram_positional() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"a","bash",1000.0,1100.0);call(&mut r,"b","bash",2000.0,2500.0);call(&mut r,"c","read",2100.0,2400.0);let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();assert_eq!(p["non_eval_wave_size_histogram"],"1:1:0:0:0:0:0:0");}
    #[test] fn cap_accounting() {let mut r=ParallelTelemetryRegistry::default();let cap=crate::wave_assembler::MAX_TRACKED_CALLS;for i in 0..cap+500 {r.record("s",json!({"kind":"start","toolCallId":format!("c{i}"),"toolName":"bash","atMs":i+1000}));}for i in 0..cap+500 {r.record("s",json!({"kind":"end","toolCallId":format!("c{i}"),"toolName":"bash","atMs":i+100000}));}let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();assert_eq!(p["dropped_calls"],500);assert_eq!(p["non_eval_joined_calls"],cap);assert_eq!(p["incomplete_calls"],0);assert_eq!(p["clock_anomalies"],0);}
    #[test] fn turn_without_tools_no_event() {let mut r=ParallelTelemetryRegistry::default();r.start_turn("s",1000.0);r.end_turn("s",1900.0);assert!(build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").is_none());}
    #[test] fn repeated_shutdown_no_event() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"a","bash",1.0,2.0);assert!(build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").is_some());r.clear("s");assert!(r.snapshot("s").is_none());}
    #[test] fn shutdown_mid_turn() {let mut r=ParallelTelemetryRegistry::default();r.start_turn("s",1000.0);call(&mut r,"a","bash",1000.0,1500.0);r.record("s",json!({"kind":"start","toolCallId":"pending","toolName":"read","atMs":1600.0}));let p=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash").unwrap();assert_eq!(p["non_eval_joined_calls"],1);assert_eq!(p["incomplete_calls"],1);r.clear("s");r.record("s",json!({"kind":"end","toolCallId":"pending","toolName":"read","atMs":1700.0}));assert!(r.snapshot("s").is_none());}
    #[test] fn independent_sessions() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"a","bash",1.0,2.0);for (kind,at) in [("start",1.0),("end",2.0)] {r.record("other",json!({"kind":kind,"toolCallId":"b","toolName":"read","atMs":at}));}let a=build_parallelism_summary(&r.snapshot("s").unwrap(),"hash-a").unwrap();let b=build_parallelism_summary(&r.snapshot("other").unwrap(),"hash-b").unwrap();assert_ne!(a["$session_id"],b["$session_id"]);r.clear("s");assert!(r.snapshot("other").is_some());}
    #[test] fn unknown_shutdown_spares_live() {let mut r=ParallelTelemetryRegistry::default();call(&mut r,"a","bash",1.0,2.0);r.clear("");assert!(r.snapshot("s").is_some());}
}
