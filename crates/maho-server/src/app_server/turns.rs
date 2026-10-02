use super::{projection::EventProjector, registry::{JsonRpcError, MethodRegistration, MethodScope}, server_core::ServerCore, thread_registry::ThreadRegistry, turn_log::{CompleteTurnOptions, CompleteTurnStatus, RecordTurnOptions, TurnLog}, turn_runtime::{build_turn, build_user_message, create_turn_id, parse_input}};
use maho_core::agent_session::PromptOptions;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub async fn register_turn_methods(core: &Arc<RwLock<ServerCore>>, threads: Arc<ThreadRegistry>, log: Arc<Mutex<TurnLog>>) {
    let weak = Arc::downgrade(core);
    let mut core = core.write().await;
    for method in ["turn/steer","turn/interrupt"] {
        let threads = threads.clone();
        let log = log.clone(); let weak = weak.clone();
        core.registry.register(method.into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
            let threads = threads.clone();
            let log = log.clone(); let weak = weak.clone();
            Box::pin(async move {
                let params = &context.request["params"];
                let id = params["threadId"].as_str().ok_or_else(||JsonRpcError::new(-32602,"Invalid params"))?;
                let entry = threads.get_loaded_thread(id).await.map_err(|error|JsonRpcError::new(-32600,error))?;
                let (session,active) = {let entry = entry.lock().await;(entry.session.clone(),entry.active_turn.clone())};
                let Some(active) = active else {return Err(JsonRpcError::new(-32600,format!("No active turn for thread {id}")));};
                let expected = if method == "turn/steer" {"expectedTurnId"} else {"turnId"};
                if params[expected].as_str() != Some(&active) {return Err(JsonRpcError::new(-32600,format!("Turn id mismatch: expected {} but active turn is {active}",params[expected].as_str().unwrap_or_default())));}
                if method == "turn/interrupt" {entry.lock().await.interrupted = true;session.abort().await;return Ok(json!({}));}
                let input = params["input"].as_array().ok_or_else(||JsonRpcError::new(-32602,"Invalid params"))?;
                let parsed = parse_input(input)?;
                session.steer(&parsed.text,None,maho_core::agent_session::QueuedInputOptions {source:Some(maho_ext_api::InputSource::Rpc),..Default::default()}).await.map_err(|error|JsonRpcError::new(-32603,error))?;
                let user = build_user_message(params["clientUserMessageId"].as_str(),&parsed.content);
                log.lock().await.append_item(id,&active,user.as_object().cloned().ok_or_else(||JsonRpcError::new(-32603,"Invalid user message"))?).map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
                if let Some(core) = weak.upgrade() {
                    let now = chrono::Utc::now().timestamp_millis();
                    emit(&core,&entry,json!({"method":"item/started","params":{"threadId":id,"turnId":active,"item":user,"startedAtMs":now}})).await;
                    emit(&core,&entry,json!({"method":"item/completed","params":{"threadId":id,"turnId":active,"item":user,"completedAtMs":now}})).await;
                }
                Ok(json!({"turnId":active}))
            })
        })});
    }
    core.registry.register("turn/start".into(), MethodRegistration { requires_init: true, experimental: false, scope: MethodScope::Thread, handler: Arc::new(move |context| {
        let threads = threads.clone(); let log = log.clone(); let weak = weak.clone();
        Box::pin(async move {
            let params = &context.request["params"];
            let id = params["threadId"].as_str().ok_or_else(|| JsonRpcError::new(-32602,"Invalid params"))?.to_owned();
            let input = params["input"].as_array().ok_or_else(|| JsonRpcError::new(-32602,"Invalid params"))?;
            let parsed = parse_input(input)?;
            let entry = threads.get_loaded_thread(&id).await.map_err(|error|JsonRpcError::new(-32600,error))?;
            let (session, cwd, tasks) = { let entry = entry.lock().await; (entry.session.clone(), entry.cwd.clone(), entry.tasks.clone()) };
            let guard = tasks.lock_owned().await;
            let turn_id = create_turn_id(); let now = chrono::Utc::now(); let started = now.timestamp_millis() as f64;
            { let mut entry = entry.lock().await; entry.active_turn = Some(turn_id.clone()); entry.interrupted = false; entry.updated_at = now.to_rfc3339_opts(chrono::SecondsFormat::Millis,true); }
            let user = build_user_message(params["clientUserMessageId"].as_str(), &parsed.content);
            { let mut log = log.lock().await; log.record_turn(&id, RecordTurnOptions { turn_id:turn_id.clone(), started_at:now.to_rfc3339_opts(chrono::SecondsFormat::Millis,true), status:None, completed_at:None, error:None }); log.append_item(&id,&turn_id,user.as_object().cloned().ok_or_else(||JsonRpcError::new(-32603,"Invalid user message"))?).map_err(|error|JsonRpcError::new(-32603,error.to_string()))?; }
            let turn = build_turn(&turn_id,"inProgress",started,None,&[],None);
            let response = json!({"turn":turn});
            context.connection.defer_until_responded(move || { tokio::spawn(async move {
                let _guard = guard;
                let Some(core) = weak.upgrade() else { return; };
                let initial = [json!({"method":"thread/status/changed","params":{"threadId":id,"status":{"type":"active","activeFlags":[]}}}), json!({"method":"turn/started","params":{"threadId":id,"turn":turn}}), json!({"method":"item/started","params":{"threadId":id,"turnId":turn_id,"item":user,"startedAtMs":started}}), json!({"method":"item/completed","params":{"threadId":id,"turnId":turn_id,"item":user,"completedAtMs":started}})];
                for notification in initial { emit(&core,&entry,notification).await; }
                let (send, mut events) = tokio::sync::mpsc::unbounded_channel();
                let subscription = session.subscribe(Arc::new(move |event| {
                    if let maho_ext_api::AgentSessionEvent::Agent(event) = event {
                        match serde_json::to_value(event) { Ok(event) => { if send.send(event).is_err() { eprintln!("app-server turn event receiver closed"); } }, Err(error) => eprintln!("app-server event encoding failed: {error}") }
                    }
                }));
                let prompt = session.prompt(&parsed.text, PromptOptions { source:Some(maho_ext_api::InputSource::Rpc), ..Default::default() });
                tokio::pin!(prompt);
                let mut projector = EventProjector::new(id.clone(),turn_id.clone(),cwd);
                let result = loop { tokio::select! {
                    result = &mut prompt => break result,
                    event = events.recv() => if let Some(event) = event { project(&core,&entry,&log,&id,&turn_id,&mut projector,&event).await; },
                } };
                drop(subscription);
                while let Ok(event) = events.try_recv() { project(&core,&entry,&log,&id,&turn_id,&mut projector,&event).await; }
                for notification in projector.finalize() {
                    if notification["method"] == "item/completed" && let Some(item) = notification["params"]["item"].as_object()
                        && let Err(error) = log.lock().await.append_item(&id,&turn_id,item.clone()) { eprintln!("app-server turn log: {error}"); }
                    emit(&core,&entry,notification).await;
                }
                let completion = chrono::Utc::now();
                let interrupted = entry.lock().await.interrupted;
                let status = if interrupted { CompleteTurnStatus::Interrupted } else if result.is_ok() { CompleteTurnStatus::Completed } else { CompleteTurnStatus::Failed };
                let items = { let mut log = log.lock().await; if let Err(error) = log.complete_turn(&id,&turn_id,CompleteTurnOptions {status,completed_at:completion.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),error:result.as_ref().err().cloned()}) {eprintln!("app-server turn log: {error}");} super::turn_runtime::read_logged_items(&mut log,&id,&turn_id) };
                { let mut entry = entry.lock().await; entry.active_turn = None; entry.updated_at = completion.to_rfc3339_opts(chrono::SecondsFormat::Millis,true); }
                let turn = build_turn(&turn_id,if interrupted {"interrupted"} else if result.is_ok() {"completed"} else {"failed"},started,Some(completion.timestamp_millis() as f64),&items,result.as_ref().err().map(String::as_str));
                emit(&core,&entry,json!({"method":"thread/status/changed","params":{"threadId":id,"status":{"type":"idle"}}})).await;
                for notification in super::turn_terminal::turn_terminal_notifications(&id,turn) {emit(&core,&entry,notification).await;}
            }); });
            Ok(response)
        })
    }) });
}
async fn emit(core: &Arc<RwLock<ServerCore>>, entry: &Arc<Mutex<super::thread_registry::ThreadEntry>>, notification: Value) {
    if notification["method"] == "thread/status/changed" {
        if let Err(error) = core.read().await.broadcast_notification(notification,chrono::Utc::now().timestamp_millis() as u64).await { eprintln!("app-server notification: {}",error.message); }
        return;
    }
    let subscribers = entry.lock().await.subscribers.clone();
    let mut delivered = false;
    for id in subscribers {
        match core.read().await.send_notification_to_connection(&id,notification.clone(),chrono::Utc::now().timestamp_millis() as u64).await {
            Ok(sent) => delivered |= sent,
            Err(error) => eprintln!("app-server notification: {}",error.message),
        }
    }
    if !delivered && matches!(notification["method"].as_str(),Some("turn/completed"|"error")) {
        let mut entry = entry.lock().await;
        entry.queued_terminal_notifications.push(notification);
        if entry.queued_terminal_notifications.len() > 100 { entry.queued_terminal_notifications.remove(0); }
    }
}
async fn project(core: &Arc<RwLock<ServerCore>>, entry: &Arc<Mutex<super::thread_registry::ThreadEntry>>, log: &Mutex<TurnLog>, thread_id: &str, turn_id: &str, projector: &mut EventProjector, event: &Value) {
    for notification in projector.project(event).notifications {
        if notification["method"] == "item/completed" && let Some(item) = notification["params"]["item"].as_object()
            && let Err(error) = log.lock().await.append_item(thread_id,turn_id,item.clone()) { eprintln!("app-server turn log: {error}"); }
        emit(core,entry,notification).await;
    }
}
