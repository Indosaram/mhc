//! Tool dispatch seam (senpi#3566): the single place a tool call is resolved to a
//! placement and executed, and where user bash (`!`, RPC `bash`) resolves its
//! BashOperations.
//!
//! The dispatcher kind follows the session's tool runtime (`--tool-runtime`, absent
//! = in-process). Nothing moves yet: `InProcessToolDispatcher` calls
//! `definition.execute` exactly as the wrappers did before, and `ChildToolDispatcher`
//! refuses child-placed work with `tool_runtime_unavailable` until the tool child
//! runtime lands (senpi#3566 todo 4). An explicit `operations` override stays with
//! its caller, so caller-injected work is never re-routed.
use super::child_bash_operations::child_bash_operations;
use super::in_process_module::{
    InProcessLive, InProcessModuleRuntime, InProcessRun, StuckModuleNotice, run_module_in_process,
};
use super::offload::{BoxError, OffloadError, OffloadErrorCode, resolve_path};
use async_trait::async_trait;
use maho_ext_api::AbortSignal;
use maho_tool_child::host_calls_server::HostCallHandler;
pub use maho_tool_child::session_child::{
    ToolChildKindCall, ToolChildKindRunner, ToolChildModuleJob, ToolChildRunResult, ToolChildRunner,
};
pub use maho_tools::definition::{ToolChildJob, ToolChildJobPlacement};
use maho_tools::{
    bash::{BashExecOptions, BashExit, BashOperations, create_local_bash_operations},
    definition::{ToolCall, ToolContent, ToolError, ToolFuture, ToolResult},
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    panic::AssertUnwindSafe,
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

pub use maho_ext_api::ToolPlacement;

pub const TOOL_RUNTIME_UNAVAILABLE: &str = "tool_runtime_unavailable";

/// The session's tool runtime (`--tool-runtime`, `cli/tool-runtime.ts`'s `ToolRuntimeKind`): maho-core sits below
/// maho-cli, so the two-valued kind the dispatcher selects on is declared here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolRuntimeKind {
    #[default]
    InProcess,
    Child,
}

impl ToolRuntimeKind {
    /// The `--tool-runtime` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in-process",
            Self::Child => "child",
        }
    }
}

/// `{ code: "tool_runtime_unavailable" }`, the details of an unavailable result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolUnavailableDetails;

impl ToolUnavailableDetails {
    pub fn to_value(self) -> Value {
        json!({ "code": TOOL_RUNTIME_UNAVAILABLE })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDispatchedInfo {
    pub tool_call_id: String,
    pub placement: ToolPlacement,
    pub pid: i64,
    pub generation: u64,
}

pub type ToolDispatchObserver = Arc<dyn Fn(&ToolDispatchedInfo) + Send + Sync>;

pub type ToolDispatchNotify =
    Arc<dyn Fn(&str, Option<serde_json::Map<String, Value>>) + Send + Sync>;

pub use maho_ext_api::ToolPlacementInfo;

pub use maho_ext_api::ToolDispatchCall;

/// Where an offloaded module ran, and what it returned.
#[derive(Clone, Debug, PartialEq)]
pub struct OffloadOutcome {
    pub value: Value,
    pub placement: ToolPlacement,
    pub pid: i64,
    pub generation: u64,
}

#[derive(Clone)]
pub struct ToolDispatcherOptions {
    pub runtime: ToolRuntimeKind,
    pub notify: Option<ToolDispatchNotify>,
    pub child_runner: Option<ToolChildRunner>,
    pub child_kind_runner: Option<ToolChildKindRunner>,
    /// The session's host-call handler: an in-process `ctx.offload` module gets the same host the child's worker builds.
    pub host_calls: Option<HostCallHandler>,
    /// How long past its deadline an in-process module may run before it is reported stuck (default 10 s).
    pub stuck_grace_ms: Option<u64>,
    /// Told when an in-process module is stuck (it keeps its locks; their waiters fail fast).
    pub on_module_stuck: Option<Arc<dyn Fn(StuckModuleNotice) + Send + Sync>>,
}

impl ToolDispatcherOptions {
    pub fn new(runtime: ToolRuntimeKind) -> Self {
        Self {
            runtime,
            notify: None,
            child_runner: None,
            child_kind_runner: None,
            host_calls: None,
            stuck_grace_ms: None,
            on_module_stuck: None,
        }
    }
}

#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    fn kind(&self) -> ToolRuntimeKind;
    async fn run<'a>(&self, call: ToolDispatchCall<'a>) -> Result<ToolResult, BoxError>;
    fn bash_operations(&self, shell_path: Option<&str>) -> Arc<dyn BashOperations>;
    /// Subscribes `observer`; the returned closure unsubscribes it.
    fn on_dispatched(&self, observer: ToolDispatchObserver) -> Box<dyn FnOnce() + Send + Sync>;
    /// Runs a registered child module (`ctx.offload`) where this runtime places it.
    async fn offload(
        &self,
        job: ToolChildModuleJob,
        signal: Option<AbortSignal>,
    ) -> Result<OffloadOutcome, BoxError>;
    /// The session is closed or replaced: work this runtime still runs in this process is closed; settles once it let go.
    async fn dispose(&self);
}

