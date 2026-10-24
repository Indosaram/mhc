//! senpi `modes/rpc/tool-child/session-child.ts` (pin 26acd9353): one session's tool child (senpi#3566, plan todo 4).
//! The child starts on the first job routed to it, never earlier; a dead child is started again (a new generation) by
//! the next job. Status is the minimal row todo 14 extends: `session_tool_child{state: "spawned" | "exited" |
//! "respawned", generation, pid}` records and the `list_sessions` row `tool_child: {state: "none" | "live" | "exited",
//! generation?, pid?}`.
//!
//! The runner types upstream imports from `core/tool-dispatch/tool-dispatcher.ts` (`ToolChildModuleJob`,
//! `ToolChildRunResult`, `ToolChildRunner`, `ToolChildKindRunner`) live in maho-core, which sits above this crate; the
//! minimal shapes this module needs are declared here (see the lane report) and maho-core adapts them.
use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::{BoxFuture, FutureExt, Shared};
use maho_ext_api::{AbortSignal, ToolChildModule};
use serde_json::{Map, Value};

use crate::client::{
    RunOptions, ToolChildClient, ToolChildClientOptions, ToolChildEnv, ToolChildError,
    ToolChildState, ToolChildStatus, ToolChildStatusState, ToolChildWake,
};
use crate::host_calls_server::HostCallHandler;
use crate::job_policy::{CrashLoop, fixture_deadline};
use crate::launch::ToolChildCommand;
use crate::launch_env_names::{ProcessEnv, process_env};
use crate::module_job::{MODULE_JOB_KIND, TOOL_CHILD_MODULE_ROOTS_ENV};

/// The tool child's `module` job (senpi#3566; the same shape todo 8's `ctx.offload` uses).
#[derive(Clone, Debug, PartialEq)]
pub struct ToolChildModuleJob {
    pub module_path: String,
    pub export: String,
    pub args: Value,
    /// A lost job is retried once on a fresh child only when this is true.
    pub idempotent: Option<bool>,
    /// Overrides the job's default deadline (`ctx.offload`'s `deadlineMs`).
    pub deadline_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolChildRunResult {
    pub value: Value,
    pub pid: i64,
    pub generation: u64,
}

/// What a job's dispatcher attaches to its host calls: the tool call the job runs for (`{ toolCallId }`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolChildHostContext {
    pub tool_call_id: Option<String>,
}

/// The call of a built-in kind job (`ToolChildKindRunner`'s third parameter).
pub struct ToolChildKindCall {
    pub tool_call_id: String,
    pub signal: Option<AbortSignal>,
    pub on_update: Option<Arc<dyn Fn(Value) + Send + Sync>>,
}

/// Runs a `module` job on the session's tool child, starting the child on first use.
pub type ToolChildRunner = Arc<
    dyn Fn(
            ToolChildModuleJob,
            Option<AbortSignal>,
        ) -> BoxFuture<'static, Result<ToolChildRunResult, ToolChildError>>
        + Send
        + Sync,
>;

