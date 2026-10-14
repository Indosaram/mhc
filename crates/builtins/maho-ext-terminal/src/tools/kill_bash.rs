use crate::manager::TerminalManager;
use super::context::{TerminalToolResult,text_result,error_result,resolve_terminal_id};
pub fn execute_kill_bash(manager:&mut TerminalManager,id:Option<&str>,all:bool)->TerminalToolResult {
    if all {let count=manager.size();return match manager.teardown() {Ok(())=>text_result(format!("Killed {count} session(s).")),Err(error)=>error_result(error.to_string())};}
    let Some(id)=id.filter(|id|!id.is_empty()) else {return error_result("Provide `bash_id` or set `all:true`.");};let resolved=resolve_terminal_id(manager,id);
    match manager.stop(&resolved) {Ok(true)=>text_result(format!("Killed {id}.")),Ok(false)=>error_result(format!("No terminal session found with id: {id}")),Err(error)=>error_result(error.to_string())}
}
/// `execute_kill_bash` for a shared manager, as kill-bash.ts awaits it: the stop/teardown exit
/// graces are awaited with the manager lock released, never blocking the async runtime thread.
pub async fn execute_kill_bash_shared(manager:&std::sync::Arc<std::sync::Mutex<TerminalManager>>,id:Option<&str>,all:bool)->TerminalToolResult {
    let poisoned=||error_result("terminal manager state poisoned");
    if all {
        let Ok(count)=manager.lock().map(|manager|manager.size()) else {return poisoned();};
        return match crate::manager::teardown_shared(manager).await {Ok(())=>text_result(format!("Killed {count} session(s).")),Err(error)=>error_result(error.to_string())};
    }
    let Some(id)=id.filter(|id|!id.is_empty()) else {return error_result("Provide `bash_id` or set `all:true`.");};
    let Ok(resolved)=manager.lock().map(|manager|resolve_terminal_id(&manager,id)) else {return poisoned();};
    match crate::manager::stop_shared(manager,&resolved).await {Ok(true)=>text_result(format!("Killed {id}.")),Ok(false)=>error_result(format!("No terminal session found with id: {id}")),Err(error)=>error_result(error.to_string())}
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
    #[tokio::test]
    async fn shared_kill_awaits_stop_without_holding_the_manager_lock() {
        let manager=std::sync::Arc::new(std::sync::Mutex::new(TerminalManager::default()));
        let id=manager.lock().unwrap().create("read",maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg("read value")).unwrap();manager.lock().unwrap().bind_monitor_id("mon_saved",&id);
        assert_eq!(execute_kill_bash_shared(&manager,Some("missing"),false).await.is_error,Some(true));
        assert_eq!(execute_kill_bash_shared(&manager,Some("mon_saved"),false).await.is_error,None);assert!(manager.lock().unwrap().get(&id).unwrap().exited().unwrap());
        assert_eq!(execute_kill_bash_shared(&manager,None,true).await.is_error,None);assert_eq!(manager.lock().unwrap().size(),0);
    }
}
