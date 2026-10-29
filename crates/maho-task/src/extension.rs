//! The delegation tools on the maho extension API: `task` (spawn one or a batch, foreground or
//! background), `task_output` (retrieve or wait for a result), `task_list`, `task_resume`,
//! `task_cancel`, and the `/tasks` command. Background results are delivered back to the session
//! as a follow-up message when they settle.
use crate::agents::{DEFAULT_AGENT, Discovery, discover};
use crate::control::{self, Caller, ContextUi, GatewaySteer, SteerChannel};
use crate::isolation::{TaskObserver, WorkspaceIsolation};
use crate::manager::{SettledCallback, SpawnRequest, TaskManager, TaskManagerOptions};
use crate::models::{ModelContext, ModelRequest, category_description, category_names, choose_model};
use crate::record::{StateDir, default_state_root};
use crate::runner::{CancelToken, TaskRunner};
use crate::settings::{
    TASK_ID_ENV, TaskSettings, can_spawn_at_depth, current_depth, current_root_session,
};
use crate::summary::status_line;
use maho_ext_api::{
    CustomMessage, DeliverAs, Extension, ExtensionApi, NotificationType, SendMessageOptions,
    ToolCall, ToolContent, ToolDefinition, ToolError, ToolResult,
};
use maho_lsp::hooks::HookChain;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Tools this extension registers.
pub const TOOL_NAMES: &[&str] = &[
    "task",
    "task_output",
    "task_list",
    "task_resume",
    "task_cancel",
    "task_send",
];
/// The command that cancels a task interactively (task-control.md §3).
pub const KILL_COMMAND: &str = "task-kill";
/// `custom_type` of a delivered background result.
pub const RESULT_MESSAGE_TYPE: &str = "task-result";

pub type ModelLister = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Host-provided pieces of the delegation extension.
#[derive(Clone)]
pub struct TaskExtension {
    /// mhc's agent dir: user agents in `<dir>/agents`, task state in `<dir>/tasks`.
    pub agent_dir: Option<PathBuf>,
    pub runner: Arc<dyn TaskRunner>,
    /// `provider/id` of the models with usable credentials (called once, lazily).
    pub available_models: ModelLister,
    pub isolation: Option<Arc<dyn WorkspaceIsolation>>,
    pub observers: Vec<Arc<dyn TaskObserver>>,
    pub hooks: HookChain,
    /// Tools and commands layered on the task machinery (e.g. team tools), registered with the
    /// task tools under the same depth gate.
    pub addons: Vec<Arc<dyn TaskAddon>>,
}

/// Sends a displayed follow-up message (`custom_type`, text, details) to the session that owns
/// the tools; the message triggers a turn.
pub type Notifier = Arc<dyn Fn(&str, &str, Value) + Send + Sync>;

/// Per-call access to the delegation machinery for tools layered on top of it.
#[derive(Clone)]
pub struct TaskScope {
    shared: Arc<Shared>,
    info: Arc<CallInfo>,
}