#[derive(Clone, Default)]
pub struct InProcessDispatchOptions {
    pub host_calls: Option<HostCallHandler>,
    pub stuck_grace_ms: Option<u64>,
    pub on_module_stuck: Option<Arc<dyn Fn(StuckModuleNotice) + Send + Sync>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Placement of one call: only a definition that opted into `"child"` while the
/// session runs the child runtime dispatches there, and never when the definition
/// is server-pinned (injected `operations` / policy override captured at creation).
pub fn resolve_tool_placement(info: &ToolPlacementInfo, runtime: ToolRuntimeKind) -> ToolPlacement {
    if runtime != ToolRuntimeKind::Child {
        return ToolPlacement::Server;
    }
    if info.placement != ToolPlacement::Child {
        return ToolPlacement::Server;
    }
    if info.server_pinned {
        return ToolPlacement::Server;
    }
    ToolPlacement::Child
}

/// What `BaseToolDispatcher` holds for every dispatcher kind.
struct BaseToolDispatcher {
    kind: ToolRuntimeKind,
    observers: Arc<Mutex<Vec<(u64, ToolDispatchObserver)>>>,
    next_observer: AtomicU64,
    pin_notified: Mutex<HashSet<String>>,
    notify_sink: Option<ToolDispatchNotify>,
}

impl BaseToolDispatcher {
    fn new(kind: ToolRuntimeKind, notify: Option<ToolDispatchNotify>) -> Self {
        Self {
            kind,
            observers: Arc::default(),
            next_observer: AtomicU64::new(0),
            pin_notified: Mutex::default(),
            notify_sink: notify,
        }
    }

    fn on_dispatched(&self, observer: ToolDispatchObserver) -> Box<dyn FnOnce() + Send + Sync> {
        let id = self.next_observer.fetch_add(1, Ordering::Relaxed);
        lock(&self.observers).push((id, observer));
        let observers = self.observers.clone();
        Box::new(move || lock(&observers).retain(|(entry, _)| *entry != id))
    }

    fn notify(&self, event: &str, data: Option<serde_json::Map<String, Value>>) {
        if let Some(sink) = &self.notify_sink {
            sink(event, data);
        }
    }

    fn placement_for(&self, info: &ToolPlacementInfo) -> ToolPlacement {
        let placement = resolve_tool_placement(info, self.kind);
        if placement == ToolPlacement::Server
            && self.kind == ToolRuntimeKind::Child
            && info.placement == ToolPlacement::Child
            && lock(&self.pin_notified).insert(info.name.clone())
        {
            let mut data = serde_json::Map::new();
            data.insert("tool".into(), Value::String(info.name.clone()));
            data.insert("reason".into(), Value::String("server_pinned".into()));
            self.notify("tool_placement_pinned", Some(data));
        }
        placement
    }

    async fn run_on_server(&self, call: ToolDispatchCall<'_>) -> Result<ToolResult, BoxError> {
        self.emit_dispatched(ToolDispatchedInfo {
            tool_call_id: call.tool_call_id.clone(),
            placement: ToolPlacement::Server,
            pid: i64::from(std::process::id()),
            generation: 0,
        });
        let result = (call.definition.execute)(ToolCall {
            id: &call.tool_call_id,
            params: call.params.clone(),
            signal: call.signal.clone().unwrap_or_default(),
            on_update: call.on_update.clone(),
            context: call.ctx,
        })
        .await?;
        Ok(result)
    }

