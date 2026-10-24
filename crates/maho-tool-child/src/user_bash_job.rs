//! senpi `modes/rpc/tool-child/user-bash-job.ts` (pin 26acd9353).
//!
//! The session's user bash (`!cmd`, `AgentSession.executeBash`) as a tool-child job (senpi#3566 todo 9): the command
//! runs on the session's child through the launch barrier (`launch_own_group`), its output streams back as job updates
//! (`{ data: <base64> }`), and the result is the exit code (signal terminations as 128 + signal number). Interception,
//! events, recording and `excludeFromContext` stay on the server; this job only runs the process.
//!
//! Shell resolution: `utils/shell.ts` `getShellConfig` lives in maho-cli / maho-agent, which sit above this crate, so
//! the POSIX branch of it is reimplemented locally (`get_shell_config` below); this job is POSIX-only upstream too.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures::future::FutureExt;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::entry::{HandlerError, ToolChildJobContext, ToolChildJobHandler};
use crate::entry_error::ToolChildJobError;
use crate::launch_barrier::{OwnGroupLaunch, OwnGroupOptions, StdioKind, launch_own_group};
use crate::protocol::{LaunchApi, LaunchOrigin};

pub const USER_BASH_JOB_KIND: &str = "user.bash";

#[derive(Clone, Debug, PartialEq)]
pub struct UserBashJobArgs {
    pub command: String,
    pub cwd: String,
    pub env: HashMap<String, String>,
    pub shell_path: Option<String>,
    pub timeout_seconds: Option<f64>,
}

/// One streamed chunk of output; base64 so binary output survives the JSON channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserBashUpdate {
    pub data: String,
}

pub fn parse_user_bash_args(value: Option<&Value>) -> Result<UserBashJobArgs, ToolChildJobError> {
    let field = |name: &str| {
        value
            .and_then(Value::as_object)
            .and_then(|object| object.get(name))
    };
    let (Some(command), Some(cwd), Some(env)) = (
        field("command").and_then(Value::as_str),
        field("cwd").and_then(Value::as_str),
        field("env").and_then(Value::as_object),
    ) else {
        return Err(ToolChildJobError::new(
            "user_bash_bad_args",
            "a user bash job needs command, cwd and env",
        ));
    };
    Ok(UserBashJobArgs {
        command: command.to_owned(),
        cwd: cwd.to_owned(),
        env: env
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_owned())))
            .collect(),
        shell_path: field("shellPath")
            .and_then(Value::as_str)
            .map(str::to_owned),
        timeout_seconds: field("timeoutSeconds")
            .and_then(Value::as_f64)
            .filter(|seconds| *seconds > 0.0),
    })
}

/// `ShellConfig` of `utils/shell.ts`, reduced to what a user bash launch reads.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShellConfig {
    shell: String,
    args: Vec<String>,
    /// `commandTransport === "stdin"` (legacy WSL bash only, which is never a POSIX path).
    command_from_stdin: bool,
}

const GIT_BASH_PATH_ENV: &str = "SENPI_GIT_BASH_PATH";

/// `resolveShellKind` + `getShellConfigForPath` for a POSIX shell path: cmd -> `/c`, powershell -> `-NoProfile
/// -Command`, anything else `-c`.
fn shell_config_for_path(path: &str) -> ShellConfig {
    let normalized = path.replace('\\', "/").to_lowercase();
    let base = normalized.rsplit('/').next().unwrap_or_default();
    let base = base.strip_suffix(".exe").unwrap_or(base);
    let args: &[&str] = match base {
        "cmd" => &["/c"],
        "powershell" | "pwsh" => &["-NoProfile", "-Command"],
        _ => &["-c"],
    };
    ShellConfig {
        shell: path.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        command_from_stdin: false,
    }
}

/// `findExecutableOnPath` on Unix: `which <name>`, first line of its output.
fn find_executable_on_path(name: &str) -> Option<String> {
    let output = std::process::Command::new("which")
        .arg(name)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.trim().lines().next()?;
    (!first.is_empty()).then(|| first.to_owned())
}

