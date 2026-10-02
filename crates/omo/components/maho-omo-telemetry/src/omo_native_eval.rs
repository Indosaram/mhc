use serde_json::Value;
pub const SENPI_EVAL_EXECUTION_EVENT:&str="senpi.eval.execution";
#[derive(Debug,Default,Clone,PartialEq)]
pub struct EvalExecutionRollup { pub event_count:u64,pub rejected_count:u64,pub ok_count:u64,pub detached_count:u64,pub measured_execution_duration_ms_sum:f64,pub nested_tool_call_count:u64,pub nested_tool_call_ok_count:u64,pub nested_tool_call_error_count:u64,pub nested_tool_call_pending_count:u64,pub measured_nested_tool_duration_ms_sum:f64,pub truncated_execution_count:u64 }
#[derive(Debug,PartialEq)]
pub enum EvalExecutionParseResult { Accepted {cell_id:String,rollup:EvalExecutionRollup},Rejected {cell_id:String},Ignored }
#[derive(Default)]
struct Aggregate { count:u64,duration:f64,ok:u64,error:u64,pending:u64 }
fn count(v:&Value) -> Option<u64> { let n=v.as_f64()?; if !n.is_finite() || !(0.0..=9_007_199_254_740_991.0).contains(&n) || n.fract().abs()>f64::EPSILON {return None;} v.as_u64().or_else(||format!("{n:.0}").parse().ok()) }
fn duration(v:&Value) -> Option<f64> { v.as_f64().filter(|n|n.is_finite() && *n>=0.0) }
fn aggregate(v:&Value) -> Option<Aggregate> { let a=Aggregate {count:count(v.get("count")?)?,duration:duration(v.get("totalDurationMs")?)?,ok:count(v.get("okCount")?)?,error:count(v.get("errorCount")?)?,pending:count(v.get("pendingCount")?)?}; if a.count!=a.ok+a.error+a.pending {return None;} Some(a) }
impl Aggregate { fn add(&mut self,a:Self) {self.count+=a.count;self.duration+=a.duration;self.ok+=a.ok;self.error+=a.error;self.pending+=a.pending;} }
pub fn parse_eval_execution_event(v:&Value) -> EvalExecutionParseResult {
    let Some(cell)=v.get("cellId").and_then(Value::as_str).filter(|s|!s.is_empty()) else {return EvalExecutionParseResult::Ignored;};
    let parse=(|| {
        if v.get("version")?.as_f64()?!=1.0 || v.get("detailLevel")?.as_str()?!="full" || !matches!(v.get("language")?.as_str()?,"js"|"py"|"rb"|"jl") {return None;}
        let ok=v.get("ok")?.as_bool()?; let detached=v.get("detached")?.as_bool()?;
        for k in ["startedAt","completedAt"] {if !v.get(k)?.as_f64()?.is_finite() {return None;}}
        let elapsed=duration(v.get("durationMs")?)?;
        if let Some(kernel)=v.get("kernelDurationMs") {duration(kernel)?;}
        let calls=count(v.get("toolCallCount")?)?;let pending=count(v.get("pendingToolCallCount")?)?;
        v.get("toolCalls")?.as_array()?;
        if !v.get("distinctToolsCalled")?.as_array()?.iter().all(Value::is_string) {return None;}
        let truncated=v.get("toolAggregatesTruncated")?.as_bool()?;
        let mut totals=Aggregate::default(); for value in v.get("toolAggregates")?.as_object()?.values() {totals.add(aggregate(value)?);}
        match (truncated,v.get("toolAggregateOverflow")) {(true,Some(value))=>totals.add(aggregate(value)?),(false,None)=>{},(true,None)|(false,Some(_))=>return None}
        if totals.count!=calls || totals.pending!=pending {return None;}
        Some(EvalExecutionRollup {event_count:1,rejected_count:0,ok_count:u64::from(ok),detached_count:u64::from(detached),measured_execution_duration_ms_sum:elapsed,nested_tool_call_count:calls,nested_tool_call_ok_count:totals.ok,nested_tool_call_error_count:totals.error,nested_tool_call_pending_count:pending,measured_nested_tool_duration_ms_sum:totals.duration,truncated_execution_count:u64::from(truncated)})
    })();
    match parse {Some(rollup)=>EvalExecutionParseResult::Accepted {cell_id:cell.into(),rollup},None=>EvalExecutionParseResult::Rejected {cell_id:cell.into()}}
}
pub fn add_eval_execution_rollup(a:&EvalExecutionRollup,b:&EvalExecutionRollup) -> EvalExecutionRollup { EvalExecutionRollup {event_count:a.event_count+b.event_count,rejected_count:a.rejected_count+b.rejected_count,ok_count:a.ok_count+b.ok_count,detached_count:a.detached_count+b.detached_count,measured_execution_duration_ms_sum:a.measured_execution_duration_ms_sum+b.measured_execution_duration_ms_sum,nested_tool_call_count:a.nested_tool_call_count+b.nested_tool_call_count,nested_tool_call_ok_count:a.nested_tool_call_ok_count+b.nested_tool_call_ok_count,nested_tool_call_error_count:a.nested_tool_call_error_count+b.nested_tool_call_error_count,nested_tool_call_pending_count:a.nested_tool_call_pending_count+b.nested_tool_call_pending_count,measured_nested_tool_duration_ms_sum:a.measured_nested_tool_duration_ms_sum+b.measured_nested_tool_duration_ms_sum,truncated_execution_count:a.truncated_execution_count+b.truncated_execution_count} }
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    fn payload() -> Value {json!({"version":1,"detailLevel":"full","cellId":"eval-1","language":"js","ok":true,"startedAt":1000,"completedAt":1030,"durationMs":30,"kernelDurationMs":24,"detached":false,"toolCallCount":2,"pendingToolCallCount":0,"toolCalls":[],"distinctToolsCalled":["read","bash"],"toolAggregates":{"read":{"count":1,"totalDurationMs":12,"okCount":1,"errorCount":0,"pendingCount":0},"bash":{"count":1,"totalDurationMs":18,"okCount":1,"errorCount":0,"pendingCount":0}},"toolAggregatesTruncated":false})}
    fn accepted(v:&Value) -> EvalExecutionRollup {match parse_eval_execution_event(v) {EvalExecutionParseResult::Accepted {rollup,..}=>rollup,other=>panic!("{other:?}")}}
    #[test] fn scalar_projection() {let mut v=payload();v["toolCalls"]=json!([{"name":"read","args":{"path":"/secret"},"resultPreview":"private"}]);let r=accepted(&v);assert_eq!(r.event_count,1);assert_eq!(r.nested_tool_call_count,2);assert!((r.measured_nested_tool_duration_ms_sum-30.0).abs()<f64::EPSILON);let s=format!("{r:?}");for secret in ["/secret","private","read","bash"] {assert!(!s.contains(secret));}}
    #[test] fn count_authoritative() {let mut v=payload();v["toolCallCount"]=json!(40);v["toolAggregates"]=json!({"read":{"count":40,"totalDurationMs":400,"okCount":40,"errorCount":0,"pendingCount":0}});let r=accepted(&v);assert_eq!(r.nested_tool_call_count,40);assert!((r.measured_nested_tool_duration_ms_sum-400.0).abs()<f64::EPSILON);}
    #[test] fn overflow() {let mut v=payload();v["toolCallCount"]=json!(3);v["pendingToolCallCount"]=json!(1);v["toolAggregates"]=json!({"read":{"count":2,"totalDurationMs":20,"okCount":1,"errorCount":1,"pendingCount":0}});v["toolAggregatesTruncated"]=json!(true);v["toolAggregateOverflow"]=json!({"count":1,"totalDurationMs":15,"okCount":0,"errorCount":0,"pendingCount":1});let r=accepted(&v);assert_eq!(r.nested_tool_call_count,3);assert_eq!(r.nested_tool_call_error_count,1);assert_eq!(r.nested_tool_call_pending_count,1);assert_eq!(r.truncated_execution_count,1);assert!((r.measured_nested_tool_duration_ms_sum-35.0).abs()<f64::EPSILON);}
    #[test] fn rejects_malformed() {for (key,value) in [("version",json!(2)),("detailLevel",json!("metadata")),("durationMs",json!(-1)),("toolCallCount",json!(1.5)),("toolCallCount",json!(1)),("pendingToolCallCount",json!(1)),("toolAggregatesTruncated",json!(true)),("toolAggregateOverflow",json!({}))] {let mut v=payload();v[key]=value;assert!(matches!(parse_eval_execution_event(&v),EvalExecutionParseResult::Rejected {..}));}}
    #[test] fn ignores_no_cell() {for v in [Value::Null,json!("event"),json!(1),json!([]),json!({}),json!({"cellId":""}),json!({"cellId":42})] {assert_eq!(parse_eval_execution_event(&v),EvalExecutionParseResult::Ignored);}}
    #[test] fn adds_every_scalar() {let a=accepted(&payload());let mut v=payload();v["ok"]=json!(false);v["detached"]=json!(true);v["toolCallCount"]=json!(1);v["toolAggregates"]=json!({"bash":{"count":1,"totalDurationMs":7,"okCount":0,"errorCount":1,"pendingCount":0}});let r=add_eval_execution_rollup(&a,&accepted(&v));assert_eq!(r.event_count,2);assert_eq!(r.ok_count,1);assert_eq!(r.detached_count,1);assert_eq!(r.nested_tool_call_count,3);assert_eq!(r.nested_tool_call_ok_count,2);assert_eq!(r.nested_tool_call_error_count,1);assert!((r.measured_nested_tool_duration_ms_sum-37.0).abs()<f64::EPSILON);}
}
