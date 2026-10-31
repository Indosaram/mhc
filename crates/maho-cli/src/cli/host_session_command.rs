//! Port of senpi `cli/host-session-command.ts` (pin 903b8336, senpi#3073).
//!
//! `mhc host session <command>`: short-lived, attach-before-act clients against the shared host.
//! Every command prints exactly one JSON line on stdout and classifies its outcome in the exit
//! code. Usage errors also print the help text on stderr.
//!
//! The request itself is performed by `maho_rpc::host_session_runner::run_host_session_request`
//! (senpi `modes/rpc/host-session-runner.ts`). `run_host_session_command_with` takes the runner as
//! an argument so the argv/JSON/exit contract is testable without a host, as upstream's test does
//! with `vi.spyOn(runner, "runHostSessionRequest")`.
//!
//! senpi's outer `catch` (a thrown runner error becomes `host_error`, exit 1) has no counterpart:
//! the Rust runner never fails, it returns every error as an outcome.
//!
//! `--rpc-key-root`: upstream scopes the key root with `withRpcKeyRoot` (AsyncLocalStorage). The
//! Rust probe reads `SENPI_RPC_KEY_ROOT`, then the target's agent dir, which is the same default
//! (`SENPI_RPC_KEY_ROOT ?? agentDir`). An explicit root is resolved and handed to the runner;
//! the production runner has no per-request key-root field yet, so it applies only through the
//! environment default.

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

use maho_rpc::host_launch_spec::{ResolvedHostLaunchSpec, load_host_launch_spec};
use maho_rpc::host_runner::{HOST_EXIT_USAGE, HostOutcome, HostTarget};
use maho_rpc::host_session_runner::{
    HostSessionAction, HostSessionModel, HostSessionOpen, HostSessionRequest, UnloadChildren,
};
use maho_rpc::host_session_wait::{HostSessionWait, WaitUntil};
use serde_json::{Value, json};

const USAGE: &str = "usage: {app} host session <command> [--socket <path>] [--rpc-key-root <dir>] [--json]

  open --cwd <dir> [--model <provider/id>] [--name <text>] [--prompt <text|@file>] [--launch-spec <file>]
  close <ref>
  model <ref> <provider/id>
  prompt <ref> <text|@file>
  steer <ref> <text|@file>
  abort <ref>
  read <ref> [--tail <N>] [--since <entryId>] [--messages]
  state <ref>
  list
  wait <ref> [--until idle|done] [--timeout <ms>]

Every command prints one JSON line. wait defaults to idle and 600000 ms.
@file is verbatim UTF-8 prompt text (BOM stripped), not an attachment wrapper.
References accept a host session id, durable session id, path, or name.";

const ACTIONS: [&str; 11] = [
    "open", "close", "unload", "model", "prompt", "steer", "abort", "read", "state", "list", "wait",
];

/// senpi `CommandArgs<HostSessionRequest>`: one command without its target and launch spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostSessionCommand {
    Open {
        cwd: String,
        model: Option<HostSessionModel>,
        name: Option<String>,
        prompt: Option<String>,
    },
    Close {
        reference: String,
    },
    Unload {
        reference: String,
        interrupt: bool,
        force: bool,
        clear_queue: bool,
        children: UnloadChildren,
    },
    Model {
        reference: String,
        model: HostSessionModel,
    },
    Prompt {
        reference: String,
        text: String,
    },
    Steer {
        reference: String,
        text: String,
    },
    Abort {
        reference: String,
    },
    Read {
        reference: String,
        tail: Option<usize>,
        since: Option<String>,
        messages: bool,
    },
    State {
        reference: String,
    },
    List,
    Wait {
        reference: String,
        until: WaitUntil,
        timeout_ms: u64,
    },
}

/// senpi `ParsedHostSessionArgs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedHostSessionArgs {
    pub command: HostSessionCommand,
    pub socket: Option<String>,
    pub spec_path: Option<String>,
    pub key_root: Option<String>,
}

fn parse_model(text: &str) -> Result<HostSessionModel, String> {
    let parts: Vec<&str> = text.split('/').collect();
    match parts.as_slice() {
        [provider, id] if !provider.is_empty() && !id.is_empty() => Ok(HostSessionModel {
            provider: (*provider).to_owned(),
            id: (*id).to_owned(),
        }),
        _ => Err("Model must be provider/id.".to_owned()),
    }
}

