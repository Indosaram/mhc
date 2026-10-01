use serde_json::{Value,json};
use crate::{manager::TerminalManager,monitor_registry::{MonitorRegistry,CommandMonitor,MonitorSnapshotEntry,MonitorFireWindow,allocate_monitor_id},shared::*};
use super::context::{TerminalToolResult,text_result,error_result};

pub const DEFAULT_MONITOR_TIMEOUT_MS:u64=300_000;
pub const MAX_MONITOR_TIMEOUT_MS:u64=3_600_000;
pub fn monitor_schema()->Value {json!({"type":"object","properties":{"action":{"type":"string","enum":["create","rearm"]},"description":{"type":"string","minLength":1,"maxLength":200},"command":{"type":"string"},"path":{"type":"string","minLength":1},"event":{"type":"string","enum":["create","modify"]},"filter":{"type":"string"},"timeout_ms":{"type":"number","minimum":1,"maximum":MAX_MONITOR_TIMEOUT_MS},"persistent":{"type":"boolean"},"bash_id":{"type":"string"}}})}
pub fn execute_monitor(manager:&mut TerminalManager,registry:&mut MonitorRegistry,input:&Value,cwd:&std::path::Path)->TerminalToolResult {
    if input.get("action").and_then(Value::as_str)==Some("rearm") {
        let id=input.get("bash_id").and_then(Value::as_str).filter(|id|!id.is_empty());
        if let Some(id)=id {
            let id=manager.resolve_id(id).unwrap_or_else(||id.to_owned());
            if !registry.snapshot().iter().any(|record|record.id==id) {return error_result(format!("No active monitor found with id: {id}"));}
            let resumed=registry.resume(Some(std::slice::from_ref(&id)));
            return text_result(match resumed.first() {None=>format!("Monitor {id} is not paused; no action taken."),Some((_,0))=>format!("Monitor {id} re-armed."),Some((_,dropped))=>format!("Monitor {id} re-armed ({dropped} line(s) dropped while muted).")});
        }
        let resumed=registry.resume(None);if resumed.is_empty() {return text_result("No paused monitors to re-arm.");}
        let total=resumed.iter().map(|(_,dropped)|dropped).sum::<usize>();
        return text_result(if total>0 {format!("Re-armed {} paused monitor(s) ({total} line(s) dropped while muted).",resumed.len())} else {format!("Re-armed {} paused monitor(s).",resumed.len())});
    }
    let description=input.get("description").and_then(Value::as_str).filter(|value|!value.is_empty());
    let command=input.get("command").and_then(Value::as_str).filter(|value|!value.is_empty());
    let path=input.get("path").and_then(Value::as_str).filter(|value|!value.is_empty());
    if description.is_some()&&command.is_some()&&path.is_some() {return error_result("monitor accepts either command or path, not both.");}
    if let (Some(description),Some(path))=(description,path) {
        if input.get("filter").is_some() {return error_result("Native file monitors do not support filter.");}
        let persistent=input.get("persistent").and_then(Value::as_bool)==Some(true);
        let timeout=if persistent {DURABLE_MONITOR_EXPIRY_MS} else {input.get("timeout_ms").and_then(Value::as_f64).unwrap_or(DEFAULT_MONITOR_TIMEOUT_MS as f64).trunc().clamp(1.0,MAX_MONITOR_TIMEOUT_MS as f64) as u64};
        let event=if input.get("event").and_then(Value::as_str)==Some("modify") {crate::terminal_manifest_model::FileEvent::Modify} else {crate::terminal_manifest_model::FileEvent::Create};
        let reservation=match manager.reserve() {Ok(Some(reservation))=>reservation,Ok(None)=>return error_result("Cannot create file monitor: terminal capacity is already in use."),Err(error)=>return error_result(error.to_string())};
        let registration=if persistent {registry.register_persistent_file(description,&cwd.join(path),event)} else {registry.register_file(description,&cwd.join(path),event,timeout)};
        let (id,monitor_id)=match registration {Ok(ids)=>ids,Err(error)=>return error_result(error.to_string())};
        registry.reserve_file_capacity(&id,reservation);
        manager.bind_monitor_id(&monitor_id,&id);
        let mut result=text_result(format!("Monitor started with ID: {monitor_id}"));result.details=json!({"monitor_id":monitor_id,"bash_id":id,"monitor":true}).as_object().cloned();return result;
    }
    let (Some(description),Some(command))=(description,command) else {return error_result("monitor requires description and command or path to start a watcher.");};
    let filter=input.get("filter").and_then(Value::as_str);
    let filter_regex=match filter.map(fancy_regex::Regex::new).transpose() {Ok(regex)=>regex,Err(_)=>return error_result(format!("Invalid monitor filter regex: {}",filter.unwrap_or_default()))};
    let persistent=input.get("persistent").and_then(Value::as_bool)==Some(true);
    let timeout=input.get("timeout_ms").and_then(Value::as_f64).unwrap_or(DEFAULT_MONITOR_TIMEOUT_MS as f64).trunc().clamp(1.0,MAX_MONITOR_TIMEOUT_MS as f64) as u64;
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("epoch").as_secs_f64()*1000.0;
    let mut options=maho_pty::PtySessionOptions::new("/bin/sh").arg("-c").arg(command).cwd(cwd);
    if !persistent {options=options.timeout(std::time::Duration::from_millis(timeout));}
    let monitor_id=match allocate_monitor_id() {Ok(id)=>id,Err(error)=>return error_result(error.to_string())};
    let id=match manager.create(command,options) {Ok(id)=>id,Err(error)=>return error_result(error.to_string())};
    let record=CommandMonitor::new(MonitorSnapshotEntry {id:id.clone(),monitor_id:Some(monitor_id.clone()),description:description.to_owned(),command:Some(command.to_owned()),filter:filter.map(str::to_owned),persistent:Some(persistent),deadline_ms:(!persistent).then_some(now+timeout as f64),expires_at:persistent.then_some(now+DURABLE_MONITOR_EXPIRY_MS as f64),fire_window:persistent.then_some(MonitorFireWindow {start_ms:now,count:0}),..Default::default()},filter_regex);
    if let Err(error)=registry.register(manager.get(&id).expect("created runtime"),record) {return error_result(error.to_string());}
    manager.bind_monitor_id(&monitor_id,&id);
    let mut result=text_result(format!("Monitor started with ID: {monitor_id}"));result.details=json!({"monitor_id":monitor_id,"bash_id":id,"monitor":true}).as_object().cloned();result
}

