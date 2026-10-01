use std::sync::{Arc,Mutex};
use std::time::Duration;
use maho_pty::{PtySessionOptions,PtyExit};
use maho_tools::definition::ToolCall;
use serde_json::{Value,json};
use crate::manager::TerminalManager;
use crate::shared::{DEFAULT_COLS,DEFAULT_ROWS,FOREGROUND_ENV_OVERRIDES};
use crate::output_format::format_terminal_tool_output;
use super::context::{TerminalToolResult,text_result,error_result,noticed_result};
use super::spawn::describe_exit;
async fn wait_exit(mut receiver:tokio::sync::watch::Receiver<Option<Result<PtyExit,String>>>)->Result<PtyExit,String> {loop {if let Some(exit)=receiver.borrow_and_update().clone() {return exit;}receiver.changed().await.map_err(|error|error.to_string())?;}}
pub async fn execute_bash(manager:Arc<Mutex<TerminalManager>>,call:ToolCall<'_>)->Result<TerminalToolResult,String> {
    let command=call.params.get("command").and_then(Value::as_str).ok_or("command must be a string")?;
    let background=call.params.get("run_in_background").and_then(Value::as_bool).unwrap_or(false);
    if !background&&call.signal.is_aborted() {return Ok(error_result("Command aborted"));}
    let dimension=|key:&str,fallback:u16|call.params.get(key).and_then(Value::as_f64).filter(|value|value.is_finite()&&*value>=1.0).map_or(fallback,|value|value.trunc() as u16);
    let mut options=PtySessionOptions::new("/bin/sh").arg("-c").arg(command).size(dimension("cols",DEFAULT_COLS),dimension("rows",DEFAULT_ROWS));
    if let Some(context)=call.context {options=options.cwd(context.cwd()).env("PI_SESSION_ID",context.session_manager().session_id());if let Some(path)=context.session_manager().session_file() {options=options.env("PI_SESSION_FILE",path.to_string_lossy());}}
    if !background {for (key,value) in FOREGROUND_ENV_OVERRIDES {options=options.env(*key,*value);}if let Some(timeout)=call.params.get("timeout").and_then(Value::as_f64) {options=options.timeout(Duration::try_from_secs_f64(timeout).map_err(|error|error.to_string())?);}}
    let (id,exit)={let mut manager=manager.lock().map_err(|_|"terminal manager state poisoned")?;let id=manager.create(command,options).map_err(|error|error.to_string())?;let runtime=manager.get(&id).ok_or("created terminal session missing")?;(id,runtime.subscribe_exit())};
    if background {tokio::select! {_=wait_exit(exit)=>{},_=tokio::time::sleep(Duration::from_millis(250))=>{}}let mut manager=manager.lock().map_err(|_|"terminal manager state poisoned")?;let early=format_terminal_tool_output(&manager.get(&id).ok_or("terminal session missing")?.read_delta().map_err(|error|error.to_string())?.text).text;let mut result=text_result(format!("Command running in background with ID: {id}{}",if early.is_empty() {String::new()} else {format!("\n\n{early}")}));result.details=Some(json!({"bash_id":id,"background":true}).as_object().ok_or("background details missing")?.clone());return Ok(result);}
    let outcome=match super::foreground_detach::foreground_outcome(exit,&call.signal,Duration::from_secs(60)).await? {
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
    let mut result=text_result(format!("Command is still running; auto-detached to background with ID: {id}.\n\nPartial output:\n{}",if formatted.text.is_empty() {"(no output yet)"} else {&formatted.text}));result.details=Some(json!({"bash_id":id,"background":true,"auto_detached":true,"status":"running"}).as_object().ok_or("detach details missing")?.clone());Ok(result)
}