/// Runs a job of a built-in kind on the session's tool child, streaming its updates.
pub type ToolChildKindRunner = Arc<
    dyn Fn(
            String,
            Option<Value>,
            ToolChildKindCall,
        ) -> BoxFuture<'static, Result<ToolChildRunResult, ToolChildError>>
        + Send
        + Sync,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionToolChildRecordState {
    Spawned,
    Exited,
    Respawned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionToolChildRecord {
    pub state: SessionToolChildRecordState,
    pub generation: u64,
    pub pid: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionToolChildRowState {
    None,
    Live,
    Exited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionToolChildRow {
    pub state: SessionToolChildRowState,
    pub generation: Option<u64>,
    pub pid: Option<i64>,
}

pub struct SessionToolChildOptions {
    pub dir: PathBuf,
    pub cwd: PathBuf,
    /// Installed extension directories the child may load `module` jobs from, read at every spawn.
    pub module_roots: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub on_record: Arc<dyn Fn(SessionToolChildRecord) + Send + Sync>,
    pub env: Option<ProcessEnv>,
    pub command: Option<ToolChildCommand>,
    /// Serves the child's reverse-bridge calls; the request's `context` is a `ToolChildHostContext` naming the calling
    /// tool call.
    pub on_host_call: Option<HostCallHandler>,
    /// Receives the child's wakes (`ToolChildWake`); the session hands each to the job definition of its kind.
    pub on_wake: Option<Arc<dyn Fn(ToolChildWake) + Send + Sync>>,
    /// The crash loop's clock (milliseconds); tests inject a fake one. Default: the wall clock.
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
}

/// The tool-declared child module of one registered tool: `tool.definition.childModule` and `tool.sourceInfo.path`
/// (maho's `ToolDefinition` has no `child_module` field, so the caller supplies the pair).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredToolChildModule {
    pub child_module: Option<ToolChildModule>,
    pub source_path: String,
}

/// The files of the extensions whose tools declare a `childModule`: the only places a `module` job may load from.
pub fn tool_child_module_roots(tools: &[RegisteredToolChildModule]) -> Vec<String> {
    // Exactly the files tools declared, never their directory: a single-file extension in a broad directory (a project
    // root, `extensions/`) would otherwise grant every module beside it.
    let mut roots: Vec<String> = Vec::new();
    for tool in tools {
        let Some(child_module) = &tool.child_module else {
            continue;
        };
        if tool.source_path.starts_with('<') {
            continue;
        }
        let path = Path::new(&child_module.path);
        let resolved = if path.is_absolute() {
            child_module.path.clone()
        } else {
            let base = Path::new(&tool.source_path)
                .parent()
                .unwrap_or_else(|| Path::new(""));
            normalize(&base.join(path)).to_string_lossy().into_owned()
        };
        if !roots.contains(&resolved) {
            roots.push(resolved);
        }
    }
    roots
}

/// `path.resolve`'s lexical normalization of `.` and `..` segments.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

type Starting = Shared<BoxFuture<'static, Result<(), ToolChildError>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Core {
    options_env: Option<ProcessEnv>,
    client: Arc<ToolChildClient>,
    starting: Mutex<Option<Starting>>,
    last: Mutex<Option<(u64, i64)>>,
    closed: Mutex<bool>,
    crash_loop: Mutex<CrashLoop>,
    on_record: Arc<dyn Fn(SessionToolChildRecord) + Send + Sync>,
}

pub struct SessionToolChild {
    core: Arc<Core>,
}

impl SessionToolChild {
    pub fn new(options: SessionToolChildOptions) -> Self {
        let now = options.now.unwrap_or_else(|| {
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0.0, |elapsed| elapsed.as_millis() as f64)
            })
        });
        let base_env = options.env.clone();
        let module_roots = options.module_roots.clone();
        let env = ToolChildEnv::Dynamic(Arc::new(move || {
            let mut env = base_env.clone().unwrap_or_else(process_env);
            env.insert(
                TOOL_CHILD_MODULE_ROOTS_ENV.to_owned(),
                serde_json::to_string(&module_roots()).unwrap_or_else(|_| "[]".to_owned()),
            );
            env
        }));
        let core = Arc::new_cyclic(|weak: &std::sync::Weak<Core>| {
            let weak = weak.clone();
            let client = Arc::new(ToolChildClient::new(ToolChildClientOptions {
                dir: options.dir,
                cwd: options.cwd,
                env: Some(env),
                command: options.command,
                on_status: Some(Arc::new(move |status| {
                    if let Some(core) = weak.upgrade() {
                        core.on_status(status);
                    }
                })),
                on_host_call: options.on_host_call,
                on_wake: options.on_wake,
                ..ToolChildClientOptions::default()
            }));
            Core {
                options_env: options.env,
                client,
                starting: Mutex::new(None),
                last: Mutex::new(None),
                closed: Mutex::new(false),
                crash_loop: Mutex::new(CrashLoop::new(move || now())),
                on_record: options.on_record,
            }
        });
        Self { core }
    }

    /// Runs a module job on this session's child. A job lost to the child's exit is retried exactly once, on a fresh
    /// generation, and only when it is idempotent; a non-idempotent job reports the error and is never re-run.
    pub fn runner(&self) -> ToolChildRunner {
        let core = self.core.clone();
        Arc::new(move |job, signal| {
            let core = core.clone();
            async move {
                match core.run_once(&job, signal.clone()).await {
                    Ok(result) => Ok(result),
                    Err(error) => {
                        let lost_to_exit = error.code == "tool_child_exited";
                        if !lost_to_exit
                            || job.idempotent != Some(true)
                            || signal.as_ref().is_some_and(AbortSignal::is_aborted)
                        {
                            return Err(error);
                        }
                        core.run_once(&job, signal).await
                    }
                }
            }
            .boxed()
        })
    }

    /// Runs a job of a built-in kind (`ToolDefinition.childJob`, e.g. `terminal.bash`) on this session's child, streaming
    /// its updates to `on_update`. Never retried: such a job's state (a PTY session) does not survive a child exit.
    pub fn kind_runner(&self) -> ToolChildKindRunner {
        let core = self.core.clone();
        Arc::new(move |kind, args, call| {
            let core = core.clone();
            async move {
                core.ensure_live().await?;
                let generation = core.client.generation();
                let pid = core.client.pid().map_or(-1, i64::from);
                let context: Arc<dyn Any + Send + Sync> = Arc::new(ToolChildHostContext {
                    tool_call_id: Some(call.tool_call_id),
                });
                let value = core
                    .client
                    .run(
                        &kind,
                        args,
                        call.signal,
                        RunOptions {
                            host_context: Some(context),
                            on_update: call.on_update,
                            ..RunOptions::default()
                        },
                    )
                    .await?;
                Ok(ToolChildRunResult {
                    value,
                    pid,
                    generation,
                })
            }
            .boxed()
        })
    }

    pub fn row(&self) -> SessionToolChildRow {
        let state = self.core.client.state();
        let Some((generation, pid)) = *lock(&self.core.last) else {
            return SessionToolChildRow {
                state: SessionToolChildRowState::None,
                generation: None,
                pid: None,
            };
        };
        if state == ToolChildState::Live {
            return SessionToolChildRow {
                state: SessionToolChildRowState::Live,
                generation: Some(self.core.client.generation()),
                pid: Some(pid),
            };
        }
        SessionToolChildRow {
            state: SessionToolChildRowState::Exited,
            generation: Some(generation),
            pid: Some(pid),
        }
    }

    pub async fn close(&self) {
        *lock(&self.core.closed) = true;
        let starting = lock(&self.core.starting).clone();
        if let Some(starting) = starting {
            let _ = starting.await;
        }
        self.core.client.close().await;
        self.core.client.ended().await;
    }
}

