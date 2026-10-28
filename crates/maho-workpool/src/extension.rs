//! Registration on the maho extension API.
//!
//! - [`WorkpoolAddon`] is a `maho-task` addon: in a parent session it contributes the `workpool`
//!   tool under the task extension's depth gate, state directory and concurrency limit, and
//!   dispatches workers as background delegated tasks ([`TaskHost`]).
//! - [`WorkerExtension`] runs in every session: in a pool worker process (found through
//!   [`WorkerIndex`] from `MHC_TASK_ROOT_SESSION` + `MHC_TASK_ID`) it registers the yield-only
//!   `workpool` view and proxies for the pool's extra tools; in a parent session it reattaches
//!   pools at session start, holds them on user stop, releases them on resume, stops dispatch at
//!   shutdown, and serves extra-tool calls from workers.
use crate::bridge::{self, CallRequest, CallResponse};
use crate::engine::{AGGREGATE_MESSAGE_TYPE, PoolEngine};
use crate::host::{BoxFuture, DeliveryDone, StartedWorker, ToolCatalog, ToolSpec, WorkerEntry, WorkerHost, WorkerIndex, WorkerStart};
use crate::model::{AgentSpec, PoolError, codes};
use crate::store::PoolStore;
use crate::tool::{self, Caller, TOOL_NAME, ToolOutcome};
use maho_ext_api::{
    AgentToolResult, CustomMessage, DeliverAs, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionEvent, SendMessageOptions, ToolContent, ToolDefinition, ToolError, ToolResult,
};
use maho_task::{AddonTool, TaskAddon, TaskManager, TaskScope, TaskStatus};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How often a re-attached worker run owned by an ended process is checked for exit.
const ORPHAN_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The live parent runtime of this process: message sender, tool lister and executor.
#[derive(Clone)]
struct ParentRuntime {
    api: Arc<ExtensionApi>,
}

static RUNTIME: Mutex<Option<ParentRuntime>> = Mutex::new(None);
static ENGINES: Mutex<Option<HashMap<PathBuf, PoolEngine>>> = Mutex::new(None);

fn runtime() -> Option<ParentRuntime> {
    lock(&RUNTIME).clone()
}

fn engines() -> Vec<PoolEngine> {
    lock(&ENGINES).as_ref().map(|m| m.values().cloned().collect()).unwrap_or_default()
}

fn to_tool_result(outcome: ToolOutcome) -> ToolResult {
    ToolResult {
        content: vec![ToolContent::text(outcome.text)],
        details: Some(outcome.details),
        structured_content: None,
        is_error: outcome.is_error.then_some(true),
    }
}

fn to_agent_result(text: String, details: Value, is_error: bool) -> AgentToolResult {
    let mut result = AgentToolResult::text(text);
    result.details = details;
    if is_error {
        result.is_error = Some(true);
    }
    result
}

fn agent_item(agent: &AgentSpec, prompt: &str, name: &str) -> Value {
    let mut item = serde_json::Map::new();
    item.insert("task".into(), json!(prompt));
    item.insert("name".into(), json!(name));
    match agent {
        AgentSpec::Category { category, model, .. } => {
            item.insert("category".into(), json!(category));
            if let Some(model) = model {
                item.insert("model".into(), json!(model));
            }
        }
        AgentSpec::Subagent { subagent_type, model, .. } => {
            item.insert("agent".into(), json!(subagent_type));
            if let Some(model) = model {
                item.insert("model".into(), json!(model));
            }
        }
    }
    Value::Object(item)
}

/// [`WorkerHost`] over `maho-task`: workers are background, quiet delegated tasks grouped by
/// pool id.
pub struct TaskHost {
    scope: TaskScope,
    index: WorkerIndex,
    root_session: String,
}

impl TaskHost {
    pub fn new(scope: TaskScope, index: WorkerIndex) -> Self {
        let root_session = maho_task::settings::current_root_session(scope.session_id());
        Self { scope, index, root_session }
    }

    fn manager(&self) -> Arc<TaskManager> {
        self.scope.manager()
    }

