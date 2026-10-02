use std::sync::{Arc,Mutex};
use maho_ext_api::types::{Extension,ExtensionApi,EventKind,EventResult,ExtensionFailure};
use maho_tools::definition::{ToolDefinition,ToolResult,ToolContent,ToolError};
use serde_json::{Value,json};
use crate::manager::TerminalManager;
use crate::tools::{bash_input::{execute_bash_input,BashInputInput},bash_output::execute_bash_output_view,bash_resize::execute_bash_resize,kill_bash::execute_kill_bash,context::TerminalToolResult};
pub struct TerminalExtension;
pub fn monitor_state_payload(snapshot:&[crate::monitor_registry::MonitorSnapshotEntry])->Value {
    let monitors=snapshot.iter().map(|entry| {
        let mut value=json!({"id":entry.id,"description":entry.description,"paused":entry.paused,"startedAtMs":entry.started_at_ms,"command":entry.command,"filter":entry.filter,"deadlineMs":entry.deadline_ms,"lastFiredAtMs":entry.last_fired_at_ms});
        for (key,field) in [("command",entry.command.as_ref().map(|value|json!(value))),("filter",entry.filter.as_ref().map(|value|json!(value))),("persistent",entry.persistent.map(|value|json!(value))),("deadlineMs",entry.deadline_ms.map(|value|json!(value))),("fireCount",entry.fire_count.map(|value|json!(value))),("lastFiredAtMs",entry.last_fired_at_ms.map(|value|json!(value)))] {if let Some(field)=field {value[key]=field;}}
        value
    }).collect::<Vec<_>>();json!({"activeCount":snapshot.len(),"monitors":monitors})
}
fn bind_monitor_events(mut state:tokio::sync::watch::Receiver<Vec<crate::monitor_registry::MonitorSnapshotEntry>>,sender:Arc<ExtensionApi>)->tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let snapshot=state.borrow_and_update().clone();let payload=monitor_state_payload(&snapshot);
            sender.events.emit("terminal_monitor_state",&payload);if let Err(error)=sender.rpc_emit("terminal_monitor_state",&payload) {eprintln!("monitor state RPC delivery failed: {error}");}
            sender.events.emit("wake_source_state",&json!({"source":"terminal-monitors","activeCount":snapshot.len(),"monitors":snapshot.iter().map(|entry|json!({"id":entry.id,"description":entry.description,"startedAtMs":entry.started_at_ms})).collect::<Vec<_>>()}));
            if state.changed().await.is_err() {return;}
        }
    })
}
fn tool_result(result:TerminalToolResult)->Result<ToolResult,ToolError> {
    if result.is_error==Some(true) {return Err(ToolError::Message(result.content.into_iter().map(|part|part.text).collect::<Vec<_>>().join("\n")));}
    Ok(ToolResult {content:result.content.into_iter().map(|part|ToolContent::Text {text:part.text,audience:part.audience.map(|_|"model".to_owned())}).collect(),details:result.details.map(Value::Object)})
}
impl Extension for TerminalExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let manager=Arc::new(Mutex::new(TerminalManager::default()));
        let completion_delivery=Arc::new(Mutex::new(None));
        let terminal_notifier=Arc::new(Mutex::new(crate::notify::TerminalNotifier::default()));
        let notifier:Arc<Mutex<Option<crate::monitor_notify::MonitorNotifier>>>=Arc::new(Mutex::new(None));
        let event_notifier=notifier.clone();
        let monitors=Arc::new(Mutex::new(crate::monitor_registry::MonitorRegistry::new(move |event| {
            if let Some(notifier)=event_notifier.lock().expect("monitor notifier").as_ref() && let Err(error)=notifier.notify_event(event) {eprintln!("monitor delivery failed: {error}");}
        })));
        let status_task:Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>=Arc::new(Mutex::new(None));
        for kind in [EventKind::SessionParked,EventKind::SessionResumed] {
            let monitors=monitors.clone();
            api.on(kind,Arc::new(move |_,_| {let monitors=monitors.clone();Box::pin(async move {
                let monitors=monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?;
                if kind==EventKind::SessionParked {monitors.park();}else {monitors.unpark();}
                Ok(EventResult::None)
            })}));
        }
        let status_monitors=monitors.clone();let start_status=status_task.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,ctx| {let monitors=status_monitors.clone();let status=start_status.clone();Box::pin(async move {
            let mut status=status.lock().map_err(|_|ExtensionFailure::new("monitor status state poisoned"))?;if let Some(task)=status.take() {task.abort();}
            let receiver=monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.subscribe_state();let ui=ctx.ui.clone();
            *status=Some(crate::monitor_status_ticker::bind_monitor_status(receiver,move |text|ui.set_status(crate::monitor_status::MONITOR_STATUS_KEY,text.as_deref())));
            Ok(EventResult::None)
        })}));
        let sender=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        let telemetry_task:Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>=Arc::new(Mutex::new(None));
        let telemetry_monitors=monitors.clone();let telemetry_sender=sender.clone();let start_telemetry=telemetry_task.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,_| {let monitors=telemetry_monitors.clone();let sender=telemetry_sender.clone();let task=start_telemetry.clone();Box::pin(async move {
            let mut task=task.lock().map_err(|_|ExtensionFailure::new("monitor telemetry state poisoned"))?;if let Some(previous)=task.take() {previous.abort();}
            *task=Some(bind_monitor_events(monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.subscribe_state(),sender));Ok(EventResult::None)
        })}));
        let stepped_aside=Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notice_shown=Arc::new(std::sync::atomic::AtomicBool::new(false));
        for kind in [EventKind::SessionStart,EventKind::ModelSelect] {
            let sender=sender.clone();let stepped_aside=stepped_aside.clone();let notice_shown=notice_shown.clone();let completion_delivery=completion_delivery.clone();
            api.on(kind,Arc::new(move |event,ctx| {let sender=sender.clone();let stepped_aside=stepped_aside.clone();let notice_shown=notice_shown.clone();let completion_delivery=completion_delivery.clone();Box::pin(async move {
                let model=match event {maho_ext_api::types::ExtensionEvent::ModelSelect(event)=>Some(&event.model),_=>ctx.model.as_ref()};
                let enabled=std::env::var("PI_ANTHROPIC_BASH").is_ok_and(|value|matches!(value.trim().to_lowercase().as_str(),"1"|"true"|"yes"|"on"));
                let step_aside=enabled&&model.is_some_and(|model|serde_json::to_value(&model.api).ok().and_then(|value|value.as_str().map(str::to_owned)).as_deref()==Some("anthropic-messages"));
                stepped_aside.store(step_aside,std::sync::atomic::Ordering::SeqCst);
                let mut active=sender.get_active_tools()?;
                if !step_aside&&!active.iter().any(|tool|tool=="bash") {active.push("bash".to_owned());}
                for companion in crate::shared::TERMINAL_COMPANION_TOOLS {if !active.iter().any(|tool|tool==companion) {active.push((*companion).to_owned());}}
                if step_aside&&!notice_shown.swap(true,std::sync::atomic::Ordering::SeqCst) {ctx.ui.notify("native Anthropic bash active — monitor sessions remain available",maho_ext_api::types::NotificationType::Info);}
                if !step_aside {notice_shown.store(false,std::sync::atomic::Ordering::SeqCst);}
                sender.set_active_tools(active)?;
                *completion_delivery.lock().map_err(|_|ExtensionFailure::new("terminal delivery state poisoned"))?=crate::notify::get_terminal_notification_delivery(crate::settings::TERMINAL_SETTINGS_DEFAULTS.notify,Some(match ctx.mode {maho_ext_api::types::ExtensionMode::Print=>"print",maho_ext_api::types::ExtensionMode::Json=>"json",_=>"interactive"}),model.is_some(),false);
                Ok(EventResult::None)
            })}));
        }
        let prompt_sender=sender.clone();
        api.on(EventKind::BeforeAgentStart,Arc::new(move |event,_| {let sender=prompt_sender.clone();let stepped_aside=stepped_aside.clone();Box::pin(async move {
            if stepped_aside.load(std::sync::atomic::Ordering::SeqCst) {return Ok(EventResult::None);}
            let maho_ext_api::types::ExtensionEvent::BeforeAgentStart(event)=event else {return Ok(EventResult::None);};
            let eval_only=sender.get_all_tools()?.iter().any(|tool|tool.name=="eval");
            Ok(EventResult::BeforeAgentStart(maho_ext_api::types::BeforeAgentStartEventResult {system_prompt:Some(format!("{}\n{}",event.system_prompt,crate::prompt::build_terminal_prompt_section(eval_only))),..Default::default()}))
        })}));
        let bash_sender=sender.clone();let lifecycle_delivery=completion_delivery.clone();
        let lifecycle_notifier=notifier.clone();let lifecycle_monitors=monitors.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,ctx| {let notifier=lifecycle_notifier.clone();let monitors=lifecycle_monitors.clone();let sender=sender.clone();let lifecycle_delivery=lifecycle_delivery.clone();Box::pin(async move {
            use maho_ext_api::types::{ExtensionMode,CustomMessage,SendMessageOptions,DeliverAs};
            *lifecycle_delivery.lock().map_err(|_|ExtensionFailure::new("terminal delivery state poisoned"))?=crate::notify::get_terminal_notification_delivery(crate::settings::TERMINAL_SETTINGS_DEFAULTS.notify,Some(match ctx.mode {ExtensionMode::Print=>"print",ExtensionMode::Json=>"json",_=>"interactive"}),ctx.model.is_some(),false);
            notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.take();
            if matches!(ctx.mode,ExtensionMode::Print|ExtensionMode::Json)||ctx.model.is_none() {return Ok(EventResult::None);}
            let delivery=crate::monitor_notify::MonitorNotifier::new(crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor,move |injection| {
                if !injection.pause_ids.is_empty() {monitors.lock().expect("monitor registry").pause(&injection.pause_ids);}
                if let Err(error)=sender.send_message(CustomMessage {custom_type:crate::monitor_notify::MONITOR_NOTIFICATION_CUSTOM_TYPE.to_owned(),content:vec![ToolContent::text(injection.content)],display:false,details:Some(injection.details)},SendMessageOptions {trigger_turn:true,deliver_as:Some(DeliverAs::Steer)}) {eprintln!("monitor notification failed: {error}");}
            });
            *notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?=Some(delivery);Ok(EventResult::None)
        })}));
        let bash_manager=Arc::clone(&manager);
        let completion_tasks:Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>=Arc::new(Mutex::new(vec![]));let bash_tasks=completion_tasks.clone();let cleanup_terminal_notifier=terminal_notifier.clone();
        let mut bash=ToolDefinition::new("bash","Execute a shell command in a persistent PTY-backed session.",json!({"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"number"},"description":{"type":"string"},"run_in_background":{"type":"boolean"},"cols":{"type":"number"},"rows":{"type":"number"}},"required":["command"]}),Arc::new(move |call| {let manager=Arc::clone(&bash_manager);let sender=bash_sender.clone();let delivery=completion_delivery.clone();let notifier=terminal_notifier.clone();let tasks=bash_tasks.clone();Box::pin(async move {
            let result=crate::tools::bash::execute_bash(manager.clone(),call).await.map_err(ToolError::Message)?;
            if let Some(details)=&result.details && details.get("background").and_then(Value::as_bool)==Some(true) && let Some(id)=details.get("bash_id").and_then(Value::as_str) {
                let id=id.to_owned();let exit=manager.lock().map_err(|_|ToolError::Message("terminal manager state poisoned".to_owned()))?.get(&id).map(|runtime|runtime.subscribe_exit());
                if let Some(mut exit)=exit {let task=tokio::spawn(async move {
                    loop {
                        if exit.borrow_and_update().is_some() {break;}
                        if exit.changed().await.is_err() {return;}
                    }
                    let (status,output)={let mut manager=manager.lock().expect("terminal manager");let Some(runtime)=manager.get(&id) else {return;};(runtime.exit_result().ok().flatten(),runtime.full_output().unwrap_or_default())};
                    notifier.lock().expect("terminal notifier").notify_completion(&id,*delivery.lock().expect("terminal delivery"),status.as_ref(),&output,|content,delivery| {
                        use maho_ext_api::types::{CustomMessage,SendMessageOptions,DeliverAs};
                        if let Err(error)=sender.send_message(CustomMessage {custom_type:crate::notify::TERMINAL_NOTIFICATION_CUSTOM_TYPE.to_owned(),content:vec![ToolContent::text(content)],display:false,details:None},SendMessageOptions {trigger_turn:true,deliver_as:Some(if delivery==crate::notify::NotificationDelivery::Steer {DeliverAs::Steer} else {DeliverAs::FollowUp})}) {eprintln!("terminal notification failed: {error}");}
                    });
                });let mut tasks=tasks.lock().map_err(|_|ToolError::Message("terminal completion tasks poisoned".to_owned()))?;tasks.retain(|task|!task.is_finished());tasks.push(task);}
            }
            tool_result(result)
        })}));
        bash.exposure=Some(maho_tools::definition::ToolExposure::Eval);api.register_tool(bash);
        for (name,description,properties,required) in [
            ("bash_output","Read output from a background bash session without blocking.",json!({"bash_id":{"type":"string"},"filter":{"type":"string"},"view":{"type":"string","enum":["log","screen"]}}),vec!["bash_id"]),
            ("bash_input","Write stdin or named keys to a live background bash session.",json!({"bash_id":{"type":"string"},"input":{"type":"string"},"keys":{"type":"array","items":{"type":"string"}},"submit":{"type":"boolean"}}),vec!["bash_id"]),
            ("bash_resize","Resize a live background bash session's PTY.",json!({"bash_id":{"type":"string"},"cols":{"type":"number"},"rows":{"type":"number"}}),vec!["bash_id","cols","rows"]),
            ("kill_bash","Terminate a background bash session and its process tree.",json!({"bash_id":{"type":"string"},"all":{"type":"boolean"}}),Vec::new()),
        ] {
            let manager=Arc::clone(&manager);
            let monitors=monitors.clone();
            let tool=ToolDefinition::new(name,description,json!({"type":"object","properties":properties,"required":required}),Arc::new(move |call| {let manager=Arc::clone(&manager);let monitors=monitors.clone();Box::pin(async move {
                let mut manager=manager.lock().map_err(|_|ToolError::Message("terminal manager state poisoned".to_owned()))?;
                let id=call.params.get("bash_id").and_then(Value::as_str);
                if name=="kill_bash" {
                    let mut monitors=monitors.lock().map_err(|_|ToolError::Message("monitor registry state poisoned".to_owned()))?;
                    let all=call.params.get("all").and_then(Value::as_bool).unwrap_or(false);
                    if all {
                        let mut count=manager.size();
                        for id in monitors.snapshot().into_iter().filter(|record|record.id.starts_with("watch_")).map(|record|record.id) {count+=usize::from(monitors.stop_file(&id));}
                        manager.teardown().map_err(|error|ToolError::Message(error.to_string()))?;
                        return tool_result(crate::tools::context::text_result(format!("Killed {count} session(s).")));
                    }
                    if let Some(id)=id {
                        let resolved=manager.resolve_id(id).unwrap_or_else(||id.to_owned());
                        if monitors.stop_file(&resolved) {return tool_result(crate::tools::context::text_result(format!("Killed {id}.")));}
                    }
                    return tool_result(execute_kill_bash(&mut manager,id,all));
                }
                let id=id.ok_or_else(||ToolError::Message("bash_id must be a string".to_owned()))?;let resolved=manager.resolve_id(id).unwrap_or_else(||id.to_owned());let runtime=manager.get(&resolved);
                match name {
                    "bash_output"=>{let mut result=execute_bash_output_view(runtime.as_deref(),id,call.params.get("filter").and_then(Value::as_str),call.params.get("view").and_then(Value::as_str)==Some("screen")).map_err(|error|ToolError::Message(error.to_string()))?;let registry=monitors.lock().map_err(|_|ToolError::Message("monitor registry state poisoned".to_owned()))?;if runtime.is_some()&&let Some(entry)=registry.snapshot().iter().find(|entry|entry.id==resolved) {result=crate::tools::bash_output::with_monitor_state(result,id,entry.paused,registry.muted_dropped(&resolved));}tool_result(result)},
                    "bash_input"=>{let keys=call.params.get("keys").and_then(Value::as_array).map(|keys|keys.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default();tool_result(execute_bash_input(runtime,BashInputInput {bash_id:id,input:call.params.get("input").and_then(Value::as_str),keys:&keys,submit:call.params.get("submit").and_then(Value::as_bool)}))},
                    "bash_resize"=>tool_result(execute_bash_resize(runtime.as_deref(),id,call.params.get("cols").and_then(Value::as_f64).unwrap_or(f64::NAN),call.params.get("rows").and_then(Value::as_f64).unwrap_or(f64::NAN))),_=>unreachable!(),
                }
            })}));
            if name=="bash_output" {api.register_tool_with_renderers(tool,crate::tools::render::output_renderers()).expect("valid builtin bash_output registration");} else {api.register_tool(tool);}
        }
        let tool_manager=manager.clone();let tool_monitors=monitors.clone();let tool_notifier=notifier.clone();
        api.register_tool(ToolDefinition::new("monitor","Subscribe to command output or file changes instead of polling.",crate::tools::monitor::monitor_schema(),Arc::new(move |call| {
            let manager=tool_manager.clone();let monitors=tool_monitors.clone();let notifier=tool_notifier.clone();Box::pin(async move {
                let mut manager=manager.lock().map_err(|_|ToolError::Message("terminal manager state poisoned".to_owned()))?;
                let mut monitors=monitors.lock().map_err(|_|ToolError::Message("monitor registry state poisoned".to_owned()))?;
                let cwd=call.context.map(|context|context.cwd().to_path_buf()).unwrap_or(std::env::current_dir()?);
                let result=crate::tools::monitor::execute_monitor(&mut manager,&mut monitors,&call.params,&cwd);
                if call.params.get("action").and_then(Value::as_str)==Some("rearm") && let Some(notifier)=notifier.lock().map_err(|_|ToolError::Message("monitor notifier state poisoned".to_owned()))?.as_ref() {
                    notifier.resume(monitors.snapshot().iter().filter(|record|!record.paused).map(|record|record.id.clone()).collect()).map_err(ToolError::Message)?;
                }
                tool_result(result)
            })
        })));
        let activity=notifier.clone();
        let input_monitors=monitors.clone();
        api.on(EventKind::Input,Arc::new(move |event,_| {let notifier=activity.clone();let monitors=input_monitors.clone();Box::pin(async move {
            if matches!(event,maho_ext_api::types::ExtensionEvent::Input(event) if event.source==maho_ext_api::types::InputSource::Extension) {return Ok(EventResult::None);}
            let resumed=monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.resume(None);
            if let Some(notifier)=notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.as_ref() {notifier.note_activity().map_err(ExtensionFailure::new)?;if !resumed.is_empty() {notifier.resume(resumed.into_iter().map(|(id,_)|id).collect()).map_err(ExtensionFailure::new)?;}}
            Ok(EventResult::None)
        })}));
        let activity=notifier.clone();
        api.on(EventKind::ToolCall,Arc::new(move |_,_| {let notifier=activity.clone();Box::pin(async move {if let Some(notifier)=notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.as_ref() {notifier.note_activity().map_err(ExtensionFailure::new)?;}Ok(EventResult::None)})}));
        let cleanup=Arc::clone(&manager);
        api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {let tasks=completion_tasks.clone();let notifier=cleanup_terminal_notifier.clone();Box::pin(async move {for task in tasks.lock().map_err(|_|ExtensionFailure::new("terminal completion tasks poisoned"))?.drain(..) {task.abort();}*notifier.lock().map_err(|_|ExtensionFailure::new("terminal notifier state poisoned"))?=crate::notify::TerminalNotifier::default();Ok(EventResult::None)})}));
        api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {let task=telemetry_task.clone();Box::pin(async move {if let Some(task)=task.lock().map_err(|_|ExtensionFailure::new("monitor telemetry state poisoned"))?.take() {task.abort();}Ok(EventResult::None)})}));
        api.on(EventKind::SessionShutdown,Arc::new(move |_,ctx| {let status=status_task.clone();Box::pin(async move {if let Some(task)=status.lock().map_err(|_|ExtensionFailure::new("monitor status state poisoned"))?.take() {task.abort();}ctx.ui.set_status(crate::monitor_status::MONITOR_STATUS_KEY,None);Ok(EventResult::None)})}));
        api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {let manager=Arc::clone(&cleanup);let monitors=monitors.clone();let notifier=notifier.clone();Box::pin(async move {notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.take();monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.dispose();manager.lock().map_err(|_|ExtensionFailure::new("terminal manager state poisoned"))?.teardown().map_err(|error|ExtensionFailure::new(error.to_string()))?;Ok(EventResult::None)})}));
    }
}
#[cfg(test)]
mod tests {
    use super::*;use maho_ext_api::types::*;
    #[tokio::test]
    async fn registered_file_monitor_stable_id_kill_releases_shared_capacity()->Result<(),ToolError> {
        let dir=tempfile::tempdir()?;let mut api=ExtensionApi::new(LoadedExtension::new("terminal",dir.path().to_owned(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);
        let result=(api.registered.tools[5].definition.execute)(maho_tools::definition::ToolCall {id:"file",params:json!({"description":"watch","path":dir.path().join("file"),"persistent":true}),signal:Default::default(),on_update:None,context:None}).await?;
        let id=result.details.as_ref().unwrap()["monitor_id"].as_str().unwrap();assert!(id.starts_with("mon_"));
        let result=(api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {id:"kill",params:json!({"bash_id":id}),signal:Default::default(),on_update:None,context:None}).await?;assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text==&format!("Killed {id}.")));
        let result=(api.registered.tools[5].definition.execute)(maho_tools::definition::ToolCall {id:"next",params:json!({"description":"next","path":dir.path().join("next"),"persistent":true}),signal:Default::default(),on_update:None,context:None}).await?;assert!(result.details.as_ref().unwrap()["monitor_id"].as_str().unwrap().starts_with("mon_"));
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {id:"all",params:json!({"all":true}),signal:Default::default(),on_update:None,context:None}).await?;Ok(())
    }
    #[tokio::test]
    async fn registry_state_reaches_native_event_bus_and_rpc() {
        let dir=tempfile::tempdir().unwrap();let bus=EventBus::default();let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let state_sender=sender.clone();let rpc_sender=sender;
        let _state=bus.on("terminal_monitor_state",Arc::new(move |data| {state_sender.send(("state",data.clone())).unwrap();}));let _rpc=bus.on("senpi:extension-rpc-event",Arc::new(move |data| {rpc_sender.send(("rpc",data.clone())).unwrap();}));
        let api=Arc::new(ExtensionApi::new(LoadedExtension::new("terminal",dir.path().to_owned(),SourceInfo::default()),ExtensionSessionProfile::default(),bus,ExtensionRuntime::default()));let mut registry=crate::monitor_registry::MonitorRegistry::new(|_|{});let task=bind_monitor_events(registry.subscribe_state(),api);
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            let (_,initial)=events.recv().await.unwrap();assert_eq!(initial["activeCount"],0);let (kind,rpc)=events.recv().await.unwrap();assert_eq!(kind,"rpc");assert_eq!(rpc["data"],initial);
            let (id,_)=registry.register_persistent_file("watch",&dir.path().join("file"),crate::terminal_manifest_model::FileEvent::Create).unwrap();let (kind,live)=events.recv().await.unwrap();assert_eq!(kind,"state");assert_eq!(live["activeCount"],1);assert_eq!(live["monitors"][0]["id"],id);assert_eq!(live["monitors"][0]["persistent"],true);assert_eq!(live["monitors"][0].get("command"),Some(&Value::Null));assert_eq!(live["monitors"][0]["fireCount"],0);let (_,rpc)=events.recv().await.unwrap();assert_eq!(rpc["data"],live);
            registry.stop_file(&id);let (_,empty)=events.recv().await.unwrap();assert_eq!(empty["activeCount"],0);assert_eq!(events.recv().await.unwrap().1["data"],empty);
        }).await.unwrap();task.abort();assert!(task.await.unwrap_err().is_cancelled());
    }
    #[tokio::test] async fn registered_bash_runs_real_pty_and_companion_peeks()->Result<(),ToolError> {
        let mut api=ExtensionApi::new(LoadedExtension::new("terminal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);
        let result=(api.registered.tools[0].definition.execute)(maho_tools::definition::ToolCall {id:"c1",params:json!({"command":"stty -echo; printf 'ready\\n'"}),signal:Default::default(),on_update:None,context:None}).await?;
        assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text=="ready"));
        let id="bash_1";
        let result=(api.registered.tools[1].definition.execute)(maho_tools::definition::ToolCall {id:"c2",params:json!({"bash_id":id}),signal:Default::default(),on_update:None,context:None}).await?;
        assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text.contains("status: completed exit_code: 0")));
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {id:"c3",params:json!({"all":true}),signal:Default::default(),on_update:None,context:None}).await?;Ok(())
    }
    #[test] fn native_companions_register_flat_schemas_and_shutdown() {let mut api=ExtensionApi::new(LoadedExtension::new("terminal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);assert_eq!(api.registered.tools.iter().map(|tool|tool.definition.name.as_str()).collect::<Vec<_>>(),vec!["bash","bash_output","bash_input","bash_resize","kill_bash","monitor"]);for tool in &api.registered.tools {assert_eq!(tool.definition.parameters["type"],"object");assert!(tool.definition.parameters.get("properties").is_some());}assert_eq!(api.registered.handlers[&EventKind::SessionShutdown].len(),4);}
}
