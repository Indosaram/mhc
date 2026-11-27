//! Port of `packages/coding-agent/src/core/tools/shell-operations.ts`.

use crate::definition::{AbortSignal, ToolError, ToolFuture};
use crate::posix_signals::posix_signal_number;
use crate::tool_machine::ToolMachine;
use maho_machine_ops::{
    EnvMode, ExecRequest, ExecShell, ExecStream, MachineOpError, OpErrorCode, StatRequest,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;

pub fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<Duration>, ToolError> {
    match timeout {
        None => Ok(None),
        Some(seconds) if !seconds.is_finite() || seconds <= 0.0 => Err(ToolError::Message(
            "Invalid timeout: must be a finite number of seconds".into(),
        )),
        Some(seconds) if seconds * 1000.0 > MAX_TIMEOUT_MS => Err(ToolError::Message(format!(
            "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
        ))),
        Some(seconds) => Ok(Some(Duration::from_secs_f64(seconds))),
    }
}

/// Which pipe a chunk came from (`onData(data, stream)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BashStream {
    Stdout,
    Stderr,
}

pub type BashDataCallback = Arc<dyn Fn(&[u8], BashStream) -> Result<(), ToolError> + Send + Sync>;

pub struct BashExecOptions {
    pub on_data: BashDataCallback,
    /// Ask operations to redact credential-shaped output.
    pub redact_secrets_in_output: bool,
    pub signal: AbortSignal,
    pub timeout: Option<f64>,
    pub env: Option<BTreeMap<String, String>>,
    /// Session identifier; local host may attest the spawned shell to it.
    pub session_id: Option<String>,
}

pub struct BashExit {
    pub exit_code: Option<i32>,
}

pub trait BashOperations: Send + Sync {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a Path,
        options: BashExecOptions,
    ) -> ToolFuture<'a, BashExit>;
}

pub type ResolveShellFn = Arc<dyn Fn() -> Result<Option<ExecShell>, ToolError> + Send + Sync>;
pub type CheckCwdFn = Arc<dyn Fn(&Path, &AbortSignal) -> ToolFuture<'static, ()> + Send + Sync>;
pub type DefaultEnvFn = Arc<dyn Fn() -> Option<BTreeMap<String, String>> + Send + Sync>;
pub type SignalNumberFn = Arc<dyn Fn(&str) -> Option<i32> + Send + Sync>;
pub type AttestSessionSpawnFn = Arc<dyn Fn(&str, u32) -> Box<dyn FnOnce() + Send> + Send + Sync>;