    fn tool_specs(names: &[String]) -> Vec<ToolSpec> {
        let Some(runtime) = runtime() else { return Vec::new() };
        let Ok(all) = runtime.api.get_all_tools() else { return Vec::new() };
        names
            .iter()
            .filter_map(|name| all.iter().find(|t| &t.name == name))
            .map(|t| ToolSpec { name: t.name.clone(), description: t.description.clone(), parameters: t.parameters.clone() })
            .collect()
    }
}

impl WorkerHost for TaskHost {
    fn check_policy(&self, _agent: &AgentSpec) -> Result<(), String> {
        let depth = maho_task::settings::current_depth();
        if !maho_task::settings::can_spawn_at_depth(self.scope.settings().max_depth, depth) {
            return Err(format!("task depth {depth} may not spawn workers (task.max_depth {})", self.scope.settings().max_depth));
        }
        Ok(())
    }

    fn resolve(&self, agent: &AgentSpec) -> Result<Value, String> {
        let request = self.scope.request(&agent_item(agent, agent.prompt(), "pool-worker"), None)?;
        Ok(json!({"agent": request.agent.name, "model": request.choice.model, "thinking": request.choice.thinking}))
    }

    fn tool_catalog(&self, agent: &AgentSpec) -> ToolCatalog {
        let all = runtime().and_then(|r| r.api.get_all_tools().ok()).unwrap_or_default();
        let parent: Vec<String> = all.iter().map(|t| t.name.clone()).collect();
        // The worker runs the same mhc build, so it already has every built-in tool.
        let builtin: Vec<String> = all
            .iter()
            .filter(|t| t.source_info.source == "builtin" || t.source_info.path.starts_with("<builtin:"))
            .map(|t| t.name.clone())
            .collect();
        let (worker, denied) = match self.scope.request(&agent_item(agent, agent.prompt(), "pool-worker"), None) {
            Ok(request) => {
                let mut denied = request.agent.disallowed_tools.clone();
                denied.extend(request.choice.disabled_tools.iter().cloned());
                let worker = match &request.agent.tools {
                    // An explicit allowlist: those tools, and nothing outside it is allowed.
                    Some(allow) => {
                        denied.extend(parent.iter().filter(|t| !allow.contains(t) && !builtin.contains(t)).cloned());
                        allow.clone()
                    }
                    None => builtin,
                };
                (worker, denied)
            }
            Err(_) => (builtin, Vec::new()),
        };
        ToolCatalog { parent, worker, denied }
    }

    fn tools_available(&self, tools: &[String]) -> bool {
        let Some(runtime) = runtime() else { return false };
        let Ok(all) = runtime.api.get_all_tools() else { return false };
        tools.iter().all(|name| all.iter().any(|t| &t.name == name))
    }

    fn free_slots(&self) -> Option<usize> {
        let limit = self.scope.settings().concurrency;
        if limit == 0 {
            return None;
        }
        let active = self.manager().list().iter().filter(|r| r.status.is_active()).count();
        Some(limit.saturating_sub(active))
    }

    fn start(&self, request: &WorkerStart) -> Result<StartedWorker, PoolError> {
        let name = format!("pool-{}", request.pool_name);
        let mut spawn = self
            .scope
            .request(&agent_item(&request.agent, &request.prompt, &name), None)
            .map_err(|message| PoolError::new(codes::ADMISSION_REFUSED, message))?;
        spawn.quiet = true;
        spawn.group = Some(request.pool_id.clone());
        spawn.isolated = false;
        if !request.tools.is_empty() {
            // Worker tool allowlist keeps its defaults; the extra tools come through proxies.
            spawn.agent.disallowed_tools.retain(|t| !request.tools.contains(t));
        }
        // The worker must not delegate further: remove the delegation tools from its set.
        for name in maho_task::TOOL_NAMES {
            if !spawn.agent.disallowed_tools.iter().any(|t| t.as_str() == *name) {
                spawn.agent.disallowed_tools.push((*name).to_owned());
            }
        }
        // The task id is allocated by `start`, which launches the child asynchronously; the
        // index entry is written right after, well before the child process has booted far
        // enough to load extensions. A worker that yields before the owner committed its start
        // is matched against the pending binding (engine).
        let record = self.manager().start(spawn).map_err(|message| PoolError::new(codes::SPAWN_FAILED, message))?;
        let entry = WorkerEntry {
            state_dir: self.manager().state().root.clone(),
            pool_id: request.pool_id.clone(),
            task_id: record.id.clone(),
            tools: Self::tool_specs(&request.tools),
        };
        if let Err(error) = self.index.write(&self.root_session, &entry) {
            maho_ext_api::diagnostic(&format!("workpool: cannot record worker {}: {error}", record.id));
        }
        Ok(StartedWorker { task_id: record.id, session_id: Some(record.session_id) })
    }

