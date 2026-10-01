use crate::manager::TerminalManager;
use super::context::{TerminalToolResult,text_result,error_result};
pub fn execute_kill_bash(manager:&mut TerminalManager,id:Option<&str>,all:bool)->TerminalToolResult {
    if all {let count=manager.size();return match manager.teardown() {Ok(())=>text_result(format!("Killed {count} session(s).")),Err(error)=>error_result(error.to_string())};}
    let Some(id)=id.filter(|id|!id.is_empty()) else {return error_result("Provide `bash_id` or set `all:true`.");};let resolved=manager.resolve_id(id).unwrap_or_else(||id.to_owned());
    match manager.stop(&resolved) {Ok(true)=>text_result(format!("Killed {id}.")),Ok(false)=>error_result(format!("No terminal session found with id: {id}")),Err(error)=>error_result(error.to_string())}
}