    fn emit_dispatched(&self, info: ToolDispatchedInfo) {
        let observers: Vec<ToolDispatchObserver> = lock(&self.observers)
            .iter()
            .map(|(_, observer)| observer.clone())
            .collect();
        for observer in observers {
            // An observer is diagnostics only: its failure must never become the tool call's failure.
            if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(|| observer(&info))) {
                let mut data = serde_json::Map::new();
                data.insert(
                    "message".into(),
                    Value::String(panic_message(panic.as_ref())),
                );
                self.notify("tool_dispatch_observer_failed", Some(data));
            }
        }
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    "observer panicked".to_owned()
}

pub struct InProcessToolDispatcher {
    base: BaseToolDispatcher,
    options: InProcessDispatchOptions,
    live: InProcessLive,
    disposed: AtomicBool,
}

impl InProcessToolDispatcher {
    pub fn new(notify: Option<ToolDispatchNotify>, options: InProcessDispatchOptions) -> Self {
        Self {
            base: BaseToolDispatcher::new(ToolRuntimeKind::InProcess, notify),
            options,
            live: Arc::default(),
            disposed: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl ToolDispatcher for InProcessToolDispatcher {
    fn kind(&self) -> ToolRuntimeKind {
        ToolRuntimeKind::InProcess
    }

    async fn run<'a>(&self, call: ToolDispatchCall<'a>) -> Result<ToolResult, BoxError> {
        self.base.run_on_server(call).await
    }

    fn bash_operations(&self, shell_path: Option<&str>) -> Arc<dyn BashOperations> {
        create_local_bash_operations(shell_path)
    }

    fn on_dispatched(&self, observer: ToolDispatchObserver) -> Box<dyn FnOnce() + Send + Sync> {
        self.base.on_dispatched(observer)
    }

    async fn offload(
        &self,
        job: ToolChildModuleJob,
        signal: Option<AbortSignal>,
    ) -> Result<OffloadOutcome, BoxError> {
        if self.disposed.load(Ordering::SeqCst) {
            return Err(OffloadError::new(
                OffloadErrorCode::OffloadSessionClosed,
                format!("{}: the session was closed", job.module_path),
            )
            .into());
        }
        let value = run_module_in_process(
            job,
            signal,
            InProcessModuleRuntime {
                host_calls: self.options.host_calls.clone(),
                stuck_grace_ms: self.options.stuck_grace_ms,
                on_stuck: self.options.on_module_stuck.clone(),
                live: Some(self.live.clone()),
            },
        )
        .await?;
        Ok(OffloadOutcome {
            value,
            placement: ToolPlacement::Server,
            pid: i64::from(std::process::id()),
            generation: 0,
        })
    }

    /// Closes every in-process module run still live; settles once each has released its locks (`InProcessRun::close`).
    async fn dispose(&self) {
        self.disposed.store(true, Ordering::SeqCst);
        let runs: Vec<Arc<InProcessRun>> = lock(&self.live).clone();
        futures::future::join_all(runs.iter().map(|run| run.close())).await;
    }
}

/// Refuses child-placed work and child-backed bash until the tool child runtime
/// lands (senpi#3566 todo 4 / todo 9); server-placed work still runs in-process,
/// so a session under `--tool-runtime child` stays usable at this stage.
pub struct ChildToolDispatcher {
    base: BaseToolDispatcher,
    child_runner: Option<ToolChildRunner>,
    kind_runner: Option<ToolChildKindRunner>,
}

impl ChildToolDispatcher {
    pub fn new(
        notify: Option<ToolDispatchNotify>,
        child_runner: Option<ToolChildRunner>,
        kind_runner: Option<ToolChildKindRunner>,
    ) -> Self {
        Self {
            base: BaseToolDispatcher::new(ToolRuntimeKind::Child, notify),
            child_runner,
            kind_runner,
        }
    }

    /// The server's body first (its own state), then the child's job; the result carries both contents, the child's details.
    async fn run_on_both(
        &self,
        runner: &ToolChildKindRunner,
        child_job: &ToolChildJob,
        call: ToolDispatchCall<'_>,
    ) -> Result<ToolResult, BoxError> {
        let spec = clone_spec(&call);
        let local = self.base.run_on_server(call).await?;
        let mut remote = self.run_kind(runner, child_job, spec).await?;
        let mut content = local.content;
        content.append(&mut remote.content);
        remote.content = content;
        Ok(remote)
    }

    async fn run_kind(
        &self,
        runner: &ToolChildKindRunner,
        child_job: &ToolChildJob,
        call: ToolDispatchCall<'_>,
    ) -> Result<ToolResult, BoxError> {
        let on_update = call.on_update.clone();
        let kind = child_job.kind.clone();
        let args = (child_job.args)(&call.params, &call.tool_call_id, call.ctx);
        let ToolChildRunResult {
            value,
            pid,
            generation,
        } = runner(
            kind.clone(),
            Some(args),
            ToolChildKindCall {
                tool_call_id: call.tool_call_id.clone(),
                signal: call.signal.clone(),
                on_update: on_update.map(|on_update| {
                    Arc::new(move |update: Value| {
                        if let Some(result) = as_tool_result(&update) {
                            // A streamed update is best effort: its failure never fails the call.
                            let _ = on_update(result);
                        }
                    }) as Arc<dyn Fn(Value) + Send + Sync>
                }),
            },
        )
        .await?;
        self.base.emit_dispatched(ToolDispatchedInfo {
            tool_call_id: call.tool_call_id.clone(),
            placement: ToolPlacement::Child,
            pid,
            generation,
        });
        let Some(result) = as_tool_result(&value) else {
            return Err(format!(
                "tool_child_invalid_result: {kind} must return {{ content: [...] }}"
            )
            .into());
        };
        match &child_job.result {
            None => Ok(result),
            Some(map) => Ok(map(&call.params, result, generation)),
        }
    }

    async fn run_on_child(
        &self,
        runner: &ToolChildRunner,
        job: ToolChildModuleJob,
        call: &ToolDispatchCall<'_>,
    ) -> Result<ToolResult, BoxError> {
        let ToolChildRunResult {
            value,
            pid,
            generation,
        } = runner(job.clone(), call.signal.clone()).await?;
        self.base.emit_dispatched(ToolDispatchedInfo {
            tool_call_id: call.tool_call_id.clone(),
            placement: ToolPlacement::Child,
            pid,
            generation,
        });
        as_tool_result(&value).ok_or_else(|| {
            format!(
                "tool_child_invalid_result: {}#{} must return {{ content: [...] }}",
                job.module_path, job.export
            )
            .into()
        })
    }
}

/// The call without its `ctx` borrow's lifetime tied to the run: a second view for the child half of `"both"`.
fn clone_spec<'a>(call: &ToolDispatchCall<'a>) -> ToolDispatchCall<'a> {
    ToolDispatchCall {
        definition: call.definition,
        placement: call.placement.clone(),
        tool_call_id: call.tool_call_id.clone(),
        params: call.params.clone(),
        signal: call.signal.clone(),
        on_update: call.on_update.clone(),
        ctx: call.ctx,
        source_path: call.source_path.clone(),
    }
}

/// Hands a dispatcher to the extension host's wrappers (`maho_ext_api::ToolRunDispatch`).
pub struct DispatcherRun(pub Arc<dyn ToolDispatcher>);

impl maho_ext_api::ToolRunDispatch for DispatcherRun {
    fn run_tool<'a>(
        &'a self,
        call: ToolDispatchCall<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolResult, BoxError>> + Send + 'a>,
    > {
        Box::pin(async move { self.0.run(call).await })
    }
}

#[async_trait]
impl ToolDispatcher for ChildToolDispatcher {
    fn kind(&self) -> ToolRuntimeKind {
        ToolRuntimeKind::Child
    }

