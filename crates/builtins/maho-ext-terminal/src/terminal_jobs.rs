//! The terminal's tool bodies as tool-child job kinds (senpi#3566 todo 9): the child owns one TerminalManager - its PTYs
//! and background sessions - for the life of the child, and runs the same bash / bash_output / bash_input / bash_resize
//! / kill_bash bodies the in-process extension runs. Every command starts behind the execution barrier: the go-line
//! shim runs inside the PTY (echo off around the read), the server records the started session's group, and only then
//! is the go-line written. Foreground output streams back as job updates.
use crate::manager::TerminalManager;
use crate::notify::NOTICE_TAIL_MAX_CHARS;
use crate::output_format::sanitize_terminal_output;
use crate::settings::{TERMINAL_SETTINGS_DEFAULTS, TimeoutAction};
use crate::tools::bash::{execute_configured_bash_with_launch, pty_bash_schema};
use crate::tools::bash_input::{BashInputInput, bash_input_schema, execute_bash_input};
use crate::tools::bash_output::{bash_output_schema, execute_bash_output_view};
use crate::tools::bash_resize::{bash_resize_schema, execute_bash_resize};
use crate::tools::child_placement::{
    TERMINAL_CHILD_JOB_KINDS, TerminalBackgroundWake, child_terminal_id_scope,
};
use crate::tools::context::{
    TerminalBarrierFuture, TerminalChildLaunch, TerminalExitView, TerminalLaunchBarrier,
    TerminalLaunchSession, TerminalToolResult, resolve_terminal_id,
};
use crate::tools::kill_bash::{execute_kill_bash_shared, kill_bash_schema};
use maho_tool_child::entry::{
    HandlerError, ToolChildJobContext, ToolChildJobError, ToolChildJobHandler,
};
use maho_tool_child::launch_gate::LaunchRequest;
use maho_tool_child::protocol::{LaunchApi, LaunchContainment, LaunchLifetime, LaunchOrigin};
use maho_tools::definition::{ToolCall, ToolError, ToolResult, ToolUpdateCallback};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// Inside the PTY: no echo while it waits for the go-line, then the real command keeps the PTY's pid and group.
const PTY_GO_SHIM: &str = "stty -echo 2>/dev/null; IFS= read -r _go || exit 127; stty echo 2>/dev/null; exec \"$0\" \"$@\"";

pub use crate::tools::child_placement::TERMINAL_CHILD_JOB_KINDS as TERMINAL_JOB_KINDS;

/// What the server sends with every terminal job: the tool's params and the session facts its body reads.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalJobArgs {
    pub params: Value,
    pub tool_call_id: String,
    pub cwd: String,
    pub env: HashMap<String, String>,
    pub shell_path: Option<String>,
    pub default_cols: f64,
    pub default_rows: f64,
    pub timeout_action: Option<TimeoutAction>,
}

/// `Number(value) || 0`: a number as is, a numeric string converted, anything else (or NaN) zero.
fn number_or_zero(value: Option<&Value>) -> f64 {
    let number = match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::String(text)) => text.trim().parse::<f64>().unwrap_or(0.0),
        Some(Value::Bool(true)) => 1.0,
        _ => 0.0,
    };
    if number.is_nan() { 0.0 } else { number }
}

pub fn parse_args(value: &Value) -> Result<TerminalJobArgs, ToolChildJobError> {
    let field = |name: &str| value.as_object().and_then(|fields| fields.get(name));
    let (Some(Value::String(cwd)), Some(Value::String(tool_call_id)), Some(Value::Object(env))) =
        (field("cwd"), field("toolCallId"), field("env"))
    else {
        return Err(ToolChildJobError::new(
            "terminal_job_bad_args",
            "a terminal job needs cwd, toolCallId and env",
        ));
    };
    Ok(TerminalJobArgs {
        params: field("params").cloned().unwrap_or(Value::Null),
        tool_call_id: tool_call_id.clone(),
        cwd: cwd.clone(),
        env: env
            .iter()
            .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
            .collect(),
        shell_path: field("shellPath")
            .and_then(Value::as_str)
            .map(str::to_owned),
        default_cols: number_or_zero(field("defaultCols")),
        default_rows: number_or_zero(field("defaultRows")),
        timeout_action: match field("timeoutAction").and_then(Value::as_str) {
            Some("background") => Some(TimeoutAction::Background),
            Some("kill") => Some(TimeoutAction::Kill),
            _ => None,
        },
    })
}