pub struct ShellExecHost {
    pub resolve_shell: ResolveShellFn,
    pub check_cwd: CheckCwdFn,
    pub env_mode: EnvMode,
    pub default_env: DefaultEnvFn,
    pub signal_number: SignalNumberFn,
    pub on_spawn: Option<Arc<dyn Fn(u32) + Send + Sync>>,
    pub on_exit: Option<Arc<dyn Fn(u32) + Send + Sync>>,
    pub attest_session_spawn: Option<AttestSessionSpawnFn>,
    pub after_exec: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// A host that knows only `machine`: the machine's shell and environment, cwd checked there, and
/// signal numbers of the machine's own OS.
pub fn machine_shell_exec_host(machine: ToolMachine, shell: Option<ExecShell>) -> ShellExecHost {
    let check_machine = machine.clone();
    let sig_machine = machine;
    ShellExecHost {
        resolve_shell: Arc::new(move || Ok(shell.clone())),
        check_cwd: Arc::new(move |cwd, signal| {
            let cwd_str = cwd.to_string_lossy().into_owned();
            let m = check_machine.clone();
            let sig = signal.clone();
            Box::pin(async move {
                let ctx = m.context(Some(&sig), None);
                m.ops()
                    .files()
                    .stat(
                        StatRequest {
                            path: cwd_str,
                            access: None,
                        },
                        &ctx,
                    )
                    .await
                    .map(|_| ())
                    .map_err(|err| ToolError::Message(err.message))
            })
        }),
        env_mode: EnvMode::Overlay,
        default_env: Arc::new(|| None),
        signal_number: Arc::new(move |signal_name| {
            let os = sig_machine.ops().info().host().os;
            posix_signal_number(&os, signal_name)
        }),
        on_spawn: None,
        on_exit: None,
        attest_session_spawn: None,
        after_exec: None,
    }
}

struct AfterExecGuard(Option<Arc<dyn Fn() + Send + Sync>>);

impl Drop for AfterExecGuard {
    fn drop(&mut self) {
        if let Some(after_exec) = self.0.take() {
            after_exec();
        }
    }
}

/// Shell execution used by the built-in shell tools: one `process.exec` of `machine`.
pub fn create_shell_operations(
    shell_name: &str,
    machine: ToolMachine,
    host: ShellExecHost,
) -> Arc<dyn BashOperations> {
    let shell_name = shell_name.to_owned();
    let host = Arc::new(host);
    Arc::new(MachineShellOperations {
        shell_name,
        machine,
        host,
    })
}

struct MachineShellOperations {
    shell_name: String,
    machine: ToolMachine,
    host: Arc<ShellExecHost>,
}

impl BashOperations for MachineShellOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a Path,
        options: BashExecOptions,
    ) -> ToolFuture<'a, BashExit> {
        Box::pin(async move {
            let timeout_duration = resolve_timeout_ms(options.timeout)?;
            let timeout_ms = timeout_duration.map(|d| d.as_millis().max(1) as u64);
            if options.signal.is_aborted() {
                return Err(ToolError::Message("aborted".into()));
            }
            let shell = (self.host.resolve_shell)()?;
            if (self.host.check_cwd)(cwd, &options.signal).await.is_err() {
                return Err(ToolError::Message(format!(
                    "Working directory does not exist: {}\nCannot execute {} commands.",
                    cwd.display(),
                    self.shell_name
                )));
            }

            let callback_error: Arc<Mutex<Option<ToolError>>> = Arc::new(Mutex::new(None));
            let callback_abort = AbortSignal::default();
            let on_output = {
                let on_data = Arc::clone(&options.on_data);
                let callback_error = Arc::clone(&callback_error);
                let callback_abort = callback_abort.clone();
                Arc::new(move |chunk: &[u8], stream: ExecStream| {
                    if callback_abort.is_aborted() {
                        return Ok(());
                    }
                    let bash_stream = match stream {
                        ExecStream::Stdout => BashStream::Stdout,
                        ExecStream::Stderr => BashStream::Stderr,
                    };
                    if let Err(err) = on_data(chunk, bash_stream) {
                        let msg = err.to_string();
                        if let Ok(mut guard) = callback_error.lock()
                            && guard.is_none()
                        {
                            *guard = Some(err);
                        }
                        callback_abort.abort();
                        return Err(MachineOpError::new(OpErrorCode::Aborted, msg));
                    }
                    Ok(())
                })
            };

            let exec_signal = AbortSignal::any(&[options.signal.clone(), callback_abort]);
            type AttestationSlot = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;
            let attestation: AttestationSlot = Arc::new(Mutex::new(None));

            let on_spawn = match (&self.host.attest_session_spawn, &options.session_id) {
                (Some(attest), Some(session_id)) => {
                    let attest = Arc::clone(attest);
                    let session_id = session_id.clone();
                    let host_on_spawn = self.host.on_spawn.clone();
                    let attestation = Arc::clone(&attestation);
                    Some(Arc::new(move |pid: u32| {
                        let release = attest(&session_id, pid);
                        if let Ok(mut guard) = attestation.lock() {
                            *guard = Some(release);
                        }
                        if let Some(cb) = &host_on_spawn {
                            cb(pid);
                        }
                    }) as maho_machine_ops::ExecPidCallback)
                }
                _ => self.host.on_spawn.clone(),
            };

            let on_exit = match (&self.host.attest_session_spawn, &options.session_id) {
                (Some(_), Some(_)) => {
                    let host_on_exit = self.host.on_exit.clone();
                    let attestation = Arc::clone(&attestation);
                    Some(Arc::new(move |pid: u32| {
                        if let Ok(mut guard) = attestation.lock()
                            && let Some(release) = guard.take()
                        {
                            release();
                        }
                        if let Some(cb) = &host_on_exit {
                            cb(pid);
                        }
                    }) as maho_machine_ops::ExecPidCallback)
                }
                _ => self.host.on_exit.clone(),
            };

            let _after_guard = AfterExecGuard(self.host.after_exec.clone());
            let env = options.env.or_else(|| (self.host.default_env)());
            let ctx = self.machine.context(Some(&exec_signal), None);
            let req = ExecRequest {
                command: command.to_owned(),
                cwd: Some(cwd.to_path_buf()),
                env,
                env_mode: self.host.env_mode,
                shell,
                timeout_ms: Some(timeout_ms),
                on_output: Some(on_output),
                on_spawn,
                on_exit,
            };

            let result = self.machine.ops().process().exec(req, &ctx).await;

            if let Ok(mut guard) = callback_error.lock()
                && let Some(err) = guard.take()
            {
                return Err(err);
            }
            if options.signal.is_aborted() {
                return Err(ToolError::Message("aborted".into()));
            }

            let exec_result = match result {
                Ok(r) => r,
                Err(err) => return Err(ToolError::Message(err.message)),
            };
            if exec_result.timed_out {
                return Err(ToolError::Message(format!(
                    "timeout:{}",
                    options.timeout.unwrap_or_default()
                )));
            }
            let exit_code = exec_result.exit_code.or_else(|| {
                exec_result
                    .signal
                    .as_deref()
                    .and_then(|sig| (self.host.signal_number)(sig))
                    .map(|n| 128 + n)
                    .or(Some(1))
            });
            Ok(BashExit { exit_code })
        })
    }
}