    fn follow_up(&self, task_id: &str, message: &str) -> Result<(), PoolError> {
        self.manager()
            .resume(task_id, Some(message), true)
            .map(|_| ())
            .map_err(|message| PoolError::new(codes::WORKER_NOT_CONTINUABLE, message))
    }

    fn wait_run(&self, task_id: &str) -> BoxFuture<()> {
        let manager = self.manager();
        let id = task_id.to_owned();
        Box::pin(async move {
            // A run owned by this process settles through the manager's watch channel. A run
            // re-attached from an ended process has no channel here; its record is re-read until
            // the child exits (`get` reconciles a dead pid to `interrupted`).
            loop {
                match manager.wait(&id, None).await {
                    Some(record) if record.status.is_active() => tokio::time::sleep(ORPHAN_CHECK).await,
                    _ => return,
                }
            }
        })
    }

    fn run_active(&self, task_id: &str) -> Option<bool> {
        self.manager().get(task_id).map(|r| r.status.is_active())
    }

    fn cancel(&self, task_id: &str) {
        if self.manager().get(task_id).is_some_and(|r| r.status != TaskStatus::Cancelled) {
            self.manager().cancel(task_id);
        }
    }

    fn deliver(&self, _session: &str, text: String, details: Value, done: DeliveryDone) {
        let Some(runtime) = runtime() else {
            done(Err("no live session to deliver to".to_owned()));
            return;
        };
        let sent = runtime.api.send_message_with_receipt(
            CustomMessage {
                custom_type: AGGREGATE_MESSAGE_TYPE.to_owned(),
                content: vec![ToolContent::text(text)],
                display: true,
                details: Some(details),
            },
            SendMessageOptions { trigger_turn: true, deliver_as: Some(DeliverAs::FollowUp) },
        );
        match sent {
            Ok(receipt) => receipt.on_settled(done),
            Err(error) => done(Err(error.to_string())),
        }
    }

    fn notice(&self, _session: &str, text: &str) {
        maho_ext_api::diagnostic(text);
    }
}

/// The `workpool` tool as a task addon (parent sessions).
#[derive(Clone, Debug, Default)]
pub struct WorkpoolAddon {
    /// mhc's agent dir (holds the worker index); `None` uses the task state dir's parent.
    pub agent_dir: Option<PathBuf>,
}

impl WorkpoolAddon {
    fn index(&self, scope: &TaskScope) -> WorkerIndex {
        match &self.agent_dir {
            Some(dir) => WorkerIndex::in_agent_dir(dir),
            None => WorkerIndex::new(scope.manager().state().root.join("..").join("workpool-workers")),
        }
    }

    /// The engine of the calling session's task state dir; created (and reattached) on first use.
    pub fn engine(&self, scope: &TaskScope) -> PoolEngine {
        let root = scope.manager().state().root.clone();
        let engine = {
            let mut guard = lock(&ENGINES);
            let map = guard.get_or_insert_with(HashMap::new);
            if let Some(engine) = map.get(&root) {
                return engine.clone();
            }
            let host = Arc::new(TaskHost::new(scope.clone(), self.index(scope)));
            let engine = PoolEngine::new(PoolStore::in_state_dir(&root), host);
            map.insert(root, engine.clone());
            engine
        };
        engine.reattach(scope.session_id());
        start_call_server(&engine);
        engine
    }
}