impl TaskScope {
    /// The task manager of the calling session (same state directory as `task`).
    pub fn manager(&self) -> Arc<TaskManager> {
        self.shared.manager(&self.info.session_id)
    }
    pub fn session_id(&self) -> &str {
        &self.info.session_id
    }
    /// The calling session as seen by the task controls.
    pub fn caller(&self) -> Caller {
        caller(&self.info)
    }
    /// A scope outside a live session (embedding, tests): `session_id` calls from `cwd` with the
    /// configuration `view`. Background results are not delivered anywhere.
    pub fn detached(config: TaskExtension, cwd: PathBuf, session_id: &str, view: Value) -> Self {
        let settings = TaskSettings::from_view(Some(&view));
        let discovery = discover(&cwd, config.agent_dir.as_deref(), view.get("agents"));
        let steer = steer_channel(config.agent_dir.as_deref(), &view);
        let shared = Arc::new(Shared {
            config,
            cwd: cwd.clone(),
            view,
            settings,
            discovery,
            depth: current_depth(),
            available: OnceLock::new(),
            sender: None,
            steer,
        });
        Self {
            shared,
            info: Arc::new(CallInfo {
                cwd,
                session_id: session_id.to_owned(),
                parent_model: None,
            }),
        }
    }
    /// `task_send` (task-control.md §1) from this scope.
    pub async fn send(&self, params: Value) -> Result<ToolResult, ToolError> {
        run_send_tool(Arc::clone(&self.shared), params, (*self.info).clone()).await
    }
    pub fn cwd(&self) -> &Path {
        &self.info.cwd
    }
    /// The merged configuration view.
    pub fn view(&self) -> &Value {
        &self.shared.view
    }
    pub fn settings(&self) -> &TaskSettings {
        &self.shared.settings
    }
    /// A background spawn request from a `task`-shaped item (`task`, `agent`, `category`,
    /// `model`, `name`, `isolated`), with the `task` tool's agent and model rules.
    ///
    /// # Errors
    /// Missing task, unknown agent, or no usable model.
    pub fn request(&self, item: &Value, context: Option<&str>) -> Result<SpawnRequest, String> {
        let mut request = spawn_request(&self.shared, item, context, Some(true), &self.info)?;
        request.background = true;
        Ok(request)
    }
    /// The session's message sender, when it has one.
    pub fn notifier(&self) -> Option<Notifier> {
        let sender = self.shared.sender.clone()?;
        Some(Arc::new(move |custom_type: &str, text: &str, details: Value| {
            let _ = sender.send_message(
                CustomMessage {
                    custom_type: custom_type.to_owned(),
                    content: vec![ToolContent::text(text)],
                    display: true,
                    details: Some(details),
                },
                SendMessageOptions {
                    trigger_turn: true,
                    deliver_as: Some(DeliverAs::FollowUp),
                },
            );
        }))
    }
}

/// Handler of an addon tool.
pub type AddonHandler = Arc<
    dyn Fn(TaskScope, Value, CancelToken) -> crate::runner::BoxFuture<'static, Result<ToolResult, ToolError>>
        + Send
        + Sync,
>;

/// A tool contributed by a [`TaskAddon`].
#[derive(Clone)]
pub struct AddonTool {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
    pub handler: AddonHandler,
}

/// A slash command contributed by a [`TaskAddon`]; the handler's text is shown to the user.
#[derive(Clone)]
pub struct AddonCommand {
    pub name: String,
    pub description: String,
    pub handler: Arc<dyn Fn(TaskScope, String) -> String + Send + Sync>,
}

/// Tools and commands built on the task machinery.
pub trait TaskAddon: Send + Sync {
    fn tools(&self) -> Vec<AddonTool>;
    fn commands(&self) -> Vec<AddonCommand> {
        Vec::new()
    }
    /// Handles a `task_send` to `<group>/<member>` (or `<group>/*`); `None` = not this addon's.
    fn route_message(
        &self,
        _scope: &TaskScope,
        _group: &str,
        _member: &str,
        _message: &str,
    ) -> Option<Result<ToolResult, String>> {
        None
    }
}

static MANAGERS: Mutex<Option<HashMap<PathBuf, Arc<TaskManager>>>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Shared {
    config: TaskExtension,
    cwd: PathBuf,
    view: Value,
    settings: TaskSettings,
    discovery: Discovery,
    depth: u32,
    available: OnceLock<Vec<String>>,
    sender: Option<Arc<ExtensionApi>>,
    /// Steering channel into running children (task-control.md §1.1).
    steer: Option<Arc<dyn SteerChannel>>,
}

impl Shared {
    fn available(&self) -> &[String] {
        self.available.get_or_init(|| (self.config.available_models)())
    }