/// The execution barrier of one job: `wrap` puts the go-line shim in front of the command, `release` records the
/// started session with the server (ledgered as `pty.spawn`) and only then writes the go-line.
struct JobLaunchBarrier {
    job: ToolChildJobContext,
}

impl TerminalLaunchBarrier for JobLaunchBarrier {
    fn wrap(&self, command: &str, args: &[String]) -> (String, Vec<String>) {
        let mut wrapped = vec!["-c".to_owned(), PTY_GO_SHIM.to_owned(), command.to_owned()];
        wrapped.extend(args.iter().cloned());
        ("/bin/sh".to_owned(), wrapped)
    }

    fn release(&self, session: TerminalLaunchSession) -> TerminalBarrierFuture<'_> {
        Box::pin(async move {
            let Some(pid) = session.pid else {
                return Err("terminal_spawn_failed: the PTY reported no pid".to_owned());
            };
            let gate = &self.job.gate;
            let gated = gate
                .request(LaunchRequest {
                    job_id: self.job.job_id.clone(),
                    origin: LaunchOrigin::Terminal,
                    api: LaunchApi::PtySpawn,
                    containment: LaunchContainment::OwnGroup,
                    lifetime: LaunchLifetime::Owned,
                    pid: Some(pid),
                    pgid: Some(pid),
                })
                .map_err(|error| error.to_string())?;
            let launch_id = gated.launch_id.clone();
            let done_gate = gate.clone();
            let mut exit = session.exit.clone();
            tokio::spawn(async move {
                loop {
                    let settled = exit.borrow_and_update().clone();
                    if let Some(settled) = settled {
                        let exit_code = settled.ok().and_then(|exit| exit.exit_code);
                        done_gate.done(&launch_id, exit_code, None);
                        return;
                    }
                    if exit.changed().await.is_err() {
                        done_gate.done(&launch_id, None, None);
                        return;
                    }
                }
            });
            if !gate.skips_ack_wait() {
                gated
                    .acked
                    .await
                    .map_err(|_| "the launch was never acknowledged".to_owned())?;
            }
            gate.invoke_now(&gated.launch_id)
                .map_err(|error| error.to_string())?;
            (session.write)(b"go\n")
        })
    }
}

/// `utf16 slice(start)` of text, with a split surrogate pair decoding to U+FFFD like the lossy JS slice.
fn utf16_slice_from(text: &str, start: usize) -> String {
    let units = text.encode_utf16().collect::<Vec<_>>();
    String::from_utf16_lossy(&units[start.min(units.len())..])
}

/// The tail a background exit notice shows, sanitized here so the wake carries only what the notice can use.
fn exit_summary(id: &str, runtime: &TerminalExitView) -> Value {
    let sanitized = sanitize_terminal_output(runtime.full_output());
    let output = sanitized.trim_end();
    let length = output.encode_utf16().count();
    let output = if length > NOTICE_TAIL_MAX_CHARS {
        utf16_slice_from(output, length - NOTICE_TAIL_MAX_CHARS - 1)
    } else {
        output.to_owned()
    };
    json!({
        "event": "background_exit",
        "id": id,
        "exit": runtime.exit_result.as_ref().map_or(Value::Null, |exit| json!({
            "exitCode": exit.exit_code,
            "signal": Value::Null,
            "cancelled": exit.cancelled,
            "timedOut": exit.timed_out,
        })),
        "output": output,
    })
}

fn wake_value(wake: &TerminalBackgroundWake) -> Value {
    match wake {
        TerminalBackgroundWake::BackgroundStart {
            id,
            description,
            started_at_ms,
        } => json!({
            "event": "background_start",
            "id": id,
            "description": description,
            "startedAtMs": started_at_ms,
        }),
        TerminalBackgroundWake::BackgroundExit { id, exit, output } => {
            exit_summary(id, &TerminalExitView::new(exit.clone(), output.clone()))
        }
    }
}

