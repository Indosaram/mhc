//! Port of `packages/machine-ops/src/process/exec.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::process::oom_score::prefer_exec_child_as_oom_victim;
use crate::process::scopes::{RUNTIME_ENV, SCOPE_ENV, ScopeRegistry, new_scope_token};
use crate::process::wait_child::{StdioStream, WaitForChildProcessOptions, wait_child_with_stdio};
use crate::types::{
    AbortSignal, CommandTransport, EnvMode, ExecRequest, ExecResult, ExecShell, ExecStream,
    OpContext,
};
use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

pub type KillTreeFn = Arc<dyn Fn(u32) + Send + Sync>;

/// Variables the machine path passes through to a child; everything else is dropped.
const CURATED_ENV_KEYS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "TZ",
    "SystemRoot",
    "SYSTEMROOT",
    "ComSpec",
    "PATHEXT",
    "WINDIR",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
];

pub fn curated_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for &key in CURATED_ENV_KEYS {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_owned(), value);
        }
    }
    env
}

pub fn curated_env_from(source: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for &key in CURATED_ENV_KEYS {
        if let Some(value) = source.get(key) {
            env.insert(key.to_owned(), value.clone());
        }
    }
    env
}

pub fn default_shell() -> ExecShell {
    if cfg!(windows) {
        let path = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_owned());
        ExecShell {
            path,
            args: vec!["/d".to_owned(), "/s".to_owned(), "/c".to_owned()],
            command_transport: CommandTransport::Argv,
        }
    } else {
        ExecShell {
            path: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned()],
            command_transport: CommandTransport::Argv,
        }
    }
}

struct CappedBuffer {
    bytes: Vec<u8>,
    cap: usize,
    truncated: bool,
}

impl CappedBuffer {
    fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            cap,
            truncated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let room = self.cap.saturating_sub(self.bytes.len());
        if room == 0 {
            if !chunk.is_empty() {
                self.truncated = true;
            }
            return;
        }
        let take = chunk.len().min(room);
        if take < chunk.len() {
            self.truncated = true;
        }
        self.bytes.extend_from_slice(&chunk[..take]);
    }
}