    fn state_root(&self, session_id: &str) -> PathBuf {
        if let Some(dir) = &self.settings.state_dir {
            return if dir.is_absolute() { dir.clone() } else { self.cwd.join(dir) };
        }
        let base = self
            .config
            .agent_dir
            .clone()
            .unwrap_or_else(|| self.cwd.join(".omo"));
        default_state_root(&base, session_id)
    }

    fn manager(&self, session_id: &str) -> Arc<TaskManager> {
        let root = self.state_root(session_id);
        let manager = {
            let mut managers = lock(&MANAGERS);
            let managers = managers.get_or_insert_with(HashMap::new);
            Arc::clone(managers.entry(root.clone()).or_insert_with(|| {
                TaskManager::new(TaskManagerOptions {
                    state: StateDir::new(root),
                    settings: self.settings.clone(),
                    runner: Arc::clone(&self.config.runner),
                    isolation: self.config.isolation.clone(),
                    observers: self.config.observers.clone(),
                    hooks: self.config.hooks.clone(),
                    depth: self.depth,
                })
            }))
        };
        manager.set_steer_channel(self.steer.clone());
        if let Some(sender) = &self.sender {
            let sender = Arc::clone(sender);
            let callback: SettledCallback = Arc::new(move |record, text| {
                let _ = sender.send_message(
                    CustomMessage {
                        custom_type: RESULT_MESSAGE_TYPE.to_owned(),
                        content: vec![ToolContent::text(text)],
                        display: true,
                        details: Some(json!({"id": record.id, "status": record.status.as_str()})),
                    },
                    SendMessageOptions {
                        trigger_turn: true,
                        deliver_as: Some(DeliverAs::FollowUp),
                    },
                );
            });
            manager.set_on_settled(Some(callback));
        }
        manager
    }
}

fn text_result(text: String, details: Value) -> ToolResult {
    ToolResult {
        content: vec![ToolContent::text(text)],
        details: Some(details),
        structured_content: None,
        is_error: None,
    }
}

fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn bool_param(params: &Value, key: &str) -> Option<bool> {
    params.get(key).and_then(Value::as_bool)
}

#[derive(Clone)]
struct CallInfo {
    cwd: PathBuf,
    session_id: String,
    parent_model: Option<String>,
}

fn call_info(call: &ToolCall<'_>, fallback_cwd: &Path) -> CallInfo {
    match call.context {
        Some(context) => CallInfo {
            cwd: context.cwd().to_path_buf(),
            session_id: context.session_manager().session_id().to_owned(),
            parent_model: context
                .model()
                .map(|model| format!("{}/{}", model.provider, model.id)),
        },
        None => CallInfo {
            cwd: fallback_cwd.to_path_buf(),
            session_id: "default".to_owned(),
            parent_model: None,
        },
    }
}

fn spawn_request(
    shared: &Shared,
    item: &Value,
    context: Option<&str>,
    background: Option<bool>,
    info: &CallInfo,
) -> Result<SpawnRequest, String> {
    let task = str_param(item, "task").ok_or("Missing required parameter: task")?;
    let agent_name = str_param(item, "agent").unwrap_or(DEFAULT_AGENT);
    let Some(agent) = shared.discovery.get(agent_name) else {
        return Err(format!(
            "Unknown agent \"{agent_name}\". Available agents: {}",
            shared.discovery.names().join(", ")
        ));
    };
    let choice = choose_model(
        agent,
        &ModelRequest {
            explicit_model: str_param(item, "model"),
            category: str_param(item, "category"),
        },
        &ModelContext {
            view: Some(&shared.view),
            available: shared.available(),
            parent_model: info.parent_model.as_deref(),
        },
    )?;
    Ok(SpawnRequest {
        agent: agent.clone(),
        task: task.to_owned(),
        context: context.map(str::to_owned),
        name: str_param(item, "name").map(str::to_owned),
        choice,
        cwd: info.cwd.clone(),
        background: bool_param(item, "background")
            .or(background)
            .unwrap_or(agent.background),
        isolated: bool_param(item, "isolated").unwrap_or(shared.settings.isolation_enabled),
        group: None,
        pane: bool_param(item, "pane"),
        quiet: false,
        owner_session: Some(info.session_id.clone()),
        root_session: Some(current_root_session(&info.session_id)),
    })
}

