use std::path::Path;
use lsp_core::{abort::AbortController,request_context::{LspRequestCapabilities,LspRequestContext}};
use lsp_daemon::daemon_client::{call_tool_via_daemon,CallToolOptions,ToolExecutionResult};
use maho_ext_api::{AbortSignal,ToolError};
use serde_json::{Map,Value};
pub fn to_daemon_tool_name(name:&str) -> Result<&'static str,ToolError> {
    match name {
        "lsp_diagnostics"=>Ok("diagnostics"),"lsp_goto_definition"=>Ok("goto_definition"),"lsp_find_references"=>Ok("find_references"),"lsp_symbols"=>Ok("symbols"),"lsp_prepare_rename"=>Ok("prepare_rename"),"lsp_rename"=>Ok("rename"),
        _=>Err(ToolError::Message(format!("Unsupported Senpi LSP daemon tool: {name}"))),
    }
}
pub fn current_senpi_request_context(cwd:&Path,home:&Path) -> std::io::Result<LspRequestContext> {
    let cwd=if cwd.exists() {std::fs::canonicalize(cwd)?} else {cwd.to_path_buf()};
    Ok(LspRequestContext {cwd:cwd.to_string_lossy().into_owned(),project_config_paths:vec![cwd.join(".pi/lsp-client.json").to_string_lossy().into_owned()],user_config_path:home.join(".pi/lsp-client.json").to_string_lossy().into_owned(),install_decisions_path:home.join(".pi/lsp-install-decisions.json").to_string_lossy().into_owned(),capabilities:LspRequestCapabilities {install_decision_tool:false}})
}
pub async fn call_packaged_daemon_tool(name:&str,args:Map<String,Value>,context:LspRequestContext,signal:AbortSignal) -> Result<ToolExecutionResult,ToolError> {
    let name=to_daemon_tool_name(name)?;
    let controller=AbortController::new();
    if signal.is_aborted() {controller.abort();}
    let call=call_tool_via_daemon(name,args,CallToolOptions {context:Some(context),signal:Some(controller.signal()),..Default::default()});
    tokio::pin!(call);
    let result=tokio::select! {
        result=&mut call=>result,
        ()=signal.cancelled()=> {controller.abort(); call.await}
    };
    result.map_err(|e|ToolError::Message(e.to_string()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn maps_all_six_tools() {for (name,expected) in [("lsp_diagnostics","diagnostics"),("lsp_goto_definition","goto_definition"),("lsp_find_references","find_references"),("lsp_symbols","symbols"),("lsp_prepare_rename","prepare_rename"),("lsp_rename","rename")] {assert_eq!(to_daemon_tool_name(name).unwrap(),expected);}}
    #[test] fn unsupported_tool_rejected() {assert!(to_daemon_tool_name("lsp_install_decision").is_err());}
    #[test] fn context_uses_pi_config_and_disables_install() {let t=tempfile::tempdir().unwrap();let c=current_senpi_request_context(t.path(),Path::new("/home/fixture")).unwrap();assert_eq!(c.project_config_paths,vec![t.path().join(".pi/lsp-client.json").to_string_lossy().into_owned()]);assert_eq!(c.user_config_path,"/home/fixture/.pi/lsp-client.json");assert_eq!(c.install_decisions_path,"/home/fixture/.pi/lsp-install-decisions.json");assert!(!c.capabilities.install_decision_tool);}
}
