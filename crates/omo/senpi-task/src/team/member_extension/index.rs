//! Port of `team/member-extension/index.ts`.
//!
//! Host seams (senpi `ExtensionAPI`, `setInterval` timers and the task record store
//! factory) are modelled as minimal traits/closures so tests can drive them directly.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value, json};
use team_core::config::TeamModeConfig;
use utils::logger::log;

use crate::state::{TaskId, parse_task_id};
use crate::team::member_extension::qa_inject_hold::create_qa_after_inject_hold;
use crate::team::member_extension::self_poller::{
    MemberSelfPoller, MemberSelfPollerDeps, SelfPollerError, TeamMessageEvent,
    create_member_self_poller,
};
use crate::team::member_extension::tools::{
    MemberTaskSendDeps, MemberTaskSendTool, create_member_task_send_tool,
};

pub use crate::team::member_extension::identity::{
    MEMBER_EXTENSION_BUNDLE_NAME, MEMBER_IDENTITY_ENV, MEMBER_PROCESS_ENV_NAMES,
    MEMBER_TASK_ID_ENV, MEMBER_TEAM_CONFIG_ENV, is_team_member_process,
};

pub const MEMBER_POLL_INTERVAL_MS: u64 = 1_000;
pub const ACK_POLL_INTERVAL_MS: u64 = 100;
pub const SESSION_DIR_ENV: &str = "SENPI_CODING_AGENT_SESSION_DIR";
pub const TEAM_MESSAGE_CUSTOM_TYPE: &str = "senpi-task:team-message";

// ---------------------------------------------------------------------------
// Parsed env + errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ParsedMemberExtensionEnv {
    pub team_run_id: String,
    pub member_name: String,
    pub task_id: TaskId,
    pub state_dir: String,
    pub session_dir: String,
    /// `base_dir` is guaranteed to be `Some`.
    pub config: TeamModeConfig,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberExtensionConfigErrorCode {
    MissingEnv,
    InvalidIdentity,
    InvalidTaskId,
    InvalidTeamConfig,
}

impl MemberExtensionConfigErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingEnv => "missing_env",
            Self::InvalidIdentity => "invalid_identity",
            Self::InvalidTaskId => "invalid_task_id",
            Self::InvalidTeamConfig => "invalid_team_config",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct MemberExtensionConfigError {
    pub message: String,
    pub code: MemberExtensionConfigErrorCode,
}

impl MemberExtensionConfigError {
    pub fn new(message: impl Into<String>, code: MemberExtensionConfigErrorCode) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }

    pub fn name(&self) -> &'static str {
        "MemberExtensionConfigError"
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MemberExtensionError {
    #[error(transparent)]
    Config(#[from] MemberExtensionConfigError),
    #[error(transparent)]
    Poller(#[from] SelfPollerError),
}

// ---------------------------------------------------------------------------
// Host seams
// ---------------------------------------------------------------------------

/// Custom message sent through `pi.sendMessage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    pub custom_type: String,
    pub content: String,
    pub display: bool,
}

/// Options passed to `pi.sendMessage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageOptions {
    pub trigger_turn: bool,
    pub deliver_as: String,
}

pub type ExtensionEventHandler = Box<dyn Fn() -> Result<(), MemberExtensionError> + Send + Sync>;

/// Minimal model of senpi's `ExtensionAPI` used by the member extension.
pub trait ExtensionApi: Send + Sync {
    fn send_message(&self, message: CustomMessage, options: SendMessageOptions);
    fn register_tool(&self, tool: MemberTaskSendTool);
    fn on(&self, event: &str, handler: ExtensionEventHandler);
}

pub type IntervalCallback = Box<dyn Fn() + Send + Sync>;

/// Handle returned by [`IntervalScheduler::set_interval`] (`clearInterval`).
pub trait IntervalHandle: Send + Sync {
    fn clear(&self);
}

/// Models `setInterval`.
pub trait IntervalScheduler: Send + Sync {
    fn set_interval(&self, interval_ms: u64, callback: IntervalCallback) -> Box<dyn IntervalHandle>;
}

/// Default scheduler: one background thread per interval, stoppable via a condvar.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadIntervalScheduler;

type StopSignal = Arc<(Mutex<bool>, Condvar)>;

struct ThreadIntervalHandle {
    stop: StopSignal,
}

impl IntervalHandle for ThreadIntervalHandle {
    fn clear(&self) {
        let (lock, cvar) = &*self.stop;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cvar.notify_all();
    }
}