/// The gateway steering channel of `<agent dir>/gateway` (or `$MAHO_GATEWAY_DIR`).
fn steer_channel(agent_dir: Option<&Path>, view: &Value) -> Option<Arc<dyn SteerChannel>> {
    let agent_dir = agent_dir?;
    let env = |key: &str| std::env::var(key).ok();
    Some(Arc::new(GatewaySteer {
        store: maho_gateway::Store::new(maho_gateway::default_root(agent_dir, &env)),
        enabled: maho_gateway::Settings::from_view(Some(view), &env).enabled,
    }))
}

fn caller(info: &CallInfo) -> Caller {
    Caller {
        session_id: info.session_id.clone(),
        root_session: current_root_session(&info.session_id),
    }
}

fn outcome_result(outcome: control::Outcome) -> ToolResult {
    let is_error = outcome.is_error().then_some(true);
    ToolResult {
        content: vec![ToolContent::text(outcome.text)],
        details: Some(outcome.details),
        structured_content: None,
        is_error,
    }
}

async fn run_send_tool(shared: Arc<Shared>, params: Value, info: CallInfo) -> Result<ToolResult, ToolError> {
    let to = str_param(&params, "to").unwrap_or_default().to_owned();
    let message = params.get("message").and_then(Value::as_str).unwrap_or_default().to_owned();
    if to.is_empty() {
        return Ok(outcome_result(control::Outcome {
            kind: "invalid_arguments",
            text: "Missing required parameter: to".into(),
            details: json!({"kind": "invalid_arguments"}),
        }));
    }
    let all_sessions = bool_param(&params, "all_sessions").unwrap_or(false);
    if let Some((group, member)) = control::split_team_target(&to) {
        if let Err(outcome) = control::check_message(&message) {
            return Ok(outcome_result(outcome));
        }
        let scope = TaskScope {
            shared: Arc::clone(&shared),
            info: Arc::new(info.clone()),
        };
        for addon in &shared.config.addons {
            if let Some(result) = addon.route_message(&scope, group, member, &message) {
                return result.map_err(ToolError::Message);
            }
        }
    }
    let manager = shared.manager(&info.session_id);
    Ok(outcome_result(control::send(&manager, &caller(&info), &to, &message, all_sessions)))
}

async fn run_task_tool(shared: Arc<Shared>, params: Value, info: CallInfo, cancel: CancelToken) -> Result<ToolResult, ToolError> {
    let manager = shared.manager(&info.session_id);
    let context = str_param(&params, "context");
    let background = bool_param(&params, "background");
    let items: Vec<Value> = match params.get("tasks").and_then(Value::as_array) {
        Some(items) if !items.is_empty() => items.clone(),
        _ => vec![params.clone()],
    };
    let mut requests = Vec::new();
    for item in &items {
        requests.push(spawn_request(&shared, item, context, background, &info).map_err(ToolError::Message)?);
    }
    let mut started = Vec::new();
    for request in requests {
        started.push(manager.start(request).map_err(ToolError::Message)?);
    }
    let mut sections = Vec::new();
    for record in &started {
        if record.background {
            sections.push(format!(
                "Started background task {} (agent {}, model {}). Its result is delivered automatically when it finishes; task_output id=\"{}\" checks it earlier.",
                record.id,
                record.agent,
                record.model.as_deref().unwrap_or("(default)"),
                record.id
            ));
        }
    }
    for record in started.iter().filter(|record| !record.background) {
        tokio::select! {
            _ = manager.wait(&record.id, None) => {}
            () = cancel.cancelled() => {
                for record in started.iter().filter(|record| !record.background) {
                    manager.cancel(&record.id);
                }
                return Err(ToolError::Aborted);
            }
        }
        if let Some(text) = manager.result_text(&record.id) {
            sections.push(text);
        }
    }
    let ids: Vec<&str> = started.iter().map(|record| record.id.as_str()).collect();
    Ok(text_result(sections.join("\n\n"), json!({"ids": ids})))
}

