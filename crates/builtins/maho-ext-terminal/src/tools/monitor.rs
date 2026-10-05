use serde_json::{Value,json};
use crate::{manager::TerminalManager,monitor_registry::{MonitorRegistry,CommandMonitor,MonitorSnapshotEntry,MonitorFireWindow,allocate_monitor_id},shared::*};
use super::context::{TerminalToolResult,text_result,error_result,resolve_terminal_id};

pub const DEFAULT_MONITOR_TIMEOUT_MS:u64=300_000;
pub const MAX_MONITOR_TIMEOUT_MS:u64=3_600_000;
pub fn monitor_schema()->Value {json!({"type":"object","properties":{"action":{"type":"string","enum":["create","rearm"]},"description":{"type":"string","minLength":1,"maxLength":200},"command":{"type":"string"},"path":{"type":"string","minLength":1},"event":{"type":"string","enum":["create","modify"]},"filter":{"type":"string"},"timeout_ms":{"type":"number","minimum":1,"maximum":MAX_MONITOR_TIMEOUT_MS},"persistent":{"type":"boolean"},"bash_id":{"type":"string"}}})}
pub fn execute_monitor(manager:&mut TerminalManager,registry:&mut MonitorRegistry,input:&Value,cwd:&std::path::Path,approved_parent:Option<&std::path::Path>)->TerminalToolResult {
    execute_configured_monitor(manager,registry,input,cwd,approved_parent,None,&crate::settings::TERMINAL_SETTINGS_DEFAULTS)
}
/// True only for the file-watch create branch; the command and rearm branches need no file approval
/// and must not consult the carrier.
fn is_file_watch(call:&maho_tools::definition::ToolCall)->bool {
    call.params.get("action").and_then(Value::as_str)!=Some("rearm") && call.params.get("path").is_some() && call.params.get("command").is_none()
}
/// Resolves the approved parent for a file watch through the agreed `ToolContext` accessor. An
/// unsupported, retired or unavailable carrier propagates as a tool error (never swallowed into
/// "no approval"), and an absent context is an error rather than a silent fallback; a successful
/// `Ok(None)` means the source attached no approval, matching upstream's `approvedParent ===
/// undefined`. The command and rearm branches return `Ok(None)` without calling the accessor.
pub fn approved_parent_for(call:&maho_tools::definition::ToolCall)->Result<Option<std::path::PathBuf>,maho_tools::definition::ToolError> {
    use maho_tools::definition::ToolError;
    if !is_file_watch(call) {return Ok(None);}
    let context=call.context.ok_or_else(||ToolError::Message("monitor file watch requires a tool context to resolve the approved parent".to_owned()))?;
    context.take_approved_monitor_parent(call.id,&call.params).map_err(|error|ToolError::Message(error.to_string()))
}
pub fn execute_configured_monitor(manager:&mut TerminalManager,registry:&mut MonitorRegistry,input:&Value,cwd:&std::path::Path,approved_parent:Option<&std::path::Path>,shell:Option<&str>,settings:&crate::settings::ResolvedTerminalSettings)->TerminalToolResult {
    if input.get("action").and_then(Value::as_str)==Some("rearm") {
        let id=input.get("bash_id").and_then(Value::as_str).filter(|id|!id.is_empty());
        if let Some(id)=id {
            let id=resolve_terminal_id(manager,id);
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
        let registration=if persistent {registry.register_persistent_file_with_identity(description,&cwd.join(path),event,approved_parent)} else {registry.register_file_with_identity(description,&cwd.join(path),event,timeout,None,approved_parent)};
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
    let mut options=match super::spawn::command_options(command,Some(cwd),shell) {Ok(options)=>options,Err(error)=>return error_result(error.to_string())};
    options=options.size(settings.default_cols as u16,settings.default_rows as u16);
    if !persistent {options=options.timeout(std::time::Duration::from_millis(timeout));}
    let monitor_id=match allocate_monitor_id() {Ok(id)=>id,Err(error)=>return error_result(error.to_string())};
    let id=match manager.create(command,options) {Ok(id)=>id,Err(error)=>return error_result(error.to_string())};
    let record=CommandMonitor::new(MonitorSnapshotEntry {id:id.clone(),monitor_id:Some(monitor_id.clone()),description:description.to_owned(),command:Some(command.to_owned()),filter:filter.map(str::to_owned),persistent:Some(persistent),deadline_ms:(!persistent).then_some(now+timeout as f64),expires_at:persistent.then_some(now+DURABLE_MONITOR_EXPIRY_MS as f64),fire_window:persistent.then_some(MonitorFireWindow {start_ms:now,count:0}),..Default::default()},filter_regex);
    if let Err(error)=registry.register(manager.get(&id).expect("created runtime"),record) {return error_result(error.to_string());}
    manager.bind_monitor_id(&monitor_id,&id);
    let mut result=text_result(format!("Monitor started with ID: {monitor_id}"));result.details=json!({"monitor_id":monitor_id,"bash_id":id,"monitor":true}).as_object().cloned();result
}

pub async fn execute_monitor_recorded(manager:&mut TerminalManager,registry:&mut MonitorRegistry,input:&Value,cwd:&std::path::Path,approved_parent:Option<&std::path::Path>,writer:Option<&mut crate::terminal_manifest::TerminalManifestWriter>)->TerminalToolResult {
    use crate::terminal_manifest_model::{MonitorRegistration,MonitorSpec,FileEvent};
    let Some(writer)=writer else {return execute_configured_monitor(manager,registry,input,cwd,approved_parent,None,&crate::settings::TERMINAL_SETTINGS_DEFAULTS);};
    let persistent=input.get("persistent").and_then(Value::as_bool)==Some(true);
    if input.get("action").and_then(Value::as_str)!=Some("rearm")&&persistent&&writer.durable_count()>=MAX_DURABLE_MONITORS {
        return error_result(format!("Cannot start another persistent monitor: this session already holds {MAX_DURABLE_MONITORS} durable monitors (the maximum). Stop one with kill_bash first."));
    }
    let result=execute_configured_monitor(manager,registry,input,cwd,approved_parent,None,&crate::settings::TERMINAL_SETTINGS_DEFAULTS);
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("epoch").as_secs_f64()*1000.0;
    if result.is_error.is_none()&&input.get("action").and_then(Value::as_str)!=Some("rearm")&&let Some(details)=&result.details&&let Some(monitor_id)=details.get("monitor_id").and_then(Value::as_str) {
        let description=input["description"].as_str().expect("registered description").to_owned();
        let spec=if let Some(command)=input.get("command").and_then(Value::as_str) {MonitorSpec::Command {description,command:command.to_owned(),filter:input.get("filter").and_then(Value::as_str).map(str::to_owned),cwd:Some(cwd.to_string_lossy().into_owned()),persistent}} else {MonitorSpec::File {description,path:input["path"].as_str().expect("registered path").to_owned(),event:if input.get("event").and_then(Value::as_str)==Some("modify") {FileEvent::Modify} else {FileEvent::Create},timeout_ms:input.get("timeout_ms").and_then(Value::as_f64).unwrap_or(DEFAULT_MONITOR_TIMEOUT_MS as f64),cwd:cwd.to_string_lossy().into_owned(),approved_parent:approved_parent.map(|parent|parent.to_string_lossy().into_owned()),persistent}};
        writer.record_register(MonitorRegistration {monitor_id:monitor_id.to_owned(),spec},now).await;
        if let Some(id)=details.get("bash_id").and_then(Value::as_str)&&let Some(checkpoint)=registry.file_checkpoint(id) {writer.schedule_checkpoint(monitor_id,checkpoint);}
    }
    if let Err(error)=writer.observe_monitor_state(&registry.snapshot(),now).await {return error_result(error);}
    result
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[tokio::test]
    async fn configured_file_monitor_uses_the_approved_parent_and_refuses_a_mismatch() {
        let dir=tempfile::tempdir().unwrap();let mut manager=TerminalManager::default();let mut registry=MonitorRegistry::new(|_|{});
        let path=dir.path().join("watch");std::fs::write(&path,b"x").unwrap();
        let approved=std::fs::canonicalize(dir.path()).unwrap();
        let ok=execute_configured_monitor(&mut manager,&mut registry,&json!({"description":"approved","path":"watch","event":"modify"}),dir.path(),Some(&approved),None,&crate::settings::TERMINAL_SETTINGS_DEFAULTS);
        assert!(ok.is_error.is_none(),"an approved parent registers the watch: {:?}",ok.content);
        let before=registry.snapshot();
        let other=tempfile::tempdir().unwrap();
        let mismatch=execute_configured_monitor(&mut manager,&mut registry,&json!({"description":"retarget","path":"watch","event":"modify"}),dir.path(),Some(other.path()),None,&crate::settings::TERMINAL_SETTINGS_DEFAULTS);
        assert_eq!(mismatch.is_error,Some(true),"a parent that does not match the approval is refused");
        assert_eq!(registry.snapshot(),before,"a refused registration preserves the existing watch");
        registry.dispose();manager.teardown().unwrap();
    }
    struct Session;
    impl maho_tools::definition::ToolSessionManager for Session {fn session_id(&self)->&str{"s"} fn session_file(&self)->Option<&std::path::Path>{None}}
    pub(crate) struct StubContext {cwd:std::path::PathBuf,session:Session,approval:Result<Option<std::path::PathBuf>,String>,called:std::sync::Arc<std::sync::atomic::AtomicBool>}
    impl maho_tools::definition::ToolContext for StubContext {
        fn cwd(&self)->&std::path::Path {&self.cwd}
        fn model(&self)->Option<&maho_ai::model::Model> {None}
        fn thinking_level(&self)->Option<maho_ai::types::ThinkingLevel> {None}
        fn session_manager(&self)->&dyn maho_tools::definition::ToolSessionManager {&self.session}
        fn goal_store_file(&self)->Option<&std::path::Path> {None}
        fn take_approved_monitor_parent(&self,_id:&str,_input:&serde_json::Value)->Result<Option<std::path::PathBuf>,maho_tools::definition::ToolError> {
            self.called.store(true,std::sync::atomic::Ordering::SeqCst);
            self.approval.clone().map_err(maho_tools::definition::ToolError::Message)
        }
    }
    pub(crate) fn stub(approval:Result<Option<std::path::PathBuf>,String>)->StubContext {StubContext {cwd:"/tmp".into(),session:Session,approval,called:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))}}
    #[test]
    fn an_erroring_accessor_propagates_and_registers_nothing() {
        let context=stub(Err("monitor admission identity is not supported by this extension context".into()));
        let call=maho_tools::definition::ToolCall {id:"c1",params:json!({"description":"watch","path":"file","event":"modify"}),signal:Default::default(),on_update:None,context:Some(&context)};
        let error=approved_parent_for(&call).expect_err("an erroring carrier must propagate");
        assert!(error.to_string().contains("not supported"),"{error}");
        assert!(context.called.load(std::sync::atomic::Ordering::SeqCst),"the file branch consulted the carrier");
    }
    #[test]
    fn command_and_rearm_branches_never_consult_the_carrier() {
        let context=stub(Err("must not be called".into()));
        for params in [json!({"description":"cmd","command":"read value"}),json!({"action":"rearm","bash_id":"watch_1"})] {
            let call=maho_tools::definition::ToolCall {id:"c1",params,signal:Default::default(),on_update:None,context:Some(&context)};
            assert_eq!(approved_parent_for(&call).unwrap(),None);
        }
        assert!(!context.called.load(std::sync::atomic::Ordering::SeqCst),"the accessor is never consulted without a file watch");
    }
    #[test]
    fn an_absent_context_is_an_error_for_a_file_watch() {
        let call=maho_tools::definition::ToolCall {id:"c1",params:json!({"description":"watch","path":"file"}),signal:Default::default(),on_update:None,context:None};
        assert!(approved_parent_for(&call).is_err(),"no blanket fallback when the context is absent");
    }
    #[tokio::test]
    async fn recorded_file_monitor_preserves_approved_parent() {
        let dir=tempfile::tempdir().unwrap();let approved=std::fs::canonicalize(dir.path()).unwrap();let mut manager=TerminalManager::default();let mut registry=MonitorRegistry::new(|_|{});let mut writer=crate::terminal_manifest::TerminalManifestWriter::new(dir.path(),"s");
        let result=execute_monitor_recorded(&mut manager,&mut registry,&json!({"description":"approved","path":"watch","persistent":true}),dir.path(),Some(&approved),Some(&mut writer)).await;
        assert!(result.is_error.is_none());let saved=writer.store.read().await.unwrap().unwrap();let manifest=crate::restore::parse_terminal_manifest(&saved,"s").unwrap();assert_eq!(manifest.monitors.len(),1);assert_eq!(manifest.monitors[0].approved_parent.as_deref(),Some(approved.to_str().unwrap()));
        registry.dispose();manager.teardown().unwrap();
    }
    #[tokio::test]
    async fn live_command_snapshot_records_registration_epoch() {
        let mut manager=TerminalManager::default();let mut registry=MonitorRegistry::new(|_|{});
        let before=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()*1000.0;
        let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"live","command":"read value"}),std::path::Path::new("/tmp"),None);assert!(result.is_error.is_none());
        let after=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()*1000.0;
        let snapshot=registry.snapshot();assert_eq!(snapshot.len(),1);assert!(snapshot[0].started_at_ms>=before&&snapshot[0].started_at_ms<=after);let deadline=snapshot[0].deadline_ms.unwrap();assert!(deadline>=before+DEFAULT_MONITOR_TIMEOUT_MS as f64&&deadline<=after+DEFAULT_MONITOR_TIMEOUT_MS as f64);
        registry.dispose();manager.teardown().unwrap();
    }
    #[tokio::test]
    async fn configured_monitor_geometry_reaches_real_shell() {
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});let mut manager=TerminalManager::default();
        let mut settings=crate::settings::TERMINAL_SETTINGS_DEFAULTS;settings.default_cols=91.0;settings.default_rows=33.0;
        let result=execute_configured_monitor(&mut manager,&mut registry,&json!({"description":"geometry","command":"stty size","filter":"^33 91$"}),std::path::Path::new("/tmp"),None,Some("/bin/bash"),&settings);assert!(result.is_error.is_none());
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Line {line,..}) if line=="33 91"));assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Summary {..})));}).await.unwrap();registry.dispose();manager.teardown().unwrap();
    }
    #[tokio::test]
    async fn durable_admission_rejects_before_file_registration() {
        let dir=tempfile::tempdir().unwrap();let mut manager=TerminalManager::default();let mut registry=MonitorRegistry::new(|_|{});let mut writer=crate::terminal_manifest::TerminalManifestWriter::new(dir.path(),"s");
        for index in 0..MAX_DURABLE_MONITORS {let result=execute_monitor_recorded(&mut manager,&mut registry,&json!({"description":"watch","path":format!("watch-{index}"),"persistent":true}),dir.path(),None,Some(&mut writer)).await;assert!(result.is_error.is_none());}
        let before=registry.snapshot().len();let result=execute_monitor_recorded(&mut manager,&mut registry,&json!({"description":"rejected","path":"extra","persistent":true}),dir.path(),None,Some(&mut writer)).await;
        assert_eq!(result.is_error,Some(true));assert_eq!(registry.snapshot().len(),before);assert_eq!(writer.durable_count(),MAX_DURABLE_MONITORS);assert_eq!(manager.active_size().unwrap(),MAX_DURABLE_MONITORS);
        registry.dispose();assert_eq!(manager.active_size().unwrap(),0);
    }
    #[tokio::test]
    async fn persistent_file_has_expiry_without_ephemeral_deadline() {
        let dir=tempfile::tempdir().unwrap();let mut manager=TerminalManager::new(1);let mut registry=MonitorRegistry::new(|_|{});
        let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"durable","path":"watched","persistent":true,"timeout_ms":1}),dir.path(),None);assert!(result.is_error.is_none());
        let snapshot=registry.snapshot();assert_eq!(snapshot[0].persistent,Some(true));assert!(snapshot[0].deadline_ms.is_none());assert!(snapshot[0].expires_at.is_some());assert!(registry.stop_file("watch_1"));assert_eq!(manager.active_size().unwrap(),0);
    }
    #[tokio::test]
    async fn file_watch_holds_shared_capacity_until_killed() {
        let dir=tempfile::tempdir().unwrap();let mut manager=TerminalManager::new(1);let mut registry=MonitorRegistry::new(|_|{});
        let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"file","path":"watched"}),dir.path(),None);assert!(result.is_error.is_none());assert_eq!(manager.active_size().unwrap(),1);
        let blocked=execute_monitor(&mut manager,&mut registry,&json!({"description":"second","command":"true"}),dir.path(),None);assert_eq!(blocked.is_error,Some(true));assert_eq!(manager.size(),0);
        assert!(registry.stop_file("watch_1"));assert_eq!(manager.active_size().unwrap(),0);
    }
    #[tokio::test]
    async fn tool_spawns_and_delivers_native_monitor_completion() {
        let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let mut registry=MonitorRegistry::new(move |event| {sender.send(event).unwrap();});let mut manager=TerminalManager::default();
        let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"ready","command":"stty -echo; printf 'ready\\n'","filter":"^ready$"}),std::path::Path::new("/tmp"),None);
        assert!(result.is_error.is_none());let id=result.details.as_ref().unwrap()["monitor_id"].as_str().unwrap();assert_eq!(manager.resolve_id(id),Some("bash_1".to_owned()));
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Line {line,..}) if line=="ready"));assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Summary {summary,..}) if summary=="watcher completed (exit code 0)"));}).await.unwrap();manager.teardown().unwrap();
    }
    #[test] fn schema_is_flat_and_invalid_branch_does_not_spawn() {assert!(monitor_schema().get("properties").is_some());let mut manager=TerminalManager::default();let mut registry=MonitorRegistry::new(|_|{});let result=execute_monitor(&mut manager,&mut registry,&json!({"description":"both","command":"true","path":"file"}),std::path::Path::new("/tmp"),None);assert_eq!(result.is_error,Some(true));assert_eq!(manager.size(),0);}
}