/// senpi `integer`: digits only, a safe integer, at least `minimum`.
fn integer(text: &str, minimum: u64) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>()
        .ok()
        .filter(|value| *value <= 9_007_199_254_740_991 && *value >= minimum)
}

/// senpi `resolve(path)` against the process cwd.
fn resolve(path: &str) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let mut normal = PathBuf::new();
    for part in absolute.components() {
        match part {
            std::path::Component::ParentDir => {
                normal.pop();
            }
            std::path::Component::CurDir => {}
            other => normal.push(other.as_os_str()),
        }
    }
    normal.to_string_lossy().into_owned()
}

/// senpi `parseHostSessionArgs`. `Err` carries the usage detail.
pub fn parse_host_session_args(args: &[String]) -> Result<ParsedHostSessionArgs, String> {
    let action = args.first().map_or("", String::as_str);
    if !ACTIONS.contains(&action) {
        return Err(format!("Unknown host session command \"{action}\"."));
    }
    let rest = &args[1..];
    let mut values = std::collections::HashMap::<&str, String>::new();
    let mut positional: Vec<String> = Vec::new();
    let mut messages = false;
    let (mut interrupt, mut force, mut clear_queue) = (false, false, false);
    let mut index = 0;
    while index < rest.len() {
        let flag = rest[index].as_str();
        index += 1;
        if flag == "--json" {
            continue;
        }
        if action == "unload" {
            match flag {
                "--interrupt" => {
                    interrupt = true;
                    continue;
                }
                "--force" => {
                    force = true;
                    continue;
                }
                "--clear-queue" => {
                    clear_queue = true;
                    continue;
                }
                _ => {}
            }
        }
        if flag == "--messages" && action == "read" {
            messages = true;
            continue;
        }
        if !flag.starts_with("--") {
            positional.push(flag.to_owned());
            continue;
        }
        let allowed = flag == "--socket"
            || flag == "--rpc-key-root"
            || (action == "open"
                && ["--cwd", "--model", "--name", "--prompt", "--launch-spec"].contains(&flag))
            || (action == "read" && ["--tail", "--since"].contains(&flag))
            || (action == "wait" && ["--until", "--timeout"].contains(&flag))
            || (action == "unload" && flag == "--children");
        let value = rest.get(index);
        index += 1;
        match value {
            Some(value) if allowed && !value.starts_with("--") => {
                values.insert(flag, value.clone());
            }
            _ => return Err(format!("Invalid option or missing value: {flag}.")),
        }
    }
    let count = match action {
        "open" | "list" => 0,
        "model" | "prompt" | "steer" => 2,
        _ => 1,
    };
    if positional.len() != count {
        return Err(format!("{action} expects {count} positional argument(s)."));
    }
    let reference = positional.first().cloned().unwrap_or_default();
    let socket = values.get("--socket").cloned();
    let spec_path = values.get("--launch-spec").cloned();
    let key_root = values.get("--rpc-key-root").cloned();
    let command = match action {
        "open" => {
            let cwd = values
                .get("--cwd")
                .filter(|cwd| !cwd.is_empty())
                .ok_or("open requires --cwd <dir>.")?;
            let model = values
                .get("--model")
                .map(|model| parse_model(model))
                .transpose()?;
            HostSessionCommand::Open {
                cwd: resolve(cwd),
                model,
                name: values.get("--name").cloned(),
                prompt: values.get("--prompt").cloned(),
            }
        }
        "model" => HostSessionCommand::Model {
            reference,
            model: parse_model(&positional[1])?,
        },
        "prompt" => HostSessionCommand::Prompt {
            reference,
            text: positional[1].clone(),
        },
        "steer" => HostSessionCommand::Steer {
            reference,
            text: positional[1].clone(),
        },
        "close" => HostSessionCommand::Close { reference },
        "abort" => HostSessionCommand::Abort { reference },
        "state" => HostSessionCommand::State { reference },
        "list" => HostSessionCommand::List,
        "unload" => {
            let children = match values.get("--children").map_or("refuse", String::as_str) {
                "refuse" => UnloadChildren::Refuse,
                "detach" => UnloadChildren::Detach,
                _ => return Err("--children must be refuse or detach.".to_owned()),
            };
            HostSessionCommand::Unload {
                reference,
                interrupt,
                force,
                clear_queue,
                children,
            }
        }
        "read" => {
            let tail = match values.get("--tail") {
                None => None,
                Some(text) => {
                    Some(integer(text, 1).ok_or("--tail must be a positive integer.")? as usize)
                }
            };
            if messages && values.contains_key("--since") {
                return Err("--since cannot be combined with --messages.".to_owned());
            }
            HostSessionCommand::Read {
                reference,
                tail,
                since: values.get("--since").cloned(),
                messages,
            }
        }
        "wait" => {
            let until = match values.get("--until").map_or("idle", String::as_str) {
                "idle" => WaitUntil::Idle,
                "done" => WaitUntil::Done,
                _ => return Err("--until must be idle or done.".to_owned()),
            };
            let timeout_ms = integer(values.get("--timeout").map_or("600000", String::as_str), 0)
                .ok_or("--timeout must be a non-negative integer.")?;
            HostSessionCommand::Wait {
                reference,
                until,
                timeout_ms,
            }
        }
        _ => return Err(format!("Unknown host session command \"{action}\".")),
    };
    Ok(ParsedHostSessionArgs {
        command,
        socket,
        spec_path,
        key_root,
    })
}

