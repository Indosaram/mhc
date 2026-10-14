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
    /// senpi `AppServerRuntime.turns`.
    pub turns: super::turns::TurnEngine,
    provider_account_unsubscribe: std::sync::Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>,
    /// senpi `createAppServerRuntime(requestShutdown)`: extension `ctx.shutdown()` lands here.
    pub shutdown_request: Arc<AppServerShutdownRequest>,
}
/// The runtime half of senpi `requestShutdown`: the first reason wins and wakes the mode loop.
#[derive(Default)]
pub struct AppServerShutdownRequest {
    reason: std::sync::Mutex<Option<String>>,
    notify: tokio::sync::Notify,
}
impl AppServerShutdownRequest {
    pub fn request(&self, reason: &str) {
        let mut current = self.reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.is_none() { *current = Some(reason.to_owned()); self.notify.notify_one(); }
    }
    pub async fn requested(&self) -> String {
        loop {
            if let Some(reason) = self.reason.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() { return reason; }
            self.notify.notified().await;
        }
    }
}
type ThreadNotificationSender = tokio::sync::mpsc::UnboundedSender<(String, Value)>;
fn queue_thread_notification(sender: &ThreadNotificationSender, thread_id: &str, notification: Value) {
    if sender.send((thread_id.to_owned(), notification)).is_err() { eprintln!("app-server thread notification dropped: runtime is closed"); }
}
/// senpi `createBoundAppServerSession` bindings: relay extension RPC events as `extension_event`,
/// extension errors as thread `error` notifications, and `ctx.shutdown()` as a server shutdown.
/// `binding` holds the pinned `initialNotifications` while `bindingExtensions` is true.
type BindingNotifications = Arc<std::sync::Mutex<Option<Vec<Value>>>>;
fn app_server_extension_bindings(sender: ThreadNotificationSender, shutdown: Arc<AppServerShutdownRequest>, binding: BindingNotifications) -> maho_core::agent_session::ExtensionBindingsFactory {
    Arc::new(move |session: &maho_core::agent_session::AgentSession| {
        let thread_id = session.session_id();
        let events = sender.clone();
        let event_thread = thread_id.clone();
        let binding = binding.clone();
        session.add_extension_rpc_event_listener(Arc::new(move |name: &str, data: &Value| {
            let notification = json!({"method":"extension_event","params":{"type":"extension_event","name":name,"data":data,"threadId":event_thread}});
            if let Some(initial) = binding.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() { initial.push(notification); return; }
            queue_thread_notification(&events, &event_thread, notification);
        }));
        let errors = sender.clone();
        let shutdown = shutdown.clone();
        maho_core::agent_session::ExtensionBindings {
            on_error: Some(Arc::new(move |error: &maho_ext_api::ExtensionError| {
                let mut params = json!({"extensionPath":error.extension_path,"event":error.event,"error":error.error});
                if let Some(stack) = &error.stack { params["stack"] = json!(stack); }
                queue_thread_notification(&errors, &thread_id, json!({"method":"error","params":params}));
            })),
            shutdown_handler: Some(Arc::new(move || shutdown.request("extension shutdown"))),
            ..Default::default()
        }
    })
}
/// Wrap a session factory so every created session is bound for app-server delivery. A factory
/// that never runs the first bind with the supplied bindings is rebound after it returns.
fn bind_app_server_session_factory(inner: SessionFactory, sender: ThreadNotificationSender, shutdown: Arc<AppServerShutdownRequest>, registry: Arc<std::sync::OnceLock<std::sync::Weak<ThreadRegistry>>>) -> SessionFactory {
    Arc::new(move |mut options: maho_core::sdk::CreateAgentSessionOptions| {
        let binding: BindingNotifications = Arc::new(std::sync::Mutex::new(Some(Vec::new())));
        let bindings = app_server_extension_bindings(sender.clone(), shutdown.clone(), binding.clone());
        let bound = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mark = bound.clone();
        let first_bind = bindings.clone();
        options.extension_bindings = Some(Arc::new(move |session: &maho_core::agent_session::AgentSession| {
            mark.store(true, std::sync::atomic::Ordering::SeqCst);
            first_bind(session)
        }));
        let inner = inner.clone();
        let registry = registry.clone();
        Box::pin(async move {
            let session = inner(options).await;
            {
                // End `bindingExtensions` and hand the buffer over under one lock, so no live
                // event can be queued ahead of the bind-time notifications.
                let mut buffered = binding.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let initial = buffered.take().unwrap_or_default();
                if let (Ok(session), Some(registry)) = (&session, registry.get().and_then(std::sync::Weak::upgrade)) { registry.queue_initial_notifications(&session.session_id(), initial); }
            }
            let session = session?;
            if !bound.load(std::sync::atomic::Ordering::SeqCst) { session.rebind_extension_context(bindings(&session)).await; }
            Ok::<_, String>(session)
        })
    })
}