    async fn run<'a>(&self, call: ToolDispatchCall<'a>) -> Result<ToolResult, BoxError> {
        if self.base.placement_for(&call.placement) == ToolPlacement::Server {
            return self.base.run_on_server(call).await;
        }
        if let Some(child_job) = call.placement.child_job.clone() {
            let location = match &child_job.placement {
                Some(placement) => placement(&call.params),
                None => ToolChildJobPlacement::Child,
            };
            if location == ToolChildJobPlacement::Server {
                return self.base.run_on_server(call).await;
            }
            let Some(kind_runner) = self.kind_runner.clone() else {
                return Ok(unavailable_tool_result(&call.placement.name));
            };
            if location == ToolChildJobPlacement::Both {
                return self.run_on_both(&kind_runner, &child_job, call).await;
            }
            return self.run_kind(&kind_runner, &child_job, call).await;
        }
        let job = child_module_job(&call);
        let (Some(job), Some(runner)) = (job, self.child_runner.clone()) else {
            return Ok(unavailable_tool_result(&call.placement.name));
        };
        self.run_on_child(&runner, job, &call).await
    }

    fn bash_operations(&self, shell_path: Option<&str>) -> Arc<dyn BashOperations> {
        match &self.kind_runner {
            None => unavailable_bash_operations(),
            Some(runner) => child_bash_operations(runner.clone(), shell_path),
        }
    }