/// senpi `promptText`: `@file` is the file's verbatim UTF-8 text with a leading BOM stripped.
fn prompt_text(text: &str) -> Result<String, String> {
    match text.strip_prefix('@') {
        None => Ok(text.to_owned()),
        Some(path) => std::fs::read_to_string(path)
            .map(|body| match body.strip_prefix('\u{feff}') {
                Some(stripped) => stripped.to_owned(),
                None => body,
            })
            .map_err(|error| format!("{path}: {error}")),
    }
}

/// Where a run's output goes: one JSON line on stdout, help text on stderr.
pub struct HostSessionOutput<'a> {
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
}

/// senpi `emit`: one line in one write, so a launcher exiting cannot truncate the answer.
fn emit(output: &mut HostSessionOutput<'_>, payload: &Value) {
    let line = format!("{payload}\n");
    let _ = output
        .stdout
        .write_all(line.as_bytes())
        .and_then(|()| output.stdout.flush());
}

fn usage(output: &mut HostSessionOutput<'_>, reason: &str, detail: &str) -> i32 {
    let usage = USAGE.replace("{app}", &maho_core::config::app_name());
    let _ = write!(output.stderr, "{detail}\n{usage}\n");
    emit(
        output,
        &json!({ "action": "error", "reason": reason, "detail": detail }),
    );
    HOST_EXIT_USAGE
}

/// senpi `resolveHostSocket`.
fn resolve_host_socket(explicit: Option<&str>, agent_dir: &Path) -> String {
    explicit
        .map(str::to_owned)
        .or_else(|| {
            maho_core::brand::env_value("RPC_SOCKET", &maho_core::config::current_env())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| {
            agent_dir
                .join("rpc")
                .join("rpc.sock")
                .to_string_lossy()
                .into_owned()
        })
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    }
}

fn into_action(
    command: HostSessionCommand,
    spec: Option<ResolvedHostLaunchSpec>,
) -> HostSessionAction {
    match command {
        HostSessionCommand::Open {
            cwd,
            model,
            name,
            prompt,
        } => HostSessionAction::Open {
            spec: spec.unwrap_or_default(),
            open: HostSessionOpen {
                cwd,
                model,
                name,
                prompt,
            },
        },
        HostSessionCommand::Close { reference } => HostSessionAction::Close { reference },
        HostSessionCommand::Unload {
            reference,
            interrupt,
            force,
            clear_queue,
            children,
        } => HostSessionAction::Unload {
            reference,
            interrupt,
            force,
            clear_queue,
            children,
        },
        HostSessionCommand::Model { reference, model } => {
            HostSessionAction::Model { reference, model }
        }
        HostSessionCommand::Prompt { reference, text } => {
            HostSessionAction::Prompt { reference, text }
        }
        HostSessionCommand::Steer { reference, text } => {
            HostSessionAction::Steer { reference, text }
        }
        HostSessionCommand::Abort { reference } => HostSessionAction::Abort { reference },
        HostSessionCommand::Read {
            reference,
            tail,
            since,
            messages,
        } => HostSessionAction::Read {
            reference,
            tail,
            since,
            messages,
        },
        HostSessionCommand::State { reference } => HostSessionAction::State { reference },
        HostSessionCommand::List => HostSessionAction::List,
        HostSessionCommand::Wait {
            reference,
            until,
            timeout_ms,
        } => HostSessionAction::Wait {
            reference,
            wait: HostSessionWait { until, timeout_ms },
        },
    }
}