type ManagerCell = Arc<Mutex<Option<Arc<Mutex<TerminalManager>>>>>;
type JobFuture = Pin<Box<dyn Future<Output = Result<Option<Value>, HandlerError>> + Send>>;

/// Tells the server something happened on this child outside any job's lifetime (a background session started or
/// exited); the wake is never dropped, a failed delivery is reported on stderr.
fn tell(job: &ToolChildJobContext, event: &str, id: &str, value: Value) {
    let sent = (job.wake)(TERMINAL_JOB_KINDS.bash.to_owned(), value);
    let (event, id) = (event.to_owned(), id.to_owned());
    tokio::spawn(async move {
        if let Err(error) = sent.await {
            maho_ext_api::diagnostic(&format!(
                "tool-child: terminal {event} for {id} not delivered: {error}"
            ));
        }
    });
}

/// Upstream's `ctx.onBackgroundStart` / `ctx.onBackgroundExit` of the child's tool context, for a session the bash
/// body left running: its start now, its exit when the session settles.
fn relay_background(
    job: &ToolChildJobContext,
    manager: &Arc<Mutex<TerminalManager>>,
    params: &Value,
    result: &TerminalToolResult,
) {
    let Some(details) = &result.details else {
        return;
    };
    if details.get("background").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let Some(id) = details.get("bash_id").and_then(Value::as_str) else {
        return;
    };
    let description = params
        .get("description")
        .or_else(|| params.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let started_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0);
    tell(
        job,
        "background_start",
        id,
        wake_value(&TerminalBackgroundWake::BackgroundStart {
            id: id.to_owned(),
            description: description.to_owned(),
            started_at_ms,
        }),
    );
    let exit = manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(id)
        .map(|runtime| runtime.subscribe_exit());
    let Some(mut exit) = exit else {
        return;
    };
    let (job, manager, id) = (job.clone(), manager.clone(), id.to_owned());
    tokio::spawn(async move {
        loop {
            if exit.borrow_and_update().is_some() {
                break;
            }
            if exit.changed().await.is_err() {
                return;
            }
        }
        let view = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .map(|runtime| TerminalExitView::of_runtime(runtime))
            .unwrap_or_else(|| TerminalExitView::new(None, String::new()));
        let summary = exit_summary(&id, &view);
        tell(&job, "background_exit", &id, summary);
    });
}

/// The child's one terminal manager (shared by every terminal job of this child) is made on the first job, its ids
/// scoped by this child's generation so they never collide with the server's or an earlier child's.
fn manager_for(
    cell: &ManagerCell,
    generation: u64,
) -> Result<Arc<Mutex<TerminalManager>>, ToolChildJobError> {
    let mut slot = cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(manager) = slot.as_ref() {
        return Ok(manager.clone());
    }
    let manager = TerminalManager::default()
        .with_id_scope(&child_terminal_id_scope(generation))
        .map_err(|error| ToolChildJobError::new("tool_child_job_failed", error.to_string()))?;
    let manager = Arc::new(Mutex::new(manager));
    *slot = Some(manager.clone());
    Ok(manager)
}

/// The schema of the job's tool: the params are checked before the body runs (`Value.Check`).
fn kind_schema(kind: &str) -> Value {
    if kind == TERMINAL_CHILD_JOB_KINDS.bash {
        pty_bash_schema()
    } else if kind == TERMINAL_CHILD_JOB_KINDS.output {
        bash_output_schema()
    } else if kind == TERMINAL_CHILD_JOB_KINDS.input {
        bash_input_schema()
    } else if kind == TERMINAL_CHILD_JOB_KINDS.resize {
        bash_resize_schema()
    } else {
        kill_bash_schema()
    }
}

fn check_params(kind: &str, params: &Value) -> Result<(), ToolChildJobError> {
    let schema = kind_schema(kind);
    if maho_ai::utils::validation::strict_errors(&schema, params).is_empty() {
        Ok(())
    } else {
        Err(ToolChildJobError::new(
            "terminal_job_bad_args",
            "the tool's parameters do not match its schema",
        ))
    }
}

