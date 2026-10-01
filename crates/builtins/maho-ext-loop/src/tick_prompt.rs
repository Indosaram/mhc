use serde::Serialize;
use crate::types::*;
pub type LoopMode=LoopKind;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
#[serde(rename_all="lowercase")]
pub enum TickDelivery { Full,Reminder,Prompt }
#[derive(Clone,Debug,PartialEq)]
pub enum LoopFileSnapshot { Absent,Present { path:String,content:String,mtime_ms:f64,size:f64,content_hash:String } }
pub struct TickPromptInput { pub loop_id:LoopId,pub delivery_id:DeliveryId,pub mode:LoopMode,pub payload:LoopPayload,pub reentry_prompt:String,pub delivery_state:SentinelDeliveryState,pub loop_file:LoopFileSnapshot }
#[derive(Clone,Debug,PartialEq,Serialize)]
#[serde(rename_all="camelCase")]
pub struct LoopTickDetails { pub loop_id:LoopId,pub delivery_id:DeliveryId,pub mode:LoopMode,pub delivery:TickDelivery,#[serde(skip_serializing_if="Option::is_none")] pub sentinel:Option<LoopSentinel>,#[serde(skip_serializing_if="Option::is_none")] pub loop_file:Option<LoopFileFingerprint> }
pub struct TickPromptResult { pub text:String,pub delivery:TickDelivery,pub details:LoopTickDetails,pub delivery_state:SentinelDeliveryState }
fn mode_rule(mode:LoopMode,reentry:&str)->String {
    if mode==LoopMode::Fixed { return "The fixed recurring schedule fires the next tick automatically - do not call `schedule_wakeup` from this tick.\nIf this tick errors, the schedule stays armed and the next occurrence still fires.".into(); }
    let reentry=serde_json::Value::String(reentry.into()).to_string();
    format!("As the last action of this turn, call `schedule_wakeup` with `prompt: {reentry}` so the loop stays alive; a turn that ends without it burns the single keepalive fallback and then the loop ends.\nTo end the loop instead, call `schedule_wakeup` with `{{ stop: true }}` and a reason.\nWhen the next step is gated on observable state, do not sleep or poll: watch terminal output with `monitor`, inspect it with `bash_output`, stop the watcher with `kill_bash`, and let task-notification wakeups resume this loop. With one of those armed, the scheduled delay is only a fallback heartbeat (1200-1800s is the normal idle range).\nSet `noop: true` only when this tick found no actionable change, and notify the user only on changes worth acting on - not once per tick.")
}
fn header(base:&str,mode:LoopMode)->String { if mode==LoopMode::Dynamic { format!("{base} (dynamic pacing)") } else { base.into() } }
fn join(blocks:&[&str])->String { blocks.iter().copied().filter(|block|!block.is_empty()).collect::<Vec<_>>().join("\n\n") }
pub fn build_tick_message(input:TickPromptInput)->TickPromptResult {
    let mut state=input.delivery_state; let rule=mode_rule(input.mode,&input.reentry_prompt);
    let mut details=LoopTickDetails { loop_id:input.loop_id,delivery_id:input.delivery_id.clone(),mode:input.mode,delivery:TickDelivery::Full,sentinel:None,loop_file:None };
    let (text,delivery)=match input.payload {
        LoopPayload::Prompt { prompt }=>(join(&[&prompt,&rule]),TickDelivery::Prompt),
        LoopPayload::Sentinel { sentinel }=>{
            details.sentinel=Some(sentinel);
            if matches!(sentinel,LoopSentinel::LoopFile|LoopSentinel::LoopFileDynamic) {
                match input.loop_file {
                    LoopFileSnapshot::Absent=>{
                        state.last_loop_file_delivered=None; state.force_full_delivery=false;
                        (join(&[&header("# /loop tick - loop.md absent",input.mode),"The loop-tasks file used by this loop is currently absent. Perform the autonomous maintenance check for this tick. Keep the loop alive so a later tick can pick up the file if it reappears.",&rule]),TickDelivery::Full)
                    },
                    LoopFileSnapshot::Present { path,content,mtime_ms,size,content_hash }=>{
                        let full=state.force_full_delivery || state.last_loop_file_delivered.as_ref().is_none_or(|previous|previous.path!=path || previous.mtime_ms!=mtime_ms || previous.size!=size || previous.content_hash!=content_hash);
                        if full {
                            let fingerprint=LoopFileFingerprint { path,mtime_ms,size,content_hash,anchor_delivery_id:input.delivery_id };
                            details.loop_file=Some(fingerprint.clone()); state.last_loop_file_delivered=Some(fingerprint); state.force_full_delivery=false;
                            (join(&["# /loop tick - loop.md tasks","The user configured a loop-tasks file. Work through the tasks defined below; these are the instructions for this tick and every subsequent tick (the reminder on later fires refers back to this message).",&format!("---\n{content}\n---"),&rule]),TickDelivery::Full)
                        } else {
                            details.loop_file=state.last_loop_file_delivered.clone();
                            (join(&["# /loop tick - loop.md tasks","Continue the loop.md tasks established by the most recent full loop.md instruction message in this conversation. Re-read that anchored message and perform the next applicable work.",&rule]),TickDelivery::Reminder)
                        }
                    },
                }
            } else {
                let full=state.force_full_delivery || !state.autonomous_preamble_delivered;
                let body=if full { "No loop-tasks file is configured, so run the standard autonomous maintenance pass for this tick:\n\n1. Inspect current state: active tasks, running terminal commands and monitors, and any delegated child work.\n2. Make concrete progress where progress is possible; do not merely narrate.\n3. Avoid repeating unchanged status - stay silent when nothing moved.\n4. Surface only actionable state changes to the user, once per state change." } else { "Continue the autonomous maintenance pass established by the most recent full autonomous loop instruction message in this conversation. Re-read that anchored message and perform the next applicable work." };
                if full { state.autonomous_preamble_delivered=true; state.force_full_delivery=false; }
                (join(&[&header("# Autonomous loop tick",input.mode),body,&rule]),if full { TickDelivery::Full } else { TickDelivery::Reminder })
            }
        },
    };
    details.delivery=delivery; TickPromptResult { text,delivery,details,delivery_state:state }
}
#[cfg(test)] mod tests {
    use super::*;
    fn input(state:SentinelDeliveryState,snapshot:LoopFileSnapshot)->TickPromptInput { TickPromptInput { loop_id:"loop".into(),delivery_id:"delivery".into(),mode:LoopMode::Fixed,payload:LoopPayload::Sentinel { sentinel:LoopSentinel::LoopFile },reentry_prompt:"/loop".into(),delivery_state:state,loop_file:snapshot } }
    fn snapshot(hash:&str)->LoopFileSnapshot { LoopFileSnapshot::Present { path:"loop.md".into(),content:"tasks".into(),mtime_ms:1.0,size:5.0,content_hash:hash.into() } }
    #[test] fn unchanged_file_reuses_anchor_and_changed_hash_reanchors() { let first=build_tick_message(input(Default::default(),snapshot("a"))); assert_eq!(first.delivery,TickDelivery::Full); let second=build_tick_message(input(first.delivery_state,snapshot("a"))); assert_eq!(second.delivery,TickDelivery::Reminder); assert_eq!(second.details.loop_file.unwrap().anchor_delivery_id,"delivery"); let third=build_tick_message(input(second.delivery_state,snapshot("b"))); assert_eq!(third.delivery,TickDelivery::Full); }
    #[test] fn absent_file_clears_anchor_and_reappearance_delivers_full() { let first=build_tick_message(input(Default::default(),snapshot("a"))); let absent=build_tick_message(input(first.delivery_state,LoopFileSnapshot::Absent)); assert!(absent.delivery_state.last_loop_file_delivered.is_none()); let result=build_tick_message(input(absent.delivery_state,snapshot("a"))); assert_eq!(result.delivery,TickDelivery::Full); }
    #[test] fn force_full_delivery_resets_after_anchor() { let first=build_tick_message(input(Default::default(),snapshot("a"))); let mut state=first.delivery_state; state.force_full_delivery=true; let result=build_tick_message(input(state,snapshot("a"))); assert_eq!(result.delivery,TickDelivery::Full); assert!(!result.delivery_state.force_full_delivery); }
    #[test] fn verbatim_prompt_does_not_mutate_sentinel_state() { let mut input=input(SentinelDeliveryState { force_full_delivery:true,..Default::default() },LoopFileSnapshot::Absent); input.payload=LoopPayload::Prompt { prompt:"check".into() }; let state=input.delivery_state.clone(); let result=build_tick_message(input); assert_eq!(result.delivery,TickDelivery::Prompt); assert_eq!(result.delivery_state,state); assert!(result.details.sentinel.is_none()); }
    #[test] fn autonomous_full_then_reminder() { let mut first=input(Default::default(),LoopFileSnapshot::Absent); first.payload=LoopPayload::Sentinel { sentinel:LoopSentinel::AutonomousDynamic }; first.mode=LoopMode::Dynamic; let first=build_tick_message(first); assert!(first.delivery_state.autonomous_preamble_delivered); let mut second=input(first.delivery_state,LoopFileSnapshot::Absent); second.payload=LoopPayload::Sentinel { sentinel:LoopSentinel::Autonomous }; let second=build_tick_message(second); assert_eq!(second.delivery,TickDelivery::Reminder); }
}