impl IntervalScheduler for ThreadIntervalScheduler {
    fn set_interval(&self, interval_ms: u64, callback: IntervalCallback) -> Box<dyn IntervalHandle> {
        let stop: StopSignal = Arc::new((Mutex::new(false), Condvar::new()));
        let thread_stop = Arc::clone(&stop);
        let interval = Duration::from_millis(interval_ms);
        let spawned = std::thread::Builder::new()
            .name("senpi-task-member-interval".to_string())
            .spawn(move || {
                let (lock, cvar) = &*thread_stop;
                loop {
                    {
                        let guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
                        let (guard, _) = cvar
                            .wait_timeout_while(guard, interval, |stopped| !*stopped)
                            .unwrap_or_else(PoisonError::into_inner);
                        if *guard {
                            return;
                        }
                    }
                    callback();
                }
            });
        if let Err(error) = spawned {
            log(
                "senpi-task member extension timer spawn failed",
                Some(&json!({ "error": error.to_string() })),
            );
        }
        Box::new(ThreadIntervalHandle { stop })
    }
}

/// Models `store.appendEvent(taskId, event)` of the task record store.
pub type TaskEventAppender = Arc<dyn Fn(&str, TeamMessageEvent) + Send + Sync>;

/// Models `createTaskRecordStore({ project_dir: stateDir, task: { state_dir: stateDir } })`.
pub type CreateEventAppenderFn = Box<dyn Fn(&ParsedMemberExtensionEnv) -> TaskEventAppender>;

pub struct MemberExtensionOptions {
    /// Stands in for `process.env`.
    pub env: HashMap<String, String>,
    pub create_event_appender: CreateEventAppenderFn,
    /// Defaults to [`ThreadIntervalScheduler`].
    pub scheduler: Option<Arc<dyn IntervalScheduler>>,
}

// ---------------------------------------------------------------------------
// Active runtime registry (WeakMap<ExtensionAPI, ActiveRuntime>)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RuntimeTimers {
    started: bool,
    poll_timer: Option<Box<dyn IntervalHandle>>,
    ack_timer: Option<Box<dyn IntervalHandle>>,
}

struct ActiveRuntime {
    poller: Arc<MemberSelfPoller>,
    scheduler: Arc<dyn IntervalScheduler>,
    state: Mutex<RuntimeTimers>,
}

impl ActiveRuntime {
    fn state(&self) -> MutexGuard<'_, RuntimeTimers> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_started(&self) -> bool {
        self.state().started
    }
}

type RuntimeRegistry = Vec<(Weak<dyn ExtensionApi>, Arc<ActiveRuntime>)>;

static ACTIVE_RUNTIMES: Mutex<RuntimeRegistry> = Mutex::new(Vec::new());

fn registry() -> MutexGuard<'static, RuntimeRegistry> {
    let mut guard = ACTIVE_RUNTIMES.lock().unwrap_or_else(PoisonError::into_inner);
    guard.retain(|(key, _)| key.strong_count() > 0);
    guard
}