async fn run_output_tool(shared: Arc<Shared>, params: Value, info: CallInfo) -> Result<ToolResult, ToolError> {
    let manager = shared.manager(&info.session_id);
    let id = str_param(&params, "id").ok_or_else(|| ToolError::Message("Missing required parameter: id".into()))?;
    let record = if bool_param(&params, "wait").unwrap_or(false) {
        let requested = params
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .map(Duration::from_millis);
        manager.wait(id, Some(shared.settings.wait_for(requested))).await
    } else {
        manager.get(id)
    };
    let Some(record) = record else {
        return Err(ToolError::Message(format!("Unknown task \"{id}\"")));
    };
    let text = if record.status.is_active() {
        format!("{}\nThe task is still {}.", status_line(&record), record.status.as_str())
    } else {
        manager.result_text(id).unwrap_or_default()
    };
    Ok(text_result(text, json!({"id": record.id, "status": record.status.as_str()})))
}

fn list_text(manager: &TaskManager) -> String {
    let records = manager.list();
    if records.is_empty() {
        return "No tasks in this session.".to_owned();
    }
    records.iter().map(status_line).collect::<Vec<_>>().join("\n")
}

fn tool_description(shared: &Shared) -> String {
    let mut text = String::from(
        "Delegate work to a subagent that runs as its own mhc session with a fresh context.\n\
Give `task` a self-contained assignment (target files, the change, how to tell it is done); the subagent cannot see this conversation. \
Use `tasks` with a shared `context` to run several independent subagents concurrently. \
Foreground calls return each `<task-result>`; `background: true` returns an id at once and the result is delivered as a follow-up message when the subagent finishes (do not poll; use task_output only when blocked on it). \
A finished task keeps its session: task_resume sends it a follow-up instead of starting over.\n\nAgents:\n",
    );
    for agent in &shared.discovery.agents {
        text.push_str(&format!(
            "- `{}`{}: {}\n",
            agent.name,
            if agent.read_only { " (read-only)" } else { "" },
            agent.description
        ));
    }
    let categories = category_names(Some(&shared.view));
    if !categories.is_empty() {
        text.push_str("\nCategories (`category` picks the model configured for that kind of work):\n");
        for name in categories {
            match category_description(Some(&shared.view), &name) {
                Some(description) => text.push_str(&format!("- `{name}`: {description}\n")),
                None => text.push_str(&format!("- `{name}`\n")),
            }
        }
    }
    text
}

fn task_item_properties() -> Value {
    json!({
        "task": {"type": "string", "description": "Self-contained assignment for the subagent"},
        "agent": {"type": "string", "description": "Agent to run as (default `task`)"},
        "category": {"type": "string", "description": "Configured category whose model to use"},
        "model": {"type": "string", "description": "Explicit `provider/model` (overrides agent and category models)"},
        "name": {"type": "string", "description": "CamelCase id for the task (generated when omitted)"},
        "background": {"type": "boolean", "description": "Return immediately and deliver the result later"},
        "isolated": {"type": "boolean", "description": "Run in an isolated copy of the workspace (needs an isolation backend)"},
        "pane": {"type": "boolean", "description": "Follow the run in a terminal pane when mhc runs inside tmux/cmux (default: configured behaviour)"}
    })
}

type Handler = Arc<
    dyn Fn(Arc<Shared>, Value, CallInfo, CancelToken) -> crate::runner::BoxFuture<'static, Result<ToolResult, ToolError>>
        + Send
        + Sync,
>;