impl Core {
    async fn run_once(
        self: &Arc<Self>,
        job: &ToolChildModuleJob,
        signal: Option<AbortSignal>,
    ) -> Result<ToolChildRunResult, ToolChildError> {
        self.ensure_live().await?;
        let generation = self.client.generation();
        let pid = self.client.pid().map_or(-1, i64::from);
        let env = self.options_env.clone().unwrap_or_else(process_env);
        let deadline_ms = job
            .deadline_ms
            .or_else(|| fixture_deadline(&job.export, &|name| env.get(name).cloned()));
        let context: Arc<dyn Any + Send + Sync> = Arc::new(ToolChildHostContext {
            tool_call_id: calling_tool_call_id(&job.args),
        });
        let mut wire = Map::new();
        wire.insert("modulePath".into(), Value::String(job.module_path.clone()));
        wire.insert("export".into(), Value::String(job.export.clone()));
        wire.insert("args".into(), job.args.clone());
        if let Some(idempotent) = job.idempotent {
            wire.insert("idempotent".into(), Value::Bool(idempotent));
        }
        if let Some(deadline) = job.deadline_ms {
            wire.insert("deadlineMs".into(), Value::from(deadline));
        }
        let value = self
            .client
            .run(
                MODULE_JOB_KIND,
                Some(Value::Object(wire)),
                signal,
                RunOptions {
                    idempotent: Some(job.idempotent == Some(true)),
                    deadline_ms,
                    host_context: Some(context),
                    on_update: None,
                },
            )
            .await?;
        Ok(ToolChildRunResult {
            value,
            pid,
            generation,
        })
    }

