//! Port of senpi v2026.10.27-2 `cli/host-session-command.ts`: `mhc host session <command>` parses its
//! argv, performs one `maho_rpc::host_session_runner` request inside the endpoint key root, and prints
//! exactly one JSON line with the runner's exit code.

use std::io::Write;
use std::path::{Path, PathBuf};

use maho_rpc::host_launch_spec::{
    HostLaunchSpecError, default_host_launch_spec, load_host_launch_spec,
};
use maho_rpc::host_runner::{HOST_EXIT_USAGE, HostTarget};
use maho_rpc::host_session_runner::{
    HostSessionAction, HostSessionModel, HostSessionOpen, HostSessionRequest, UnloadChildren,
    run_host_session_request,
};
use maho_rpc::host_session_wait::{HostSessionWait, WaitUntil};

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

/// senpi `CommandArgs<HostSessionRequest>`: one parsed command, before its target and spec are attached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostSessionCommand {
    Open(HostSessionOpen),
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
        wait: HostSessionWait,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedHostSessionArgs {
    pub command: HostSessionCommand,
    pub socket: Option<String>,
    pub spec_path: Option<String>,
    pub key_root: Option<String>,
}

const ACTIONS: [&str; 11] = [
    "open", "close", "unload", "model", "prompt", "steer", "abort", "read", "state", "list", "wait",
];

pub fn parse_host_session_args(args: &[String]) -> Result<ParsedHostSessionArgs, String> {
    let action = args.first().map(String::as_str).unwrap_or_default();
    if !ACTIONS.contains(&action) {
        return Err(format!("Unknown host session command \"{action}\"."));
    }
    let rest = &args[1..];
    let mut values = std::collections::HashMap::<String, String>::new();
    let mut positional = Vec::<String>::new();
    let mut messages = false;
    let (mut interrupt, mut force, mut clear_queue) = (false, false, false);
    let mut index = 0;
    while index < rest.len() {
        let flag = rest[index].as_str();
        index += 1;
        if flag == "--json" {
            continue;
        }
        if action == "unload" && matches!(flag, "--interrupt" | "--force" | "--clear-queue") {
            match flag {
                "--interrupt" => interrupt = true,
                "--force" => force = true,
                _ => clear_queue = true,
            }
            continue;
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
        let Some(value) = value.filter(|value| allowed && !value.starts_with("--")) else {
            return Err(format!("Invalid option or missing value: {flag}."));
        };
        values.insert(flag.to_owned(), value.clone());
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
    let command = match action {
        "open" => {
            let Some(cwd) = values.get("--cwd").filter(|cwd| !cwd.is_empty()) else {
                return Err("open requires --cwd <dir>.".to_owned());
            };
            let model = values
                .get("--model")
                .map(|model| parse_model(model))
                .transpose()?;
            HostSessionCommand::Open(HostSessionOpen {
                cwd: resolve(cwd),
                model,
                name: values.get("--name").cloned(),
                prompt: values.get("--prompt").cloned(),
            })
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
                Some(text) => Some(
                    integer(text, 1)
                        .ok_or_else(|| "--tail must be a positive integer.".to_owned())?
                        as usize,
                ),
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
        _ => {
            let until = match values.get("--until").map_or("idle", String::as_str) {
                "idle" => WaitUntil::Idle,
                "done" => WaitUntil::Done,
                _ => return Err("--until must be idle or done.".to_owned()),
            };
            let timeout_ms =
                integer(values.get("--timeout").map_or("600000", String::as_str), 0)
                    .ok_or_else(|| "--timeout must be a non-negative integer.".to_owned())?;
            HostSessionCommand::Wait {
                reference,
                wait: HostSessionWait { until, timeout_ms },
            }
        }
    };
    Ok(ParsedHostSessionArgs {
        command,
        socket: values.get("--socket").cloned(),
        spec_path: values.get("--launch-spec").cloned(),
        key_root: values.get("--rpc-key-root").cloned(),
    })
}

fn parse_model(text: &str) -> Result<HostSessionModel, String> {
    let parts = text.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        [provider, id] if !provider.is_empty() && !id.is_empty() => Ok(HostSessionModel {
            provider: (*provider).to_owned(),
            id: (*id).to_owned(),
        }),
        _ => Err("Model must be provider/id.".to_owned()),
    }
}

/// A decimal integer at least `minimum` and within JavaScript's safe range (senpi `integer`).
fn integer(text: &str, minimum: u64) -> Option<u64> {
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>()
        .ok()
        .filter(|value| *value <= MAX_SAFE_INTEGER && *value >= minimum)
}

fn resolve(path: &str) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| PathBuf::from(path))
        .to_string_lossy()
        .into_owned()
}

fn emit(payload: &serde_json::Value) -> Result<(), String> {
    writeln!(std::io::stdout().lock(), "{payload}").map_err(|error| error.to_string())
}

