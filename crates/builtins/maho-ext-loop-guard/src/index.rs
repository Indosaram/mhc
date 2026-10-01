use std::sync::{Arc,Mutex};
use maho_ext_api::{AbortSource,CustomMessage,DeliverAs,EventBus,EventKind,EventResult,Extension,ExtensionApi,ExtensionContext,ExtensionEvent,ExtensionFailure,InputSource,NotificationType,SendMessageOptions,ToolCallEventResult,ToolContent};
use crate::{detectors::{detect_loop,LoopGuardDetection,NoticeGate},escalation::{IdenticalEscalationDecision,IdenticalLoopEscalation},notice::*,tracker::ToolCallTracker};
const WAKE_SOURCE:&str="loop-guard-hard-stop";
#[derive(Default)]
struct State { tracker:ToolCallTracker,gate:NoticeGate,escalation:IdenticalLoopEscalation,pending_recovery_tool_name:Option<String>,recovery_wake_source_active:bool,continuation_hold_active:bool }
impl State {
    fn set_wake(&mut self,events:&EventBus,active:bool) { if self.recovery_wake_source_active==active { return; } self.recovery_wake_source_active=active; events.emit("wake_source_state",&serde_json::json!({"source":WAKE_SOURCE,"activeCount":if active {1} else {0}})); }
    fn set_hold(&mut self,events:&EventBus,active:bool) { if self.continuation_hold_active==active { return; } self.continuation_hold_active=active; events.emit("continuation_hold_state",&serde_json::json!({"source":WAKE_SOURCE,"active":active})); }
    fn reset(&mut self,events:&EventBus) { self.tracker.reset(); self.gate.reset(); self.escalation.reset(); self.pending_recovery_tool_name=None; self.set_wake(events,false); self.set_hold(events,false); }
}
fn send(api:&ExtensionApi,custom_type:&str,text:String,display:bool,details:Option<serde_json::Value>,trigger_turn:bool,deliver_as:Option<DeliverAs>)->Result<(),ExtensionFailure> { api.send_message(CustomMessage { custom_type:custom_type.into(),content:vec![ToolContent::text(text)],display,details },SendMessageOptions { trigger_turn,deliver_as }) }
fn detection_details(d:&LoopGuardDetection)->serde_json::Value { match d { LoopGuardDetection::Identical { tool_name,count,fingerprint }=>serde_json::json!({"kind":"identical","toolName":tool_name,"count":count,"fingerprint":fingerprint}),LoopGuardDetection::Similar { tool_name,count,similarity,fingerprint }=>serde_json::json!({"kind":"similar","toolName":tool_name,"count":count,"similarity":similarity,"fingerprint":fingerprint}),LoopGuardDetection::Cycle { period,count,cycle_tools,fingerprint }=>serde_json::json!({"kind":"cycle","period":period,"count":count,"cycleTools":cycle_tools,"fingerprint":fingerprint}) } }
fn handle(state:&mut State,api:&ExtensionApi,event:&ExtensionEvent,ctx:&ExtensionContext)->Result<EventResult,ExtensionFailure> {
    match event {
        ExtensionEvent::SessionStart(_) | ExtensionEvent::SessionShutdown(_)=>state.reset(&api.events),
        ExtensionEvent::Input(input) if input.source!=InputSource::Extension=>state.reset(&api.events),
        ExtensionEvent::ToolExecutionStart { tool_call_id,tool_name,args }=>{
            let record=state.tracker.record(tool_name,Some(args));
            if state.escalation.observe_attempt(tool_call_id,record) { state.pending_recovery_tool_name=None; state.set_wake(&api.events,false); state.set_hold(&api.events,false); }
            if let Some(detection)=detect_loop(state.tracker.records(),&mut state.gate) { state.escalation.observe_notice(&detection); send(api,LOOP_GUARD_NOTICE_CUSTOM_TYPE,build_loop_guard_reminder(&detection),true,Some(detection_details(&detection)),false,Some(DeliverAs::Steer))?; }
        },
        ExtensionEvent::TurnEnd { .. }=>state.escalation.finish_turn(),
        ExtensionEvent::ToolCall(call)=>{
            let (tool_name,blocked_call_count)=match state.escalation.consume_tool_call(&call.tool_call_id) {
                IdenticalEscalationDecision::Allow=>return Ok(EventResult::None),
                IdenticalEscalationDecision::Block { tool_name,blocked_call_count }=>(tool_name,blocked_call_count),
                IdenticalEscalationDecision::HardStop { tool_name,blocked_call_count,announce }=>{
                    let warning=build_loop_guard_hard_stop_warning(&tool_name,blocked_call_count); state.set_wake(&api.events,true);
                    if !announce { state.set_hold(&api.events,true); }
                    if announce { send(api,LOOP_GUARD_ESCALATION_CUSTOM_TYPE,warning.clone(),true,Some(serde_json::json!({"toolName":tool_name,"blockedCallCount":blocked_call_count})),false,Some(DeliverAs::Steer))?; if ctx.has_ui { ctx.ui.notify(&warning,NotificationType::Warning); } state.pending_recovery_tool_name=Some(tool_name.clone()); }
                    ctx.abort(Some(AbortSource::System))?; (tool_name,blocked_call_count)
                },
            };
            return Ok(EventResult::ToolCall(ToolCallEventResult { block:Some(true),reason:Some(build_loop_guard_block_reason(&tool_name,blocked_call_count)),terminate:Some(false) }));
        },
        ExtensionEvent::AgentStart=>{ state.set_wake(&api.events,false); state.set_hold(&api.events,false); },
        ExtensionEvent::AgentSettled=>{ if let Some(tool_name)=state.pending_recovery_tool_name.take() { send(api,LOOP_GUARD_RECOVERY_CUSTOM_TYPE,build_loop_guard_hard_stop_steer(&tool_name),false,None,true,None)?; } },
        _=>{},
    }
    Ok(EventResult::None)
}
pub struct LoopGuardExtension;
impl Extension for LoopGuardExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let state=Arc::new(Mutex::new(State::default()));
        let actions=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        for kind in [EventKind::SessionStart,EventKind::SessionShutdown,EventKind::Input,EventKind::ToolExecutionStart,EventKind::TurnEnd,EventKind::ToolCall,EventKind::AgentStart,EventKind::AgentSettled] {
            let state=Arc::clone(&state); let actions=Arc::clone(&actions);
            api.on(kind,Arc::new(move |event,ctx| { let state=Arc::clone(&state); let actions=Arc::clone(&actions); Box::pin(async move { let mut state=state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?; handle(&mut state,&actions,event,ctx) }) }));
        }
    }
}