/// `getShellConfig(customShellPath)`, POSIX branch.
fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, ToolChildJobError> {
    // `getShellConfig` throws plain errors; the entry labels those `tool_child_job_failed`.
    let failed = |message: String| ToolChildJobError::new("tool_child_job_failed", message);
    if let Some(custom) = custom_shell_path.filter(|path| !path.is_empty()) {
        if Path::new(custom).exists() {
            return Ok(shell_config_for_path(custom));
        }
        return Err(failed(format!("Custom shell path not found: {custom}")));
    }
    if let Some(over) = std::env::var(GIT_BASH_PATH_ENV)
        .ok()
        .filter(|path| !path.is_empty())
    {
        if Path::new(&over).exists() {
            return Ok(shell_config_for_path(&over));
        }
        return Err(failed(format!(
            "{GIT_BASH_PATH_ENV} points to a missing shell: {over}"
        )));
    }
    if Path::new("/bin/bash").exists() {
        return Ok(shell_config_for_path("/bin/bash"));
    }
    if let Some(bash) = find_executable_on_path("bash") {
        return Ok(shell_config_for_path(&bash));
    }
    Ok(ShellConfig {
        shell: "sh".to_owned(),
        args: vec!["-c".to_owned()],
        command_from_stdin: false,
    })
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (`Buffer.toString("base64")`).
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(char::from(BASE64_ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `os.constants.signals[name]`; 0 for a name it does not know (the `?? 0`).
#[cfg(unix)]
fn signal_number(name: &str) -> i32 {
    match name {
        "SIGHUP" => libc::SIGHUP,
        "SIGINT" => libc::SIGINT,
        "SIGQUIT" => libc::SIGQUIT,
        "SIGILL" => libc::SIGILL,
        "SIGTRAP" => libc::SIGTRAP,
        "SIGABRT" => libc::SIGABRT,
        "SIGBUS" => libc::SIGBUS,
        "SIGFPE" => libc::SIGFPE,
        "SIGKILL" => libc::SIGKILL,
        "SIGUSR1" => libc::SIGUSR1,
        "SIGSEGV" => libc::SIGSEGV,
        "SIGUSR2" => libc::SIGUSR2,
        "SIGPIPE" => libc::SIGPIPE,
        "SIGALRM" => libc::SIGALRM,
        "SIGTERM" => libc::SIGTERM,
        "SIGCHLD" => libc::SIGCHLD,
        "SIGCONT" => libc::SIGCONT,
        "SIGSTOP" => libc::SIGSTOP,
        "SIGTSTP" => libc::SIGTSTP,
        "SIGTTIN" => libc::SIGTTIN,
        "SIGTTOU" => libc::SIGTTOU,
        "SIGURG" => libc::SIGURG,
        "SIGXCPU" => libc::SIGXCPU,
        "SIGXFSZ" => libc::SIGXFSZ,
        "SIGVTALRM" => libc::SIGVTALRM,
        "SIGPROF" => libc::SIGPROF,
        "SIGWINCH" => libc::SIGWINCH,
        "SIGIO" => libc::SIGIO,
        "SIGSYS" => libc::SIGSYS,
        _ => 0,
    }
}

#[cfg(not(unix))]
fn signal_number(_name: &str) -> i32 {
    0
}

/// `process.kill(-pid, "SIGKILL")`; the group may already have exited.
#[cfg(unix)]
fn kill_group(pid: u32) {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

/// Reads one output stream to its end, handing every chunk to `chunks` in arrival order.
async fn forward_stream<R: AsyncRead + Unpin>(
    mut stream: R,
    chunks: mpsc::UnboundedSender<Vec<u8>>,
) {
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if chunks.send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
        }
    }
}

async fn run_user_bash(
    args: UserBashJobArgs,
    job: &ToolChildJobContext,
) -> Result<Value, ToolChildJobError> {
    if tokio::fs::metadata(&args.cwd).await.is_err() {
        return Err(ToolChildJobError::new(
            "user_bash_bad_cwd",
            format!(
                "Working directory does not exist: {}\nCannot execute bash commands.",
                args.cwd
            ),
        ));
    }
    let shell = get_shell_config(args.shell_path.as_deref())?;
    let from_stdin = shell.command_from_stdin;
    let mut shell_args = shell.args.clone();
    if !from_stdin {
        shell_args.push(args.command.clone());
    }
    let mut launched = launch_own_group(OwnGroupLaunch {
        gate: Arc::clone(&job.gate),
        job_id: job.job_id.clone(),
        origin: LaunchOrigin::UserBash,
        api: LaunchApi::CpSpawn,
        program: shell.shell,
        args: shell_args,
        options: Some(OwnGroupOptions {
            cwd: Some(args.cwd.clone().into()),
            env: Some(args.env.clone()),
            stdio: [
                if from_stdin {
                    StdioKind::Pipe
                } else {
                    StdioKind::Ignore
                },
                StdioKind::Pipe,
                StdioKind::Pipe,
            ],
        }),
        lifetime: None,
    })
    .await
    // `launchOwnGroup` rejects with a raw error, which the entry labels `tool_child_job_failed`.
    .map_err(|error| ToolChildJobError::new("tool_child_job_failed", error.to_string()))?;
    if from_stdin && let Some(mut stdin) = launched.stdin.take() {
        let command = args.command.clone();
        tokio::spawn(async move {
            let _ = stdin.write_all(command.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    // Every chunk is sent in order and observed; the result settles only after the last one was sent.
    let (chunks, mut received) = mpsc::unbounded_channel::<Vec<u8>>();
    let readers: Vec<tokio::task::JoinHandle<()>> = [
        launched
            .stdout
            .take()
            .map(|stream| tokio::spawn(forward_stream(stream, chunks.clone()))),
        launched
            .stderr
            .take()
            .map(|stream| tokio::spawn(forward_stream(stream, chunks.clone()))),
    ]
    .into_iter()
    .flatten()
    .collect();
    drop(chunks);
    let sender = {
        let update = Arc::clone(&job.update);
        tokio::spawn(async move {
            let mut failure: Option<String> = None;
            while let Some(chunk) = received.recv().await {
                if let Err(error) = (update)(json!({ "data": base64_encode(&chunk) })).await
                    && failure.is_none()
                {
                    failure = Some(error.to_string());
                }
            }
            failure
        })
    };
    let pid = launched.pid;
    let abort_watcher = {
        let signal = job.signal.clone();
        tokio::spawn(async move {
            signal.cancelled().await;
            kill_group(pid);
        })
    };
    let timer = args.timeout_seconds.map(|seconds| {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await;
            kill_group(pid);
        })
    });
    let outcome = launched.exited().await;
    for reader in readers {
        let _ = reader.await;
    }
    let update_failure = sender.await.unwrap_or(None);
    // The `finally`: the timer and the abort listener stop whatever the result.
    if let Some(timer) = timer {
        timer.abort();
    }
    abort_watcher.abort();
    if job.signal.is_aborted() {
        return Err(ToolChildJobError::new("user_bash_aborted", "aborted"));
    }
    if let Some(failure) = update_failure {
        return Err(ToolChildJobError::new(
            "user_bash_output_lost",
            format!("output could not be sent: {failure}"),
        ));
    }
    if let Some(exit_code) = outcome.exit_code {
        return Ok(json!({ "exitCode": exit_code }));
    }
    Ok(match outcome.signal {
        None => json!({ "exitCode": Value::Null }),
        Some(signal) => json!({ "exitCode": 128 + signal_number(&signal) }),
    })
}

pub fn create_user_bash_job_handler() -> ToolChildJobHandler {
    Arc::new(|value: Value, job: ToolChildJobContext| {
        async move {
            if cfg!(windows) {
                return Err(HandlerError::from(ToolChildJobError::new(
                    "tool_child_platform_unsupported",
                    "user bash on the tool child is POSIX-only",
                )));
            }
            let args = parse_user_bash_args(Some(&value)).map_err(HandlerError::from)?;
            run_user_bash(args, &job)
                .await
                .map(Some)
                .map_err(HandlerError::from)
        }
        .boxed()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // user-bash-job.ts has no unit test upstream (it is exercised through the session child and
    // `test/` suites); these cover its pure parts: argument parsing, shell resolution, base64 and exit-code mapping.

    #[test]
    fn parses_args_keeping_only_string_env_values_and_positive_timeouts() {
        let parsed = parse_user_bash_args(Some(&json!({
            "command": "echo hi",
            "cwd": "/tmp",
            "env": { "A": "1", "B": 2 },
            "shellPath": "/bin/sh",
            "timeoutSeconds": 5
        })));
        let Ok(parsed) = parsed else {
            panic!("expected the args to parse");
        };
        assert_eq!(parsed.command, "echo hi");
        assert_eq!(
            parsed.env,
            HashMap::from([("A".to_owned(), "1".to_owned())])
        );
        assert_eq!(parsed.shell_path.as_deref(), Some("/bin/sh"));
        assert_eq!(parsed.timeout_seconds, Some(5.0));
        let zero = parse_user_bash_args(Some(
            &json!({ "command": "x", "cwd": "/", "env": {}, "timeoutSeconds": 0 }),
        ));
        assert_eq!(zero.map(|args| args.timeout_seconds), Ok(None));
    }

    #[test]
    fn refuses_args_missing_command_cwd_or_env() {
        for bad in [
            json!({ "cwd": "/", "env": {} }),
            json!({ "command": "x", "env": {} }),
            json!({ "command": "x", "cwd": "/" }),
        ] {
            let error = parse_user_bash_args(Some(&bad));
            assert_eq!(
                error.map_err(|error| error.code),
                Err("user_bash_bad_args".to_owned())
            );
        }
        assert!(parse_user_bash_args(None).is_err());
    }

    #[test]
    fn picks_shell_args_by_kind() {
        assert_eq!(shell_config_for_path("/bin/sh").args, vec!["-c"]);
        assert_eq!(shell_config_for_path("/usr/bin/bash").args, vec!["-c"]);
        assert_eq!(
            shell_config_for_path("C:\\Windows\\System32\\cmd.exe").args,
            vec!["/c"]
        );
        assert_eq!(
            shell_config_for_path("/usr/bin/pwsh").args,
            vec!["-NoProfile", "-Command"]
        );
    }

    #[test]
    fn a_missing_custom_shell_is_an_error() {
        let error = get_shell_config(Some("/definitely/not/a/shell"));
        assert_eq!(
            error.map_err(|error| error.message),
            Err("Custom shell path not found: /definitely/not/a/shell".to_owned())
        );
    }

    #[test]
    fn encodes_base64_like_buffer_to_string() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(&[0xff, 0x00, 0xfe, 0x01]), "/wD+AQ==");
    }

    #[cfg(unix)]
    #[test]
    fn maps_signal_names_to_numbers() {
        assert_eq!(128 + signal_number("SIGKILL"), 137);
        assert_eq!(128 + signal_number("SIGTERM"), 143);
        assert_eq!(signal_number("SIGNOPE"), 0);
    }
}
