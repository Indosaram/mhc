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
pub fn ensure_packaged_daemon_cli(env:&crate::daemon_runtime::Env,packaged:&crate::daemon_runtime::SenpiDaemonRuntime) -> Result<(),ToolError> {
    let runtime=crate::daemon_runtime::resolve_senpi_daemon_runtime(env,packaged).map_err(|error|ToolError::Message(error.to_string()))?;
    if !runtime.cli_path.is_file() {return Err(ToolError::Message(format!("Senpi packaged LSP daemon CLI is missing: {}",runtime.cli_path.display())));}
    Ok(())
}
pub async fn call_packaged_daemon_tool(name:&str,args:Map<String,Value>,context:LspRequestContext,signal:AbortSignal) -> Result<ToolExecutionResult,ToolError> {
    ensure_packaged_daemon_cli(&lsp_daemon::runtime_contract::process_env(),&lsp_daemon::paths::packaged_runtime_defaults())?;
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
    #[test] fn whitespace_home_uses_actual_home_and_branded_paths() {let t=tempfile::tempdir().unwrap();let home=resolve_request_home(Some("   "),||t.path().to_path_buf());let context=current_senpi_request_context(t.path(),&home).unwrap();assert_eq!(context.user_config_path,t.path().join(".omo").join("lsp-client.json").to_string_lossy());assert_eq!(context.install_decisions_path,t.path().join(".omo").join("lsp-install-decisions.json").to_string_lossy());assert_eq!(resolve_request_home(Some(" /custom "),||panic!("explicit home")),std::path::PathBuf::from("/custom"));}
    #[test] fn maps_all_six_tools() {for (name,expected) in [("lsp_diagnostics","diagnostics"),("lsp_goto_definition","goto_definition"),("lsp_find_references","find_references"),("lsp_symbols","symbols"),("lsp_prepare_rename","prepare_rename"),("lsp_rename","rename")] {assert_eq!(to_daemon_tool_name(name).unwrap(),expected);}}
    #[test] fn missing_packaged_cli_fails_immediately() {let t=tempfile::tempdir().unwrap();let cli=t.path().join("lsp-daemon");let packaged=crate::daemon_runtime::SenpiDaemonRuntime {cli_path:cli.clone(),version:"0.1.0".into()};let env=crate::daemon_runtime::Env::new();let error=ensure_packaged_daemon_cli(&env,&packaged).unwrap_err();assert_eq!(error.to_string(),format!("Senpi packaged LSP daemon CLI is missing: {}",cli.display()));std::fs::write(&cli,"").unwrap();assert!(ensure_packaged_daemon_cli(&env,&packaged).is_ok());}
    #[test] fn unsupported_tool_rejected() {assert!(to_daemon_tool_name("lsp_install_decision").is_err());}
    #[test] fn context_uses_branded_config_and_disables_install() {let t=tempfile::tempdir().unwrap();let root=t.path().canonicalize().unwrap();let c=current_senpi_request_context(&root,&root).unwrap();assert_eq!(c.project_config_paths,vec![root.join(".omo").join("lsp-client.json").to_string_lossy().into_owned(),root.join(".pi").join("lsp-client.json").to_string_lossy().into_owned()]);assert_eq!(c.user_config_path,root.join(".omo").join("lsp-client.json").to_string_lossy());assert_eq!(c.install_decisions_path,root.join(".omo").join("lsp-install-decisions.json").to_string_lossy());assert!(!c.capabilities.install_decision_tool);}
}