    fn on_dispatched(&self, observer: ToolDispatchObserver) -> Box<dyn FnOnce() + Send + Sync> {
        self.base.on_dispatched(observer)
    }

    /// Always on the session's child, started on demand; never in-process (no fallback).
    async fn offload(
        &self,
        job: ToolChildModuleJob,
        signal: Option<AbortSignal>,
    ) -> Result<OffloadOutcome, BoxError> {
        let Some(runner) = &self.child_runner else {
            return Err(OffloadError::new(
                OffloadErrorCode::ToolRuntimeUnavailable,
                "this session has no tool child to run the module on",
            )
            .into());
        };
        let ToolChildRunResult {
            value,
            pid,
            generation,
        } = runner(job, signal).await?;
        Ok(OffloadOutcome {
            value,
            placement: ToolPlacement::Child,
            pid,
            generation,
        })
    }

    async fn dispose(&self) {}
}

pub fn create_tool_dispatcher(options: ToolDispatcherOptions) -> Arc<dyn ToolDispatcher> {
    match options.runtime {
        ToolRuntimeKind::Child => Arc::new(ChildToolDispatcher::new(
            options.notify,
            options.child_runner,
            options.child_kind_runner,
        )),
        ToolRuntimeKind::InProcess => Arc::new(InProcessToolDispatcher::new(
            options.notify,
            InProcessDispatchOptions {
                host_calls: options.host_calls,
                stuck_grace_ms: options.stuck_grace_ms,
                on_module_stuck: options.on_module_stuck,
            },
        )),
    }
}

fn child_module_job(call: &ToolDispatchCall<'_>) -> Option<ToolChildModuleJob> {
    let child_module = call.placement.child_module.as_ref()?;
    let absolute = Path::new(&child_module.path).is_absolute();
    let base = call
        .source_path
        .as_deref()
        .map(|source| Path::new(source).parent().unwrap_or_else(|| Path::new("")));
    if !absolute && base.is_none() {
        return None;
    }
    let module_path = if absolute {
        child_module.path.clone()
    } else {
        resolve_path(base.unwrap_or_else(|| Path::new("")), &child_module.path)
    };
    Some(ToolChildModuleJob {
        module_path,
        export: child_module.export.clone(),
        args: json!({ "params": call.params, "toolCallId": call.tool_call_id }),
        idempotent: None,
        deadline_ms: None,
    })
}

/// A value is a tool result when it is an object with a `content` array.
fn as_tool_result(value: &Value) -> Option<ToolResult> {
    if !value.get("content").is_some_and(Value::is_array) {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

pub fn unavailable_tool_result(tool_name: &str) -> ToolResult {
    ToolResult {
        content: vec![ToolContent::text(format!(
            "{TOOL_RUNTIME_UNAVAILABLE}: tool \"{tool_name}\" is placed on the session tool child, which this build does not provide yet (senpi#3566)."
        ))],
        details: Some(ToolUnavailableDetails.to_value()),
        structured_content: None,
        is_error: Some(true),
    }
}

struct UnavailableBashOperations;

impl BashOperations for UnavailableBashOperations {
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _cwd: &'a Path,
        _options: BashExecOptions,
    ) -> ToolFuture<'a, BashExit> {
        Box::pin(async {
            Err(ToolError::Message(format!(
                "{TOOL_RUNTIME_UNAVAILABLE}: user bash needs the session tool child, which this build does not provide yet (senpi#3566)"
            )))
        })
    }
}