fn usage(reason: &str, detail: &str) -> Result<i32, String> {
    usage_to(
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
        reason,
        detail,
    )
}

/// senpi `usage`: the detail and the help on stderr, then exactly one JSON line on stdout.
fn usage_to(
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    reason: &str,
    detail: &str,
) -> Result<i32, String> {
    let help = USAGE.replace("{app}", &maho_core::config::app_name());
    writeln!(stderr, "{detail}\n{help}").map_err(|error| error.to_string())?;
    let payload = serde_json::json!({ "action": "error", "reason": reason, "detail": detail });
    writeln!(stdout, "{payload}").map_err(|error| error.to_string())?;
    Ok(HOST_EXIT_USAGE)
}

/// `@file` is verbatim UTF-8 prompt text with a leading BOM stripped (senpi `promptText`).
pub fn prompt_text(text: &str) -> std::io::Result<String> {
    match text.strip_prefix('@') {
        Some(path) => {
            let read = std::fs::read_to_string(path)?;
            Ok(read
                .strip_prefix('\u{FEFF}')
                .map(str::to_owned)
                .unwrap_or(read))
        }
        None => Ok(text.to_owned()),
    }
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    }
}

/// The runner action a parsed command performs, with its prompt files read; `Err` is the unreadable file.
fn into_action(
    command: HostSessionCommand,
    spec_path: Option<&str>,
) -> Result<Result<HostSessionAction, HostLaunchSpecError>, std::io::Error> {
    Ok(Ok(match command {
        HostSessionCommand::Open(mut open) => {
            if let Some(prompt) = &open.prompt {
                open.prompt = Some(prompt_text(prompt)?);
            }
            let spec = match spec_path {
                None => default_host_launch_spec(),
                Some(path) => match load_host_launch_spec(Path::new(path), platform()) {
                    Ok(spec) => spec,
                    Err(error) => return Ok(Err(error)),
                },
            };
            HostSessionAction::Open { spec, open }
        }
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
        HostSessionCommand::Prompt { reference, text } => HostSessionAction::Prompt {
            reference,
            text: prompt_text(&text)?,
        },
        HostSessionCommand::Steer { reference, text } => HostSessionAction::Steer {
            reference,
            text: prompt_text(&text)?,
        },
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
        HostSessionCommand::Wait { reference, wait } => HostSessionAction::Wait { reference, wait },
    }))
}

/// The key root a host session command uses: an explicit root is resolved against the cwd,
/// as upstream `resolve(keyRoot)`.
fn host_session_key_root(explicit: Option<&str>, agent_dir: &Path) -> PathBuf {
    let explicit_root = explicit.map(resolve);
    maho_rpc::host_probe::rpc_key_root(explicit_root.as_deref().map(Path::new), Some(agent_dir))
}