fn job_failed(message: impl Into<String>) -> HandlerError {
    let message: String = message.into();
    match message.strip_prefix("terminal_spawn_failed: ") {
        Some(reason) => Box::new(ToolChildJobError::new("terminal_spawn_failed", reason)),
        None => message.into(),
    }
}

fn result_value(result: TerminalToolResult) -> Result<Option<Value>, HandlerError> {
    Ok(Some(
        serde_json::to_value(result.into_tool_result()).map_err(|error| error.to_string())?,
    ))
}

async fn run_job(
    kind: &'static str,
    cell: ManagerCell,
    value: Value,
    job: ToolChildJobContext,
) -> Result<Option<Value>, HandlerError> {
    let args = parse_args(&value)?;
    let manager = manager_for(&cell, job.generation)?;
    check_params(kind, &args.params)?;
    let params = &args.params;
    let text_param = |name: &str| params.get(name).and_then(Value::as_str);
    if kind == TERMINAL_CHILD_JOB_KINDS.bash {
        let mut settings = TERMINAL_SETTINGS_DEFAULTS;
        if args.default_cols > 0.0 {
            settings.default_cols = args.default_cols;
        }
        if args.default_rows > 0.0 {
            settings.default_rows = args.default_rows;
        }
        if let Some(action) = args.timeout_action {
            settings.timeout_action = action;
        }
        let update = job.update.clone();
        let update_call_id = args.tool_call_id.clone();
        let on_update: ToolUpdateCallback = Arc::new(move |result: ToolResult| {
            let sent = update(serde_json::to_value(&result).map_err(ToolError::from)?);
            let tool_call_id = update_call_id.clone();
            tokio::spawn(async move {
                if let Err(error) = sent.await {
                    maho_ext_api::diagnostic(&format!(
                        "tool-child: {tool_call_id} update not sent: {error}"
                    ));
                }
            });
            Ok(())
        });
        let launch = TerminalChildLaunch {
            barrier: Arc::new(JobLaunchBarrier { job: job.clone() }),
            env: args
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
            cwd: args.cwd.clone().into(),
        };
        let result = execute_configured_bash_with_launch(
            manager.clone(),
            ToolCall {
                id: &args.tool_call_id,
                params: params.clone(),
                signal: job.signal.clone(),
                on_update: Some(on_update),
                context: None,
            },
            std::env::var("PI_BASH_FOREGROUND_SECONDS").ok().as_deref(),
            &settings,
            args.shell_path.as_deref(),
            Some(&launch),
        )
        .await
        .map_err(job_failed)?;
        relay_background(&job, &manager, params, &result);
        return result_value(result);
    }
    if kind == TERMINAL_CHILD_JOB_KINDS.kill {
        let all = params.get("all").and_then(Value::as_bool).unwrap_or(false);
        return result_value(execute_kill_bash_shared(&manager, text_param("bash_id"), all).await);
    }
    let id = text_param("bash_id").unwrap_or_default();
    let mut manager = manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let resolved = resolve_terminal_id(&manager, id);
    let runtime = manager.get(&resolved);
    if kind == TERMINAL_CHILD_JOB_KINDS.output {
        let result = execute_bash_output_view(
            runtime.as_deref(),
            id,
            text_param("filter"),
            text_param("view") == Some("screen"),
        )
        .map_err(|error| error.to_string())?;
        return result_value(result);
    }
    if kind == TERMINAL_CHILD_JOB_KINDS.input {
        let keys = params
            .get("keys")
            .and_then(Value::as_array)
            .map(|keys| {
                keys.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return result_value(execute_bash_input(
            runtime,
            BashInputInput {
                bash_id: id,
                input: text_param("input"),
                keys: &keys,
                submit: params.get("submit").and_then(Value::as_bool),
            },
        ));
    }
    result_value(execute_bash_resize(
        runtime.as_deref(),
        id,
        params
            .get("cols")
            .and_then(Value::as_f64)
            .unwrap_or(f64::NAN),
        params
            .get("rows")
            .and_then(Value::as_f64)
            .unwrap_or(f64::NAN),
    ))
}

fn handler(kind: &'static str, cell: ManagerCell) -> ToolChildJobHandler {
    Arc::new(move |value: Value, job: ToolChildJobContext| -> JobFuture {
        Box::pin(run_job(kind, cell.clone(), value, job))
    })
}

/// The handlers. The child's one terminal manager is made on the first job (or injected).
pub fn create_terminal_job_handlers(
    injected: Option<Arc<Mutex<TerminalManager>>>,
) -> BTreeMap<String, ToolChildJobHandler> {
    let cell: ManagerCell = Arc::new(Mutex::new(injected));
    [
        TERMINAL_JOB_KINDS.bash,
        TERMINAL_JOB_KINDS.output,
        TERMINAL_JOB_KINDS.input,
        TERMINAL_JOB_KINDS.resize,
        TERMINAL_JOB_KINDS.kill,
    ]
    .into_iter()
    .map(|kind| (kind.to_owned(), handler(kind, cell.clone())))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use maho_tool_child::channel::open_channel_listener;
    #[cfg(unix)]
    use maho_tool_child::entry::{
        TOOL_CHILD_GENERATION_ENV, ToolChildEntryOptions, run_tool_child,
    };
    #[cfg(unix)]
    use maho_tool_child::protocol::{ChildToServer, ServerToChild, ToolChildMessage};
    #[cfg(unix)]
    use std::time::Duration;

    // senpi#3566 todo 9: the terminal's bash body runs on the tool child (the PTY shell's parent is the child), its launch
    // passes the execution barrier (ledgered as pty.spawn, acknowledged before the command's first output reaches the
    // server), and its foreground output streams back as job updates. The child runs in this process here; the server
    // side of the channel is the test.

    #[cfg(unix)]
    const BOUND: Duration = Duration::from_secs(60);

    #[test]
    fn job_args_need_cwd_tool_call_id_and_env() {
        let error =
            parse_args(&json!({ "toolCallId": "t1", "cwd": "/tmp" })).expect_err("env is required");
        assert_eq!(error.code, "terminal_job_bad_args");
        assert_eq!(
            error.to_string(),
            "a terminal job needs cwd, toolCallId and env"
        );
        let args = parse_args(&json!({
            "params": { "command": "true" },
            "toolCallId": "t1",
            "cwd": "/tmp",
            "env": { "A": "1", "B": 2 },
            "shellPath": "/bin/sh",
            "defaultCols": 80,
            "defaultRows": "24",
            "timeoutAction": "kill",
        }))
        .expect("valid args");
        assert_eq!(args.env, HashMap::from([("A".to_owned(), "1".to_owned())]));
        assert_eq!(args.shell_path.as_deref(), Some("/bin/sh"));
        assert_eq!((args.default_cols, args.default_rows), (80.0, 24.0));
        assert_eq!(args.timeout_action, Some(TimeoutAction::Kill));
        let args =
            parse_args(&json!({ "toolCallId": "t", "cwd": "/", "env": {}, "timeoutAction": "x" }))
                .expect("valid args");
        assert_eq!(
            (args.default_cols, args.default_rows, args.timeout_action),
            (0.0, 0.0, None)
        );
    }

    #[test]
    fn exit_summary_sanitizes_and_keeps_the_notice_tail() {
        let long = format!("{}\n\n", "x".repeat(NOTICE_TAIL_MAX_CHARS + 50));
        let summary = exit_summary(
            "bash_c1_1",
            &TerminalExitView::new(
                Some(maho_pty::PtyExit {
                    exit_code: Some(3),
                    cancelled: false,
                    timed_out: false,
                }),
                long,
            ),
        );
        assert_eq!(summary["exit"]["exitCode"], 3);
        assert_eq!(summary["exit"]["signal"], Value::Null);
        assert_eq!(
            summary["output"]
                .as_str()
                .map(|output| output.chars().count()),
            Some(NOTICE_TAIL_MAX_CHARS + 1)
        );
        let none = exit_summary(
            "bash_c1_2",
            &TerminalExitView::new(None, "done\n".to_owned()),
        );
        assert_eq!(none["exit"], Value::Null);
        assert_eq!(none["output"], "done");
    }

    #[test]
    fn params_are_checked_against_the_tools_schema_before_the_body_runs() {
        assert_eq!(
            check_params(TERMINAL_JOB_KINDS.output, &json!({})).map_err(|error| error.code),
            Err("terminal_job_bad_args".to_owned())
        );
        assert!(check_params(TERMINAL_JOB_KINDS.kill, &json!({})).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_foreground_bash_command_in_the_child_behind_the_barrier_and_streams_its_output()
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let listener = open_channel_listener(&dir.path().join("ch"), 1)
            .await
            .expect("listener");
        let mut env = listener.env.clone();
        env.insert(TOOL_CHILD_GENERATION_ENV.to_owned(), "1".to_owned());
        let entry = tokio::spawn(async move {
            let _ = run_tool_child(ToolChildEntryOptions {
                handlers: create_terminal_job_handlers(None),
                env: Some(env),
                exit: Some(Arc::new(|_| {})),
            })
            .await;
        });
        let server = tokio::time::timeout(BOUND, listener.accepted())
            .await
            .expect("accept in time")
            .expect("accepted");
        let (frames, mut received) = tokio::sync::mpsc::unbounded_channel();
        let replier = server.clone();
        let _unsubscribe = server.on_message(move |message| {
            // The server's half of the protocol: acknowledge every launch and every update.
            match &message {
                ToolChildMessage::Child(ChildToServer::Launch { launch_id, .. }) => {
                    let _ = replier.send(&ToolChildMessage::Server(ServerToChild::LaunchAck {
                        launch_id: launch_id.clone(),
                    }));
                }
                ToolChildMessage::Child(ChildToServer::Update { job_id, seq, .. }) => {
                    let _ = replier.send(&ToolChildMessage::Server(ServerToChild::UpdateAck {
                        job_id: job_id.clone(),
                        seq: *seq,
                    }));
                }
                _ => {}
            }
            let _ = frames.send(message);
        });
        let marker = dir.path().join("ppid");
        let command = format!(
            "echo $PPID > {}; for i in 1 2 3; do echo line-$i; sleep 0.2; done",
            marker.display()
        );
        let _ = server.send(&ToolChildMessage::Server(ServerToChild::Job {
            job_id: "j1".into(),
            kind: TERMINAL_JOB_KINDS.bash.into(),
            parent_job_id: None,
            args: Some(json!({
                "params": { "command": command },
                "toolCallId": "t1",
                "cwd": dir.path(),
                "env": std::env::vars().collect::<HashMap<_, _>>(),
                "defaultCols": 80,
                "defaultRows": 24,
            })),
            args_ref: None,
        }));
        let (mut order, mut result) = (Vec::new(), None);
        while result.is_none() {
            let message = tokio::time::timeout(BOUND, received.recv())
                .await
                .expect("a frame in time")
                .expect("channel open");
            match message {
                ToolChildMessage::Child(ChildToServer::Launch {
                    origin,
                    api,
                    containment,
                    lifetime,
                    pid,
                    pgid,
                    ..
                }) => {
                    assert_eq!(
                        (origin, api, containment, lifetime),
                        (
                            LaunchOrigin::Terminal,
                            LaunchApi::PtySpawn,
                            LaunchContainment::OwnGroup,
                            LaunchLifetime::Owned
                        )
                    );
                    assert!(pid.is_some() && pid == pgid);
                    order.push("launch");
                }
                ToolChildMessage::Child(ChildToServer::Update { .. }) => order.push("update"),
                ToolChildMessage::Child(ChildToServer::Result { value, .. }) => {
                    result = Some(value)
                }
                ToolChildMessage::Child(ChildToServer::JobError { code, message, .. }) => {
                    panic!("job failed: {code}: {message}")
                }
                _ => {}
            }
        }
        let text = result
            .as_ref()
            .and_then(|value| value["content"][0]["text"].as_str());
        assert!(
            text.is_some_and(|text| text.contains("line-3")),
            "result: {result:?}"
        );
        let marker = std::fs::read_to_string(&marker).expect("marker");
        assert_eq!(marker.trim(), std::process::id().to_string());
        // The launch was recorded before any output streamed back.
        assert_eq!(order.first(), Some(&"launch"));
        assert!(order.contains(&"update"));
        let _ = server.send(&ToolChildMessage::Server(ServerToChild::Shutdown));
        tokio::time::timeout(BOUND, entry)
            .await
            .expect("entry ends in time")
            .expect("entry task");
    }
}
