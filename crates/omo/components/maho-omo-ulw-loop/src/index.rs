use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use crate::omo_command::{to_spawn_target,run_omo_command};
pub const STEERING_REMINDER:&str="<omo-senpi-ulw-loop>\nAn active omo-agent-toolkit ulw-loop run is present in this working directory.\nBefore continuing, inspect `omo-agent-toolkit ulw-loop status --json` and use the existing .omo/ulw-loop ledger as the source of truth.\nContinue the current ulw-loop story with evidence-bound execution; do not start unrelated work until the active run is complete or checkpointed.\n</omo-senpi-ulw-loop>";
pub const CONTINUATION_PROMPT:&str="Continue the active omo-agent-toolkit ulw-loop run.\nRun `omo-agent-toolkit ulw-loop status --json` in this session cwd, inspect the active incomplete goals, and keep working until the run is complete or safely checkpointed.";
#[derive(Default)]
struct State { consecutive:usize,previous:Option<String> }
pub struct UlwLoopComponent { pub bin:Option<String>,pub js_runtime:String }
async fn status(bin:&str,runtime:&str,cwd:&std::path::Path)->(String,bool) {
    let target=to_spawn_target(bin,&["ulw-loop".into(),"status".into(),"--json".into()],"linux",runtime);
    let result=run_omo_command(&target,cwd).await;
    let active=result.code==0 && serde_json::from_str::<serde_json::Value>(&result.stdout).is_ok_and(|value|status_has_active_incomplete_run(&value));
    (result.stdout,active)
}
impl Extension for UlwLoopComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let Some(bin)=&self.bin else {
            api.on(EventKind::Input,Arc::new(|_,_|Box::pin(async{Ok(EventResult::Input(InputEventResult::Continue))})));
            api.on(EventKind::AgentEnd,Arc::new(|_,_|Box::pin(async{Ok(EventResult::None)})));return;
        };
        let bin=bin.clone();let js=self.js_runtime.clone();let state=Arc::new(Mutex::new(State::default()));
        let footer=Arc::new(Mutex::new(crate::footer_status::FooterStatus::default()));
        for kind in [EventKind::SessionStart,EventKind::ToolResult] {
            let bin=bin.clone();let js=js.clone();let footer=Arc::clone(&footer);
            api.on(kind,Arc::new(move |event,ctx|{let bin=bin.clone();let js=js.clone();let footer=Arc::clone(&footer);Box::pin(async move {
                if let ExtensionEvent::ToolResult(e)=event && !matches!(e.tool_name.as_str(),"create_goal"|"update_goal"|"bash"|"interactive_bash") {return Ok(EventResult::None);}
                let active=status(&bin,&js,&ctx.cwd).await.1;crate::footer_status::sync_shared(&footer,ctx,active);Ok(EventResult::None)
            })}));
        }
        for kind in [EventKind::SessionBeforeSwitch,EventKind::SessionShutdown] {
            let footer=Arc::clone(&footer);api.on(kind,Arc::new(move |_,_|{let footer=Arc::clone(&footer);Box::pin(async move {footer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).dispose();Ok(EventResult::None)})}));
        }
        let input_state=Arc::clone(&state);let input_bin=bin.clone();let input_js=js.clone();
        let input_footer=Arc::clone(&footer);
        api.on(EventKind::Input,Arc::new(move |event,ctx| { let state=Arc::clone(&input_state);let bin=input_bin.clone();let js=input_js.clone();let footer=Arc::clone(&input_footer);Box::pin(async move {
            let ExtensionEvent::Input(input)=event else { return Ok(EventResult::None); };
            if input.source==InputSource::Extension { return Ok(EventResult::Input(InputEventResult::Continue)); }
            *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=State::default();
            if input.streaming_behavior.is_some() { let active=status(&bin,&js,&ctx.cwd).await.1;crate::footer_status::sync_shared(&footer,ctx,active);if active { return Ok(EventResult::Input(InputEventResult::Transform{text:format!("{}\n\n{STEERING_REMINDER}",input.text),images:input.images.clone()})); } }
            Ok(EventResult::Input(InputEventResult::Continue))
        }) }));
        let runtime=api.runtime.clone();
        api.on(EventKind::AgentEnd,Arc::new(move |_,ctx| { let state=Arc::clone(&state);let bin=bin.clone();let js=js.clone();let runtime=runtime.clone();let footer=Arc::clone(&footer);Box::pin(async move {
            if state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).consecutive>=8 { return Ok(EventResult::None); }
            if !ctx.session_manager.session_id().is_empty() && maho_omo_start_work_continuation::boulder_eligibility::find_continuable_boulder_work(&ctx.cwd,ctx.session_manager.session_id()).map_err(|e|ExtensionFailure::new(e.to_string()))?.is_some() { return Ok(EventResult::None); }
            let (raw,active)=status(&bin,&js,&ctx.cwd).await;
            crate::footer_status::sync_shared(&footer,ctx,active);
            let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !active { state.previous=None;return Ok(EventResult::None); }
            if state.previous.as_ref()==Some(&raw) { return Ok(EventResult::None); }
            state.previous=Some(raw);state.consecutive+=1;
            let api=ExtensionApi::new(LoadedExtension::new("ulw-loop",ctx.cwd.clone(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
            api.send_message(CustomMessage{custom_type:"omo-senpi:ulw-continuation".into(),content:vec![ToolContent::text(CONTINUATION_PROMPT)],display:false,details:None},SendMessageOptions{trigger_turn:true,deliver_as:Some(DeliverAs::FollowUp)})?;
            Ok(EventResult::None)
        }) }));
    }
}
pub fn status_has_active_incomplete_run(value:&serde_json::Value)->bool {
    if value["ok"]!=true || !value["plan"].is_object() || value["plan"]["aggregateCompletion"]["status"]=="complete" { return false; }
    value["plan"]["goals"].as_array().is_some_and(|goals|goals.iter().any(|goal| {
        if !goal.is_object() || matches!(goal["steeringStatus"].as_str(),Some("superseded"|"blocked")) || !matches!(goal["status"].as_str(),Some("pending"|"in_progress")) { return false; }
        goal["successCriteria"].as_array().is_none_or(|criteria|criteria.is_empty() || criteria.iter().any(|c|c["status"]!="pass"))
    }))
}