/// senpi `createMcpWireStatusAdapter` + `bindLiveUpdates`: the session-owned MCP inventory of a thread.
async fn register_mcp_inventory(inventory: &Mutex<super::mcp_wire_status::McpWireStatusRegistry>, thread_id: &str, session: &maho_core::agent_session::AgentSession) {
    if inventory.lock().await.resolve(Some(thread_id)).is_some() {return;}
    let holder = Arc::new(std::sync::Mutex::new(super::mcp_wire_status::McpWireStatusAdapter::new(Default::default())));
    let handler_holder = holder.clone();
    let subscription = session.bind_mcp_wire_status::<maho_ext_mcp::service_types::McpWireStatusSnapshot>(Arc::new(move |snapshot| {handler_holder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).update(snapshot.clone());})).await;
    if let Some(subscription) = subscription {holder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).bind_live_updates(move || drop(subscription));}
    inventory.lock().await.register_thread(thread_id.to_owned(),holder);
}
fn required_string<'a>(params: &'a Value, key: &str) -> Result<&'a str, JsonRpcError> {
    params[key].as_str().filter(|value| !value.is_empty()).ok_or_else(|| JsonRpcError::new(-32603, format!("Invalid params: {key} is required")))
}
fn node_os_type() -> &'static str {
    match std::env::consts::OS { "macos" => "Darwin", "windows" => "Windows_NT", "linux" => "Linux", "freebsd" => "FreeBSD", "openbsd" => "OpenBSD", _ => "Unknown" }
}
#[cfg(unix)]
fn node_os_release() -> String { rustix::system::uname().release().to_string_lossy().into_owned() }
#[cfg(not(unix))]
fn node_os_release() -> String { String::new() }
fn node_arch() -> &'static str {
    match std::env::consts::ARCH { "x86_64" => "x64", "aarch64" => "arm64", "x86" => "ia32", other => other }
}
fn node_platform() -> &'static str {
    match std::env::consts::OS { "macos" => "darwin", "windows" => "win32", other => other }
}
impl AppServerRuntime {
    pub async fn new(agent_dir: String, cwd: String, version: String, session_dir: Option<String>, factory: Option<SessionFactory>) -> Self {
        let (thread_notification_sender, mut thread_notifications) = tokio::sync::mpsc::unbounded_channel::<(String, Value)>();
        let shutdown_request = Arc::new(AppServerShutdownRequest::default());
        let registry_slot = Arc::new(std::sync::OnceLock::<std::sync::Weak<ThreadRegistry>>::new());
        let factory = bind_app_server_session_factory(factory.unwrap_or_else(super::thread_registry::default_session_factory), thread_notification_sender, shutdown_request.clone(), registry_slot.clone());
        let threads = Arc::new(ThreadRegistry::new(agent_dir.clone(), session_dir, Some(factory)));
        if registry_slot.set(Arc::downgrade(&threads)).is_err() {unreachable!("thread registry is initialized once");}
        let thread_notification_core = Arc::new(std::sync::OnceLock::<std::sync::Weak<RwLock<ServerCore>>>::new());
        {
            // senpi `notifications.toThread`: one ordered consumer, queued as initial notifications
            // until the thread entry exists, then routed to subscribers past the allowlist.
            let threads = Arc::downgrade(&threads);
            let delivery_core = thread_notification_core.clone();
            tokio::spawn(async move {
                while let Some((thread_id, notification)) = thread_notifications.recv().await {
                    let Some(registry) = threads.upgrade() else { break; };
                    let entry = registry.loaded_or_queue_initial(&thread_id, notification.clone()).await;
                    drop(registry);
                    let Some(entry) = entry else { continue; };
                    let subscribers = entry.lock().await.subscribers.clone();
                    let mut routed = false;
                    if let Some(core) = delivery_core.get().and_then(std::sync::Weak::upgrade) {
                        for id in subscribers {
                            match core.read().await.send_thread_notification_to_connection(&id, notification.clone(), chrono::Utc::now().timestamp_millis() as u64).await {
                                Ok(sent) => routed |= sent,
                                Err(error) => eprintln!("app-server thread notification: {}", error.message),
                            }
                        }
                    }
                    if !routed && notification["method"] == "error" {
                        let mut entry = entry.lock().await;
                        entry.queued_terminal_notifications.push(notification);
                        if entry.queued_terminal_notifications.len() > 100 { entry.queued_terminal_notifications.remove(0); }
                    }
                }
            });
        }
        let turn_log = Arc::new(Mutex::new(TurnLog::default()));
        let mut core = ServerCore::new(agent_dir.clone(), version.clone(), node_os_type().into(), node_os_release(), node_arch().into(), node_platform().into());
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
        let ui_agent_dir = agent_dir.clone();
        // Pinned runtime `resourceLoaderFactory`: a reloaded `DefaultResourceLoader` per cwd.
        super::skills::register_skill_methods_with_loader(&mut core.registry, agent_dir, cwd.clone(), threads.clone(), super::skills::resource_skill_loader(Vec::new()));
        let notification_core = Arc::new(std::sync::OnceLock::<std::sync::Weak<RwLock<ServerCore>>>::new());
        let lifecycle_slot=Arc::new(std::sync::OnceLock::<Arc<super::handlers::ThreadLifecycleController>>::new());
        let turn_engine_slot=Arc::new(std::sync::OnceLock::<super::turns::TurnEngine>::new());
        let recipients = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<String,std::collections::BTreeMap<String,super::notifications::SendMessage>>::new()));
        let send_recipients = recipients.clone();
        let send: super::approval_types::SendToThreadSubscribers = Arc::new(move |id,message| {
            let sends = send_recipients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(id).map(|clients|clients.values().cloned().collect::<Vec<_>>()).unwrap_or_default();
            let count = sends.len();
            for send in sends {let delivery = send(super::envelope::populate_outbound_notification(message.clone(),chrono::Utc::now().timestamp_millis() as u64));tokio::spawn(async move {if let Err(error) = delivery.await {eprintln!("app-server request delivery: {}",error.message);}});}
            count
        });
        let approvals = Arc::new(std::sync::Mutex::new(super::approval_bridge::ApprovalBridge::new(send.clone())));
        let user_input = Arc::new(std::sync::Mutex::new(super::user_input_bridge::UserInputBridge::new(send)));
        core.approvals = Some(approvals.clone());core.user_input = Some(user_input.clone());
        {
            let (ui_approvals,ui_input,ui_dir) = (approvals.clone(),user_input.clone(),ui_agent_dir.clone());
            let ui_active_turns = threads.active_turn_ids();
            let ui_factory: super::thread_registry::SessionUiFactory = Arc::new(move |thread_id: &str| -> Option<Arc<dyn maho_ext_api::ExtensionUi>> {
                let (turn_thread_id, active) = (thread_id.to_owned(), ui_active_turns.clone());
                super::approval_ui_context::AppServerUiContext::new(ui_approvals.clone(),ui_input.clone(),thread_id.to_owned(),Arc::new(move || super::thread_registry::active_turn_id_or_default(&active,&turn_thread_id)),std::path::Path::new(&ui_dir))
                    .map(|ui| Arc::new(ui) as Arc<dyn maho_ext_api::ExtensionUi>).map_err(|error| eprintln!("app-server UI context: {}",error.message)).ok()
            });
            let binder_inventory = mcp_inventory.clone();
            let binder: super::thread_registry::SessionBinder = Arc::new(move |thread_id: String, session: maho_core::agent_session::AgentSession, ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>| -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
                let inventory = binder_inventory.clone();
                Box::pin(async move {
                    if let Some(ui) = ui && let Err(error) = session.rebind_extension_ui(ui).await {eprintln!("app-server UI binding: {error}");}
                    register_mcp_inventory(&inventory,&thread_id,&session).await;
                })
            });
            threads.bind_sessions(ui_factory,binder);
        }
        let fuzzy_core = notification_core.clone();
        let fuzzy_search = super::fuzzy_search_service::FuzzyFileSearchService::new(Arc::new(move |notification| {
            if let Some(core) = fuzzy_core.get().and_then(std::sync::Weak::upgrade) {tokio::spawn(async move {if let Err(error) = core.read().await.broadcast_notification(notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server fuzzy search notification: {}",error.message);}});}
        }));
        super::fuzzy_search_methods::register_fuzzy_file_search_methods(&mut core.registry,fuzzy_search.clone());
        let extension_threads = threads.clone();
        super::registry::register_extension_request_method(&mut core.registry,Arc::new(move |thread_id: String| {
            let threads = extension_threads.clone();
            Box::pin(async move {
                let entry = threads.get_loaded_thread(&thread_id).await.map_err(|error| JsonRpcError::new(-32603, error))?;
                let session = entry.lock().await.session.clone();
                Ok::<_, JsonRpcError>(Arc::new(session) as Arc<dyn super::registry::ExtensionRequestTarget>)
            })
        }));
        for method in ["thread/start", "thread/resume", "thread/read", "thread/unsubscribe", "thread/loaded/list", "thread/name/set"] {
            let threads = threads.clone(); let turn_log = turn_log.clone(); let cwd = cwd.clone(); let version = version.clone();
            let notification_core = notification_core.clone();
            let lifecycle_slot=lifecycle_slot.clone();let turn_engine_slot=turn_engine_slot.clone();
            let recipients = recipients.clone();let approvals = approvals.clone();let user_input = user_input.clone();
            let mcp_inventory = mcp_inventory.clone();
            let ui_agent_dir = ui_agent_dir.clone();
            core.registry.register(method.into(), MethodRegistration { requires_init: true, experimental: false, scope: MethodScope::Thread, handler: Arc::new(move |context| {
                let threads = threads.clone(); let turn_log = turn_log.clone(); let cwd = cwd.clone(); let version = version.clone();
                let notification_core = notification_core.clone();
                let lifecycle_slot=lifecycle_slot.clone();let turn_engine_slot=turn_engine_slot.clone();
                let recipients = recipients.clone();let approvals = approvals.clone();let user_input = user_input.clone();
                let mcp_inventory = mcp_inventory.clone();
                let ui_agent_dir = ui_agent_dir.clone();
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
                    } else {
                        let id = required_string(params, "threadId")?;
                        // Pinned `resume`: `ThreadNotFoundError` surfaces as `no rollout found for thread id`.
                        threads.resume_thread(id).await.map_err(|error| if method == "thread/resume" && error == format!("Thread not found: {id}") {format!("no rollout found for thread id {id}")} else {error})
                    }.map_err(|error| JsonRpcError::new(-32603, error))?;
                    if requested_name.is_none() && let Some(engine) = turn_engine_slot.get() { engine.observe_thread(&entry).await; }
                    let mut entry = entry.lock().await;
                    if let Some(name) = requested_name {
                        entry.session.set_session_name(&name);
                        let id = entry.id.clone();
                        context.connection.defer_task_until_responded(move || tokio::spawn(async move {
                            tokio::task::yield_now().await;
                            if let Some(core) = notification_core.get().and_then(std::sync::Weak::upgrade)
                                && let Err(error) = core.read().await.broadcast_notification(json!({"method":"thread/name/updated","params":{"threadId":id,"threadName":name}}),chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server name notification: {}",error.message);}
                        }));
                        return Ok(json!({}));
                    }
                    if let Some(lifecycle)=lifecycle_slot.get() {lifecycle.clear_idle_timer(&entry.id);}
                    if method != "thread/read" { entry.subscribers.insert(context.connection.id.clone()); }
                    let mut log = turn_log.lock().await;
                    let wire = build_wire_thread(&entry, &mut log, method == "thread/resume" || params["includeTurns"] == true, &version).await.map_err(|error| JsonRpcError::new(-32603, error.to_string()))?;
                    if method == "thread/read" { return Ok(json!({"thread":wire})); }
                    let model = entry.session.model();
                    let tier = entry.session.service_tier().map(|tier| match tier { maho_ext_api::ServiceTier::Auto => "auto", maho_ext_api::ServiceTier::Flex => "flex", maho_ext_api::ServiceTier::Priority => "priority" });
                    let mut response = json!({"thread":wire,"model":model.id,"modelProvider":model.provider,"serviceTier":tier,"cwd":entry.cwd,"runtimeWorkspaceRoots":[entry.cwd],"instructionSources":[],"approvalPolicy":if method == "thread/start" {super::start_options::requested_approval_policy(params)} else {json!("never")},"approvalsReviewer":"user","sandbox":{"type":"dangerFullAccess"},"activePermissionProfile":null,"reasoningEffort":entry.session.thinking_level(),"multiAgentMode":"explicitRequestOnly"});
                    if method == "thread/resume" { response["initialTurnsPage"] = Value::Null; }
                    let queued = std::mem::take(&mut entry.queued_terminal_notifications);
                    let client_id = context.connection.id.clone();
                    let thread_id = entry.id.clone();
                    let mcp_session = entry.session.clone();
                    // senpi `subscribe` (queued terminal flush, then pending approval/user-input replay)
                    // and the resume `thread/status/changed` broadcast run before the response.
                    if let Some(core) = notification_core.get().and_then(std::sync::Weak::upgrade) {
                        let core = core.read().await;
                        for notification in queued {if let Err(error) = core.send_thread_notification_to_connection(&client_id,notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server terminal replay: {}",error.message);}}
                        if let Some(connection) = core.get_connection(&client_id) {
                            recipients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(thread_id.clone()).or_default().insert(client_id.clone(),connection.send.clone());
                            approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replay_pending_for_thread(&thread_id);
                            user_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replay_pending_for_thread(&thread_id);
                        }
                        if method == "thread/resume" && let Err(error) = core.broadcast_notification(json!({"method":"thread/status/changed","params":{"threadId":thread_id,"status":{"type":"idle"}}}),chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server lifecycle notification: {}",error.message);}
                    }
                    let started = (method == "thread/start").then(|| json!({"method":"thread/started","params":{"thread":response["thread"]}}));
                    context.connection.defer_task_until_responded(move || tokio::spawn(async move {
                        tokio::task::yield_now().await;
                        if let Some(started) = started && let Some(core) = notification_core.get().and_then(std::sync::Weak::upgrade)
                            && let Err(error) = core.read().await.broadcast_notification(started,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server lifecycle notification: {}",error.message);}
                        // Sessions are bound at creation (UI, then MCP inventory); an entry whose
                        // inventory was dropped is rebound here in the same order.
                        if mcp_inventory.lock().await.resolve(Some(&thread_id)).is_none() {
                            let (turn_thread_id, active) = (thread_id.clone(), threads.active_turn_ids());
                            if let Ok(ui) = super::approval_ui_context::AppServerUiContext::new(approvals.clone(),user_input.clone(),thread_id.clone(),Arc::new(move || super::thread_registry::active_turn_id_or_default(&active,&turn_thread_id)),std::path::Path::new(&ui_agent_dir)) {let _ = mcp_session.rebind_extension_ui(Arc::new(ui)).await;}
                            register_mcp_inventory(&mcp_inventory,&thread_id,&mcp_session).await;
                        }
                    }));
                    Ok(response)
                })
            }) });
        }
        {
            let threads = threads.clone(); let turn_log = turn_log.clone(); let version = version.clone();
            let notification_core = notification_core.clone();
            let lifecycle_slot=lifecycle_slot.clone();let turn_engine_slot=turn_engine_slot.clone();
            let recipients = recipients.clone();let approvals = approvals.clone();let user_input = user_input.clone();
            let mcp_inventory = mcp_inventory.clone();
            let ui_agent_dir = ui_agent_dir.clone();
            core.registry.register("thread/fork".into(), MethodRegistration { requires_init: true, experimental: false, scope: MethodScope::Thread, handler: Arc::new(move |context| {
                let threads = threads.clone(); let turn_log = turn_log.clone(); let version = version.clone();
                let notification_core = notification_core.clone();
                let lifecycle_slot=lifecycle_slot.clone();let turn_engine_slot=turn_engine_slot.clone();
                let recipients = recipients.clone();let approvals = approvals.clone();let user_input = user_input.clone();
                let mcp_inventory = mcp_inventory.clone();
                let ui_agent_dir = ui_agent_dir.clone();
                Box::pin(async move {
                    let params = &context.request["params"];
                    let source = required_string(params,"threadId")?.to_owned();
                    let requested_cwd = params["cwd"].as_str().map(str::to_owned);
                    let entry = threads.fork_thread(&source,requested_cwd).await.map_err(|error| JsonRpcError::new(-32603, error))?;
                    if let Some(engine) = turn_engine_slot.get() { engine.observe_thread(&entry).await; }
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
                    // senpi `subscribe` runs before the response; only `thread/started` is deferred.
                    if let Some(core) = notification_core.get().and_then(std::sync::Weak::upgrade) {
                        let core = core.read().await;
                        for notification in queued {if let Err(error) = core.send_thread_notification_to_connection(&client_id,notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server fork terminal replay: {}",error.message);}}
                        if let Some(connection) = core.get_connection(&client_id) {
                            recipients.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(thread_id.clone()).or_default().insert(client_id.clone(),connection.send.clone());
                            approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replay_pending_for_thread(&thread_id);
                            user_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).replay_pending_for_thread(&thread_id);
                        }
                    }
                    let started = json!({"method":"thread/started","params":{"thread":response["thread"]}});
                    context.connection.defer_task_until_responded(move || tokio::spawn(async move {
                        tokio::task::yield_now().await;
                        if let Some(core) = notification_core.get().and_then(std::sync::Weak::upgrade)
                            && let Err(error) = core.read().await.broadcast_notification(started,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server fork notification: {}",error.message);}
                        // Sessions are bound at creation (UI, then MCP inventory); an entry whose
                        // inventory was dropped is rebound here in the same order.
                        if mcp_inventory.lock().await.resolve(Some(&thread_id)).is_none() {
                            let (turn_thread_id, active) = (thread_id.clone(), threads.active_turn_ids());
                            if let Ok(ui) = super::approval_ui_context::AppServerUiContext::new(approvals.clone(),user_input.clone(),thread_id.clone(),Arc::new(move || super::thread_registry::active_turn_id_or_default(&active,&turn_thread_id)),std::path::Path::new(&ui_agent_dir)) {let _ = mcp_session.rebind_extension_ui(Arc::new(ui)).await;}
                            register_mcp_inventory(&mcp_inventory,&thread_id,&mcp_session).await;
                        }
                    }));
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
        if thread_notification_core.set(Arc::downgrade(&core)).is_err() {unreachable!("thread notification core is initialized once");}
        let lifecycle=super::handlers::ThreadLifecycleController::new(Arc::downgrade(&core),threads.clone(),mcp_inventory.clone(),std::time::Duration::from_secs(30*60));
        if lifecycle_slot.set(lifecycle.clone()).is_err() {unreachable!("lifecycle initialized once");}
        let turns = super::turns::register_turn_methods(&core, threads.clone(), turn_log.clone()).await;
        if turn_engine_slot.set(turns.clone()).is_err() {unreachable!("turn engine initialized once");}
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
                let send = move || tokio::spawn(async move { if let Err(error) = core.read().await.broadcast_notification(notification,chrono::Utc::now().timestamp_millis() as u64).await {eprintln!("app-server provider account notification: {}",error.message);} });
                // Pinned `subscribeToProviderAccountEvents`: inside a dispatch, defer until its response.
                match super::registry::ACTIVE_DISPATCH.try_with(|dispatch| dispatch.clone()) {
                    Ok(dispatch) => dispatch.defer_task_until_responded(send),
                    Err(_) => drop(send()),
                }
            }
        }));
        Self { core, threads, turn_log, fuzzy_search, mcp_inventory,approvals,user_input,lifecycle,turns, provider_account_unsubscribe:std::sync::Mutex::new(Some(provider_account_unsubscribe)), shutdown_request }
    }
    pub async fn dispose(&self) {
        if let Some(unsubscribe) = self.provider_account_unsubscribe.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {unsubscribe();}
        self.lifecycle.dispose();
        for thread in self.threads.list_loaded().await {if let Some(id) = thread["id"].as_str() {
            self.approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_pending_for_thread(id);
            self.user_input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_pending_for_thread(id);
        }}
        self.fuzzy_search.dispose();self.threads.dispose().await;
        let mut inventory = self.mcp_inventory.lock().await;
        for id in inventory.thread_ids() {inventory.remove_thread(&id);}
    }
}
