use std::{collections::BTreeMap,sync::{Arc,Mutex},time::Duration,process::Stdio};
use serde_json::{Value,json};
use crate::{config_schema::{McpServerConfig,Transport},errors::{McpError,McpErrorKind},log::{McpLogger,redact_mcp_log_text}};
pub const MCP_STDIO_DIAGNOSTIC_TIMEOUT_MS:u64=5000;
const MAX_BYTES:usize=2048;
fn meaningful_lines(text:&str)->Vec<String> {text.lines().map(str::trim).filter(|line|!line.is_empty()).map(|line|redact_mcp_log_text(line).unwrap_or_else(|_|"<redaction failed>".into())).collect()}
fn bound(lines:&[String])->String {
    let mut output=String::new();
    for line in lines {let next=if output.is_empty(){line.clone()}else{format!("{output}\n{line}")};if next.len()>MAX_BYTES {break;}output=next;}
    if output.is_empty(){lines.first().map_or_else(String::new,|line|line.chars().take(MAX_BYTES).collect())}else{output}
}
fn captured(logger:&McpLogger)->Vec<String> {
    meaningful_lines(&logger.get_ring_buffer().iter().filter_map(|line|serde_json::from_str::<Value>(line).ok()).filter(|line|line.get("channel").and_then(Value::as_str)==Some("stderr")).filter_map(|line|line.get("message").and_then(Value::as_str).map(str::to_owned)).collect::<Vec<_>>().join("\n"))
}
fn connect_error(server:&str,cause:&McpError,diagnostic:Option<String>)->McpError {
    let mut message=format!("MCP server {server} failed during connect: {}",cause.message);
    if let Some(diagnostic)=diagnostic.filter(|text|!text.is_empty()){message.push('\n');message.push_str(&diagnostic);}
    let mut error=McpError::new(McpErrorKind::Connect,message);error.server_name=Some(server.into());error.phase=Some("connect".into());error.retriable=true;
    error.cause=Some(Box::new(json!({"message":cause.message,"cause":cause.cause})));error
}
pub fn diagnose_captured_mcp_connect_failure(server:&str,config:&McpServerConfig,cause:&McpError,logger:&McpLogger)->Option<McpError> {
    if config.transport!=Some(Transport::Stdio){return None;}
    let lines=captured(logger);if lines.is_empty(){None}else{Some(connect_error(server,cause,Some(bound(&lines))))}
}
pub async fn diagnose_mcp_connect_failure(server:&str,config:&McpServerConfig,env:Option<&BTreeMap<String,String>>,cause:&McpError,logger:Arc<Mutex<McpLogger>>)->McpError {
    if config.transport!=Some(Transport::Stdio){return cause.clone();}
    let lines=captured(&logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    if !lines.is_empty(){return connect_error(server,cause,Some(bound(&lines)));}
    let Some(command)=config.command.as_ref().filter(|command|!command.trim().is_empty()) else{return connect_error(server,cause,None);};
    let mut merged=BTreeMap::new();for key in ["HOME","LOGNAME","PATH","SHELL","TERM","USER"] {if let Ok(value)=std::env::var(key) && !value.starts_with("()") {merged.insert(key.to_owned(),value);}}
    if let Some(env)=env {merged.extend(env.clone());}merged.extend(config.env.clone().unwrap_or_default());
    let mut process=tokio::process::Command::new(command);process.args(config.args.as_deref().unwrap_or(&[])).env_clear().envs(&merged).stdin(Stdio::null()).kill_on_drop(true);
    if let Some(cwd)=&config.cwd {process.current_dir(cwd);}
    let diagnostic=match tokio::time::timeout(Duration::from_millis(MCP_STDIO_DIAGNOSTIC_TIMEOUT_MS),process.output()).await {
        Err(_)=>Some(format!("diagnostic rerun timed out after {MCP_STDIO_DIAGNOSTIC_TIMEOUT_MS}ms")),
        Ok(Err(error)) if error.kind()==std::io::ErrorKind::NotFound=>{
            let cwd=config.cwd.clone().unwrap_or_else(||std::env::current_dir().map_or_else(|_|String::new(),|path|path.display().to_string()));
            Some(meaningful_lines(&format!("command not found: {command}\ncwd: {cwd}\nPATH: {}\nInstall the command, add it to PATH, or configure an absolute command path in mcp.json.",merged.get("PATH").map_or("",String::as_str))).join("\n"))
        }
        Ok(Err(_))=>None,
        Ok(Ok(output))=>{let lines=meaningful_lines(&format!("{}\n{}",String::from_utf8_lossy(&output.stderr),String::from_utf8_lossy(&output.stdout)));if lines.is_empty(){None}else{Some(bound(&lines))}}
    };
    connect_error(server,cause,diagnostic)
}