pub fn unavailable_bash_operations() -> Arc<dyn BashOperations> {
    Arc::new(UnavailableBashOperations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::ToolChildModule;
    use maho_tools::ToolDefinition;
    use std::sync::Mutex as StdMutex;

    fn definition() -> ToolDefinition {
        ToolDefinition::new(
            "probe",
            "probe",
            json!({ "type": "object" }),
            Arc::new(|_call| Box::pin(async { Ok(ToolResult::text("in-process")) })),
        )
    }

    fn info(name: &str) -> ToolPlacementInfo {
        ToolPlacementInfo {
            name: name.into(),
            ..Default::default()
        }
    }

    fn call<'a>(
        definition: &'a ToolDefinition,
        placement: ToolPlacementInfo,
        source_path: Option<&str>,
    ) -> ToolDispatchCall<'a> {
        ToolDispatchCall {
            definition,
            placement,
            tool_call_id: "call-1".into(),
            params: json!({ "n": 1 }),
            signal: None,
            on_update: None,
            ctx: None,
            source_path: source_path.map(str::to_owned),
        }
    }

    type Jobs = Arc<StdMutex<Vec<ToolChildModuleJob>>>;

    fn recording_runner(value: Value) -> (ToolChildRunner, Jobs) {
        let jobs: Jobs = Arc::default();
        let recorded = jobs.clone();
        let runner: ToolChildRunner = Arc::new(move |job, _signal| {
            lock(&recorded).push(job);
            let value = value.clone();
            Box::pin(async move {
                Ok(ToolChildRunResult {
                    value,
                    pid: 4242,
                    generation: 3,
                })
            })
        });
        (runner, jobs)
    }

    fn child_module(path: &str) -> Option<ToolChildModule> {
        Some(ToolChildModule {
            path: path.into(),
            export: "run".into(),
        })
    }

    // senpi#3566 todo 4: under the child runtime, a child-placed tool with a file-backed body dispatches as a `module` job
    // through the injected runner; everything else keeps its explicit behavior (server or tool_runtime_unavailable).
    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_child_placed_tools_file_backed_body_as_a_module_job_and_reports_pid_and_generation()
     {
        let result = json!({ "content": [{ "type": "text", "text": "from child" }], "details": { "ok": true } });
        let (runner, jobs) = recording_runner(result);
        let dispatcher = ChildToolDispatcher::new(None, Some(runner), None);
        let dispatched: Arc<StdMutex<Vec<ToolDispatchedInfo>>> = Arc::default();
        let sink = dispatched.clone();
        let _off = dispatcher.on_dispatched(Arc::new(move |info| lock(&sink).push(info.clone())));
        let def = definition();
        let placement = ToolPlacementInfo {
            placement: ToolPlacement::Child,
            child_module: child_module("./body.ts"),
            ..info("probe")
        };
        let out = dispatcher
            .run(call(&def, placement, Some("/ext/dir/index.ts")))
            .await
            .expect("dispatched");
        assert_eq!(out.content, vec![ToolContent::text("from child")]);
        assert_eq!(out.details, Some(json!({ "ok": true })));
        assert_eq!(
            *lock(&jobs),
            vec![ToolChildModuleJob {
                module_path: "/ext/dir/body.ts".into(),
                export: "run".into(),
                args: json!({ "params": { "n": 1 }, "toolCallId": "call-1" }),
                idempotent: None,
                deadline_ms: None,
            }]
        );
        assert_eq!(
            *lock(&dispatched),
            vec![ToolDispatchedInfo {
                tool_call_id: "call-1".into(),
                placement: ToolPlacement::Child,
                pid: 4242,
                generation: 3
            }]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn never_falls_back_to_in_process_no_runner_or_no_child_module_gives_tool_runtime_unavailable()
     {
        let def = definition();
        let with_module = ToolPlacementInfo {
            placement: ToolPlacement::Child,
            child_module: child_module("/abs/body.ts"),
            ..info("probe")
        };
        let no_runner = ChildToolDispatcher::new(None, None, None)
            .run(call(&def, with_module, None))
            .await
            .expect("result");
        assert_eq!(no_runner.is_error, Some(true));
        assert_eq!(
            no_runner.details,
            Some(json!({ "code": TOOL_RUNTIME_UNAVAILABLE }))
        );
        let (runner, jobs) = recording_runner(json!({ "content": [] }));
        let no_module = ChildToolDispatcher::new(None, Some(runner.clone()), None)
            .run(call(
                &def,
                ToolPlacementInfo {
                    placement: ToolPlacement::Child,
                    ..info("probe")
                },
                None,
            ))
            .await
            .expect("result");
        assert_eq!(
            no_module.details,
            Some(json!({ "code": TOOL_RUNTIME_UNAVAILABLE }))
        );
        let relative_without_source = ToolPlacementInfo {
            placement: ToolPlacement::Child,
            child_module: child_module("./body.ts"),
            ..info("probe")
        };
        let unresolved = ChildToolDispatcher::new(None, Some(runner), None)
            .run(call(&def, relative_without_source, None))
            .await
            .expect("result");
        assert_eq!(
            unresolved.details,
            Some(json!({ "code": TOOL_RUNTIME_UNAVAILABLE }))
        );
        assert!(lock(&jobs).is_empty());
    }

    #[tokio::test]
    async fn keeps_server_placed_and_server_pinned_tools_on_the_server() {
        let (runner, jobs) = recording_runner(json!({ "content": [] }));
        let dispatcher = ChildToolDispatcher::new(None, Some(runner), None);
        let def = definition();
        let pinned = ToolPlacementInfo {
            placement: ToolPlacement::Child,
            server_pinned: true,
            child_module: child_module("/abs/b.ts"),
            ..info("probe")
        };
        let out = dispatcher
            .run(call(&def, pinned, None))
            .await
            .expect("pinned");
        assert_eq!(out.content, vec![ToolContent::text("in-process")]);
        let out = dispatcher
            .run(call(&def, info("probe"), None))
            .await
            .expect("server");
        assert_eq!(out.content, vec![ToolContent::text("in-process")]);
        assert!(lock(&jobs).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_child_result_that_is_not_a_tool_result() {
        let (runner, _jobs) = recording_runner(json!({ "nope": true }));
        let def = definition();
        let placement = ToolPlacementInfo {
            placement: ToolPlacement::Child,
            child_module: child_module("/abs/body.ts"),
            ..info("probe")
        };
        let error = ChildToolDispatcher::new(None, Some(runner), None)
            .run(call(&def, placement, None))
            .await
            .err()
            .map(|error| error.to_string());
        assert!(error.is_some_and(|message| message.contains("tool_child_invalid_result")));
    }

    // senpi#3574 review: a dispatch observer is diagnostics only. One that throws must not fail or alter the tool call, must
    // not stop later observers, and must be reported through the session's notify sink on BOTH runtimes.
    #[tokio::test]
    async fn a_throwing_dispatch_observer_leaves_the_result_intact_and_is_reported_once() {
        for runtime in [ToolRuntimeKind::InProcess, ToolRuntimeKind::Child] {
            type Notices = Arc<StdMutex<Vec<(String, Option<serde_json::Map<String, Value>>)>>>;
            let notices: Notices = Arc::default();
            let sink = notices.clone();
            let mut options = ToolDispatcherOptions::new(runtime);
            options.notify = Some(Arc::new(move |event, data| {
                lock(&sink).push((event.to_owned(), data));
            }));
            let dispatcher = create_tool_dispatcher(options);
            let seen: Arc<StdMutex<Vec<(String, ToolPlacement)>>> = Arc::default();
            let record = seen.clone();
            let _first = dispatcher.on_dispatched(Arc::new(|_| panic!("observer exploded")));
            let _second = dispatcher.on_dispatched(Arc::new(move |info| {
                lock(&record).push((info.tool_call_id.clone(), info.placement));
            }));
            let def = ToolDefinition::new(
                "probe",
                "probe",
                json!({ "type": "object" }),
                Arc::new(|_call| {
                    Box::pin(async {
                        Ok(ToolResult {
                            details: Some(json!({ "ok": true })),
                            ..ToolResult::text("ran")
                        })
                    })
                }),
            );
            let result = dispatcher
                .run(ToolDispatchCall {
                    params: json!({}),
                    ..call(&def, info("probe"), None)
                })
                .await
                .expect("ran");
            assert_eq!(result.content, vec![ToolContent::text("ran")]);
            assert_eq!(result.details, Some(json!({ "ok": true })));
            assert_eq!(
                *lock(&seen),
                vec![("call-1".to_owned(), ToolPlacement::Server)]
            );
            let notices = lock(&notices);
            assert_eq!(notices.len(), 1, "{runtime:?}");
            assert_eq!(notices[0].0, "tool_dispatch_observer_failed");
            assert_eq!(
                notices[0].1.as_ref().and_then(|data| data.get("message")),
                Some(&Value::String("observer exploded".into()))
            );
        }
    }
}