impl TaskAddon for WorkpoolAddon {
    fn tools(&self) -> Vec<AddonTool> {
        if self.agent_dir.as_deref().and_then(worker_entry).is_some() {
            // A worker gets the yield-only view from `WorkerExtension` instead.
            return Vec::new();
        }
        let addon = self.clone();
        vec![AddonTool {
            name: TOOL_NAME.to_owned(),
            label: "Work pool".to_owned(),
            description: tool::DESCRIPTION.to_owned(),
            parameters: tool::parameters(),
            handler: Arc::new(move |scope, params, _cancel| {
                let addon = addon.clone();
                Box::pin(async move {
                    let engine = addon.engine(&scope);
                    let caller = if scope.session_id().trim().is_empty() || scope.session_id() == "default" {
                        Caller::Anonymous
                    } else {
                        Caller::Parent { session: scope.session_id().to_owned() }
                    };
                    let outcome = tokio::task::spawn_blocking(move || tool::run(&engine, &caller, &params))
                        .await
                        .map_err(|error| ToolError::Message(format!("workpool: {error}")))?;
                    Ok(to_tool_result(outcome))
                })
            }),
        }]
    }
}

/// Serves extra-tool calls from workers while this process owns granting pools.
fn start_call_server(engine: &PoolEngine) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
    let store = engine.store().clone();
    let execute: bridge::Executor = Arc::new(|name: String, params: Value| {
        Box::pin(async move {
            let Some(runtime) = runtime() else {
                return CallResponse { text: format!("{}: the parent session is gone", codes::TOOLS_UNAVAILABLE), is_error: true };
            };
            match runtime.api.execute_tool(&name, params, maho_ext_api::ExecuteToolOptions::default()).await {
                Ok(result) => CallResponse {
                    text: serde_json::to_value(&result.content)
                        .ok()
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|block| block.get("text").and_then(Value::as_str).map(str::to_owned))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    is_error: result.is_error.unwrap_or(false),
                },
                Err(error) => CallResponse { text: error.to_string(), is_error: true },
            }
        }) as BoxFuture<CallResponse>
    });
    handle.spawn(async move {
        // Whether any pool grants extra tools is re-read every 50 checks (5 s), so a session
        // without such pools only lists its records occasionally.
        let mut granting = false;
        let mut tick: u32 = 0;
        loop {
            if runtime().is_none() {
                return;
            }
            if tick % 50 == 0 {
                granting = bridge::has_granting_pools(&store);
            }
            tick = tick.wrapping_add(1);
            if granting {
                bridge::serve_once(&store, &execute).await;
            }
            tokio::time::sleep(bridge::CALL_CHECK).await;
        }
    });
}

/// This process's pool assignment, when it runs as a pool worker of a parent using `agent_dir`.
pub fn worker_entry(agent_dir: &Path) -> Option<WorkerEntry> {
    let task_id = std::env::var(maho_task::settings::TASK_ID_ENV).ok()?;
    let root = std::env::var(maho_task::settings::ROOT_SESSION_ENV).ok()?;
    WorkerIndex::in_agent_dir(agent_dir).read(root.trim(), task_id.trim())
}

/// Session-level wiring (see the module docs).
#[derive(Clone)]
pub struct WorkerExtension {
    pub agent_dir: PathBuf,
    /// The delegation extension of this process; lets session start reattach pools before the
    /// first `workpool` call. `None` reattaches on first use only.
    pub tasks: Option<maho_task::TaskExtension>,
}

impl WorkerExtension {
    fn addon(&self) -> WorkpoolAddon {
        WorkpoolAddon { agent_dir: Some(self.agent_dir.clone()) }
    }
}

fn definition(name: &str, label: &str, description: &str, parameters: Value) -> ToolDefinition {
    let mut definition = ToolDefinition::new(
        name,
        description,
        parameters,
        Arc::new(|_| Box::pin(async { Err(ToolError::Message("Extension context required".into())) })),
    );
    label.clone_into(&mut definition.label);
    definition
}