pub async fn execute_monitor_recorded(manager:&mut TerminalManager,registry:&mut MonitorRegistry,input:&Value,cwd:&std::path::Path,writer:Option<&mut crate::terminal_manifest::TerminalManifestWriter>)->TerminalToolResult {
    use crate::terminal_manifest_model::{MonitorRegistration,MonitorSpec,FileEvent};
    let Some(writer)=writer else {return execute_monitor(manager,registry,input,cwd);};
    let persistent=input.get("persistent").and_then(Value::as_bool)==Some(true);
    if input.get("action").and_then(Value::as_str)!=Some("rearm")&&persistent&&writer.durable_count()>=MAX_DURABLE_MONITORS {
        return error_result(format!("Cannot start another persistent monitor: this session already holds {MAX_DURABLE_MONITORS} durable monitors (the maximum). Stop one with kill_bash first."));
    }
    let result=execute_monitor(manager,registry,input,cwd);
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("epoch").as_secs_f64()*1000.0;
    if result.is_error.is_none()&&input.get("action").and_then(Value::as_str)!=Some("rearm")&&let Some(details)=&result.details&&let Some(monitor_id)=details.get("monitor_id").and_then(Value::as_str) {
        let description=input["description"].as_str().expect("registered description").to_owned();
        let spec=if let Some(command)=input.get("command").and_then(Value::as_str) {MonitorSpec::Command {description,command:command.to_owned(),filter:input.get("filter").and_then(Value::as_str).map(str::to_owned),cwd:Some(cwd.to_string_lossy().into_owned()),persistent}} else {MonitorSpec::File {description,path:input["path"].as_str().expect("registered path").to_owned(),event:if input.get("event").and_then(Value::as_str)==Some("modify") {FileEvent::Modify} else {FileEvent::Create},timeout_ms:input.get("timeout_ms").and_then(Value::as_f64).unwrap_or(DEFAULT_MONITOR_TIMEOUT_MS as f64),cwd:cwd.to_string_lossy().into_owned(),approved_parent:None,persistent}};
        writer.record_register(MonitorRegistration {monitor_id:monitor_id.to_owned(),spec},now).await;
        if let Some(id)=details.get("bash_id").and_then(Value::as_str)&&let Some(checkpoint)=registry.file_checkpoint(id) {writer.schedule_checkpoint(monitor_id,checkpoint);}
    }
    if let Err(error)=writer.observe_monitor_state(&registry.snapshot(),now).await {return error_result(error);}
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn durable_admission_rejects_before_file_registration() {
        let dir=tempfile::tempdir().unwrap();let mut manager=TerminalManager::default();let mut registry=MonitorRegistry::new(|_|{});let mut writer=crate::terminal_manifest::TerminalManifestWriter::new(dir.path(),"s");
        for index in 0..MAX_DURABLE_MONITORS {let result=execute_monitor_recorded(&mut manager,&mut registry,&json!({"description":"watch","path":format!("watch-{index}"),"persistent":true}),dir.path(),Some(&mut writer)).await;assert!(result.is_error.is_none());}
        let before=registry.snapshot().len();let result=execute_monitor_recorded(&mut manager,&mut registry,&json!({"description":"rejected","path":"extra","persistent":true}),dir.path(),Some(&mut writer)).await;
        assert_eq!(result.is_error,Some(true));assert_eq!(registry.snapshot().len(),before);assert_eq!(writer.durable_count(),MAX_DURABLE_MONITORS);assert_eq!(manager.active_size().unwrap(),MAX_DURABLE_MONITORS);
        registry.dispose();assert_eq!(manager.active_size().unwrap(),0);
    }
    #[tokio::test]
    async fn persistent_file_has_expiry_without_ephemeral_deadline() {
        let dir=tempfile::tempdir().unwrap();let mut manager=TerminalManager::new(1);let mut registry=MonitorRegistry::new(|_|{});
        let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"durable","path":"watched","persistent":true,"timeout_ms":1}),dir.path());assert!(result.is_error.is_none());
        let snapshot=registry.snapshot();assert_eq!(snapshot[0].persistent,Some(true));assert!(snapshot[0].deadline_ms.is_none());assert!(snapshot[0].expires_at.is_some());assert!(registry.stop_file("watch_1"));assert_eq!(manager.active_size().unwrap(),0);
    }
    #[tokio::test]
    async fn file_watch_holds_shared_capacity_until_killed() {
        let dir=tempfile::tempdir().unwrap();let mut manager=TerminalManager::new(1);let mut registry=MonitorRegistry::new(|_|{});
        let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"file","path":"watched"}),dir.path());assert!(result.is_error.is_none());assert_eq!(manager.active_size().unwrap(),1);
        let blocked=execute_monitor(&mut manager,&mut registry,&json!({"description":"second","command":"true"}),dir.path());assert_eq!(blocked.is_error,Some(true));assert_eq!(manager.size(),0);
        assert!(registry.stop_file("watch_1"));assert_eq!(manager.active_size().unwrap(),0);
    }
    #[tokio::test]
    async fn tool_spawns_and_delivers_native_monitor_completion() {
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});let mut manager=TerminalManager::default();
        let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"ready","command":"stty -echo; printf 'ready\\n'","filter":"^ready$"}),std::path::Path::new("/tmp"));
        assert!(result.is_error.is_none());let id=result.details.as_ref().unwrap()["monitor_id"].as_str().unwrap();assert_eq!(manager.resolve_id(id),Some("bash_1".to_owned()));
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Line {line,..}) if line=="ready"));assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Summary {summary,..}) if summary=="watcher completed (exit code 0)"));}).await.unwrap();manager.teardown().unwrap();
    }
    #[test] fn schema_is_flat_and_invalid_branch_does_not_spawn() {assert!(monitor_schema().get("properties").is_some());let mut manager=TerminalManager::default();let mut registry=MonitorRegistry::new(|_|{});let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"both","command":"true","path":"file"}),std::path::Path::new("/tmp"));assert_eq!(result.is_error,Some(true));assert_eq!(manager.size(),0);}
}