/// POSIX: SIGKILL the group, falling back to the leader; Windows: terminate the process.
pub fn default_kill_tree(pid: u32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill, killpg};
        use nix::unistd::Pid;
        let raw = i32::try_from(pid).unwrap_or(i32::MAX);
        if killpg(Pid::from_raw(raw), Signal::SIGKILL).is_err() {
            let _ = kill(Pid::from_raw(raw), Signal::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/pid", &pid.to_string(), "/t", "/f"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Run one command in its own process group (with a scope token when a registry is configured),
/// capped capture, a timeout and abort.
pub async fn run_exec(
    req: ExecRequest,
    ctx: &OpContext,
    scopes: Option<&ScopeRegistry>,
    kill_tree: &KillTreeFn,
) -> Result<ExecResult, MachineOpError> {
    let effective_timeout_ms = match req.timeout_ms {
        Some(Some(ms)) => Some(ms),
        Some(None) => None,
        None => ctx.limits.exec_timeout_default_ms,
    };
    if let Some(0) = effective_timeout_ms {
        return Err(MachineOpError::new(
            OpErrorCode::InvalidArgument,
            "timeoutMs must be positive",
        ));
    }
    let shell = req.shell.clone().unwrap_or_else(default_shell);
    let token = if let Some(scopes) = scopes {
        let tok = new_scope_token();
        let preview: String = req.command.chars().take(64).collect();
        scopes.record(&tok, &ctx.principal.id, &preview).await?;
        Some(tok)
    } else {
        None
    };

    let mut env = match req.env_mode {
        EnvMode::Exact => BTreeMap::new(),
        EnvMode::Overlay => curated_env(),
    };
    if let Some(overrides) = &req.env {
        for (k, v) in overrides {
            env.insert(k.clone(), v.clone());
        }
    }
    if let Some(tok) = &token {
        env.insert(SCOPE_ENV.to_owned(), tok.clone());
        env.remove(RUNTIME_ENV);
    }

    let via_stdin = matches!(shell.command_transport, CommandTransport::Stdin);
    let started_at_ms = now_ms();
    let started = Instant::now();

    let mut cmd = tokio::process::Command::new(&shell.path);
    cmd.args(&shell.args);
    if !via_stdin {
        cmd.arg(&req.command);
    }
    if let Some(cwd) = &req.cwd {
        cmd.current_dir(cwd);
    }
    cmd.env_clear();
    cmd.envs(&env);
    cmd.stdin(if via_stdin {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            if let (Some(scopes), Some(tok)) = (scopes, token.as_deref()) {
                scopes.release(tok).await;
            }
            return Err(MachineOpError::from_io(
                &err,
                Some("spawn"),
                Some(std::path::Path::new(&shell.path)),
            ));
        }
    };

    if via_stdin && let Some(mut stdin) = child.stdin.take() {
        let cmd_bytes = req.command.as_bytes().to_vec();
        tokio::spawn(async move {
            let _ = stdin.write_all(&cmd_bytes).await;
            let _ = stdin.shutdown().await;
        });
    }

    let pid = child.id();
    if let Some(pid) = pid {
        prefer_exec_child_as_oom_victim(pid);
        if let (Some(scopes), Some(tok)) = (scopes, token.as_deref()) {
            scopes.attach(tok, pid);
        }
        if let Some(on_spawn) = &req.on_spawn {
            on_spawn(pid);
        }
    }

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let killed_signal = AbortSignal::default();
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let aborted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let trigger_kill = {
        let kill_tree = Arc::clone(kill_tree);
        let killed_signal = killed_signal.clone();
        move || {
            if let Some(pid) = pid {
                kill_tree(pid);
            }
            killed_signal.abort();
        }
    };

    // Watch ctx.signal and timeout concurrently while wait_child_with_stdio runs.
    let abort_watcher = {
        let ctx_signal = ctx.signal.clone();
        let aborted = Arc::clone(&aborted);
        let trigger_kill = trigger_kill.clone();
        tokio::spawn(async move {
            ctx_signal.cancelled().await;
            aborted.store(true, std::sync::atomic::Ordering::SeqCst);
            trigger_kill();
        })
    };

    let timeout_watcher = effective_timeout_ms.map(|ms| {
        let timed_out = Arc::clone(&timed_out);
        let trigger_kill = trigger_kill.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
            trigger_kill();
        })
    });

    if ctx.signal.is_aborted() {
        aborted.store(true, std::sync::atomic::Ordering::SeqCst);
        trigger_kill();
    }

    let mut stdout_buf = CappedBuffer::new(ctx.limits.exec_stdout_max_bytes);
    let mut stderr_buf = CappedBuffer::new(ctx.limits.exec_stderr_max_bytes);
    let mut callback_error: Option<MachineOpError> = None;

    let waited_res = wait_child_with_stdio(
        &mut child,
        stdout_pipe,
        stderr_pipe,
        WaitForChildProcessOptions {
            signal: Some(killed_signal.clone()),
            abort_exit_grace_ms: None,
        },
        |stream, chunk| {
            let exec_stream = match stream {
                StdioStream::Stdout => {
                    stdout_buf.push(chunk);
                    ExecStream::Stdout
                }
                StdioStream::Stderr => {
                    stderr_buf.push(chunk);
                    ExecStream::Stderr
                }
            };
            if let Some(on_output) = &req.on_output
                && callback_error.is_none()
                && let Err(err) = on_output(chunk, exec_stream)
            {
                callback_error = Some(err);
                trigger_kill();
            }
        },
    )
    .await;

    abort_watcher.abort();
    if let Some(tw) = timeout_watcher {
        tw.abort();
    }

    let child_already_exited = matches!(child.try_wait(), Ok(Some(_)))
        || waited_res
            .as_ref()
            .is_ok_and(|w| w.exit_code.is_some() || w.signal.is_some());

    if let (Some(pid), Some(on_exit)) = (pid, req.on_exit.clone()) {
        if child_already_exited {
            on_exit(pid);
        } else {
            // Child survived the kill grace period: fire `on_exit` once it actually exits.
            tokio::spawn(async move {
                let _ = child.wait().await;
                on_exit(pid);
            });
        }
    }

    if let (Some(scopes), Some(tok)) = (scopes, token.as_deref()) {
        scopes.release(tok).await;
    }

    if let Some(err) = callback_error {
        return Err(err);
    }

    let waited = waited_res?;
    Ok(ExecResult {
        pid,
        exit_code: waited.exit_code,
        signal: waited.signal,
        stdout: stdout_buf.bytes,
        stderr: stderr_buf.bytes,
        stdout_truncated: stdout_buf.truncated,
        stderr_truncated: stderr_buf.truncated,
        timed_out: timed_out.load(std::sync::atomic::Ordering::SeqCst),
        aborted: aborted.load(std::sync::atomic::Ordering::SeqCst),
        started_at_ms,
        duration_ms: started.elapsed().as_millis() as u64,
        scope_token: token,
    })
}
