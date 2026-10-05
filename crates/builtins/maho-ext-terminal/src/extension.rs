use std::sync::{Arc,Mutex};
use maho_ext_api::types::{Extension,ExtensionApi,EventKind,EventResult,ExtensionFailure};
use maho_tools::definition::{ToolDefinition,ToolResult,ToolContent,ToolError};
use serde_json::{Value,json};
use crate::manager::TerminalManager;
use crate::tools::{bash_input::{execute_bash_input,BashInputInput},bash_output::execute_bash_output_view,bash_resize::execute_bash_resize,kill_bash::execute_kill_bash,context::TerminalToolResult};
pub struct TerminalExtension;
type Backgrounds=Arc<Mutex<indexmap::IndexMap<String,crate::session_bundle::BackgroundSession>>>;
fn publish_backgrounds(backgrounds:&Backgrounds,sender:&ExtensionApi) {
    let items=backgrounds.lock().expect("terminal backgrounds").values().map(|entry|json!({"id":entry.id,"description":entry.description,"startedAtMs":entry.started_at_ms})).collect::<Vec<_>>();sender.events.emit("wake_source_state",&json!({"source":"terminal-background-sessions","activeCount":items.len(),"items":items}));
}
fn bind_completion(id:String,manager:Arc<Mutex<TerminalManager>>,backgrounds:Backgrounds,sender:Arc<ExtensionApi>,delivery:Arc<Mutex<Option<crate::notify::NotificationDelivery>>>,notifier:Arc<Mutex<crate::notify::TerminalNotifier>>)->Option<tokio::task::JoinHandle<()>> {
    let mut exit=manager.lock().expect("terminal manager").get(&id)?.subscribe_exit();
    Some(tokio::spawn(async move {
        loop {
            if exit.borrow_and_update().is_some() {break;}
            if exit.changed().await.is_err() {return;}
        }
        backgrounds.lock().expect("terminal backgrounds").shift_remove(&id);publish_backgrounds(&backgrounds,&sender);
        let (status,output)={let mut manager=manager.lock().expect("terminal manager");let Some(runtime)=manager.get(&id) else {return;};(runtime.exit_result().ok().flatten(),runtime.full_output().unwrap_or_default())};
        notifier.lock().expect("terminal notifier").notify_completion(&id,*delivery.lock().expect("terminal delivery"),status.as_ref(),&output,|content,delivery| {
            use maho_ext_api::types::{CustomMessage,SendMessageOptions,DeliverAs};
            if let Err(error)=sender.send_message(CustomMessage {custom_type:crate::notify::TERMINAL_NOTIFICATION_CUSTOM_TYPE.to_owned(),content:vec![ToolContent::text(content)],display:false,details:None},SendMessageOptions {trigger_turn:true,deliver_as:Some(if delivery==crate::notify::NotificationDelivery::Steer {DeliverAs::Steer}else {DeliverAs::FollowUp})}) {eprintln!("terminal notification failed: {error}");}
        });
    }))
}
pub fn monitor_state_payload(snapshot:&[crate::monitor_registry::MonitorSnapshotEntry])->Value {
    let monitors=snapshot.iter().map(|entry| {
        let mut value=json!({"id":entry.id,"description":entry.description,"paused":entry.paused,"startedAtMs":entry.started_at_ms,"command":entry.command,"filter":entry.filter,"deadlineMs":entry.deadline_ms,"lastFiredAtMs":entry.last_fired_at_ms});
        for (key,field) in [("command",entry.command.as_ref().map(|value|json!(value))),("filter",entry.filter.as_ref().map(|value|json!(value))),("persistent",entry.persistent.map(|value|json!(value))),("deadlineMs",entry.deadline_ms.map(|value|json!(value))),("fireCount",entry.fire_count.map(|value|json!(value))),("lastFiredAtMs",entry.last_fired_at_ms.map(|value|json!(value)))] {if let Some(field)=field {value[key]=field;}}
        value
    }).collect::<Vec<_>>();json!({"activeCount":snapshot.len(),"monitors":monitors})
}
fn monitor_ended_payload(event:&crate::monitor_registry::MonitorEndedEvent)->Value {
    json!({"id":event.id,"description":event.description,"startedAtMs":event.started_at_ms,"endedAtMs":event.ended_at_ms,"reason":event.reason.as_str(),"exitCode":event.exit_code,"fireCount":event.fire_count})
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
#[cfg(unix)]
fn terminal_now_ms()->f64 {std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0,|elapsed|elapsed.as_secs_f64()*1000.0)}
/// Upstream `terminalStateDir`: `(sessionDir, join(sessionDir, "extensions/terminal"))`, or `None`
/// when the host exposes no durable session dir. The lease lives in the terminal dir; the
/// manifest writer derives its own `extensions/terminal` base from the session dir.
#[cfg(unix)]
fn terminal_state_dir(ctx:&maho_ext_api::types::ExtensionContext)->Option<(std::path::PathBuf,std::path::PathBuf)> {
    let session_dir=ctx.session_manager.get_session_dir()?;
    if session_dir.as_os_str().is_empty()||!session_dir.is_absolute() {return None;}
    Some((session_dir.clone(),session_dir.join("extensions/terminal")))
}
/// Upstream `restoreDigestSentence`: ONE coalesced sentence with empty clauses omitted.
#[cfg(unix)]
fn restore_digest_sentence(digest:&crate::restore::RestoreDigest)->Option<String> {
    let mut clauses=Vec::new();
    for (label,count) in [("restored",digest.restored),("lost",digest.lost)] {if count>0 {clauses.push(format!("{label} {count}"));}}
    if digest.expired>0 {clauses.push(format!("expired {}",digest.expired));}
    if digest.muted>0 {clauses.push(format!("muted {}",digest.muted));}
    if digest.attached_elsewhere>0 {clauses.push(format!("attached elsewhere {}",digest.attached_elsewhere));}
    if digest.store_error {clauses.push("the terminal manifest could not be read".to_owned());}
    (!clauses.is_empty()).then(||format!("Terminal state after restart: {}.",clauses.join("; ")))
}
/// Upstream `sendTerminalReminder`: a `<system-reminder>` through the terminal notification delivery.
#[cfg(unix)]
fn send_terminal_reminder(sender:&ExtensionApi,delivery:Option<crate::notify::NotificationDelivery>,sentence:&str) {
    use maho_ext_api::types::{CustomMessage,SendMessageOptions,DeliverAs};
    let Some(delivery)=delivery else {return;};
    if let Err(error)=sender.send_message(CustomMessage {custom_type:crate::notify::TERMINAL_NOTIFICATION_CUSTOM_TYPE.to_owned(),content:vec![ToolContent::text(format!("<system-reminder>{sentence}</system-reminder>"))],display:false,details:None},SendMessageOptions {trigger_turn:true,deliver_as:Some(if delivery==crate::notify::NotificationDelivery::Steer {DeliverAs::Steer}else {DeliverAs::FollowUp})}) {eprintln!("terminal reminder failed: {error}");}
}
impl Extension for TerminalExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let manager=Arc::new(Mutex::new(TerminalManager::default()));
        let backgrounds:Backgrounds=Arc::new(Mutex::new(indexmap::IndexMap::new()));
        let configuration=Arc::new(Mutex::new((crate::settings::TERMINAL_SETTINGS_DEFAULTS,None::<String>)));
        let start_configuration=configuration.clone();let configuration_manager=manager.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,ctx| {let configuration=start_configuration.clone();let manager=configuration_manager.clone();Box::pin(async move {
            let loaded=crate::settings::load_session_settings(ctx)?;manager.lock().map_err(|_|ExtensionFailure::new("terminal manager state poisoned"))?.configure(&loaded.0);
            *configuration.lock().map_err(|_|ExtensionFailure::new("terminal configuration state poisoned"))?=loaded;Ok(EventResult::None)
        })}));
        let completion_delivery=Arc::new(Mutex::new(None));
        let terminal_notifier=Arc::new(Mutex::new(crate::notify::TerminalNotifier::default()));
        let notifier:Arc<Mutex<Option<crate::monitor_notify::MonitorNotifier>>>=Arc::new(Mutex::new(None));
        let event_notifier=notifier.clone();
        let ended_bus=api.events.clone();
        let monitors=Arc::new(Mutex::new(crate::monitor_registry::MonitorRegistry::new_with_ended(move |event| {
            if let Some(notifier)=event_notifier.lock().expect("monitor notifier").as_ref() && let Err(error)=notifier.notify_event(event) {eprintln!("monitor delivery failed: {error}");}
        },move |event| {
            let payload=monitor_ended_payload(&event);
            ended_bus.emit(crate::shared::TERMINAL_MONITOR_ENDED_EVENT,&payload);
            ended_bus.emit("senpi:extension-rpc-event",&json!({"name":crate::shared::TERMINAL_MONITOR_ENDED_EVENT,"data":payload}));
        })));
        let bundle=Arc::new(Mutex::new(crate::session_bundle::TerminalSessionBundle::from_parts(manager.clone(),monitors.clone(),backgrounds.clone())));
        let reload_bundle=bundle.clone();let reload_configuration=configuration.clone();
        api.on(EventKind::SessionStart,Arc::new(move |event,ctx| {let bundle=reload_bundle.clone();let configuration=reload_configuration.clone();Box::pin(async move {
            let previous=crate::session_bundle::claim_parked_bundle(ctx.session_manager.session_id());
            if let Some(mut previous)=previous {
                if matches!(event,maho_ext_api::types::ExtensionEvent::SessionStart(event) if event.reason==maho_ext_api::types::SessionReason::Reload) {
                    let live=bundle.lock().map_err(|_|ExtensionFailure::new("terminal bundle state poisoned"))?;live.adopt(&previous);live.manager.lock().map_err(|_|ExtensionFailure::new("terminal manager state poisoned"))?.configure(&configuration.lock().map_err(|_|ExtensionFailure::new("terminal configuration state poisoned"))?.0);
                }else {previous.teardown().map_err(|error|ExtensionFailure::new(error.to_string()))?;}
            }
            Ok(EventResult::None)
        })}));
        let status_task:Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>=Arc::new(Mutex::new(None));
        for kind in [EventKind::SessionParked,EventKind::SessionResumed] {
            let monitors=monitors.clone();let status=status_task.clone();
            api.on(kind,Arc::new(move |_,ctx| {let monitors=monitors.clone();let status=status.clone();Box::pin(async move {
                let monitors=monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?;
                if kind==EventKind::SessionParked {monitors.park();}else {monitors.unpark();}
                let mut status=status.lock().map_err(|_|ExtensionFailure::new("monitor status state poisoned"))?;
                if let Some(task)=status.take() {task.abort();}
                if kind==EventKind::SessionResumed {let ui=ctx.ui.clone();let tui=matches!(ctx.mode,maho_ext_api::types::ExtensionMode::Tui);*status=Some(crate::monitor_status_ticker::bind_monitor_status(monitors.subscribe_state(),move |text|ui.set_status(crate::monitor_status::MONITOR_STATUS_KEY,crate::monitor_status::render_monitor_status(if tui {"tui"} else {"plain"},&ui.theme(),text.as_deref()).as_deref())));}
                Ok(EventResult::None)
            })}));
        }
        let status_monitors=monitors.clone();let start_status=status_task.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,ctx| {let monitors=status_monitors.clone();let status=start_status.clone();Box::pin(async move {
            let mut status=status.lock().map_err(|_|ExtensionFailure::new("monitor status state poisoned"))?;if let Some(task)=status.take() {task.abort();}
            let receiver=monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.subscribe_state();let ui=ctx.ui.clone();let tui=matches!(ctx.mode,maho_ext_api::types::ExtensionMode::Tui);
            *status=Some(crate::monitor_status_ticker::bind_monitor_status(receiver,move |text|ui.set_status(crate::monitor_status::MONITOR_STATUS_KEY,crate::monitor_status::render_monitor_status(if tui {"tui"} else {"plain"},&ui.theme(),text.as_deref()).as_deref())));
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
            let sender=sender.clone();let stepped_aside=stepped_aside.clone();let notice_shown=notice_shown.clone();let completion_delivery=completion_delivery.clone();let configuration=configuration.clone();
            api.on(kind,Arc::new(move |event,ctx| {let sender=sender.clone();let stepped_aside=stepped_aside.clone();let notice_shown=notice_shown.clone();let completion_delivery=completion_delivery.clone();let configuration=configuration.clone();Box::pin(async move {
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
                *completion_delivery.lock().map_err(|_|ExtensionFailure::new("terminal delivery state poisoned"))?=crate::notify::get_terminal_notification_delivery(configuration.lock().map_err(|_|ExtensionFailure::new("terminal configuration state poisoned"))?.0.notify,Some(match ctx.mode {maho_ext_api::types::ExtensionMode::Print=>"print",maho_ext_api::types::ExtensionMode::Json=>"json",_=>"interactive"}),model.is_some(),false);
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
        // Terminal persistence lifecycle: upstream `adoptPersistedTerminalState` on a non-reload
        // start (acquire the lease, restore the manifest, report ONE coalesced digest) and
        // `suspendAndFlushManifest` + lease release on shutdown. The lease and the manifest both
        // live under the session dir the host exposes through `SessionManager::get_session_dir`;
        // a context without one keeps no durable terminal state, like upstream's SDK/in-memory
        // sessions. A reload generation keeps the lease: the same pid still owns it.
        let state_lifecycle:Arc<Mutex<Option<crate::terminal_state::TerminalStateLifecycle>>>=Arc::new(Mutex::new(None));
        let start_lifecycle=state_lifecycle.clone();let lifecycle_manager=manager.clone();let lifecycle_monitors=monitors.clone();let lifecycle_settings=configuration.clone();let lifecycle_sender=sender.clone();
        api.on(EventKind::SessionStart,Arc::new(move |event,ctx| {let state=start_lifecycle.clone();let manager=lifecycle_manager.clone();let monitors=lifecycle_monitors.clone();let configuration=lifecycle_settings.clone();let sender=lifecycle_sender.clone();Box::pin(async move {
            #[cfg(unix)]
            {
                let Some((session_dir,terminal_dir))=terminal_state_dir(ctx) else {return Ok(EventResult::None);};
                let session_id=ctx.session_manager.session_id().to_owned();
                let encoded=maho_core::session_sidecar_store::encoded_session_id(&session_id);
                let reload=matches!(event,maho_ext_api::types::ExtensionEvent::SessionStart(event) if event.reason==maho_ext_api::types::SessionReason::Reload);
                let now=terminal_now_ms();
                // A reload generation inherits the lease (same pid) and only rebinds the recorder so
                // manifest coverage continues; a fresh generation acquires the lease and restores.
                let mut lifecycle=crate::terminal_state::TerminalStateLifecycle::new(&session_dir,&session_id);
                if reload {
                    *state.lock().map_err(|_|ExtensionFailure::new("terminal state lifecycle poisoned"))?=Some(lifecycle);
                } else {
                    let (settings,shell)=configuration.lock().map_err(|_|ExtensionFailure::new("terminal configuration state poisoned"))?.clone();
                    let delivery=crate::notify::get_terminal_notification_delivery(settings.notify,Some(match ctx.mode {maho_ext_api::types::ExtensionMode::Print=>"print",maho_ext_api::types::ExtensionMode::Json=>"json",_=>"interactive"}),ctx.model.is_some(),false);
                    match lifecycle.acquire(&terminal_dir,&encoded,f64::from(std::process::id()),now) {
                        Ok(crate::terminal_state::TerminalStateAdoption::Acquired)=>{
                            let restore_state=lifecycle.read_restore_state().await;
                            let digest={let mut manager=manager.lock().map_err(|_|ExtensionFailure::new("terminal manager state poisoned"))?;let mut monitors=monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?;lifecycle.apply_restore(restore_state,&mut manager,&mut monitors,now,shell.as_deref(),&settings)};
                            if let Some(sentence)=restore_digest_sentence(&digest) {send_terminal_reminder(&sender,delivery,&sentence);}
                            *state.lock().map_err(|_|ExtensionFailure::new("terminal state lifecycle poisoned"))?=Some(lifecycle);
                        }
                        Ok(crate::terminal_state::TerminalStateAdoption::AttachedElsewhere {pid})=>send_terminal_reminder(&sender,delivery,&format!("Terminal monitors for this session are attached in another live process (pid {pid}); nothing was restored here.")),
                        Err(error)=>send_terminal_reminder(&sender,delivery,&format!("Terminal state after restart: the session lease could not be acquired ({error}); nothing was restored.")),
                    }
                }
            }
            #[cfg(not(unix))]
            {let _=(event,ctx,manager,monitors,configuration,sender);}
            Ok(EventResult::None)
        })}));
        let shutdown_lifecycle=state_lifecycle.clone();
        api.on(EventKind::SessionShutdown,Arc::new(move |event,ctx| {let state=shutdown_lifecycle.clone();Box::pin(async move {
            #[cfg(unix)]
            {
                if matches!(event,maho_ext_api::types::ExtensionEvent::SessionShutdown(event) if event.reason==maho_ext_api::types::SessionReason::Reload) {return Ok(EventResult::None);}
                let Some(mut lifecycle)=state.lock().map_err(|_|ExtensionFailure::new("terminal state lifecycle poisoned"))?.take() else {return Ok(EventResult::None);};
                lifecycle.record_shutdown(terminal_now_ms()).await;
                if lifecycle.is_owner() {
                    if let Err(error)=lifecycle.release() {eprintln!("terminal lease release failed: {error}");}
                } else if let Some((_session_dir,terminal_dir))=terminal_state_dir(ctx) {
                    // A reload generation inherited the lease instead of acquiring it, so it releases
                    // by (path, own pid) - which removes exactly that file and no foreign holder's.
                    let encoded=maho_core::session_sidecar_store::encoded_session_id(ctx.session_manager.session_id());
                    if let Err(error)=crate::terminal_state::release_lease_at(&terminal_dir,&encoded,f64::from(std::process::id())) {eprintln!("terminal lease release failed: {error}");}
                }
            }
            #[cfg(not(unix))]
            {let _=(event,ctx);}
            Ok(EventResult::None)
        })}));
        let bash_sender=sender.clone();let lifecycle_delivery=completion_delivery.clone();
        let lifecycle_notifier=notifier.clone();let lifecycle_monitors=monitors.clone();let lifecycle_configuration=configuration.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,ctx| {let notifier=lifecycle_notifier.clone();let monitors=lifecycle_monitors.clone();let sender=sender.clone();let lifecycle_delivery=lifecycle_delivery.clone();let configuration=lifecycle_configuration.clone();Box::pin(async move {
            use maho_ext_api::types::{ExtensionMode,CustomMessage,SendMessageOptions,DeliverAs};
            let settings=configuration.lock().map_err(|_|ExtensionFailure::new("terminal configuration state poisoned"))?.0.clone();
            let delivery_mode=crate::notify::get_terminal_notification_delivery(settings.notify,Some(match ctx.mode {ExtensionMode::Print=>"print",ExtensionMode::Json=>"json",_=>"interactive"}),ctx.model.is_some(),false);
            *lifecycle_delivery.lock().map_err(|_|ExtensionFailure::new("terminal delivery state poisoned"))?=delivery_mode;
            notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.take();
            if matches!(ctx.mode,ExtensionMode::Print|ExtensionMode::Json) {return Ok(EventResult::None);}
            let available=lifecycle_delivery.clone();
            let delivery=crate::monitor_notify::MonitorNotifier::with_delivery_guard(settings.monitor,move ||available.lock().expect("terminal delivery").is_some(),move |injection| {
                let Some(delivery_mode)=*lifecycle_delivery.lock().expect("terminal delivery") else {return;};
                if !injection.pause_ids.is_empty() {monitors.lock().expect("monitor registry").pause(&injection.pause_ids);}
                let force_wake=!injection.pause_ids.is_empty();
                if let Err(error)=sender.send_message(CustomMessage {custom_type:crate::monitor_notify::MONITOR_NOTIFICATION_CUSTOM_TYPE.to_owned(),content:vec![ToolContent::text(injection.content)],display:false,details:Some(injection.details)},SendMessageOptions {trigger_turn:true,deliver_as:Some(if delivery_mode==crate::notify::NotificationDelivery::Steer||force_wake {DeliverAs::Steer}else {DeliverAs::FollowUp})}) {eprintln!("monitor notification failed: {error}");}
            });
            *notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?=Some(delivery);
            Ok(EventResult::None)
        })}));
        let delivery_monitors=monitors.clone();let delivery_notifier=notifier.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,_| {let monitors=delivery_monitors.clone();let notifier=delivery_notifier.clone();Box::pin(async move {
            monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.bind_delivery(move |event| {if let Some(notifier)=notifier.lock().expect("monitor notifier").as_ref()&&let Err(error)=notifier.notify_event(event) {eprintln!("monitor delivery failed: {error}");}});Ok(EventResult::None)
        })}));
        let bash_manager=Arc::clone(&manager);let bash_configuration=configuration.clone();
        let completion_tasks:Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>=Arc::new(Mutex::new(vec![]));let bash_tasks=completion_tasks.clone();let cleanup_terminal_notifier=terminal_notifier.clone();
        let restored_manager=manager.clone();let restored_backgrounds=backgrounds.clone();let restored_sender=bash_sender.clone();let restored_delivery=completion_delivery.clone();let restored_notifier=terminal_notifier.clone();let restored_tasks=completion_tasks.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,_| {let manager=restored_manager.clone();let backgrounds=restored_backgrounds.clone();let sender=restored_sender.clone();let delivery=restored_delivery.clone();let notifier=restored_notifier.clone();let tasks=restored_tasks.clone();Box::pin(async move {
            publish_backgrounds(&backgrounds,&sender);let ids=backgrounds.lock().map_err(|_|ExtensionFailure::new("terminal backgrounds poisoned"))?.keys().cloned().collect::<Vec<_>>();
            for id in ids {if let Some(task)=bind_completion(id,manager.clone(),backgrounds.clone(),sender.clone(),delivery.clone(),notifier.clone()) {tasks.lock().map_err(|_|ExtensionFailure::new("terminal completion tasks poisoned"))?.push(task);}}Ok(EventResult::None)
        })}));
        let bash_backgrounds=backgrounds.clone();
        let mut bash=ToolDefinition::new("bash","Execute a shell command in a persistent PTY-backed session.",json!({"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"number"},"description":{"type":"string"},"run_in_background":{"type":"boolean"},"cols":{"type":"number"},"rows":{"type":"number"}},"required":["command"]}),Arc::new(move |call| {let manager=Arc::clone(&bash_manager);let sender=bash_sender.clone();let delivery=completion_delivery.clone();let notifier=terminal_notifier.clone();let tasks=bash_tasks.clone();let bash_configuration=bash_configuration.clone();let backgrounds=bash_backgrounds.clone();Box::pin(async move {
            let (settings,shell)=bash_configuration.lock().map_err(|_|ToolError::Message("terminal configuration state poisoned".to_owned()))?.clone();
            let description=call.params.get("description").or_else(||call.params.get("command")).and_then(Value::as_str).unwrap_or("").to_owned();let started_at_ms=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error|ToolError::Message(error.to_string()))?.as_secs_f64()*1000.0;
            let result=crate::tools::bash::execute_configured_bash(manager.clone(),call,std::env::var("PI_BASH_FOREGROUND_SECONDS").ok().as_deref(),&settings,shell.as_deref()).await.map_err(ToolError::Message)?;
            if let Some(details)=&result.details && details.get("background").and_then(Value::as_bool)==Some(true) && let Some(id)=details.get("bash_id").and_then(Value::as_str) {
                let id=id.to_owned();backgrounds.lock().map_err(|_|ToolError::Message("terminal backgrounds poisoned".to_owned()))?.insert(id.clone(),crate::session_bundle::BackgroundSession {id:id.clone(),description,started_at_ms});publish_backgrounds(&backgrounds,&sender);
                if let Some(task)=bind_completion(id,manager,backgrounds,sender,delivery,notifier) {let mut tasks=tasks.lock().map_err(|_|ToolError::Message("terminal completion tasks poisoned".to_owned()))?;tasks.retain(|task|!task.is_finished());tasks.push(task);}
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
                        let resolved=crate::tools::context::resolve_terminal_id(&manager,id);
                        if monitors.stop_file(&resolved) {return tool_result(crate::tools::context::text_result(format!("Killed {id}.")));}
                    }
                    return tool_result(execute_kill_bash(&mut manager,id,all));
                }
                let id=id.ok_or_else(||ToolError::Message("bash_id must be a string".to_owned()))?;let resolved=crate::tools::context::resolve_terminal_id(&manager,id);let runtime=manager.get(&resolved);
                match name {
                    "bash_output"=>{let mut result=execute_bash_output_view(runtime.as_deref(),id,call.params.get("filter").and_then(Value::as_str),call.params.get("view").and_then(Value::as_str)==Some("screen")).map_err(|error|ToolError::Message(error.to_string()))?;let registry=monitors.lock().map_err(|_|ToolError::Message("monitor registry state poisoned".to_owned()))?;if runtime.is_some()&&let Some(entry)=registry.snapshot().iter().find(|entry|entry.id==resolved) {result=crate::tools::bash_output::with_monitor_state(result,id,entry.paused,registry.muted_dropped(&resolved));}tool_result(result)},
                    "bash_input"=>{let keys=call.params.get("keys").and_then(Value::as_array).map(|keys|keys.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default();tool_result(execute_bash_input(runtime,BashInputInput {bash_id:id,input:call.params.get("input").and_then(Value::as_str),keys:&keys,submit:call.params.get("submit").and_then(Value::as_bool)}))},
                    "bash_resize"=>tool_result(execute_bash_resize(runtime.as_deref(),id,call.params.get("cols").and_then(Value::as_f64).unwrap_or(f64::NAN),call.params.get("rows").and_then(Value::as_f64).unwrap_or(f64::NAN))),_=>unreachable!(),
                }
            })}));
            if name=="bash_output" {api.register_tool_with_renderers(tool,crate::tools::render::output_renderers()).expect("valid builtin bash_output registration");} else {api.register_tool(tool);}
        }
        let tool_manager=manager.clone();let tool_monitors=monitors.clone();let tool_notifier=notifier.clone();let tool_configuration=configuration.clone();
        api.register_tool_with_renderers(ToolDefinition::new("monitor","Subscribe to command output or file changes instead of polling.",crate::tools::monitor::monitor_schema(),Arc::new(move |call| {
            let manager=tool_manager.clone();let monitors=tool_monitors.clone();let notifier=tool_notifier.clone();let configuration=tool_configuration.clone();Box::pin(async move {
                let mut manager=manager.lock().map_err(|_|ToolError::Message("terminal manager state poisoned".to_owned()))?;
                let mut monitors=monitors.lock().map_err(|_|ToolError::Message("monitor registry state poisoned".to_owned()))?;
                let cwd=call.context.map(|context|context.cwd().to_path_buf()).unwrap_or(std::env::current_dir()?);
                let approved_parent=crate::tools::monitor::approved_parent_for(&call)?;
                let (settings,shell)=configuration.lock().map_err(|_|ToolError::Message("terminal configuration state poisoned".to_owned()))?.clone();
                let result=crate::tools::monitor::execute_configured_monitor(&mut manager,&mut monitors,&call.params,&cwd,approved_parent.as_deref(),shell.as_deref(),&settings);
                if call.params.get("action").and_then(Value::as_str)==Some("rearm") && let Some(notifier)=notifier.lock().map_err(|_|ToolError::Message("monitor notifier state poisoned".to_owned()))?.as_ref() {
                    notifier.resume(monitors.snapshot().iter().filter(|record|!record.paused).map(|record|record.id.clone()).collect()).map_err(ToolError::Message)?;
                }
                tool_result(result)
            })
        })),crate::tools::render::monitor_renderers()).expect("valid builtin monitor registration");
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
        let shutdown_bundle=bundle.clone();
        api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {let tasks=completion_tasks.clone();let notifier=cleanup_terminal_notifier.clone();Box::pin(async move {for task in tasks.lock().map_err(|_|ExtensionFailure::new("terminal completion tasks poisoned"))?.drain(..) {task.abort();}*notifier.lock().map_err(|_|ExtensionFailure::new("terminal notifier state poisoned"))?=crate::notify::TerminalNotifier::default();Ok(EventResult::None)})}));
        api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {let task=telemetry_task.clone();Box::pin(async move {if let Some(task)=task.lock().map_err(|_|ExtensionFailure::new("monitor telemetry state poisoned"))?.take() {task.abort();}Ok(EventResult::None)})}));
        api.on(EventKind::SessionShutdown,Arc::new(move |_,ctx| {let status=status_task.clone();Box::pin(async move {if let Some(task)=status.lock().map_err(|_|ExtensionFailure::new("monitor status state poisoned"))?.take() {task.abort();}ctx.ui.set_status(crate::monitor_status::MONITOR_STATUS_KEY,None);Ok(EventResult::None)})}));
        api.on(EventKind::SessionShutdown,Arc::new(move |event,ctx| {let bundle=shutdown_bundle.clone();let notifier=notifier.clone();Box::pin(async move {
            notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.take();
            let session_id=ctx.session_manager.session_id().to_owned();
            if matches!(event,maho_ext_api::types::ExtensionEvent::SessionShutdown(event) if event.reason==maho_ext_api::types::SessionReason::Reload) {
                bundle.lock().map_err(|_|ExtensionFailure::new("terminal bundle state poisoned"))?.monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.detach_delivery();
                crate::session_bundle::park_bundle(&session_id,bundle.lock().map_err(|_|ExtensionFailure::new("terminal bundle state poisoned"))?.clone()).map_err(|error|ExtensionFailure::new(error.to_string()))?;
            }else {
                bundle.lock().map_err(|_|ExtensionFailure::new("terminal bundle state poisoned"))?.teardown().map_err(|error|ExtensionFailure::new(error.to_string()))?;
                crate::session_bundle::teardown_parked_bundle(&session_id).map_err(|error|ExtensionFailure::new(error.to_string()))?;
            }
            Ok(EventResult::None)
        })}));
    }
}
#[cfg(test)]
mod tests {
    use super::*;use maho_ext_api::types::*;
    #[tokio::test]
    async fn registered_file_monitor_accessor_error_creates_no_registration()->Result<(),ToolError> {
        // Given a real registered ingress and an accessor that fails admission.
        let dir=tempfile::tempdir()?;let mut api=ExtensionApi::new(LoadedExtension::new("terminal",dir.path().to_owned(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);
        let context=crate::tools::monitor::tests::stub(Err("admission unavailable".into()));
        let monitor=&api.registered.tools[5].definition;
        // When the registered tool is invoked, the carrier error must escape.
        let error=(monitor.execute)(maho_tools::definition::ToolCall {id:"rejected",params:json!({"description":"watch","path":dir.path().join("file"),"persistent":true}),signal:Default::default(),on_update:None,context:Some(&context)}).await.expect_err("accessor error must escape registered ingress");
        assert!(error.to_string().contains("admission unavailable"));
        // Then kill-all observes zero registrations, and the first admitted file keeps watch_1.
        let empty=(api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {id:"empty",params:json!({"all":true}),signal:Default::default(),on_update:None,context:None}).await?;
        assert!(matches!(&empty.content[0],ToolContent::Text {text,..} if text=="Killed 0 session(s)."));
        let context=crate::tools::monitor::tests::stub(Ok(None));
        let admitted=(monitor.execute)(maho_tools::definition::ToolCall {id:"admitted",params:json!({"description":"watch","path":dir.path().join("file"),"persistent":true}),signal:Default::default(),on_update:None,context:Some(&context)}).await?;
        assert_eq!(admitted.details.as_ref().unwrap()["bash_id"],"watch_1");
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {id:"cleanup",params:json!({"all":true}),signal:Default::default(),on_update:None,context:None}).await?;Ok(())
    }
    #[tokio::test]
    async fn registered_file_monitor_stable_id_kill_releases_shared_capacity()->Result<(),ToolError> {
        let dir=tempfile::tempdir()?;let mut api=ExtensionApi::new(LoadedExtension::new("terminal",dir.path().to_owned(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);
        let context=crate::tools::monitor::tests::stub(Ok(None));
        let result=(api.registered.tools[5].definition.execute)(maho_tools::definition::ToolCall {id:"file",params:json!({"description":"watch","path":dir.path().join("file"),"persistent":true}),signal:Default::default(),on_update:None,context:Some(&context)}).await?;
        let id=result.details.as_ref().unwrap()["monitor_id"].as_str().unwrap();assert!(id.starts_with("mon_"));
        let result=(api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {id:"kill",params:json!({"bash_id":id}),signal:Default::default(),on_update:None,context:None}).await?;assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text==&format!("Killed {id}.")));
        let result=(api.registered.tools[5].definition.execute)(maho_tools::definition::ToolCall {id:"next",params:json!({"description":"next","path":dir.path().join("next"),"persistent":true}),signal:Default::default(),on_update:None,context:Some(&context)}).await?;assert!(result.details.as_ref().unwrap()["monitor_id"].as_str().unwrap().starts_with("mon_"));
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
    #[test] fn native_companions_register_flat_schemas_and_shutdown() {let mut api=ExtensionApi::new(LoadedExtension::new("terminal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);assert_eq!(api.registered.tools.iter().map(|tool|tool.definition.name.as_str()).collect::<Vec<_>>(),vec!["bash","bash_output","bash_input","bash_resize","kill_bash","monitor"]);for tool in &api.registered.tools {assert_eq!(tool.definition.parameters["type"],"object");assert!(tool.definition.parameters.get("properties").is_some());}assert_eq!(api.registered.handlers[&EventKind::SessionShutdown].len(),5);}
    #[tokio::test]
    async fn monitor_ending_reaches_native_event_bus_and_rpc() {
        let dir=tempfile::tempdir().unwrap();let bus=EventBus::default();let (sender,mut events)=tokio::sync::mpsc::unbounded_channel();let ended_sender=sender.clone();let rpc_sender=sender;
        let _ended=bus.on(crate::shared::TERMINAL_MONITOR_ENDED_EVENT,Arc::new(move |data| {ended_sender.send(("ended",data.clone())).unwrap();}));let _rpc=bus.on("senpi:extension-rpc-event",Arc::new(move |data| {rpc_sender.send(("rpc",data.clone())).unwrap();}));
        let mut api=ExtensionApi::new(LoadedExtension::new("terminal",dir.path().to_owned(),SourceInfo::default()),ExtensionSessionProfile::default(),bus,ExtensionRuntime::default());TerminalExtension.register(&mut api);
        let result=(api.registered.tools[5].definition.execute)(maho_tools::definition::ToolCall {id:"monitor",params:json!({"description":"ready","command":"stty -echo; printf 'ready\\n'"}),signal:Default::default(),on_update:None,context:None}).await.unwrap();assert!(result.details.as_ref().unwrap()["monitor_id"].as_str().is_some_and(|id|id.starts_with("mon_")));
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            let mut seen_ended=false;let mut seen_rpc=false;
            while !(seen_ended&&seen_rpc) {let (kind,data)=events.recv().await.unwrap();if kind=="ended" {assert_eq!(data["reason"],"exit");assert_eq!(data["exitCode"].as_i64(),Some(0));seen_ended=true;}else if kind=="rpc"&&data["name"]==crate::shared::TERMINAL_MONITOR_ENDED_EVENT {assert_eq!(data["data"]["reason"],"exit");seen_rpc=true;}}
        }).await.unwrap();
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {id:"cleanup",params:json!({"all":true}),signal:Default::default(),on_update:None,context:None}).await.unwrap();
    }
}
