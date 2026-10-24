//! Places a terminal tool on the session's tool child (senpi#3566 todo 9): `placement: "child"` plus the job that runs
//! the same body there (`terminal_jobs.rs`). The job's arguments are built on the server from what the body would read
//! in-process - the shell environment with the session's PI_* overrides, the shell path, the default size, the cwd - so
//! the command sees the same environment wherever it runs. Under `--tool-runtime in-process` the placement is ignored and
//! `execute` runs here, unchanged.
use super::bash::session_env_overrides;
use super::context::{TerminalExitView, TerminalToolContext};
use crate::settings::TimeoutAction;
use indexmap::IndexMap;
use maho_pty::PtyExit;
use maho_tools::ToolContext;
use maho_tools::definition::{ToolChildJob, ToolChildJobPlacement, ToolContent, ToolResult};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub struct TerminalChildJobKinds {
    pub bash: &'static str,
    pub output: &'static str,
    pub input: &'static str,
    pub resize: &'static str,
    pub kill: &'static str,
}

pub const TERMINAL_CHILD_JOB_KINDS: TerminalChildJobKinds = TerminalChildJobKinds {
    bash: "terminal.bash",
    output: "terminal.output",
    input: "terminal.input",
    resize: "terminal.resize",
    kill: "terminal.kill",
};

/// A tool child's terminal manager scopes its ids by generation (`bash_c<generation>_<n>`, `idScope`), so they never
/// match the server's own (a monitor's `bash_N`, `mon_*`, `watch_*`, `rpc_*`): the id alone says who owns the session.
pub fn child_terminal_id_scope(generation: u64) -> String {
    format!("c{generation}")
}

/// `/_c(\d+)_\d+$/`: the generation digits of a child session id.
fn child_session_generation(id: &str) -> Option<&str> {
    let is_digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let (rest, number) = id.rsplit_once('_')?;
    if !is_digits(number) {
        return None;
    }
    let (_, scope) = rest.rsplit_once('_')?;
    let generation = scope.strip_prefix('c')?;
    is_digits(generation).then_some(generation)
}

pub fn is_child_session_id(id: &str) -> bool {
    child_session_generation(id).is_some()
}

/// The child generation a child session was started on, read from its id. A session lives in its child's manager, so
/// once that generation exits the id is gone: a call reaching a later child turns "unknown id" into "lost".
fn child_generation_of(id: &str) -> Option<u64> {
    child_session_generation(id)?.parse().ok()
}

/// What a child reports about a background session after the job that started it ended (`wake`).
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalBackgroundWake {
    BackgroundStart {
        id: String,
        description: String,
        started_at_ms: f64,
    },
    BackgroundExit {
        id: String,
        exit: Option<PtyExit>,
        output: String,
    },
}