/// senpi `runHostSessionCommand`: one JSON line on stdout and the outcome's exit code.
pub async fn run_host_session_command(args: &[String]) -> Result<i32, String> {
    let parsed = match parse_host_session_args(args) {
        Ok(parsed) => parsed,
        Err(detail) => return usage("usage", &detail),
    };
    let action = match into_action(parsed.command, parsed.spec_path.as_deref()) {
        Err(error) => return usage("prompt_file_unreadable", &error.to_string()),
        Ok(Err(error)) => return usage(error.reason, &error.detail),
        Ok(Ok(action)) => action,
    };
    let agent_dir = PathBuf::from(maho_core::config::get_agent_dir());
    let socket = super::host_command::resolve_host_socket(parsed.socket.as_deref(), &agent_dir);
    let key_root = host_session_key_root(parsed.key_root.as_deref(), &agent_dir);
    let request = HostSessionRequest {
        target: HostTarget { socket, agent_dir },
        action,
    };
    let outcome =
        maho_rpc::host_probe::with_rpc_key_root(key_root, run_host_session_request(request)).await;
    emit(&outcome.payload)?;
    Ok(outcome.exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }
    fn parse(args: &[&str]) -> ParsedHostSessionArgs {
        let mut args = argv(args);
        args.extend(argv(&["--socket", "/socket", "--json"]));
        parse_host_session_args(&args).unwrap()
    }
    fn model() -> HostSessionModel {
        HostSessionModel {
            provider: "anthropic".into(),
            id: "mock".into(),
        }
    }

    // #3073: pin the public argv and JSON/exit contract, including verbatim prompt files.
    #[test]
    fn parses_every_command() {
        let reference = || "rpc-1".to_owned();
        let cases = [
            (
                parse(&["open", "--cwd", ".", "--model", "anthropic/mock"]),
                HostSessionCommand::Open(HostSessionOpen {
                    cwd: resolve("."),
                    model: Some(model()),
                    name: None,
                    prompt: None,
                }),
            ),
            (
                parse(&["close", "rpc-1"]),
                HostSessionCommand::Close {
                    reference: reference(),
                },
            ),
            (
                parse(&["model", "rpc-1", "anthropic/mock"]),
                HostSessionCommand::Model {
                    reference: reference(),
                    model: model(),
                },
            ),
            (
                parse(&["prompt", "rpc-1", "hello"]),
                HostSessionCommand::Prompt {
                    reference: reference(),
                    text: "hello".into(),
                },
            ),
            (
                parse(&["steer", "rpc-1", "hello"]),
                HostSessionCommand::Steer {
                    reference: reference(),
                    text: "hello".into(),
                },
            ),
            (
                parse(&["abort", "rpc-1"]),
                HostSessionCommand::Abort {
                    reference: reference(),
                },
            ),
            (
                parse(&["read", "rpc-1", "--tail", "2", "--since", "entry"]),
                HostSessionCommand::Read {
                    reference: reference(),
                    tail: Some(2),
                    since: Some("entry".into()),
                    messages: false,
                },
            ),
            (
                parse(&["read", "rpc-1", "--messages"]),
                HostSessionCommand::Read {
                    reference: reference(),
                    tail: None,
                    since: None,
                    messages: true,
                },
            ),
            (
                parse(&["state", "rpc-1"]),
                HostSessionCommand::State {
                    reference: reference(),
                },
            ),
            (parse(&["list"]), HostSessionCommand::List),
            (
                parse(&["wait", "rpc-1"]),
                HostSessionCommand::Wait {
                    reference: reference(),
                    wait: HostSessionWait {
                        until: WaitUntil::Idle,
                        timeout_ms: 600_000,
                    },
                },
            ),
            (
                parse(&["wait", "rpc-1", "--until", "done", "--timeout", "0"]),
                HostSessionCommand::Wait {
                    reference: reference(),
                    wait: HostSessionWait {
                        until: WaitUntil::Done,
                        timeout_ms: 0,
                    },
                },
            ),
        ];
        for (parsed, command) in cases {
            assert_eq!(parsed.command, command);
            assert_eq!(parsed.socket.as_deref(), Some("/socket"));
        }
    }

    #[test]
    fn rejects_invalid_argv() {
        for args in [
            &[][..],
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
        ] {
            assert!(parse_host_session_args(&argv(args)).is_err(), "{args:?}");
        }
    }

    #[test]
    fn key_root_and_launch_spec_are_carried() {
        let parsed = parse_host_session_args(&argv(&[
            "open",
            "--cwd",
            "/w",
            "--launch-spec",
            "/spec.json",
            "--rpc-key-root",
            "/keys",
        ]))
        .unwrap();
        assert_eq!(parsed.spec_path.as_deref(), Some("/spec.json"));
        assert_eq!(parsed.key_root.as_deref(), Some("/keys"));
        let unload = parse_host_session_args(&argv(&[
            "unload",
            "ref",
            "--force",
            "--clear-queue",
            "--children",
            "detach",
        ]))
        .unwrap();
        assert_eq!(
            unload.command,
            HostSessionCommand::Unload {
                reference: "ref".into(),
                interrupt: false,
                force: true,
                clear_queue: true,
                children: UnloadChildren::Detach,
            }
        );
    }

    #[test]
    fn returns_usage_with_one_stdout_json_line_and_stderr_help() {
        let detail = parse_host_session_args(&[]).unwrap_err();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            usage_to(&mut stdout, &mut stderr, "usage", &detail),
            Ok(HOST_EXIT_USAGE)
        );
        let stdout = String::from_utf8(stdout).unwrap();
        assert_eq!(stdout.lines().count(), 1);
        let line: serde_json::Value = serde_json::from_str(stdout.trim_end()).unwrap();
        assert_eq!(line["action"], "error");
        assert_eq!(line["reason"], "usage");
        assert!(String::from_utf8(stderr).unwrap().contains("usage:"));
    }

    #[test]
    fn an_explicit_key_root_is_made_absolute() {
        let parsed = parse(&["list", "--rpc-key-root", "relative-keys"]);
        let resolved = host_session_key_root(parsed.key_root.as_deref(), Path::new("/agent"));
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("relative-keys"));
    }

    #[test]
    fn an_unreadable_prompt_file_is_reported_before_any_request() {
        let error = into_action(
            HostSessionCommand::Prompt {
                reference: "ref".into(),
                text: "@/nonexistent/host-session-prompt".into(),
            },
            None,
        )
        .err()
        .expect("unreadable");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn passes_bom_stripped_file_bytes_without_an_attachment_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("prompt.txt");
        std::fs::write(&file, "\u{FEFF}hello\n\n").unwrap();
        let action = into_action(
            HostSessionCommand::Prompt {
                reference: "ref".into(),
                text: format!("@{}", file.display()),
            },
            None,
        )
        .unwrap()
        .unwrap();
        let HostSessionAction::Prompt { text, .. } = action else {
            panic!("prompt");
        };
        assert_eq!(text, "hello\n\n");
        assert_eq!(prompt_text("plain").unwrap(), "plain");
    }
}
