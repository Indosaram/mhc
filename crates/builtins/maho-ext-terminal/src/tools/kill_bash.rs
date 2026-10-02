use crate::manager::TerminalManager;
use super::context::{TerminalToolResult,text_result,error_result};
pub fn execute_kill_bash(manager:&mut TerminalManager,id:Option<&str>,all:bool)->TerminalToolResult {
    if all {let count=manager.size();return match manager.teardown() {Ok(())=>text_result(format!("Killed {count} session(s).")),Err(error)=>error_result(error.to_string())};}
    let Some(id)=id.filter(|id|!id.is_empty()) else {return error_result("Provide `bash_id` or set `all:true`.");};let resolved=manager.resolve_id(id).unwrap_or_else(||id.to_owned());
    match manager.stop(&resolved) {Ok(true)=>text_result(format!("Killed {id}.")),Ok(false)=>error_result(format!("No terminal session found with id: {id}")),Err(error)=>error_result(error.to_string())}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_monitor_id_kills_live_runtime_and_all_clears_bindings() {
        let mut manager=TerminalManager::default();let id=manager.create("read",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("read value")).unwrap();manager.bind_monitor_id("mon_saved",&id);
        assert_eq!(execute_kill_bash(&mut manager,None,false).is_error,Some(true));assert_eq!(execute_kill_bash(&mut manager,Some("missing"),false).is_error,Some(true));
        assert_eq!(execute_kill_bash(&mut manager,Some("mon_saved"),false).is_error,None);assert!(manager.get(&id).unwrap().exited().unwrap());assert_eq!(manager.active_size().unwrap(),0);
        assert_eq!(execute_kill_bash(&mut manager,None,true).is_error,None);assert_eq!(manager.size(),0);assert_eq!(manager.resolve_id("mon_saved"),None);
    }
}
