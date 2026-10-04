use super::{registry::{JsonRpcError, MethodRegistration, MethodScope}, server_core::ServerCore, thread_registry::{SessionFactory, ThreadRegistry}, turn_log::TurnLog, wire_thread::build_wire_thread};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub struct AppServerRuntime {
    pub core: Arc<RwLock<ServerCore>>,
    pub threads: Arc<ThreadRegistry>,
    pub turn_log: Arc<Mutex<TurnLog>>,
    fuzzy_search: super::fuzzy_search_service::FuzzyFileSearchService,
    pub mcp_inventory: Arc<Mutex<super::mcp_wire_status::McpWireStatusRegistry>>,
    pub approvals: Arc<std::sync::Mutex<super::approval_bridge::ApprovalBridge>>,
    pub user_input: Arc<std::sync::Mutex<super::user_input_bridge::UserInputBridge>>,
    pub lifecycle: Arc<super::handlers::ThreadLifecycleController>,
    provider_account_unsubscribe: std::sync::Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>,
}
fn required_string<'a>(params: &'a Value, key: &str) -> Result<&'a str, JsonRpcError> {
    params[key].as_str().filter(|value| !value.is_empty()).ok_or_else(|| JsonRpcError::new(-32603, format!("Invalid params: {key} is required")))
}
impl AppServerRuntime {
    pub async fn new(agent_dir: String, cwd: String, version: String, session_dir: Option<String>, factory: Option<SessionFactory>) -> Self {
        let threads = Arc::new(ThreadRegistry::new(agent_dir.clone(), session_dir, factory));
        let turn_log = Arc::new(Mutex::new(TurnLog::default()));
        let mut core = ServerCore::new(agent_dir.clone(), version.clone(), "Linux".into(), String::new(), "x64".into(), "linux".into());
        super::account::register_account_methods(&mut core.registry, agent_dir.clone());
        super::config::register_config_methods(&mut core.registry, agent_dir.clone(), cwd.clone());
        super::models::register_remote_status_method(&mut core.registry,agent_dir.clone());
        let model_directory = agent_dir.clone();
        super::models::register_model_list_method(&mut core.registry,Arc::new(move || {
            let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {models_path:Some(std::path::Path::new(&model_directory).join("models.json")),auth_path:Some(std::path::Path::new(&model_directory).join("auth.json")),..Default::default()});
            maho_core::model_registry::ModelRegistry::new(runtime).get_available()
        }));
        let process_inventory = super::mcp_wire_status::create_process_mcp_wire_status_adapter(std::path::Path::new(&agent_dir),std::path::Path::new(&cwd),&std::env::vars().collect()).unwrap_or_else(|error| {eprintln!("app-server MCP configuration: {error}");super::mcp_wire_status::McpWireStatusAdapter::new(Default::default())});
        let mcp_inventory = Arc::new(Mutex::new(super::mcp_wire_status::McpWireStatusRegistry::new(Some(process_inventory))));
        super::catalogs::register_catalog_methods(&mut core.registry,threads.clone(),mcp_inventory.clone(),agent_dir.clone(),cwd.clone());
        super::skills::register_skill_methods(&mut core.registry, agent_dir, cwd.clone());
        let notification_core = Arc::new(std::sync::OnceLock::<std::sync::Weak<RwLock<ServerCore>>>::new());
        let lifecycle_slot=Arc::new(std::sync::OnceLock::<Arc<super::handlers::ThreadLifecycleController>>::new());
        let recipients = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<String,std::collections::BTreeMap<String,super::notifications::SendMessage>>::new()));
        let send_recipients = recipients.clone();
        let send: super::approval_types::SendToThreadSubscribers = Arc::new(move |id,message| {
            let sends = send_recipients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(id).map(|clients|clients.values().cloned().collect::<Vec<_>>()).unwrap_or_default();
            let count = sends.len();
            for send in sends {let message = super::envelope::populate_outbound_notification(message.clone(),chrono::Utc::now().timestamp_millis() as u64);tokio::spawn(async move {if let Err(error) = send(message).await {eprintln!("app-server request delivery: {}",error.message);}});}
            count
        });
        let approvals = Arc::new(std::sync::Mutex::new(super::approval_bridge::ApprovalBridge::new(send.clone())));
        let user_input = Arc::new(std::sync::Mutex::new(super::user_input_bridge::UserInputBridge::new(send)));
        core.approvals = Some(approvals.clone());core.user_input = Some(user_input.clone());
        let fuzzy_core = notification_core.clone();
        let fuzzy_search = super::fuzzy_search_service::FuzzyFileSearchService::new(Arc::new(move |notification| {
            if let Some(core) = fuzzy_core.get().and_then(std::sync::Weak::upgrade) {tokio::spawn(async move {if let Err(error) = core.read().await.broadcast_notification(notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server fuzzy search notification: {}",error.message);}});}
        }));
        super::fuzzy_search_methods::register_fuzzy_file_search_methods(&mut core.registry,fuzzy_search.clone());
        for method in ["thread/start", "thread/resume", "thread/read", "thread/unsubscribe", "thread/loaded/list", "thread/name/set"] {
            let threads = threads.clone(); let turn_log = turn_log.clone(); let cwd = cwd.clone(); let version = version.clone();
            let notification_core = notification_core.clone();
            let lifecycle_slot=lifecycle_slot.clone();
            let recipients = recipients.clone();let approvals = approvals.clone();let user_input = user_input.clone();
            let mcp_inventory = mcp_inventory.clone();
            core.registry.register(method.into(), MethodRegistration { requires_init: true, experimental: false, scope: MethodScope::Thread, handler: Arc::new(move |context| {
                let threads = threads.clone(); let turn_log = turn_log.clone(); let cwd = cwd.clone(); let version = version.clone();
                let notification_core = notification_core.clone();
                let lifecycle_slot=lifecycle_slot.clone();
                let recipients = recipients.clone();let approvals = approvals.clone();let user_input = user_input.clone();
                let mcp_inventory = mcp_inventory.clone();
                Box::pin(async move {
                    let params = &context.request["params"];
                    if method == "thread/loaded/list" { return Ok(json!({"data":threads.list_loaded().await.iter().map(|thread|thread["id"].clone()).collect::<Vec<_>>(),"nextCursor":null})); }
                    if method == "thread/unsubscribe" {
                        let id = required_string(params,"threadId")?;
                        let Ok(entry) = threads.get_loaded_thread(id).await else {return Ok(json!({"status":"notLoaded"}));};
                        let removed = entry.lock().await.subscribers.remove(&context.connection.id);
                        if let Some(clients) = recipients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(id) {clients.remove(&context.connection.id);}
                        if removed && let Some(lifecycle)=lifecycle_slot.get() {lifecycle.schedule_idle_unload_for_thread(id).await;}
                        return Ok(json!({"status":if removed {"unsubscribed"} else {"notSubscribed"}}));
                    }
                    let requested_name = if method == "thread/name/set" {Some(required_string(params,"name")?.to_owned())} else {None};
                    let entry = if method == "thread/start" {
                        let model = params["model"].as_str().and_then(|model| super::start_options::parse_model_reference(model,params["modelProvider"].as_str())).and_then(|(provider,id)| maho_ai::providers::all::get_builtin_models(provider).into_iter().find(|model|model.id == id));
                        let cwd = maho_core::paths::resolve_path(params["cwd"].as_str().unwrap_or(&cwd),&cwd,&Default::default());
                        threads.create_thread(cwd, model).await
                    } else { threads.resume_thread(required_string(params, "threadId")?).await }.map_err(|error| JsonRpcError::new(-32603, error))?;
                    let mut entry = entry.lock().await;
                    if let Some(name) = requested_name {
                        entry.session.set_session_name(&name);
                        let id = entry.id.clone();
                        context.connection.defer_until_responded(move || {tokio::spawn(async move {
                            if let Some(core) = notification_core.get().and_then(std::sync::Weak::upgrade)
                                && let Err(error) = core.read().await.broadcast_notification(json!({"method":"thread/name/updated","params":{"threadId":id,"threadName":name}}),chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server name notification: {}",error.message);}
                        });});
                        return Ok(json!({}));
                    }
                    if let Some(lifecycle)=lifecycle_slot.get() {lifecycle.clear_idle_timer(&entry.id);}
                    if method != "thread/read" { entry.subscribers.insert(context.connection.id.clone()); }
                    let mut log = turn_log.lock().await;
                    let wire = build_wire_thread(&entry, &mut log, method == "thread/resume" || params["includeTurns"] == true, &version).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
                    if method == "thread/read" { return Ok(json!({"thread":wire})); }
                    let model = entry.session.model();
                    let tier = entry.session.service_tier().map(|tier| match tier { maho_ext_api::ServiceTier::Auto => "auto", maho_ext_api::ServiceTier::Flex => "flex", maho_ext_api::ServiceTier::Priority => "priority" });
                    let mut response = json!({"thread":wire,"model":model.id,"modelProvider":model.provider,"serviceTier":tier,"cwd":entry.cwd,"runtimeWorkspaceRoots":[entry.cwd],"instructionSources":[],"approvalPolicy":super::start_options::requested_approval_policy(params),"approvalsReviewer":"user","sandbox":{"type":"dangerFullAccess"},"activePermissionProfile":null,"reasoningEffort":entry.session.thinking_level(),"multiAgentMode":"explicitRequestOnly"});
                    if method == "thread/resume" { response["initialTurnsPage"] = Value::Null; }
                    let queued = std::mem::take(&mut entry.queued_terminal_notifications);
                    let client_id = context.connection.id.clone();
                    let thread_id = entry.id.clone();
                    let mcp_session = entry.session.clone();
                    let lifecycle = if method == "thread/start" {json!({"method":"thread/started","params":{"thread":response["thread"]}})} else {json!({"method":"thread/status/changed","params":{"threadId":entry.id,"status":{"type":"idle"}}})};
                    context.connection.defer_until_responded(move || {tokio::spawn(async move {
                        if let Some(core) = notification_core.get().and_then(std::sync::Weak::upgrade) {
                            let core = core.read().await;
                            if let Some(connection) = core.get_connection(&client_id) {
                                recipients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(thread_id.clone()).or_default().insert(client_id.clone(),connection.send.clone());
                                approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replay_pending_for_thread(&thread_id);
                                user_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replay_pending_for_thread(&thread_id);
                            }
                            if let Err(error) = core.broadcast_notification(lifecycle,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server lifecycle notification: {}",error.message);}
                            for notification in queued {if let Err(error) = core.send_notification_to_connection(&client_id,notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server terminal replay: {}",error.message);}}
                        }
                        let holder = Arc::new(std::sync::Mutex::new(super::mcp_wire_status::McpWireStatusAdapter::new(maho_ext_mcp::service_types::McpWireStatusSnapshot::default())));
                        let handler_holder = holder.clone();
                        let subscription = mcp_session.subscribe_mcp_wire_status::<maho_ext_mcp::service_types::McpWireStatusSnapshot>(Arc::new(move |snapshot| {handler_holder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).update(snapshot.clone());})).await;
                        if let Some(subscription) = subscription {holder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).bind_live_updates(move || drop(subscription));}
                        mcp_inventory.lock().await.register_thread(thread_id.clone(),holder);
                        let turn_thread_id = thread_id.clone();
                        if let Ok(ui) = super::approval_ui_context::AppServerUiContext::new(approvals.clone(),user_input.clone(),thread_id.clone(),Arc::new(move || turn_thread_id.clone()),std::path::Path::new(&ui_agent_dir)) {let _ = mcp_session.rebind_extension_ui(Arc::new(ui)).await;}
                    });});
                    Ok(response)
                })
            }) });
        }
        {
            let threads = threads.clone(); let turn_log = turn_log.clone(); let version = version.clone();
            let notification_core = notification_core.clone();
            let lifecycle_slot=lifecycle_slot.clone();
            let recipients = recipients.clone();let approvals = approvals.clone();let user_input = user_input.clone();
            let mcp_inventory = mcp_inventory.clone();
            core.registry.register("thread/fork".into(), MethodRegistration { requires_init: true, experimental: false, scope: MethodScope::Thread, handler: Arc::new(move |context| {
                let threads = threads.clone(); let turn_log = turn_log.clone(); let version = version.clone();
                let notification_core = notification_core.clone();
                let lifecycle_slot=lifecycle_slot.clone();
                let recipients = recipients.clone();let approvals = approvals.clone();let user_input = user_input.clone();
                let mcp_inventory = mcp_inventory.clone();
                Box::pin(async move {
                    let params = &context.request["params"];
                    let source = required_string(params,"threadId")?.to_owned();
                    let requested_cwd = params["cwd"].as_str().map(str::to_owned);
                    let entry = threads.fork_thread(&source,requested_cwd).await.map_err(|error| JsonRpcError::new(-32603, error))?;
                    let mut entry = entry.lock().await;
                    if let Some(lifecycle)=lifecycle_slot.get() {lifecycle.clear_idle_timer(&entry.id);}
                    entry.subscribers.insert(context.connection.id.clone());
                    let mut log = turn_log.lock().await;
                    let mut wire = build_wire_thread(&entry, &mut log, true, &version).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
                    wire["forkedFromId"] = json!(source);
                    let model = entry.session.model();
                    let tier = entry.session.service_tier().map(|tier| match tier { maho_ext_api::ServiceTier::Auto => "auto", maho_ext_api::ServiceTier::Flex => "flex", maho_ext_api::ServiceTier::Priority => "priority" });
                    let response = json!({"thread":wire,"model":model.id,"modelProvider":model.provider,"serviceTier":tier,"cwd":entry.cwd,"runtimeWorkspaceRoots":[entry.cwd],"instructionSources":[],"approvalPolicy":"never","approvalsReviewer":"user","sandbox":{"type":"dangerFullAccess"},"activePermissionProfile":null,"reasoningEffort":entry.session.thinking_level(),"multiAgentMode":"explicitRequestOnly"});
                    let queued = std::mem::take(&mut entry.queued_terminal_notifications);
                    let client_id = context.connection.id.clone();
                    let thread_id = entry.id.clone();
                    let mcp_session = entry.session.clone();
                    let started = json!({"method":"thread/started","params":{"thread":response["thread"]}});
                    context.connection.defer_until_responded(move || {tokio::spawn(async move {
                        if let Some(core) = notification_core.get().and_then(std::sync::Weak::upgrade) {
                            let core = core.read().await;
                            if let Some(connection) = core.get_connection(&client_id) {
                                recipients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(thread_id.clone()).or_default().insert(client_id.clone(),connection.send.clone());
                                approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replay_pending_for_thread(&thread_id);
                                user_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replay_pending_for_thread(&thread_id);
                            }
                            if let Err(error) = core.broadcast_notification(started,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server fork notification: {}",error.message);}
                            for notification in queued {if let Err(error) = core.send_notification_to_connection(&client_id,notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server fork terminal replay: {}",error.message);}}
                        }
                        let holder = Arc::new(std::sync::Mutex::new(super::mcp_wire_status::McpWireStatusAdapter::new(maho_ext_mcp::service_types::McpWireStatusSnapshot::default())));
                        let handler_holder = holder.clone();
                        let subscription = mcp_session.subscribe_mcp_wire_status::<maho_ext_mcp::service_types::McpWireStatusSnapshot>(Arc::new(move |snapshot| {handler_holder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).update(snapshot.clone());})).await;
                        if let Some(subscription) = subscription {holder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).bind_live_updates(move || drop(subscription));}
                        mcp_inventory.lock().await.register_thread(thread_id.clone(),holder);
                        let turn_thread_id = thread_id.clone();
                        if let Ok(ui) = super::approval_ui_context::AppServerUiContext::new(approvals.clone(),user_input.clone(),thread_id.clone(),Arc::new(move || turn_thread_id.clone()),std::path::Path::new(&ui_agent_dir)) {let _ = mcp_session.rebind_extension_ui(Arc::new(ui)).await;}
                    });});
                    Ok(response)
                })
            })});
        }
        let disconnected_threads = threads.clone();
        let disconnected_lifecycle=lifecycle_slot.clone();
        core.on_disconnect = Some(Arc::new(move |id| {
            recipients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|_,clients| {clients.remove(&id);!clients.is_empty()});
            let threads = disconnected_threads.clone();let lifecycle=disconnected_lifecycle.clone(); tokio::spawn(async move {
                let affected=threads.remove_connection(&id).await;
                if let Some(lifecycle)=lifecycle.get() {for id in affected {lifecycle.schedule_idle_unload_for_thread(&id).await;}}
            });
        }));
        let core = Arc::new(RwLock::new(core));
        if notification_core.set(Arc::downgrade(&core)).is_err() {unreachable!("notification core is initialized once");}
        let lifecycle=super::handlers::ThreadLifecycleController::new(Arc::downgrade(&core),threads.clone(),mcp_inventory.clone(),std::time::Duration::from_secs(30*60));
        if lifecycle_slot.set(lifecycle.clone()).is_err() {unreachable!("lifecycle initialized once");}
        super::turns::register_turn_methods(&core, threads.clone(), turn_log.clone()).await;
        super::goal_handlers::register_thread_goal_handlers(&core,threads.clone()).await;
        super::settings_handlers::register_thread_settings(&core,threads.clone()).await;
        let archive = Arc::new(super::archive_state::ThreadArchiveState::new(threads.session_dir.as_ref().map(Into::into)));
        super::handlers::register_storage_lifecycle_handlers(&core,threads.clone(),archive.clone(),mcp_inventory.clone(),version.clone()).await;
        super::handlers::register_compaction_handler(&core,threads.clone(),turn_log.clone()).await;
        super::metadata_handlers::register_metadata_handlers(&core,threads.clone(),turn_log.clone(),archive.clone(),version.clone()).await;
        super::list_handlers::register_list_handlers(&core,threads.clone(),archive.clone(),version.clone()).await;
        super::search::register_search_handler(&core,threads.clone(),archive.clone(),turn_log.clone(),version).await;
        super::history_handlers::register_history_handlers(&core,threads.clone(),archive,turn_log.clone()).await;
        let provider_core = Arc::downgrade(&core);
        let provider_account_unsubscribe = maho_core::subscribe_provider_account_events(Arc::new(move |event| {
            let notification = super::account::provider_account_event_notification_native(&event);
            if let Some(core) = provider_core.upgrade() {
                tokio::spawn(async move { if let Err(error) = core.read().await.broadcast_notification(notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server provider account notification: {}",error.message);} });
            }
        }));
        Self { core, threads, turn_log, fuzzy_search, mcp_inventory,approvals,user_input,lifecycle, provider_account_unsubscribe:std::sync::Mutex::new(Some(provider_account_unsubscribe)) }
    }
    pub async fn dispose(&self) {
        if let Some(unsubscribe) = self.provider_account_unsubscribe.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {unsubscribe();}
        self.lifecycle.dispose();
        for thread in self.threads.list_loaded().await {if let Some(id) = thread["id"].as_str() {
            self.approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_pending_for_thread(id);
            self.user_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_pending_for_thread(id);
        }}
        self.fuzzy_search.dispose();self.threads.dispose().await;
    }
}