    async fn ensure_live(self: &Arc<Self>) -> Result<(), ToolChildError> {
        if *lock(&self.closed) {
            return Err(ToolChildError::new(
                "tool_child_unavailable",
                "the session is closed",
            ));
        }
        if self.client.state() == ToolChildState::Live {
            return Ok(());
        }
        if lock(&self.crash_loop).unavailable() {
            return Err(ToolChildError::new(
                "tool_child_unavailable",
                "the tool child keeps exiting; it is paused for a minute",
            ));
        }
        let starting = {
            let mut slot = lock(&self.starting);
            match &*slot {
                Some(starting) => starting.clone(),
                None => {
                    let core = self.clone();
                    let starting: Starting = async move {
                        let outcome = core.client.start().await;
                        if outcome.is_err() {
                            // A child that dies before ready is an exit too: it counts toward the crash loop.
                            lock(&core.crash_loop).record_exit();
                        }
                        *lock(&core.starting) = None;
                        outcome
                    }
                    .boxed()
                    .shared();
                    *slot = Some(starting.clone());
                    starting
                }
            }
        };
        starting.await
    }

    fn on_status(&self, status: ToolChildStatus) {
        match status.state {
            ToolChildStatusState::Spawned => {
                let respawn = {
                    let mut last = lock(&self.last);
                    let respawn = last.is_some();
                    *last = Some((status.generation, status.pid));
                    respawn
                };
                (self.on_record)(SessionToolChildRecord {
                    state: if respawn {
                        SessionToolChildRecordState::Respawned
                    } else {
                        SessionToolChildRecordState::Spawned
                    },
                    generation: status.generation,
                    pid: status.pid,
                });
            }
            ToolChildStatusState::Exited => {
                lock(&self.crash_loop).record_exit();
                (self.on_record)(SessionToolChildRecord {
                    state: SessionToolChildRecordState::Exited,
                    generation: status.generation,
                    pid: status.pid,
                });
            }
        }
    }
}

