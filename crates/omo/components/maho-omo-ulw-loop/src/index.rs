use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use crate::omo_command::{to_spawn_target,run_omo_command};
pub const STEERING_REMINDER:&str="<omo-senpi-ulw-loop>\nAn active omo-agent-toolkit ulw-loop run is present in this working directory.\nBefore continuing, inspect `omo-agent-toolkit ulw-loop status --json` and use the existing .omo/ulw-loop ledger as the source of truth.\nContinue the current ulw-loop story with evidence-bound execution; do not start unrelated work until the active run is complete or checkpointed.\n</omo-senpi-ulw-loop>";
pub const CONTINUATION_PROMPT:&str="Continue the active omo-agent-toolkit ulw-loop run.\nRun `omo-agent-toolkit ulw-loop status --json` in this session cwd, inspect the active incomplete goals, and keep working until the run is complete or safely checkpointed.";
pub const ULW_CONTINUATION_INJECTION_KEY:&str="omo-senpi-ulw-loop-continuation";
pub const ULW_CONTINUATION_CUSTOM_TYPE:&str="omo-senpi:ulw-continuation";
#[derive(Default)]
struct State { consecutive:usize,previous:Option<String> }
pub type CommandFuture=std::pin::Pin<Box<dyn std::future::Future<Output=std::io::Result<crate::omo_command::CommandResult>>+Send>>;
pub type CommandRunner=Arc<dyn Fn(String,Vec<String>,std::path::PathBuf)->CommandFuture+Send+Sync>;
pub struct UlwLoopComponent { pub bin:Option<String>,pub js_runtime:String,pub run_command:Option<CommandRunner>,pub logger:Option<Arc<dyn ComponentLogger>> }
impl Default for UlwLoopComponent {
    fn default() -> Self {
        let env = std::env::vars().collect();
        Self::from_env(&env)
    }
}
impl UlwLoopComponent {
    pub fn from_env(env: &std::collections::BTreeMap<String, String>) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let bin = crate::omo_command::resolve_omo_bin(env, |name| {
            env.get("PATH").into_iter().flat_map(|path| std::env::split_paths(path))
                .filter(|dir| !dir.as_os_str().is_empty()).map(|dir| dir.join(name))
                .find(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0))
                .map(|path| path.to_string_lossy().into_owned())
        });
        Self { bin, js_runtime: "bun".into(), run_command: None, logger: None }
    }
}
fn log_info(logger:&Option<Arc<dyn ComponentLogger>>,message:&str,details:Option<JsonValue>){if let Some(logger)=logger{logger.info(message,details.as_ref());}}
fn log_warn(logger:&Option<Arc<dyn ComponentLogger>>,message:&str,details:Option<JsonValue>){if let Some(logger)=logger{logger.warn(message,details.as_ref());}}
async fn status(bin:&str,runtime:&str,cwd:&std::path::Path,runner:Option<&CommandRunner>,logger:&Option<Arc<dyn ComponentLogger>>)->Option<(String,bool)> {
    let target=to_spawn_target(bin,&["ulw-loop".into(),"status".into(),"--json".into()],"linux",runtime);
    let result=if let Some(run)=runner {match run(bin.into(),vec!["ulw-loop".into(),"status".into(),"--json".into()],cwd.into()).await {Ok(result)=>result,Err(error)=>{log_warn(logger,"omo-senpi ulw-loop status ignored",Some(serde_json::json!({"reason":"run-command-failed","error":error.to_string()})));return None}}}else{run_omo_command(&target,cwd).await};
    if result.code!=0 { log_warn(logger,"omo-senpi ulw-loop status ignored",Some(serde_json::json!({"reason":"non-zero-exit","code":result.code}))); return Some((result.stdout,false)); }
    let Ok(value)=serde_json::from_str::<serde_json::Value>(&result.stdout) else { log_warn(logger,"omo-senpi ulw-loop status ignored",Some(serde_json::json!({"reason":"malformed-json"}))); return Some((result.stdout,false)); };
    Some((result.stdout,status_has_active_incomplete_run(&value)))
}
impl Extension for UlwLoopComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let Some(bin)=&self.bin else {
            log_info(&self.logger,"omo-senpi ulw-loop inactive; omo binary not found",None);
            api.on(EventKind::Input,Arc::new(|_,_|Box::pin(async{Ok(EventResult::Input(InputEventResult::Continue))})));
            api.on(EventKind::AgentEnd,Arc::new(|_,_|Box::pin(async{Ok(EventResult::None)})));return;
        };
        let bin=bin.clone();let js=self.js_runtime.clone();let state=Arc::new(Mutex::new(State::default()));
        let footer=Arc::new(Mutex::new(crate::footer_status::FooterStatus::default()));
        let runner=self.run_command.clone();
        for kind in [EventKind::SessionStart,EventKind::ToolResult] {
            let bin=bin.clone();let js=js.clone();let footer=Arc::clone(&footer);let runner=runner.clone();
            api.on(kind,Arc::new(move |event,ctx|{let bin=bin.clone();let js=js.clone();let footer=Arc::clone(&footer);let runner=runner.clone();Box::pin(async move {
                if let ExtensionEvent::ToolResult(e)=event && !matches!(e.tool_name.as_str(),"create_goal"|"update_goal"|"bash"|"interactive_bash") {return Ok(EventResult::None);}
                let active=status(&bin,&js,&ctx.cwd,runner.as_ref(),&ctx.logger).await.is_some_and(|(_,active)|active);crate::footer_status::sync_shared(&footer,ctx,active);Ok(EventResult::None)
            })}));
        }
        for kind in [EventKind::SessionBeforeSwitch,EventKind::SessionShutdown] {
            let footer=Arc::clone(&footer);api.on(kind,Arc::new(move |_,_|{let footer=Arc::clone(&footer);Box::pin(async move {footer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).dispose();Ok(EventResult::None)})}));
        }
        let input_state=Arc::clone(&state);let input_bin=bin.clone();let input_js=js.clone();
        let input_footer=Arc::clone(&footer);
        let input_runner=runner.clone();
        api.on(EventKind::Input,Arc::new(move |event,ctx| { let state=Arc::clone(&input_state);let bin=input_bin.clone();let js=input_js.clone();let footer=Arc::clone(&input_footer);let runner=input_runner.clone();Box::pin(async move {
            let ExtensionEvent::Input(input)=event else { return Ok(EventResult::None); };
            if input.source==InputSource::Extension { return Ok(EventResult::Input(InputEventResult::Continue)); }
            *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=State::default();
            if input.streaming_behavior.is_some() { let active=status(&bin,&js,&ctx.cwd,runner.as_ref(),&ctx.logger).await.is_some_and(|(_,active)|active);crate::footer_status::sync_shared(&footer,ctx,active);if active { return Ok(EventResult::Input(InputEventResult::Transform{text:format!("{}\n\n{STEERING_REMINDER}",input.text),images:input.images.clone()})); } }
            Ok(EventResult::Input(InputEventResult::Continue))
        }) }));
        let runtime=api.runtime.clone();
        api.on(EventKind::AgentEnd,Arc::new(move |_,ctx| { let state=Arc::clone(&state);let bin=bin.clone();let js=js.clone();let runtime=runtime.clone();let footer=Arc::clone(&footer);let runner=runner.clone();Box::pin(async move {
            { let guard=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner); if guard.consecutive>=8 { let count=guard.consecutive; drop(guard); log_info(&ctx.logger,"omo-senpi ulw-loop continuation skipped",Some(serde_json::json!({"reason":"continuation-cap-reached","count":count}))); return Ok(EventResult::None); } }
            if !ctx.session_manager.session_id().is_empty() && maho_omo_start_work_continuation::boulder_eligibility::find_continuable_boulder_work(&ctx.cwd,ctx.session_manager.session_id()).map_err(|e|ExtensionFailure::new(e.to_string()))?.is_some() { log_info(&ctx.logger,"omo-senpi ulw-loop continuation skipped",Some(serde_json::json!({"reason":"boulder-continuation-active"}))); return Ok(EventResult::None); }
            let result=status(&bin,&js,&ctx.cwd,runner.as_ref(),&ctx.logger).await;
            let Some((raw,active))=result else {crate::footer_status::sync_shared(&footer,ctx,false);return Ok(EventResult::None);};
            crate::footer_status::sync_shared(&footer,ctx,active);
            let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !active { state.previous=None;drop(state);log_info(&ctx.logger,"omo-senpi ulw-loop continuation skipped",Some(serde_json::json!({"reason":"inactive"})));return Ok(EventResult::None); }
            if state.previous.as_ref()==Some(&raw) { drop(state);log_info(&ctx.logger,"omo-senpi ulw-loop continuation skipped",Some(serde_json::json!({"reason":"stale-status"}))); return Ok(EventResult::None); }
            state.previous=Some(raw);state.consecutive+=1;
            drop(state);
            if let Some(coordinator)=&ctx.idle_coordinator {
                coordinator.enqueue(IdleInjection{key:ULW_CONTINUATION_INJECTION_KEY.into(),source:IdleInjectionSource::UlwContinuation,custom_type:Some(ULW_CONTINUATION_CUSTOM_TYPE.into()),content:CONTINUATION_PROMPT.into(),display:Some(false),details:None,on_flushed:None,on_delivery_failed:None});
                coordinator.schedule_flush();
                return Ok(EventResult::None);
            }
            let api=ExtensionApi::new(LoadedExtension::new("ulw-loop",ctx.cwd.clone(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
            api.send_message(CustomMessage{custom_type:ULW_CONTINUATION_CUSTOM_TYPE.into(),content:vec![ToolContent::text(CONTINUATION_PROMPT)],display:false,details:None},SendMessageOptions{trigger_turn:true,deliver_as:Some(DeliverAs::FollowUp)})?;
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
