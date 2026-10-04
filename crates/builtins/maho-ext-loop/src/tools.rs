use std::sync::Arc;
use maho_ext_api::{ExtensionApi,ExtensionFuture,ToolDefinition,ToolError,ToolResult,ToolExposure,ToolExecutionMode};
use serde_json::{Value,json};
pub const SCHEDULE_WAKEUP_TOOL:&str="schedule_wakeup";
pub const MIN_WAKEUP_DELAY_SECONDS:f64=60.0;
pub const MAX_WAKEUP_DELAY_SECONDS:f64=3600.0;
pub const SCHEDULE_WAKEUP_DESCRIPTION:&str="Schedule when to resume work in /loop dynamic mode. Always pass the `prompt` argument unless stopping. Call this as the last action before ending a dynamic loop iteration so the loop remains alive; call with `stop: true` to end the active dynamic loop immediately.\n\n`delaySeconds` is clamped to 60-3600 seconds; 1200-1800 is the normal idle range, and a short delay chosen merely to poll loses prompt-cache value on many API paths. When a monitor or task notification is the primary wake source, the scheduled delay is only a fallback heartbeat.\n\nFor normal dynamic re-entry, set `prompt` to the complete original command, for example `/loop check the deploy`, preserving the user's text verbatim. Set `noop: true` only when this iteration found no actionable change; consecutive noop iterations are folded in the terminal view. Omit `noop` when stopping. Notify the user only when state changes in a way worth acting on, not once per tick.";
pub struct ScheduleWakeupRequest { pub loop_id:String,pub requested_delay_seconds:f64,pub delay_seconds:f64,pub reason:String,pub prompt:String,pub noop:bool }
pub struct ScheduleWakeupOutcome { pub wakeup_id:String,pub replaced_wakeup_id:Option<String>,pub due_at:f64,pub noop_streak:f64 }
pub struct StopDynamicLoopRequest { pub loop_id:String,pub reason:String }
pub struct StopDynamicLoopOutcome { pub ended_at:f64 }
pub struct ScheduleWakeupTarget { pub kind:crate::types::LoopKind,pub loop_id:String }
pub trait ScheduleWakeupSchedulerPort:Send+Sync {
    fn get_wakeup_target(&self)->Option<ScheduleWakeupTarget>;
    fn schedule_wakeup(&self,request:ScheduleWakeupRequest)->ExtensionFuture<'_,ScheduleWakeupOutcome>;
    fn stop_dynamic_loop(&self,request:StopDynamicLoopRequest)->ExtensionFuture<'_,StopDynamicLoopOutcome>;
}
fn dynamic_id(scheduler:&dyn ScheduleWakeupSchedulerPort)->Result<String,ToolError> { let target=scheduler.get_wakeup_target().ok_or_else(||ToolError::Message("schedule_wakeup can only be used while a dynamic /loop is active.".into()))?; if target.kind==crate::types::LoopKind::Fixed { return Err(ToolError::Message("This is a fixed /loop tick; the recurring schedule re-arms automatically. Do not call schedule_wakeup.".into())); } Ok(target.loop_id) }
fn js_trim(value:&str)->&str { value.trim_matches(|c|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')) }
pub async fn execute_schedule_wakeup(scheduler:&dyn ScheduleWakeupSchedulerPort,params:&Value)->Result<ToolResult,ToolError> {
    let stop=params["stop"]==true;
    if stop && params.get("noop").is_some() { return Err(ToolError::Message("noop must be omitted when stop is true.".into())); }
    let reason=js_trim(params["reason"].as_str().unwrap_or("")); if reason.is_empty() { return Err(ToolError::Message("reason is required and must be non-empty.".into())); }
    if stop {
        let loop_id=dynamic_id(scheduler)?; let ignored=["delaySeconds","prompt"].into_iter().filter(|field|params.get(*field).is_some()).collect::<Vec<_>>();
        let outcome=scheduler.stop_dynamic_loop(StopDynamicLoopRequest { loop_id:loop_id.clone(),reason:reason.into() }).await.map_err(|error|ToolError::Message(error.message))?;
        return Ok(ToolResult { content:vec![maho_ext_api::ToolContent::text(format!("Stopped dynamic loop {loop_id}."))],details:Some(json!({"ok":true,"action":"stopped","loopId":loop_id,"terminalReason":"stopped","reason":reason,"endedAt":outcome.ended_at,"ignoredFields":ignored})) });
    }
    let requested=params.get("delaySeconds").ok_or_else(||ToolError::Message("delaySeconds is required unless stop is true.".into()))?.as_f64().filter(|delay|delay.is_finite() && delay.fract()==0.0).ok_or_else(||ToolError::Message("delaySeconds must be a finite integer number of seconds.".into()))?;
    let prompt=params["prompt"].as_str().filter(|prompt|!js_trim(prompt).is_empty()).ok_or_else(||ToolError::Message("prompt is required and must be non-empty unless stop is true.".into()))?;
    let loop_id=dynamic_id(scheduler)?; let delay=requested.clamp(MIN_WAKEUP_DELAY_SECONDS,MAX_WAKEUP_DELAY_SECONDS); let clamped=delay!=requested; let noop=params["noop"]==true;
    let outcome=scheduler.schedule_wakeup(ScheduleWakeupRequest { loop_id:loop_id.clone(),requested_delay_seconds:requested,delay_seconds:delay,reason:reason.into(),prompt:prompt.into(),noop }).await.map_err(|error|ToolError::Message(error.message))?;
    let requested_label=maho_ai::utils::js::number_to_string(requested);
    let text=if clamped { format!("Scheduled loop {loop_id} in {delay}s; requested {requested_label}s was clamped to the supported 60-3600s range.") } else { format!("Scheduled loop {loop_id} in {delay}s.") };
    let mut details=json!({"ok":true,"action":"scheduled","loopId":loop_id,"wakeupId":outcome.wakeup_id,"requestedDelaySeconds":requested,"delaySeconds":delay,"clamped":clamped,"dueAt":outcome.due_at,"reason":reason,"prompt":prompt,"noop":noop,"noopStreak":outcome.noop_streak}); if let Some(id)=outcome.replaced_wakeup_id { details["replacedWakeupId"]=id.into(); }
    Ok(ToolResult { content:vec![maho_ext_api::ToolContent::text(text)],details:Some(details) })
}
#[cfg(test)]
#[path="tools_parity_tests.rs"]
mod parity_tests;
pub fn schedule_wakeup_schema()->Value { json!({"type":"object","properties":{"delaySeconds":{"type":"integer","description":"Dynamic loop delay in seconds. Values are clamped to 60-3600."},"reason":{"type":"string","minLength":1,"description":"Why this wakeup or stop is appropriate. Must not be blank."},"prompt":{"type":"string","description":"Prompt to dispatch when the wakeup fires. Required unless stop is true; preserve the original /loop command verbatim for normal dynamic re-entry."},"stop":{"type":"boolean","description":"End the active dynamic loop immediately instead of scheduling another wakeup."},"noop":{"type":"boolean","description":"True when this iteration observed no actionable change. Consecutive noop iterations are folded in the terminal view. Omit when stopping."}},"required":["reason"],"additionalProperties":false}) }
pub fn register_loop_tools(api:&mut ExtensionApi,scheduler:Arc<dyn ScheduleWakeupSchedulerPort>) {
    let mut definition=ToolDefinition::new(SCHEDULE_WAKEUP_TOOL,SCHEDULE_WAKEUP_DESCRIPTION,schedule_wakeup_schema(),Arc::new(move |call| { let scheduler=Arc::clone(&scheduler); Box::pin(async move { execute_schedule_wakeup(scheduler.as_ref(),&call.params).await }) })); definition.label="Schedule Wakeup".into(); definition.exposure=Some(ToolExposure::Search); definition.allow_lazy_activation=Some(false); definition.execution_mode=Some(ToolExecutionMode::Sequential);
    api.register_tool(definition);
}
#[cfg(test)] mod tests {
    use super::*;
    struct Scheduler;
    impl ScheduleWakeupSchedulerPort for Scheduler {
        fn get_wakeup_target(&self)->Option<ScheduleWakeupTarget> { Some(ScheduleWakeupTarget { kind:crate::types::LoopKind::Dynamic,loop_id:"loop".into() }) }
        fn schedule_wakeup(&self,request:ScheduleWakeupRequest)->ExtensionFuture<'_,ScheduleWakeupOutcome> { Box::pin(async move { assert_eq!(request.prompt," /loop check "); Ok(ScheduleWakeupOutcome { wakeup_id:"wake".into(),replaced_wakeup_id:None,due_at:request.delay_seconds*1000.0,noop_streak:0.0 }) }) }
        fn stop_dynamic_loop(&self,_:StopDynamicLoopRequest)->ExtensionFuture<'_,StopDynamicLoopOutcome> { Box::pin(async { Ok(StopDynamicLoopOutcome { ended_at:10.0 }) }) }
    }
    #[tokio::test] async fn clamps_delay_but_preserves_prompt() { let result=execute_schedule_wakeup(&Scheduler,&json!({"delaySeconds":1,"reason":" wait ","prompt":" /loop check "})).await.unwrap(); let details=result.details.unwrap(); assert_eq!(details["delaySeconds"],60.0); assert_eq!(details["requestedDelaySeconds"],1.0); assert_eq!(details["reason"],"wait"); assert_eq!(details["prompt"]," /loop check "); }
    #[tokio::test] async fn stopping_ignores_delay_and_prompt() { let result=execute_schedule_wakeup(&Scheduler,&json!({"stop":true,"reason":"done","delaySeconds":1,"prompt":"check"})).await.unwrap(); assert_eq!(result.details.unwrap()["ignoredFields"],json!(["delaySeconds","prompt"])); }
    #[tokio::test] async fn stop_rejects_false_noop_as_present() { let result=execute_schedule_wakeup(&Scheduler,&json!({"stop":true,"noop":false,"reason":"done"})).await; assert!(result.is_err()); }
    #[tokio::test] async fn fractional_delay_is_rejected() { let result=execute_schedule_wakeup(&Scheduler,&json!({"delaySeconds":60.5,"reason":"wait","prompt":"check"})).await; assert!(result.is_err()); }
    #[test] fn schema_is_flat_without_delay_bounds() { let schema=schedule_wakeup_schema(); assert!(schema.get("anyOf").is_none()); assert!(schema["properties"]["delaySeconds"].get("minimum").is_none()); assert_eq!(schema["required"],json!(["reason"])); }
}