impl Extension for WorkerExtension {
    fn register(&self, api: &mut ExtensionApi) {
        if let Some(entry) = worker_entry(&self.agent_dir) {
            register_worker_tools(api, entry);
            return;
        }
        *lock(&RUNTIME) = Some(ParentRuntime {
            api: Arc::new(ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone())),
        });
        let start = self.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_event, ctx| {
                let session = ctx.session_manager.session_id().to_owned();
                let cwd = ctx.cwd.clone();
                let start = start.clone();
                Box::pin(async move {
                    let mut started = false;
                    if let Some(tasks) = start.tasks.clone() {
                        let view = maho_config::load_native(&cwd, &maho_config::process_env()).view;
                        let settings = maho_task::TaskSettings::from_view(Some(&view));
                        if maho_task::settings::can_spawn_at_depth(settings.max_depth, maho_task::settings::current_depth()) {
                            let scope = TaskScope::detached(tasks, cwd, &session, view);
                            let addon = start.addon();
                            // `engine` reattaches a newly created engine itself.
                            let known = lock(&ENGINES).as_ref().is_some_and(|m| m.contains_key(&scope.manager().state().root));
                            let engine = tokio::task::spawn_blocking(move || addon.engine(&scope)).await.ok();
                            if let (true, Some(engine)) = (known, engine) {
                                let session = session.clone();
                                let _ = tokio::task::spawn_blocking(move || engine.reattach(&session)).await;
                            }
                            started = true;
                        }
                    }
                    if !started {
                        for engine in engines() {
                            engine.reattach(&session);
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        api.on(
            EventKind::SessionHoldChanged,
            Arc::new(|event, ctx| {
                let session = ctx.session_manager.session_id().to_owned();
                let change = match event {
                    ExtensionEvent::SessionHoldChanged { change, pause_seq, .. } => Some((change.clone(), *pause_seq)),
                    _ => None,
                };
                Box::pin(async move {
                    if let Some((change, seq)) = change {
                        for engine in engines() {
                            match change.as_str() {
                                "paused" => engine.hold(&session, seq),
                                "resumed" => {
                                    engine.release(&session, seq);
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        api.on(
            EventKind::SessionShutdown,
            Arc::new(|_event, _ctx| {
                for engine in engines() {
                    engine.shutdown();
                }
                lock(&RUNTIME).take();
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
    }
}

fn register_worker_tools(api: &mut ExtensionApi, entry: WorkerEntry) {
    let store = PoolStore::in_state_dir(&entry.state_dir);
    let engine = PoolEngine::worker_view(store.clone());
    let yield_entry = entry.clone();
    let yield_tool = definition(TOOL_NAME, "Work pool", tool::WORKER_DESCRIPTION, tool::worker_parameters());
    if let Err(error) = api.register_tool_with_extension_context(
        yield_tool,
        Arc::new(move |_id, params, _signal, _update, _ctx| {
            let engine = engine.clone();
            let caller = Caller::Worker { pool_id: yield_entry.pool_id.clone(), task_id: yield_entry.task_id.clone() };
            Box::pin(async move {
                let outcome = tool::run(&engine, &caller, &params);
                Ok(to_agent_result(outcome.text, outcome.details, outcome.is_error))
            })
        }),
    ) {
        maho_ext_api::diagnostic(&format!("workpool: {error}"));
    }
    for spec in entry.tools.clone() {
        let store = store.clone();
        let entry = entry.clone();
        let name = spec.name.clone();
        let proxy = definition(&spec.name, &spec.name, &spec.description, spec.parameters.clone());
        if let Err(error) = api.register_tool_with_extension_context(
            proxy,
            Arc::new(move |_id, params, _signal, _update, _ctx| {
                let store = store.clone();
                let request = CallRequest { pool_id: entry.pool_id.clone(), task_id: entry.task_id.clone(), tool: name.clone(), params };
                Box::pin(async move {
                    let response = bridge::call(&store, &request).await;
                    Ok(to_agent_result(response.text, Value::Null, response.is_error))
                })
            }),
        ) {
            maho_ext_api::diagnostic(&format!("workpool: {error}"));
        }
    }
}
