use std::{collections::BTreeMap,path::Path,time::Duration,process::Stdio};
use tokio::{io::AsyncReadExt,sync::{mpsc,oneshot}};
use maho_ai::utils::abort::{AbortController,AbortSignal};
use serde_json::{Value,json};
use crate::{spawn_args::{CursorCliArgsInput,build_cursor_cli_args},stream_parser::CursorCliStreamParser};
use nix::{sys::signal::{killpg,Signal},unistd::Pid,errno::Errno};

pub const MAX_CURSOR_CLI_PROMPT_BYTES:usize = 130000;
pub const CURSOR_CLI_ABORT_GRACE_MS:u64 = 5000;
#[derive(Debug)]
pub enum TransportError { PromptTooLarge { actual_bytes:usize,limit_bytes:usize },Aborted,Io(std::io::Error),Signal(Errno) }
impl std::fmt::Display for TransportError {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PromptTooLarge { actual_bytes,limit_bytes } => write!(f,"Cursor CLI prompt is {actual_bytes} bytes; the limit is {limit_bytes} bytes"),
            Self::Aborted => f.write_str("Cursor CLI invocation aborted"), Self::Io(error) => error.fmt(f), Self::Signal(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for TransportError {}
pub enum TransportOutcome { Completed { exit_code:Option<i32>,signal:Option<i32>,stderr:String },Aborted }
pub struct TransportHandle {
    pub pid:u32, pub events:mpsc::UnboundedReceiver<Result<Value,TransportError>>,
    pub completed:oneshot::Receiver<Result<TransportOutcome,TransportError>>, abort:AbortController,
}
impl TransportHandle { pub fn abort(&self) { self.abort.abort(None); } }
fn signal_group(pid:i32,signal:Signal) -> Result<(),TransportError> {
    match killpg(Pid::from_raw(pid),signal) { Ok(()) | Err(Errno::ESRCH) => Ok(()), Err(error) => Err(TransportError::Signal(error)) }
}
pub fn spawn_cursor_cli(executable:&Path,args:CursorCliArgsInput<'_>,account_home:&str,cwd:&Path,environment:&BTreeMap<String,String>,signal:Option<AbortSignal>) -> Result<TransportHandle,TransportError> {
    if args.prompt.len()>MAX_CURSOR_CLI_PROMPT_BYTES { return Err(TransportError::PromptTooLarge { actual_bytes:args.prompt.len(),limit_bytes:MAX_CURSOR_CLI_PROMPT_BYTES }); }
    if signal.as_ref().is_some_and(AbortSignal::aborted) { return Err(TransportError::Aborted); }
    let mut command = tokio::process::Command::new(executable);
    command.args(build_cursor_cli_args(args)).current_dir(cwd).env_clear()
        .envs(crate::environment::cursor_agent_environment(account_home,environment)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    command.process_group(0);
    let mut child = command.spawn().map_err(TransportError::Io)?;
    let pid = child.id().ok_or_else(|| TransportError::Io(std::io::Error::other("cursor-agent spawned without a process id")))?;
    let group = i32::try_from(pid).map_err(|error| TransportError::Io(std::io::Error::other(error)))?;
    let mut stdout = child.stdout.take().ok_or_else(|| TransportError::Io(std::io::Error::other("cursor-agent stdout unavailable")))?;
    let mut stderr = child.stderr.take().ok_or_else(|| TransportError::Io(std::io::Error::other("cursor-agent stderr unavailable")))?;
    let (sender,events) = mpsc::unbounded_channel(); let (done,completed) = oneshot::channel();
    let abort = AbortController::new(); let requested = abort.signal();
    tokio::spawn(async move {
        let mut parser = CursorCliStreamParser::new(Default::default()); let mut error_bytes=Vec::new();
        let mut out=[0u8;8192]; let mut err=[0u8;8192]; let mut out_open=true; let mut err_open=true;
        let mut aborted=false; let mut failure=None; let mut status=None;
        let mut deadline=Box::pin(tokio::time::sleep(Duration::from_millis(CURSOR_CLI_ABORT_GRACE_MS)));
        let mut escalated=false;
        loop {
            tokio::select! {
                _ = async { if let Some(signal)=&signal { signal.cancelled().await } else { std::future::pending::<()>().await } }, if !aborted => {
                    aborted=true; failure=signal_group(group,Signal::SIGTERM).err(); deadline.as_mut().reset(tokio::time::Instant::now()+Duration::from_millis(CURSOR_CLI_ABORT_GRACE_MS));
                }
                _ = requested.cancelled(), if !aborted => {
                    aborted=true; failure=signal_group(group,Signal::SIGTERM).err(); deadline.as_mut().reset(tokio::time::Instant::now()+Duration::from_millis(CURSOR_CLI_ABORT_GRACE_MS));
                }
                _ = &mut deadline, if aborted && !escalated => { failure=signal_group(group,Signal::SIGKILL).err(); escalated=true; }
                read=stdout.read(&mut out), if out_open => {
                    match read { Ok(0)=>out_open=false, Ok(n)=>for event in parser.push(&out[..n]) { if sender.send(Ok(event)).is_err() { break; } },
                        Err(error)=>{ failure=Some(TransportError::Io(error));out_open=false; } }
                }
                read=stderr.read(&mut err), if err_open => {
                    match read { Ok(0)=>err_open=false, Ok(n)=>{ error_bytes.extend_from_slice(&err[..n]); if error_bytes.len()>65536 { error_bytes.drain(..error_bytes.len()-65536); } },
                        Err(error)=>{ failure=Some(TransportError::Io(error));err_open=false; } }
                }
                result=child.wait(), if status.is_none() => { match result { Ok(value)=>status=Some(value),Err(error)=>{ failure=Some(TransportError::Io(error));break; } } }
            }
            if status.is_some() && !out_open && !err_open { break; }
        }
        if let Some(error)=failure { drop(done.send(Err(error))); }
        else if aborted { if sender.send(Ok(json!({"type":"aborted","kind":"aborted"}))).is_err() { drop(sender); }
            drop(done.send(Ok(TransportOutcome::Aborted))); }
        else if let Some(status)=status {
            for event in parser.finish() { if sender.send(Ok(event)).is_err() { break; } }
            use std::os::unix::process::ExitStatusExt;
            drop(done.send(Ok(TransportOutcome::Completed { exit_code:status.code(),signal:status.signal(),stderr:String::from_utf8_lossy(&error_bytes).into_owned() })));
        }
    });
    Ok(TransportHandle { pid,events,completed,abort })
}