fn define(shared: &Arc<Shared>, name: &str, label: &str, description: String, parameters: Value, handler: Handler) -> ToolDefinition {
    let shared = Arc::clone(shared);
    let mut definition = ToolDefinition::new(
        name,
        &description,
        parameters,
        Arc::new(move |call: ToolCall<'_>| {
            let shared = Arc::clone(&shared);
            let info = call_info(&call, &shared.cwd);
            let params = call.params.clone();
            let signal = call.signal.clone();
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let cancel = CancelToken::default();
                let future = handler(shared, params, info, cancel.clone());
                tokio::select! {
                    result = future => result,
                    () = signal.cancelled() => {
                        cancel.cancel();
                        Err(ToolError::Aborted)
                    }
                }
            })
        }),
    );
    label.clone_into(&mut definition.label);
    definition
}

impl TaskExtension {
    fn shared(&self, api: &ExtensionApi) -> Option<Arc<Shared>> {
        let loaded = maho_config::load_native(&api.cwd, &maho_config::process_env());
        let settings = TaskSettings::from_view(Some(&loaded.view));
        let depth = current_depth();
        if !can_spawn_at_depth(settings.max_depth, depth) {
            return None;
        }
        let discovery = discover(&api.cwd, self.agent_dir.as_deref(), loaded.view.get("agents"));
        let steer = steer_channel(self.agent_dir.as_deref(), &loaded.view);
        Some(Arc::new(Shared {
            config: self.clone(),
            cwd: api.cwd.clone(),
            view: loaded.view,
            settings,
            discovery,
            depth,
            available: OnceLock::new(),
            steer,
            sender: Some(Arc::new(ExtensionApi::new(
                api.registered.clone(),
                api.profile.clone(),
                api.events.clone(),
                api.runtime.clone(),
            ))),
        }))
    }

    /// The tool definitions for `api`'s session; empty when this session is too deep to spawn.
    pub fn tool_definitions(&self, api: &ExtensionApi) -> Vec<ToolDefinition> {
        self.shared(api).map(|shared| definitions(&shared)).unwrap_or_default()
    }
}