/// The tool call a module job runs for (the dispatcher puts it in the job's arguments); `None` for other jobs.
fn calling_tool_call_id(args: &Value) -> Option<String> {
    args.as_object()?
        .get("toolCallId")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // senpi#3566 todo 4 / 6: the session-level policy. The upstream cases that run a real `module` job on a child
    // (first-job spawn + SIGKILL respawn, concurrent first jobs, idempotent retry, non-idempotent no-retry) need the
    // module-job fixture extension and the production role entry; they live with the cli lane. The cases below need no
    // child: the closed refusal, the crash-loop pause (3 failed starts in 5 s, fake clock) and the module roots.

    const BOUND: Duration = Duration::from_secs(30);

    fn job() -> ToolChildModuleJob {
        ToolChildModuleJob {
            module_path: "/x/offload.ts".into(),
            export: "whoAmI".into(),
            args: serde_json::json!({ "code": 0 }),
            idempotent: None,
            deadline_ms: None,
        }
    }

    fn session(
        dir: &Path,
        command: ToolChildCommand,
        records: Arc<Mutex<Vec<SessionToolChildRecord>>>,
    ) -> SessionToolChild {
        SessionToolChild::new(SessionToolChildOptions {
            dir: dir.join("ch"),
            cwd: dir.into(),
            module_roots: Arc::new(Vec::new),
            on_record: Arc::new(move |record| lock(&records).push(record)),
            env: None,
            command: Some(command),
            on_host_call: None,
            on_wake: None,
            now: Some(Arc::new(|| 0.0)),
        })
    }

    #[tokio::test]
    async fn starts_nothing_until_the_first_job_and_reports_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let records = Arc::new(Mutex::new(Vec::new()));
        let child = session(
            dir.path(),
            ToolChildCommand {
                command: "/nonexistent/senpi".into(),
                args: Vec::new(),
            },
            records.clone(),
        );
        assert_eq!(
            child.row(),
            SessionToolChildRow {
                state: SessionToolChildRowState::None,
                generation: None,
                pid: None
            }
        );
        assert!(lock(&records).is_empty());
    }

    #[tokio::test]
    async fn refuses_jobs_after_close() {
        let dir = tempfile::tempdir().expect("tempdir");
        let child = session(
            dir.path(),
            ToolChildCommand {
                command: "/nonexistent/senpi".into(),
                args: Vec::new(),
            },
            Arc::new(Mutex::new(Vec::new())),
        );
        tokio::time::timeout(BOUND, child.close())
            .await
            .expect("closes in time");
        let runner = child.runner();
        let failure = tokio::time::timeout(BOUND, runner(job(), None))
            .await
            .expect("in time")
            .expect_err("closed");
        assert_eq!(failure.code, "tool_child_unavailable");
        assert!(
            failure.message.contains("tool_child_unavailable"),
            "{}",
            failure.message
        );
    }

    #[tokio::test]
    async fn pauses_after_three_exits_within_5_s_and_refuses_the_next_job_without_a_spawn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let child = session(
            dir.path(),
            ToolChildCommand {
                command: "/nonexistent/senpi".into(),
                args: Vec::new(),
            },
            Arc::new(Mutex::new(Vec::new())),
        );
        let runner = child.runner();
        for _ in 0..3 {
            let failure = tokio::time::timeout(BOUND, runner(job(), None))
                .await
                .expect("in time")
                .expect_err("no child");
            assert_eq!(failure.code, "tool_child_bootstrap_failed");
        }
        let paused = tokio::time::timeout(BOUND, runner(job(), None))
            .await
            .expect("in time")
            .expect_err("paused");
        assert_eq!(paused.code, "tool_child_unavailable");
        assert!(
            paused.message.contains("paused for a minute"),
            "{}",
            paused.message
        );
        assert_eq!(child.core.client.generation(), 3);
    }

    #[test]
    fn module_roots_are_exactly_the_declared_files_deduplicated_and_skip_synthetic_sources() {
        let tool = |module: Option<&str>, source: &str| RegisteredToolChildModule {
            child_module: module.map(|path| ToolChildModule {
                path: path.into(),
                export: "run".into(),
            }),
            source_path: source.into(),
        };
        let roots = tool_child_module_roots(&[
            tool(Some("./job.ts"), "/ext/a/index.ts"),
            tool(Some("./job.ts"), "/ext/a/other.ts"),
            tool(Some("/abs/job.ts"), "/ext/b/index.ts"),
            tool(Some("../up.ts"), "/ext/c/sub/index.ts"),
            tool(Some("./inline.ts"), "<inline:1>"),
            tool(None, "/ext/d/index.ts"),
        ]);
        assert_eq!(roots, vec!["/ext/a/job.ts", "/abs/job.ts", "/ext/c/up.ts"]);
    }

    #[test]
    fn the_calling_tool_call_comes_from_the_job_arguments() {
        assert_eq!(
            calling_tool_call_id(&serde_json::json!({ "toolCallId": "t1" })).as_deref(),
            Some("t1")
        );
        assert_eq!(
            calling_tool_call_id(&serde_json::json!({ "toolCallId": 3 })),
            None
        );
        assert_eq!(calling_tool_call_id(&Value::Null), None);
    }
}