/// senpi `runHostSessionCommand` with the runner and output injected. `run` receives the
/// request and the resolved key root.
pub async fn run_host_session_command_with<R, Fut>(
    args: &[String],
    agent_dir: &Path,
    output: &mut HostSessionOutput<'_>,
    run: R,
) -> i32
where
    R: FnOnce(HostSessionRequest, PathBuf) -> Fut,
    Fut: Future<Output = HostOutcome>,
{
    let mut parsed = match parse_host_session_args(args) {
        Ok(parsed) => parsed,
        Err(detail) => return usage(output, "usage", &detail),
    };
    let prompts = match &mut parsed.command {
        HostSessionCommand::Prompt { text, .. } | HostSessionCommand::Steer { text, .. } => {
            prompt_text(text).map(|resolved| *text = resolved)
        }
        HostSessionCommand::Open {
            prompt: Some(prompt),
            ..
        } => prompt_text(prompt).map(|resolved| *prompt = resolved),
        _ => Ok(()),
    };
    if let Err(detail) = prompts {
        return usage(output, "prompt_file_unreadable", &detail);
    }
    let target = HostTarget {
        socket: resolve_host_socket(parsed.socket.as_deref(), agent_dir),
        agent_dir: agent_dir.to_path_buf(),
    };
    let spec = match (&parsed.command, &parsed.spec_path) {
        (HostSessionCommand::Open { .. }, Some(path)) => {
            match load_host_launch_spec(Path::new(path), platform()) {
                Ok(spec) => Some(spec),
                Err(error) => return usage(output, error.reason, &error.detail),
            }
        }
        _ => None,
    };
    let key_root = match &parsed.key_root {
        Some(root) => PathBuf::from(resolve(root)),
        None => std::env::var("SENPI_RPC_KEY_ROOT")
            .ok()
            .map_or_else(|| agent_dir.to_path_buf(), PathBuf::from),
    };
    let request = HostSessionRequest {
        target,
        action: into_action(parsed.command, spec),
    };
    let outcome = run(request, key_root).await;
    emit(output, &outcome.payload);
    outcome.exit_code
}

