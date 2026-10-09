use std::path::Path;
use lsp_core::{abort::AbortController,request_context::{LspRequestCapabilities,LspRequestContext}};
use lsp_daemon::daemon_client::{call_tool_via_daemon,CallToolOptions,ToolExecutionResult};
use maho_ext_api::{AbortSignal,ToolError};
use serde_json::{Map,Value};
pub fn resolve_request_home(home:Option<&str>,fallback:impl FnOnce()->std::path::PathBuf)->std::path::PathBuf {
    home.map(str::trim).filter(|home|!home.is_empty()).map(std::path::PathBuf::from).unwrap_or_else(fallback)
}
pub fn to_daemon_tool_name(name:&str) -> Result<&'static str,ToolError> {
    match name {
        "lsp_diagnostics"=>Ok("diagnostics"),"lsp_goto_definition"=>Ok("goto_definition"),"lsp_find_references"=>Ok("find_references"),"lsp_symbols"=>Ok("symbols"),"lsp_prepare_rename"=>Ok("prepare_rename"),"lsp_rename"=>Ok("rename"),"format"=>Ok("format"),
        _=>Err(ToolError::Message(format!("Unsupported Senpi LSP daemon tool: {name}"))),
    }
}
pub fn current_senpi_request_context(cwd:&Path,home:&Path) -> std::io::Result<LspRequestContext> {
    let cwd=if cwd.exists() {std::fs::canonicalize(cwd)?} else {cwd.to_path_buf()};
    Ok(LspRequestContext {cwd:cwd.to_string_lossy().into_owned(),project_config_paths:crate::config_paths::project_lsp_config_paths(&cwd).into_iter().map(|path|path.to_string_lossy().into_owned()).collect(),user_config_path:crate::config_paths::user_lsp_config_path(home,Path::exists).to_string_lossy().into_owned(),install_decisions_path:crate::config_paths::lsp_install_decisions_path(home,Path::exists).to_string_lossy().into_owned(),capabilities:LspRequestCapabilities {install_decision_tool:false}})
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
    #[test] fn whitespace_home_uses_actual_home_and_branded_paths() {let t=tempfile::tempdir().unwrap();let home=resolve_request_home(Some("   "),||t.path().to_path_buf());let context=current_senpi_request_context(t.path(),&home).unwrap();assert_eq!(context.user_config_path,t.path().join(".maho/agent/lsp-client.json").to_string_lossy());assert_eq!(context.install_decisions_path,t.path().join(".maho/agent/lsp-install-decisions.json").to_string_lossy());assert_eq!(resolve_request_home(Some(" /custom "),||panic!("explicit home")),std::path::PathBuf::from("/custom"));}
    #[test] fn maps_all_six_tools() {for (name,expected) in [("lsp_diagnostics","diagnostics"),("lsp_goto_definition","goto_definition"),("lsp_find_references","find_references"),("lsp_symbols","symbols"),("lsp_prepare_rename","prepare_rename"),("lsp_rename","rename")] {assert_eq!(to_daemon_tool_name(name).unwrap(),expected);}}
    #[test] fn unsupported_tool_rejected() {assert!(to_daemon_tool_name("lsp_install_decision").is_err());}
    #[test] fn context_uses_branded_config_and_disables_install() {let t=tempfile::tempdir().unwrap();let c=current_senpi_request_context(t.path(),t.path()).unwrap();assert_eq!(c.project_config_paths,vec![t.path().join(".omo/lsp-client.json").to_string_lossy().into_owned(),t.path().join(".pi/lsp-client.json").to_string_lossy().into_owned()]);assert_eq!(c.user_config_path,t.path().join(".maho/agent/lsp-client.json").to_string_lossy());assert_eq!(c.install_decisions_path,t.path().join(".maho/agent/lsp-install-decisions.json").to_string_lossy());assert!(!c.capabilities.install_decision_tool);}
}
