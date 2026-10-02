use std::sync::{Arc,Mutex};
use std::time::Duration;
use maho_pty::PtyExit;
use maho_tools::definition::ToolCall;
use serde_json::{Value,json};
use crate::manager::TerminalManager;
use crate::shared::FOREGROUND_ENV_OVERRIDES;
use crate::output_format::format_terminal_tool_output;
use super::context::{TerminalToolResult,text_result,error_result,noticed_result};
use super::spawn::describe_exit;
async fn wait_exit(mut receiver:tokio::sync::watch::Receiver<Option<Result<PtyExit,String>>>)->Result<PtyExit,String> {loop {if let Some(exit)=receiver.borrow_and_update().clone() {return exit;}receiver.changed().await.map_err(|error|error.to_string())?;}}
pub async fn execute_bash(manager:Arc<Mutex<TerminalManager>>,call:ToolCall<'_>)->Result<TerminalToolResult,String> {
    execute_bash_with_window(manager,call,std::env::var("PI_BASH_FOREGROUND_SECONDS").ok().as_deref()).await
}
pub async fn execute_bash_with_window(manager:Arc<Mutex<TerminalManager>>,call:ToolCall<'_>,foreground_window:Option<&str>)->Result<TerminalToolResult,String> {
    execute_configured_bash(manager,call,foreground_window,&crate::settings::TERMINAL_SETTINGS_DEFAULTS,None).await
}
pub async fn execute_configured_bash(manager:Arc<Mutex<TerminalManager>>,call:ToolCall<'_>,foreground_window:Option<&str>,settings:&crate::settings::ResolvedTerminalSettings,shell:Option<&str>)->Result<TerminalToolResult,String> {
    let command=call.params.get("command").and_then(Value::as_str).ok_or("command must be a string")?;
    let background=call.params.get("run_in_background").and_then(Value::as_bool).unwrap_or(false);
    if !background&&call.signal.is_aborted() {return Ok(error_result("Command aborted"));}
    let dimension=|key:&str,fallback:u16|call.params.get(key).and_then(Value::as_f64).filter(|value|value.is_finite()&&*value>=1.0).map_or(fallback,|value|value.trunc() as u16);
    let mut options=super::spawn::command_options(command,call.context.map(maho_tools::definition::ToolContext::cwd),shell).map_err(|error|error.to_string())?.size(dimension("cols",settings.default_cols as u16),dimension("rows",settings.default_rows as u16));
    if let Some(context)=call.context {
        options=options.cwd(context.cwd()).env("PI_SESSION_ID",context.session_manager().session_id()).env("PI_SESSION_CWD",context.cwd().to_string_lossy());
        if let Some(path)=context.session_manager().session_file() {options=options.env("PI_SESSION_FILE",path.to_string_lossy());}
        if let Some(path)=context.goal_store_file() {options=options.env("PI_GOAL_STORE_FILE",path.to_string_lossy());}
        if let Some(model)=context.model() {options=options.env("PI_PROVIDER",&model.provider).env("PI_MODEL",&model.id);}
        if let Some(level)=context.thinking_level() {options=options.env("PI_REASONING_LEVEL",serde_json::to_value(level).map_err(|error|error.to_string())?.as_str().ok_or("reasoning level must be a string")?);}
    }
    if !background {for (key,value) in FOREGROUND_ENV_OVERRIDES {options=options.env(*key,*value);}if let Some(timeout)=call.params.get("timeout").and_then(Value::as_f64) {options=options.timeout(Duration::try_from_secs_f64(timeout).map_err(|error|error.to_string())?);}}
    let (id,exit)={let mut manager=manager.lock().map_err(|_|"terminal manager state poisoned")?;let id=manager.create(command,options).map_err(|error|error.to_string())?;let runtime=manager.get(&id).ok_or("created terminal session missing")?;(id,runtime.subscribe_exit())};
    if background {tokio::select! {_=wait_exit(exit)=>{},_=tokio::time::sleep(Duration::from_millis(250))=>{}}let mut manager=manager.lock().map_err(|_|"terminal manager state poisoned")?;let early=format_terminal_tool_output(&manager.get(&id).ok_or("terminal session missing")?.read_delta().map_err(|error|error.to_string())?.text).text;let mut result=text_result(format!("Command running in background with ID: {id}{}",if early.is_empty() {String::new()} else {format!("\n\n{early}")}));result.details=Some(json!({"bash_id":id,"background":true}).as_object().ok_or("background details missing")?.clone());return Ok(result);}
    let sleep_wait=super::sleep_wait::classify_sleep_wait(command).map_err(|error|error.to_string())?;
    let window=super::foreground_window::resolve_foreground_window_seconds(foreground_window);
    let window=if sleep_wait.is_some() {window.min(super::foreground_window::SLEEP_WAIT_WINDOW_SECONDS)} else {window};
    let detach_delay=if settings.timeout_action==crate::settings::TimeoutAction::Kill||call.params.get("timeout").and_then(Value::as_f64).is_some_and(|timeout|timeout<=window) {Duration::MAX} else {Duration::from_secs_f64(window)};
    let outcome=super::foreground_detach::foreground_outcome(exit,&call.signal,detach_delay);tokio::pin!(outcome);
    let mut updates=if call.on_update.is_some() {Some(manager.lock().map_err(|_|"terminal manager state poisoned")?.get(&id).ok_or("terminal session missing")?.subscribe_output().map_err(|error|error.to_string())?.1)}else {None};
    let started_at=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error|error.to_string())?.as_secs_f64()*1000.0;
    let activity=format!("running {}",String::from_utf16_lossy(&command.encode_utf16().take(80).collect::<Vec<_>>()));
    let emit=||->Result<(),String> {
        let output=manager.lock().map_err(|_|"terminal manager state poisoned")?.get(&id).ok_or("terminal session missing")?.full_output().map_err(|error|error.to_string())?;
        let body=format_terminal_tool_output(&output).body;let units=body.encode_utf16().collect::<Vec<_>>();let text=String::from_utf16_lossy(&units[units.len().saturating_sub(2000)..]);
        if let Some(update)=&call.on_update {update(maho_tools::definition::ToolResult {content:vec![maho_tools::definition::ToolContent::text(text)],details:Some(json!({"progress":{"activity":activity,"startedAt":started_at}}))}).map_err(|error|error.to_string())?;}Ok(())
    };
    if let Some(update)=&call.on_update {update(maho_tools::definition::ToolResult {content:vec![],details:None}).map_err(|error|error.to_string())?;}
    let mut last_emission=None;let mut trailing=None;
    let settled=loop {tokio::select! {
        settled=&mut outcome=>break settled?,
        chunk=async {updates.as_mut().expect("enabled updates").recv().await},if updates.is_some()=>{
            if chunk.is_none() {updates=None;continue;}
            let now=tokio::time::Instant::now();
            if last_emission.is_none_or(|last|now-last>=Duration::from_millis(100)) {emit()?;last_emission=Some(now);trailing=None;}else {trailing=last_emission.map(|last|last+Duration::from_millis(100));}
        }
        _=async {tokio::time::sleep_until(trailing.expect("pending progress")).await},if trailing.is_some()=>{emit()?;last_emission=Some(tokio::time::Instant::now());trailing=None;}
    }};
    if trailing.is_some() {emit()?;}
    let outcome=match settled {
        super::foreground_detach::ForegroundOutcome::Exit(exit)=>Some(exit),
        super::foreground_detach::ForegroundOutcome::Aborted=>{manager.lock().map_err(|_|"terminal manager state poisoned")?.stop(&id).map_err(|error|error.to_string())?;None},
        super::foreground_detach::ForegroundOutcome::Detached=>None,
    };
    let mut manager=manager.lock().map_err(|_|"terminal manager state poisoned")?;let runtime=manager.get(&id).ok_or("terminal session missing")?;
    let formatted=format_terminal_tool_output(&runtime.full_output().map_err(|error|error.to_string())?);
    if call.signal.is_aborted() {return Ok(error_result(format!("{}Command aborted",if formatted.text.is_empty() {String::new()} else {format!("{}\n\n",formatted.text)})));}
    if let Some(exit)=outcome {
        if exit.timed_out {return Ok(error_result(format!("{}\n\nCommand timed out after {} seconds",formatted.text,call.params.get("timeout").unwrap_or(&Value::Null))));}
        if let Some(code)=exit.exit_code && code!=0 {return Ok(error_result(format!("{}\n\nCommand exited with code {code}",formatted.text)));}
        let mut result=noticed_result(if formatted.text.is_empty() {"(no output)"} else {&formatted.text},&[formatted.marker.as_deref()]);result.details=Some(json!({"status":describe_exit(Some(&exit))}).as_object().ok_or("exit details missing")?.clone());return Ok(result);
    }
    let delta=runtime.read_delta().map_err(|error|error.to_string())?;
    let partial=format_terminal_tool_output(&delta.text).text;
    let timeout_note=call.params.get("timeout").and_then(Value::as_f64).filter(|timeout|timeout.is_finite()).map_or_else(||"not killed; it will run until exit or kill_bash".to_owned(),|timeout|format!("not killed; the original {timeout}s timeout still applies"));
    let guidance=if let Some(wait)=&sleep_wait {format!("This command is a wait ({}s sleep), so it detached immediately; the wait continues in the background. Do nothing and end your turn — completion will be reported automatically with exit status and output tail. Do NOT poll bash_output({{ bash_id: \"{id}\" }}) for it. When you are waiting for a pattern in a command's output, launch it with monitor({{ command, filter }}) instead so matching lines arrive as events. Use kill_bash({{ bash_id: \"{id}\" }}) to stop this session.",wait.seconds)} else {format!("Continue other work; completion will be reported automatically with exit status and output tail. Use bash_output({{ bash_id: \"{id}\" }}) only to peek at new output. monitor cannot attach to this session; use it for future event-driven launches. Use kill_bash({{ bash_id: \"{id}\" }}) to stop this session.")};
    let mut result=text_result(format!("Command is still running; auto-detached to background with ID: {id} ({timeout_note}).\n\nPartial output:\n{}\n\n{guidance}",if partial.is_empty() {"(no output yet)"} else {&partial}));
    let mut details=json!({"bash_id":id,"background":true,"auto_detached":true,"status":"running"});
    if sleep_wait.is_some() {details["sleep_wait"]=json!(true);}
    if delta.dropped_chars>0 {details["droppedChars"]=json!(delta.dropped_chars);}
    result.details=details.as_object().cloned();Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    struct SessionContext(std::path::PathBuf);
    impl maho_tools::definition::ToolSessionManager for SessionContext {
        fn session_id(&self)->&str {"terminal-session"}
        fn session_file(&self)->Option<&std::path::Path> {Some(std::path::Path::new("/tmp/session.jsonl"))}
    }
    impl maho_tools::definition::ToolContext for SessionContext {
        fn cwd(&self)->&std::path::Path {&self.0}
        fn model(&self)->Option<&maho_ai::model::Model> {None}
        fn thinking_level(&self)->Option<maho_ai::types::ThinkingLevel> {Some(maho_ai::types::ThinkingLevel::High)}
        fn session_manager(&self)->&dyn maho_tools::definition::ToolSessionManager {self}
        fn goal_store_file(&self)->Option<&std::path::Path> {Some(std::path::Path::new("/tmp/goal.json"))}
    }
    #[tokio::test]
    async fn huge_foreground_output_has_model_only_truncation_notice() {
        let manager=Arc::new(Mutex::new(TerminalManager::default()));
        let result=execute_bash(manager.clone(),ToolCall {id:"huge",params:json!({"command":"printf '%0204800d' 0","timeout":5}),signal:Default::default(),on_update:None,context:None}).await.unwrap();
        assert!(result.content.iter().map(|part|part.text.len()).sum::<usize>()<crate::output_format::TERMINAL_TOOL_MAX_BYTES*2);
        let marker=result.content.last().unwrap();assert_eq!(marker.audience,Some(maho_ai::types::TextAudience::Model));assert!(marker.text.contains("earlier output dropped"));assert!(!result.content.iter().filter(|part|part.audience.is_none()).any(|part|part.text.contains("earlier output dropped")));manager.lock().unwrap().teardown().unwrap();
    }
    #[tokio::test]
    async fn subscribed_progress_reports_output_before_child_is_released() {
        let manager=Arc::new(Mutex::new(TerminalManager::default()));let running=manager.clone();
        let (sender,mut updates)=tokio::sync::mpsc::unbounded_channel();
        let task=tokio::spawn(async move {execute_bash(running,ToolCall {id:"progress",params:json!({"command":"stty -echo; read start; printf 'progress-ready\\n'; read value; printf '%s' \"$value\"","timeout":5}),signal:Default::default(),on_update:Some(Arc::new(move |update| {sender.send(update).unwrap();Ok(())})),context:None}).await});
        tokio::time::timeout(Duration::from_secs(5),async {
            assert!(updates.recv().await.unwrap().content.is_empty());
            manager.lock().unwrap().get("bash_1").unwrap().write(b"begin\n").unwrap();
            let update=updates.recv().await.unwrap();assert!(matches!(&update.content[0],maho_tools::definition::ToolContent::Text {text,..} if text.contains("progress-ready")));assert!(update.details.unwrap()["progress"]["startedAt"].is_number());
            manager.lock().unwrap().get("bash_1").unwrap().write(b"released\n").unwrap();let result=task.await.unwrap().unwrap();assert!(result.content[0].text.contains("released"));
        }).await.unwrap();manager.lock().unwrap().teardown().unwrap();
    }
    #[tokio::test]
    async fn session_context_metadata_reaches_real_pty_children() {
        let dir=tempfile::tempdir().unwrap();let context=SessionContext(dir.path().to_path_buf());let manager=Arc::new(Mutex::new(TerminalManager::default()));
        let result=execute_bash(manager.clone(),ToolCall {id:"env",params:json!({"command":"printf '%s|%s|%s|%s|%s' \"$PI_SESSION_ID\" \"$PI_SESSION_CWD\" \"$PI_SESSION_FILE\" \"$PI_GOAL_STORE_FILE\" \"$PI_REASONING_LEVEL\"","timeout":5}),signal:Default::default(),on_update:None,context:Some(&context)}).await.unwrap();
        assert_eq!(result.content[0].text,format!("terminal-session|{}|/tmp/session.jsonl|/tmp/goal.json|high",dir.path().display()));manager.lock().unwrap().teardown().unwrap();
    }
    #[tokio::test]
    async fn foreground_window_detaches_live_read_without_killing_runtime() {
        let manager=Arc::new(Mutex::new(TerminalManager::default()));
        let result=execute_bash_with_window(manager.clone(),ToolCall {id:"detach",params:json!({"command":"stty -echo; read value","timeout":10}),signal:Default::default(),on_update:None,context:None},Some("0.001")).await.unwrap();
        assert_eq!(result.details.as_ref().unwrap()["auto_detached"],true);assert!(!manager.lock().unwrap().get("bash_1").unwrap().exited().unwrap());
        manager.lock().unwrap().teardown().unwrap();
    }
}