/// `TerminalExitView["exitResult"]` as the child sends it: `{ exitCode, signal, cancelled, timedOut }` or null.
fn parse_exit_result(exit: &Value) -> Option<Option<PtyExit>> {
    match exit {
        Value::Null => Some(None),
        Value::Object(fields) => Some(Some(PtyExit {
            exit_code: fields
                .get("exitCode")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            cancelled: fields
                .get("cancelled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            timed_out: fields
                .get("timedOut")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })),
        _ => None,
    }
}

fn parse_background_wake(value: &Value) -> Option<TerminalBackgroundWake> {
    let object = value.as_object()?;
    let id = object.get("id")?.as_str()?;
    if !is_child_session_id(id) {
        return None;
    }
    match object.get("event")?.as_str()? {
        "background_start" => Some(TerminalBackgroundWake::BackgroundStart {
            id: id.to_owned(),
            description: object.get("description")?.as_str()?.to_owned(),
            started_at_ms: object.get("startedAtMs")?.as_f64()?,
        }),
        "background_exit" => {
            let output = object.get("output")?.as_str()?;
            let exit = parse_exit_result(object.get("exit")?)?;
            Some(TerminalBackgroundWake::BackgroundExit {
                id: id.to_owned(),
                exit,
                output: output.to_owned(),
            })
        }
        _ => None,
    }
}

fn exit_view(exit: Option<PtyExit>, output: &str) -> TerminalExitView {
    TerminalExitView::new(exit, output.to_owned())
}

fn bash_id_of(value: &Value) -> Option<&str> {
    value.get("bash_id")?.as_str()
}

pub fn lost_session_result(id: &str, generation: u64) -> ToolResult {
    ToolResult {
        content: vec![ToolContent::text(format!(
            "bash_id {id} is lost: the session's tool child (generation {generation}) exited, which ended this session and every process it started. Start a new session with bash."
        ))],
        details: Some(json!({ "bash_id": id, "lost": true, "generation": generation })),
        structured_content: None,
        is_error: None,
    }
}

pub fn terminal_child_job(ctx: &TerminalToolContext, kind: &'static str) -> ToolChildJob {
    let is_bash = kind == TERMINAL_CHILD_JOB_KINDS.bash;
    let (wake, child_exited) = if is_bash {
        let (wake, child_exited) = background_relay(ctx);
        (Some(wake), Some(child_exited))
    } else {
        (None, None)
    };
    let args_ctx = ctx.clone();
    ToolChildJob {
        kind: kind.to_owned(),
        wake,
        child_exited,
        placement: if is_bash {
            None
        } else {
            Some(Arc::new(move |params: &Value| {
                let Some(id) = bash_id_of(params) else {
                    return if kind == TERMINAL_CHILD_JOB_KINDS.kill {
                        ToolChildJobPlacement::Both
                    } else {
                        ToolChildJobPlacement::Child
                    };
                };
                if is_child_session_id(id) {
                    ToolChildJobPlacement::Child
                } else {
                    ToolChildJobPlacement::Server
                }
            }))
        },
        result: Some(Arc::new(
            move |params: &Value, value: ToolResult, generation: u64| {
                if is_bash {
                    return value;
                }
                let Some(id) = bash_id_of(params) else {
                    return value;
                };
                match child_generation_of(id) {
                    Some(started_on) if started_on != generation => {
                        lost_session_result(id, started_on)
                    }
                    _ => value,
                }
            },
        )),
        args: Arc::new(
            move |params: &Value, tool_call_id: &str, exec_ctx: Option<&dyn ToolContext>| {
                let mut env = (args_ctx.get_env)();
                // The in-process body clears a stale inherited engine; a session that has one overrides it below.
                env.remove("OMO_BROWSER_ENGINE");
                env.extend(session_env_overrides(exec_ctx));
                let settings = args_ctx.settings();
                let mut args = json!({
                    "params": params,
                    "toolCallId": tool_call_id,
                    "cwd": exec_ctx
                        .map_or_else(|| args_ctx.cwd(), |context| context.cwd().to_path_buf())
                        .to_string_lossy(),
                    "env": env,
                    "defaultCols": settings.default_cols,
                    "defaultRows": settings.default_rows,
                    "timeoutAction": match settings.timeout_action {
                        TimeoutAction::Background => "background",
                        TimeoutAction::Kill => "kill",
                    },
                });
                if let (Some(shell_path), Some(fields)) =
                    (args_ctx.shell_path(), args.as_object_mut())
                {
                    fields.insert("shellPath".to_owned(), Value::String(shell_path));
                }
                args
            },
        ),
    }
}

type Wake = Arc<dyn Fn(&Value, u64) + Send + Sync>;
type ChildExited = Arc<dyn Fn(u64) + Send + Sync>;

/// The server's half of a background session that lives on the child: the child's start and exit arrive as wakes and go
/// to the same hooks the in-process tools call (liveness snapshot, manifest, the completion notice). A generation that
/// exits takes its sessions with it; each one still running gets its exit notice then.
fn background_relay(ctx: &TerminalToolContext) -> (Wake, ChildExited) {
    let running: Arc<Mutex<IndexMap<String, u64>>> = Arc::new(Mutex::new(IndexMap::new()));
    let wake_ctx = ctx.clone();
    let wake_running = running.clone();
    let exited_ctx = ctx.clone();
    let wake: Wake = Arc::new(move |value: &Value, generation: u64| {
        let Some(wake) = parse_background_wake(value) else {
            return;
        };
        match wake {
            TerminalBackgroundWake::BackgroundStart {
                id,
                description,
                started_at_ms,
            } => {
                wake_running
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id.clone(), generation);
                if let Some(on_start) = &wake_ctx.on_background_start {
                    on_start(&id, &description, started_at_ms);
                }
            }
            TerminalBackgroundWake::BackgroundExit { id, exit, output } => {
                wake_running
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .shift_remove(&id);
                if let Some(on_exit) = &wake_ctx.on_background_exit {
                    on_exit(&id, exit_view(exit, &output));
                }
            }
        }
    });
    let child_exited: ChildExited = Arc::new(move |generation: u64| {
        let ended = {
            let mut running = running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ids = running
                .iter()
                .filter(|(_, started_on)| **started_on == generation)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in &ids {
                running.shift_remove(id);
            }
            ids
        };
        for id in ended {
            if let Some(on_exit) = &exited_ctx.on_background_exit {
                on_exit(
                    &id,
                    exit_view(
                        Some(PtyExit {
                            exit_code: None,
                            cancelled: true,
                            timed_out: false,
                        }),
                        &format!(
                            "The session's tool child (generation {generation}) exited, which ended this session and every process it started."
                        ),
                    ),
                );
            }
        }
    });
    (wake, child_exited)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::TERMINAL_SETTINGS_DEFAULTS;
    use std::collections::BTreeMap;

    // senpi#3566 todo 9 / senpi#3740 review: a child's session ids carry its generation (`bash_c<gen>_<n>`), so they never
    // match the server's own; each call is placed with the session's owner, a later generation reports the session lost,
    // and the child's background start/exit reach the server's hooks.

    fn context() -> TerminalToolContext {
        TerminalToolContext::new(
            Arc::new(Mutex::new((TERMINAL_SETTINGS_DEFAULTS, None))),
            Arc::new(BTreeMap::new),
        )
    }

    fn text(text: &str) -> ToolResult {
        ToolResult::text(text)
    }

    #[test]
    fn reports_a_child_session_lost_once_a_later_generation_answers_for_it() {
        let output = terminal_child_job(&context(), TERMINAL_CHILD_JOB_KINDS.output);
        let result = output.result.as_ref().expect("output job maps results");
        let id = format!("bash_{}_1", child_terminal_id_scope(1));
        let same = text("still running");
        assert_eq!(result(&json!({ "bash_id": id }), same.clone(), 1), same);
        let lost = result(
            &json!({ "bash_id": id }),
            text(&format!("unknown bash_id {id}")),
            2,
        );
        assert_eq!(
            lost.details,
            Some(json!({ "bash_id": id, "lost": true, "generation": 1 }))
        );
    }

    #[test]
    fn passes_through_results_for_server_ids_and_for_calls_without_an_id() {
        let kill = terminal_child_job(&context(), TERMINAL_CHILD_JOB_KINDS.kill);
        let result = kill.result.as_ref().expect("kill job maps results");
        let value = text("unknown bash_id zz");
        assert_eq!(
            result(&json!({ "bash_id": "bash_3" }), value.clone(), 3),
            value
        );
        assert_eq!(result(&json!({ "all": true }), value.clone(), 3), value);
    }

    #[test]
    fn places_each_call_with_the_owner_its_id_names() {
        assert!(is_child_session_id("bash_c2_1"));
        assert!(!is_child_session_id("bash_1"));
        let output = terminal_child_job(&context(), TERMINAL_CHILD_JOB_KINDS.output);
        let place = output.placement.as_ref().expect("output job places calls");
        assert_eq!(
            place(&json!({ "bash_id": "bash_c2_1" })),
            ToolChildJobPlacement::Child
        );
        assert_eq!(
            place(&json!({ "bash_id": "bash_1" })),
            ToolChildJobPlacement::Server
        );
        assert_eq!(
            place(&json!({ "bash_id": "mon_abc" })),
            ToolChildJobPlacement::Server
        );
        assert_eq!(
            place(&json!({ "bash_id": "watch_1" })),
            ToolChildJobPlacement::Server
        );
        let kill = terminal_child_job(&context(), TERMINAL_CHILD_JOB_KINDS.kill);
        let place = kill.placement.as_ref().expect("kill job places calls");
        assert_eq!(place(&json!({ "all": true })), ToolChildJobPlacement::Both);
        assert!(
            terminal_child_job(&context(), TERMINAL_CHILD_JOB_KINDS.bash)
                .placement
                .is_none()
        );
    }

    #[test]
    fn hands_the_childs_start_and_exit_to_the_server_hooks_and_ends_a_session_lost_with_its_generation()
     {
        type Exit = (String, String, Option<i32>);
        let started: Arc<Mutex<Vec<String>>> = Arc::default();
        let exits: Arc<Mutex<Vec<Exit>>> = Arc::default();
        let mut hooks = context();
        let started_sink = started.clone();
        hooks.on_background_start = Some(Arc::new(move |id, _, _| {
            started_sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(id.to_owned());
        }));
        let exit_sink = exits.clone();
        hooks.on_background_exit = Some(Arc::new(move |id, view| {
            exit_sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((
                    id.to_owned(),
                    view.full_output().to_owned(),
                    view.exit_result.as_ref().and_then(|exit| exit.exit_code),
                ));
        }));
        let bash = terminal_child_job(&hooks, TERMINAL_CHILD_JOB_KINDS.bash);
        let wake = bash.wake.as_ref().expect("bash job relays wakes");
        let child_exited = bash
            .child_exited
            .as_ref()
            .expect("bash job sees child exits");
        wake(
            &json!({ "event": "background_start", "id": "bash_c1_1", "description": "a", "startedAtMs": 1 }),
            1,
        );
        wake(
            &json!({ "event": "background_start", "id": "bash_c1_2", "description": "b", "startedAtMs": 2 }),
            1,
        );
        wake(
            &json!({
                "event": "background_exit",
                "id": "bash_c1_1",
                "exit": { "exitCode": 0, "signal": null, "cancelled": false, "timedOut": false },
                "output": "done",
            }),
            1,
        );
        wake(
            &json!({ "event": "background_start", "id": "bash_1", "description": "server id", "startedAtMs": 3 }),
            1,
        );
        let snapshot = |sink: &Mutex<Vec<Exit>>| {
            sink.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        };
        assert_eq!(
            *started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ["bash_c1_1", "bash_c1_2"]
        );
        assert_eq!(
            snapshot(&exits),
            [("bash_c1_1".to_owned(), "done".to_owned(), Some(0))]
        );

        child_exited(1);
        let after = snapshot(&exits);
        assert_eq!(
            after.iter().map(|exit| exit.0.as_str()).collect::<Vec<_>>(),
            ["bash_c1_1", "bash_c1_2"]
        );
        assert!(after[1].1.contains("tool child (generation 1) exited"));
        child_exited(1);
        assert_eq!(snapshot(&exits).len(), 2);
    }
}