/// senpi `runHostSessionCommand`: the real runner against the shared host, stdout/stderr output.
pub async fn run_host_session_command(args: &[String]) -> i32 {
    let agent_dir = PathBuf::from(maho_core::config::get_agent_dir());
    let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
    let mut output = HostSessionOutput {
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    run_host_session_command_with(args, &agent_dir, &mut output, |request, _key_root| {
        maho_rpc::host_session_runner::run_host_session_request(request)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn model(provider: &str, id: &str) -> HostSessionModel {
        HostSessionModel {
            provider: provider.to_owned(),
            id: id.to_owned(),
        }
    }

    fn reference() -> String {
        "rpc-1".to_owned()
    }

    #[test]
    fn parses_every_command_with_socket_and_json() {
        let cases: Vec<(Vec<&str>, HostSessionCommand)> = vec![
            (
                vec!["open", "--cwd", ".", "--model", "anthropic/mock"],
                HostSessionCommand::Open {
                    cwd: resolve("."),
                    model: Some(model("anthropic", "mock")),
                    name: None,
                    prompt: None,
                },
            ),
            (
                vec!["close", "rpc-1"],
                HostSessionCommand::Close {
                    reference: reference(),
                },
            ),
            (
                vec!["model", "rpc-1", "anthropic/mock"],
                HostSessionCommand::Model {
                    reference: reference(),
                    model: model("anthropic", "mock"),
                },
            ),
            (
                vec!["prompt", "rpc-1", "hello"],
                HostSessionCommand::Prompt {
                    reference: reference(),
                    text: "hello".to_owned(),
                },
            ),
            (
                vec!["steer", "rpc-1", "hello"],
                HostSessionCommand::Steer {
                    reference: reference(),
                    text: "hello".to_owned(),
                },
            ),
            (
                vec!["abort", "rpc-1"],
                HostSessionCommand::Abort {
                    reference: reference(),
                },
            ),
            (
                vec!["read", "rpc-1", "--tail", "2", "--since", "entry"],
                HostSessionCommand::Read {
                    reference: reference(),
                    tail: Some(2),
                    since: Some("entry".to_owned()),
                    messages: false,
                },
            ),
            (
                vec!["read", "rpc-1", "--messages"],
                HostSessionCommand::Read {
                    reference: reference(),
                    tail: None,
                    since: None,
                    messages: true,
                },
            ),
            (
                vec!["state", "rpc-1"],
                HostSessionCommand::State {
                    reference: reference(),
                },
            ),
            (vec!["list"], HostSessionCommand::List),
            (
                vec!["wait", "rpc-1"],
                HostSessionCommand::Wait {
                    reference: reference(),
                    until: WaitUntil::Idle,
                    timeout_ms: 600_000,
                },
            ),
            (
                vec!["wait", "rpc-1", "--until", "done", "--timeout", "0"],
                HostSessionCommand::Wait {
                    reference: reference(),
                    until: WaitUntil::Done,
                    timeout_ms: 0,
                },
            ),
        ];
        for (args, command) in cases {
            let mut full = argv(&args);
            full.extend(argv(&["--socket", "/socket", "--json"]));
            let parsed =
                parse_host_session_args(&full).unwrap_or_else(|error| panic!("{args:?}: {error}"));
            assert_eq!(parsed.command, command, "{args:?}");
            assert_eq!(parsed.socket.as_deref(), Some("/socket"), "{args:?}");
        }
    }

    #[test]
    fn rejects_invalid_argv() {
        let cases: [&[&str]; 11] = [
            &[],
            &["bogus"],
            &["open"],
            &["open", "--cwd", ".", "--model", "foo"],
            &["model", "ref", "p/a/b"],
            &["read", "ref", "--tail", "0"],
            &["read", "ref", "--tail", "1.2"],
            &["wait", "ref", "--until", "later"],
            &["wait", "ref", "--timeout", "-1"],
            &["list", "--bogus"],
            &["state", "ref", "extra"],
        ];
        for args in cases {
            assert!(
                parse_host_session_args(&argv(args)).is_err(),
                "{args:?} must be rejected"
            );
        }
    }

    struct Captured {
        code: i32,
        stdout: String,
        stderr: String,
        request: Option<HostSessionRequest>,
    }

    fn run_with(args: &[&str]) -> Captured {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let mut request = None;
        let agent_dir = tempfile::tempdir().expect("agent dir");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let code = runtime.block_on(run_host_session_command_with(
            &argv(args),
            agent_dir.path(),
            &mut HostSessionOutput {
                stdout: &mut stdout,
                stderr: &mut stderr,
            },
            |sent, _key_root| {
                request = Some(sent);
                std::future::ready(HostOutcome {
                    exit_code: 0,
                    payload: json!({ "action": "prompt" }),
                })
            },
        ));
        Captured {
            code,
            stdout: String::from_utf8(stdout).expect("utf8"),
            stderr: String::from_utf8(stderr).expect("utf8"),
            request,
        }
    }

    fn single_json_line(stdout: &str) -> Value {
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one stdout line: {stdout:?}");
        serde_json::from_str(lines[0]).expect("json line")
    }

    #[test]
    fn returns_usage_with_one_stdout_json_line_and_stderr_help() {
        let run = run_with(&[]);
        assert_eq!(run.code, 2);
        let payload = single_json_line(&run.stdout);
        assert_eq!(payload["action"], "error");
        assert_eq!(payload["reason"], "usage");
        assert!(run.stderr.contains("usage:"));
        assert!(run.request.is_none());
    }

    #[test]
    fn reports_an_unreadable_prompt_file_as_usage() {
        let run = run_with(&["prompt", "ref", "@/nonexistent/host-session-prompt"]);
        assert_eq!(run.code, 2);
        assert_eq!(
            single_json_line(&run.stdout)["reason"],
            "prompt_file_unreadable"
        );
        assert!(run.request.is_none());
    }

    #[test]
    fn passes_bom_stripped_file_bytes_without_an_attachment_wrapper() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("prompt.txt");
        std::fs::write(&file, "\u{feff}hello\n\n").expect("write");
        let run = run_with(&["prompt", "ref", &format!("@{}", file.display())]);
        assert_eq!(run.code, 0);
        assert_eq!(single_json_line(&run.stdout), json!({ "action": "prompt" }));
        match run.request.map(|request| request.action) {
            Some(HostSessionAction::Prompt { reference, text }) => {
                assert_eq!(reference, "ref");
                assert_eq!(text, "hello\n\n");
            }
            other => panic!("unexpected request {other:?}"),
        }
    }
}