fn definitions(shared: &Arc<Shared>) -> Vec<ToolDefinition> {
    let mut task_properties = task_item_properties();
    if let Some(properties) = task_properties.as_object_mut() {
        properties.insert("context".into(), json!({"type": "string", "description": "Shared background for every task of a batch (goal, constraints, interfaces)"}));
        properties.insert("tasks".into(), json!({"type": "array", "description": "Batch of independent tasks run concurrently", "items": {"type": "object", "properties": task_item_properties(), "required": ["task"]}}));
    }
    let id_schema = |extra: Value| {
        let mut properties = json!({"id": {"type": "string", "description": "Task id"}});
        if let (Some(target), Some(extra)) = (properties.as_object_mut(), extra.as_object()) {
            target.extend(extra.clone());
        }
        json!({"type": "object", "properties": properties, "required": ["id"]})
    };
    vec![
        define(shared, "task", "Task", tool_description(shared), json!({"type": "object", "properties": task_properties}),
            Arc::new(|shared, params, info, cancel| Box::pin(run_task_tool(shared, params, info, cancel)))),
        define(shared, "task_output", "Task Output",
            "Get a delegated task's result (`<task-result>`) or its current status. `wait: true` blocks until it settles or `timeout_ms` passes.".to_owned(),
            id_schema(json!({"wait": {"type": "boolean"}, "timeout_ms": {"type": "number"}})),
            Arc::new(|shared, params, info, _| Box::pin(run_output_tool(shared, params, info)))),
        define(shared, "task_list", "Task List",
            "List this session's delegated tasks with their status.".to_owned(),
            json!({"type": "object", "properties": {}}),
            Arc::new(|shared, _, info, _| Box::pin(async move {
                let manager = shared.manager(&info.session_id);
                Ok(text_result(list_text(&manager), json!({})))
            }))),
        define(shared, "task_resume", "Task Resume",
            "Send a follow-up to a finished, failed, cancelled or interrupted task; it continues in its own session with its full context.".to_owned(),
            id_schema(json!({"prompt": {"type": "string", "description": "Follow-up instruction (default: continue and finish)"}, "background": {"type": "boolean"}})),
            Arc::new(|shared, params, info, cancel| Box::pin(async move {
                let manager = shared.manager(&info.session_id);
                let id = str_param(&params, "id").ok_or_else(|| ToolError::Message("Missing required parameter: id".into()))?;
                let background = bool_param(&params, "background").unwrap_or(false);
                let record = manager.resume(id, str_param(&params, "prompt"), background).map_err(ToolError::Message)?;
                if background {
                    return Ok(text_result(format!("Resumed task {} in the background (run {}).", record.id, record.runs), json!({"id": record.id})));
                }
                tokio::select! {
                    _ = manager.wait(&record.id, None) => {}
                    () = cancel.cancelled() => { manager.cancel(&record.id); return Err(ToolError::Aborted); }
                }
                Ok(text_result(manager.result_text(&record.id).unwrap_or_default(), json!({"id": record.id})))
            }))),
        define(shared, "task_cancel", "Task Cancel",
            "Cancel a delegated task by id or name: a queued task never starts, a running child is stopped, an interrupted task is marked cancelled. Messages still queued for it are dropped. Cancel is final; task_send does not continue a cancelled task.".to_owned(),
            id_schema(json!({"reason": {"type": "string", "description": "Why the task is cancelled (recorded as its error)"}})),
            Arc::new(|shared, params, info, _| Box::pin(async move {
                let manager = shared.manager(&info.session_id);
                let id = str_param(&params, "id").ok_or_else(|| ToolError::Message("Missing required parameter: id".into()))?;
                let outcome = control::cancel(&manager, &caller(&info), id, str_param(&params, "reason")).await;
                Ok(outcome_result(outcome))
            }))),
        define(shared, "task_send", "Task Send",
            "Send a message to a delegated task (id or name) or a team member (`<team>/<member>`, `<team>/*`). A queued task gets it with its first run; a running child gets it in its current turn when it can be reached, otherwise as a follow-up run once the current run finishes; a finished, failed or interrupted task continues in its own session with the message. Cancelled tasks are not continued. Messages over 32 KiB are refused: write long content to a file and send its path.".to_owned(),
            json!({"type": "object", "properties": {
                "to": {"type": "string", "description": "Task id or name, or `<team>/<member>` / `<team>/*`"},
                "message": {"type": "string", "description": "The message (at most 32 KiB)"},
                "all_sessions": {"type": "boolean", "description": "Allow messaging a task spawned by another session"}
            }, "required": ["to", "message"]}),
            Arc::new(|shared, params, info, _| Box::pin(run_send_tool(shared, params, info)))),
    ]
    .into_iter()
    .chain(addon_definitions(shared))
    .collect()
}

fn addon_definitions(shared: &Arc<Shared>) -> Vec<ToolDefinition> {
    let mut definitions = Vec::new();
    for addon in &shared.config.addons {
        for tool in addon.tools() {
            let handler = Arc::clone(&tool.handler);
            definitions.push(define(
                shared,
                &tool.name,
                &tool.label,
                tool.description.clone(),
                tool.parameters.clone(),
                Arc::new(move |shared, params, info, cancel| {
                    handler(TaskScope { shared, info: Arc::new(info) }, params, cancel)
                }),
            ));
        }
    }
    definitions
}