/// Reports whether an active member runtime is registered for `pi`.
pub fn has_active_member_runtime(pi: &Arc<dyn ExtensionApi>) -> bool {
    let key = Arc::downgrade(pi);
    registry().iter().any(|(existing, _)| existing.ptr_eq(&key))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Resolves `./omo-member.js` relative to the given extension file URL or path.
pub fn resolve_member_extension_entry_path(extension_url: &str) -> String {
    let path = extension_url.strip_prefix("file://").unwrap_or(extension_url);
    let dir = match path.rfind('/') {
        Some(index) => &path[..=index],
        None => "",
    };
    format!("{dir}{MEMBER_EXTENSION_BUNDLE_NAME}")
}

pub fn parse_member_extension_env(
    env: &HashMap<String, String>,
) -> Result<ParsedMemberExtensionEnv, MemberExtensionConfigError> {
    let identity = required_env(env, MEMBER_IDENTITY_ENV)?;
    let task_id_raw = required_env(env, MEMBER_TASK_ID_ENV)?;
    let team_config_raw = required_env(env, MEMBER_TEAM_CONFIG_ENV)?;
    let session_dir = required_env(env, SESSION_DIR_ENV)?;

    let identity_parts: Vec<&str> = identity.split("::").collect();
    let invalid_identity = || {
        MemberExtensionConfigError::new(
            format!("{MEMBER_IDENTITY_ENV} must be '<teamRunId>::<memberName>'"),
            MemberExtensionConfigErrorCode::InvalidIdentity,
        )
    };
    let [team_run_id, member_name] = identity_parts.as_slice() else {
        return Err(invalid_identity());
    };
    if !is_uuid(team_run_id) || !is_member_name(member_name) {
        return Err(invalid_identity());
    }

    let task_id = parse_task_id(task_id_raw).map_err(|_| {
        MemberExtensionConfigError::new(
            "SENPI_TASK_MEMBER_TASK_ID must be a valid st_ task id",
            MemberExtensionConfigErrorCode::InvalidTaskId,
        )
    })?;

    let raw_config = parse_json_record(team_config_raw)?;
    let state_dir = raw_config.get("stateDir").and_then(Value::as_str);
    let members = parse_members(raw_config.get("members"))?;
    // zod strips unknown keys; drop the extension-only fields before schema parsing.
    let schema_input: Map<String, Value> = raw_config
        .iter()
        .filter(|(key, _)| key.as_str() != "stateDir" && key.as_str() != "members")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let config_result = TeamModeConfig::parse(&Value::Object(schema_input));

    let malformed = || {
        MemberExtensionConfigError::new(
            "SENPI_TASK_TEAM_CONFIG is malformed",
            MemberExtensionConfigErrorCode::InvalidTeamConfig,
        )
    };
    let state_dir = match state_dir {
        Some(state_dir) if !state_dir.is_empty() => state_dir.to_string(),
        _ => return Err(malformed()),
    };
    let config = match config_result {
        Ok(config) if config.base_dir.is_some() => config,
        _ => return Err(malformed()),
    };
    if !members.iter().any(|member| member == member_name) {
        return Err(malformed());
    }

    Ok(ParsedMemberExtensionEnv {
        team_run_id: (*team_run_id).to_string(),
        member_name: (*member_name).to_string(),
        task_id,
        state_dir,
        session_dir: session_dir.to_string(),
        config,
        members,
    })
}

/// Port of the default export `registerMemberExtension(pi)`.
pub fn register_member_extension(
    pi: &Arc<dyn ExtensionApi>,
    options: MemberExtensionOptions,
) -> Result<(), MemberExtensionError> {
    if has_active_member_runtime(pi) {
        return Ok(());
    }
    let parsed = parse_member_extension_env(&options.env)?;
    let appender = (options.create_event_appender)(&parsed);
    let after_inject = create_qa_after_inject_hold(&options.env);
    let task_id = parsed.task_id.to_string();

    let poller_appender = Arc::clone(&appender);
    let poller_task_id = task_id.clone();
    let inject_target = Arc::downgrade(pi);
    let poller = create_member_self_poller(MemberSelfPollerDeps {
        team_run_id: parsed.team_run_id.clone(),
        member_name: parsed.member_name.clone(),
        config: parsed.config.clone(),
        session_dir: PathBuf::from(&parsed.session_dir),
        inject: Box::new(move |content: &str| {
            if let Some(pi) = inject_target.upgrade() {
                pi.send_message(
                    CustomMessage {
                        custom_type: TEAM_MESSAGE_CUSTOM_TYPE.to_string(),
                        content: content.to_string(),
                        display: false,
                    },
                    SendMessageOptions {
                        trigger_turn: true,
                        deliver_as: "steer".to_string(),
                    },
                );
            }
        }),
        append_event: Some(Box::new(move |event: TeamMessageEvent| {
            poller_appender(&poller_task_id, event);
        })),
        after_inject,
    });

    let scheduler = options
        .scheduler
        .unwrap_or_else(|| Arc::new(ThreadIntervalScheduler) as Arc<dyn IntervalScheduler>);
    let runtime = Arc::new(ActiveRuntime {
        poller: Arc::new(poller),
        scheduler,
        state: Mutex::new(RuntimeTimers::default()),
    });
    registry().push((Arc::downgrade(pi), Arc::clone(&runtime)));

    let tool_appender = Arc::clone(&appender);
    pi.register_tool(create_member_task_send_tool(MemberTaskSendDeps {
        team_run_id: parsed.team_run_id.clone(),
        member_name: parsed.member_name.clone(),
        task_id,
        config: parsed.config.clone(),
        members: parsed.members.clone(),
        append_event: Some(Box::new(move |task_id: &str, event: TeamMessageEvent| {
            tool_appender(task_id, event);
        })),
        now: None,
        new_message_id: None,
    }));

    let start_runtime_ref = Arc::clone(&runtime);
    pi.on(
        "session_start",
        Box::new(move || start_runtime(&start_runtime_ref).map_err(MemberExtensionError::from)),
    );
    let stop_runtime_ref = runtime;
    let stop_target = Arc::downgrade(pi);
    pi.on(
        "session_shutdown",
        Box::new(move || {
            stop_runtime(&stop_target, &stop_runtime_ref);
            Ok(())
        }),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Runtime lifecycle
// ---------------------------------------------------------------------------

fn start_runtime(runtime: &Arc<ActiveRuntime>) -> Result<(), SelfPollerError> {
    {
        let mut state = runtime.state();
        if state.started {
            return Ok(());
        }
        state.started = true;
    }
    let result = run_start(runtime);
    if result.is_err() {
        runtime.state().started = false;
    }
    result
}

fn run_start(runtime: &Arc<ActiveRuntime>) -> Result<(), SelfPollerError> {
    runtime.poller.recover_reservations()?;
    if !runtime.is_started() {
        return Ok(());
    }
    runtime.poller.poll_once(None)?;

    let mut state = runtime.state();
    if !state.started {
        return Ok(());
    }
    let poll_poller = Arc::clone(&runtime.poller);
    state.poll_timer = Some(runtime.scheduler.set_interval(
        MEMBER_POLL_INTERVAL_MS,
        Box::new(move || run_safely("poll", poll_poller.poll_once(None))),
    ));
    let ack_poller = Arc::clone(&runtime.poller);
    state.ack_timer = Some(runtime.scheduler.set_interval(
        ACK_POLL_INTERVAL_MS,
        Box::new(move || run_safely("ack", ack_poller.check_pending_acks())),
    ));
    Ok(())
}

fn stop_runtime(pi: &Weak<dyn ExtensionApi>, runtime: &Arc<ActiveRuntime>) {
    {
        let mut state = runtime.state();
        state.started = false;
        if let Some(timer) = state.poll_timer.take() {
            timer.clear();
        }
        if let Some(timer) = state.ack_timer.take() {
            timer.clear();
        }
    }
    runtime.poller.shutdown();
    registry().retain(|(key, _)| !key.ptr_eq(pi));
}

fn run_safely(operation: &str, result: Result<(), SelfPollerError>) {
    if let Err(error) = result {
        log(
            "senpi-task member extension poll failed",
            Some(&json!({
                "operation": operation,
                "error": format!("{}: {error}", error.name()),
            })),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn required_env<'a>(
    env: &'a HashMap<String, String>,
    name: &str,
) -> Result<&'a str, MemberExtensionConfigError> {
    match env.get(name) {
        Some(value) if !value.is_empty() => Ok(value.as_str()),
        _ => Err(MemberExtensionConfigError::new(
            format!("Missing {name}"),
            MemberExtensionConfigErrorCode::MissingEnv,
        )),
    }
}

fn parse_json_record(raw: &str) -> Result<Map<String, Value>, MemberExtensionConfigError> {
    if let Ok(Value::Object(record)) = serde_json::from_str::<Value>(raw) {
        return Ok(record);
    }
    Err(MemberExtensionConfigError::new(
        "SENPI_TASK_TEAM_CONFIG must be a JSON object",
        MemberExtensionConfigErrorCode::InvalidTeamConfig,
    ))
}

fn parse_members(value: Option<&Value>) -> Result<Vec<String>, MemberExtensionConfigError> {
    let malformed = || {
        MemberExtensionConfigError::new(
            "SENPI_TASK_TEAM_CONFIG.members is malformed",
            MemberExtensionConfigErrorCode::InvalidTeamConfig,
        )
    };
    let Some(Value::Array(items)) = value else {
        return Err(malformed());
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut members: Vec<String> = Vec::new();
    for item in items {
        let Some(member) = item.as_str().filter(|member| is_member_name(member)) else {
            return Err(malformed());
        };
        if seen.insert(member) {
            members.push(member.to_string());
        }
    }
    Ok(members)
}

/// `/^[a-z0-9-]+$/`
fn is_member_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// `/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i`
fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => *byte == b'-',
        14 => (b'1'..=b'8').contains(byte),
        19 => matches!(byte.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'),
        _ => byte.is_ascii_hexdigit(),
    })
}