/// Starts the gateway inbox of a delegated run (task-control.md §1.1) at session start and
/// removes its endpoint record at shutdown.
fn register_delegated_inbox(api: &mut ExtensionApi) {
    let Some(task_id) = std::env::var(TASK_ID_ENV).ok().filter(|id| !id.trim().is_empty()) else {
        return;
    };
    let sender = Arc::new(ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    ));
    let inbox: Arc<Mutex<Option<control::DelegatedInbox>>> = Arc::default();
    let start_inbox = Arc::clone(&inbox);
    api.on(
        maho_ext_api::EventKind::SessionStart,
        Arc::new(move |_event, ctx| {
            let inbox = Arc::clone(&start_inbox);
            let sender = Arc::clone(&sender);
            let task_id = task_id.clone();
            Box::pin(async move {
                if control::delegated_receiver(ctx.mode, Some(task_id.as_str())) {
                    let env = |key: &str| std::env::var(key).ok();
                    let settings = maho_gateway::Settings::load(&ctx.cwd);
                    if settings.enabled {
                        let store = maho_gateway::Store::new(maho_gateway::default_root(&ctx.agent_dir, &env));
                        let deliver: control::DeliverFn = Arc::new(move |message, options| {
                            sender.send_message(message, options).map_err(|error| error.to_string())
                        });
                        match control::DelegatedInbox::start(
                            store,
                            ctx.session_manager.session_id(),
                            &ctx.cwd.to_string_lossy(),
                            settings.poll,
                            deliver,
                        ) {
                            Ok(started) => *lock(&inbox) = Some(started),
                            Err(error) => maho_ext_api::diagnostic(&format!(
                                "task: cannot open the delegated-run inbox: {error}"
                            )),
                        }
                    }
                }
                Ok(maho_ext_api::EventResult::None)
            })
        }),
    );
    api.on(
        maho_ext_api::EventKind::SessionShutdown,
        Arc::new(move |_event, _ctx| {
            lock(&inbox).take();
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        }),
    );
}

impl Extension for TaskExtension {
    fn register(&self, api: &mut ExtensionApi) {
        register_delegated_inbox(api);
        let Some(shared) = self.shared(api) else {
            return;
        };
        for definition in definitions(&shared) {
            if let Err(error) = api.try_register_tool(definition) {
                maho_ext_api::diagnostic(&format!("task: {error}"));
            }
        }
        let command_shared = Arc::clone(&shared);
        api.register_command(
            "tasks",
            Some("List delegated tasks of this session".to_owned()),
            None,
            Arc::new(move |_raw, ctx| {
                let manager = command_shared.manager(ctx.session_manager.session_id());
                Box::pin(async move {
                    ctx.ui.notify(&list_text(&manager), NotificationType::Info);
                    Ok(())
                })
            }),
        );
        let kill_shared = Arc::clone(&shared);
        api.register_command(
            KILL_COMMAND,
            Some("Cancel one of this session's delegated tasks".to_owned()),
            None,
            Arc::new(move |_raw, ctx| {
                let manager = kill_shared.manager(ctx.session_manager.session_id());
                let session_id = ctx.session_manager.session_id().to_owned();
                Box::pin(async move {
                    let caller = Caller {
                        root_session: current_root_session(&session_id),
                        session_id,
                    };
                    control::task_kill(&manager, &caller, &ContextUi(ctx)).await;
                    Ok(())
                })
            }),
        );
        for addon in &shared.config.addons {
            for command in addon.commands() {
                let command_shared = Arc::clone(&shared);
                let handler = Arc::clone(&command.handler);
                api.register_command(
                    &command.name,
                    Some(command.description.clone()),
                    None,
                    Arc::new(move |raw, ctx| {
                        let scope = TaskScope {
                            info: Arc::new(CallInfo {
                                cwd: command_shared.cwd.clone(),
                                session_id: ctx.session_manager.session_id().to_owned(),
                                parent_model: None,
                            }),
                            shared: Arc::clone(&command_shared),
                        };
                        let text = handler(scope, raw.to_string());
                        Box::pin(async move {
                            ctx.ui.notify(&text, NotificationType::Info);
                            Ok(())
                        })
                    }),
                );
            }
        }
    }
}
