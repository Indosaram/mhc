use std::{process::Stdio, time::Instant};
use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use super::types::*;

#[derive(Clone, Debug, Deserialize)]
pub struct BashArgs { pub command: String, pub cwd: Option<String> }
pub struct BashTool { parameters: JsonObject, output: OutputLimits }
pub fn bash_tool(output: OutputLimits) -> Result<BashTool> {
    let parameters = serde_json::from_str(r#"{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"}},"required":["command"]}"#)?;
    Ok(BashTool { parameters, output })
}
impl ToolDeclaration for BashTool {
    type Args = BashArgs;
    fn name(&self) -> &str { "bash" }
    fn description(&self) -> &str { "Run a shell command" }
    fn parameters(&self) -> &JsonObject { &self.parameters }
    fn replay(&self) -> Replay { Replay::Unsafe }
    fn output(&self) -> OutputLimits { self.output.clone() }
    fn execute<'a>(&'a self, args: BashArgs, api: &'a dyn ToolApi, ctx: &'a Context) -> LocalBoxFuture<'a, Result<ToolResult>> {
        Box::pin(async move { execute(args, &|chunk| api.stream(chunk), ctx).await })
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BashDetails { exit_code: Option<i32>, signal: Option<String>, ms: u64 }
fn io_error(error: std::io::Error) -> Error { Error::Storage(format!("bash: {error}")) }
#[cfg(not(windows))]
async fn pipe(mut pipe: impl AsyncRead + Unpin, stream: &dyn Fn(&[u8]) -> Result<()>) -> Result<()> {
    let mut chunk = [0_u8; 8192];
    loop {
        let count = pipe.read(&mut chunk).await.map_err(io_error)?;
        if count == 0 { return Ok(()); }
        stream(&chunk[..count])?;
    }
}
#[cfg(windows)]
fn windows_pipe(handle: std::os::windows::io::OwnedHandle) -> tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
    // Tokio's Windows child pipes use uncancellable blocking-pool reads. A Git Bash
    // descendant can retain a writer after the launcher exits; dropping the read
    // future then leaves runtime shutdown waiting forever. Like env/nodejs.rs,
    // keep these reads on dedicated threads, never on the runtime's blocking pool.
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut pipe = std::fs::File::from(handle);
        let mut chunk = [0_u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => if tx.blocking_send(Ok(chunk[..count].to_vec())).is_err() { break; },
                Err(error) => { let _ = tx.blocking_send(Err(error)); break; }
            }
        }
    });
    rx
}
#[cfg(windows)]
async fn pipe(mut chunks: tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>, stream: &dyn Fn(&[u8]) -> Result<()>) -> Result<()> {
    while let Some(chunk) = chunks.recv().await {
        stream(&chunk.map_err(io_error)?)?;
    }
    Ok(())
}
#[cfg(windows)]
fn kill_windows_descendants(pid: u32) -> Result<()> {
    use crate::harness::env::windows_process_tree::{WindowsProcessRow, windows_tree_kill_plan};
    use winsafe::{HPROCESS, HPROCESSLIST, co};

    let native_error = |error: winsafe::co::ERROR| io_error(std::io::Error::other(error.to_string()));
    // Snapshot while the owned root is still alive: killing it first loses the
    // ancestry needed to find Git Bash grandchildren retaining our pipe writers.
    // Native calls avoid PowerShell startup and serial taskkill timeout budgets.
    let mut snapshot = HPROCESSLIST::CreateToolhelp32Snapshot(co::TH32CS::SNAPPROCESS, None).map_err(native_error)?;
    let mut rows = Vec::new();
    for entry in snapshot.iter_processes() {
        let entry = entry.map_err(native_error)?;
        let created_at = HPROCESS::OpenProcess(co::PROCESS::QUERY_LIMITED_INFORMATION, false, entry.th32ProcessID)
            .and_then(|process| process.GetProcessTimes())
            .map(|(created, _, _, _)| (u128::from(created.dwHighDateTime) << 32) | u128::from(created.dwLowDateTime))
            .unwrap_or(0);
        rows.push(WindowsProcessRow {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            created_at,
            name: entry.szExeFile(),
        });
    }
    let plan = windows_tree_kill_plan(&rows, pid, std::process::id());
    let mut failure = None;
    // Leaves first; keep the root alive until every descendant is addressed.
    for target in plan.kill.into_iter().rev().filter(|target| *target != pid) {
        let result = HPROCESS::OpenProcess(
            co::PROCESS::TERMINATE | co::PROCESS::QUERY_LIMITED_INFORMATION | co::PROCESS::SYNCHRONIZE,
            false,
            target,
        ).and_then(|process| {
            let (created, _, _, _) = process.GetProcessTimes()?;
            let created_at = (u128::from(created.dwHighDateTime) << 32) | u128::from(created.dwLowDateTime);
            // Recheck identity through the same handle used for termination.
            if rows.iter().any(|row| row.pid == target && row.created_at == created_at) {
                process.TerminateProcess(1).or_else(|error| {
                    if process.WaitForSingleObject(Some(0))? == co::WAIT::OBJECT_0 { Ok(()) } else { Err(error) }
                })?;
            }
            Ok(())
        });
        if let Err(error) = result
            && error != co::ERROR::INVALID_PARAMETER
            && failure.is_none()
        {
            failure = Some(native_error(error));
        }
    }
    match failure { Some(error) => Err(error), None => Ok(()) }
}
async fn execute(args: BashArgs, stream: &dyn Fn(&[u8]) -> Result<()>, ctx: &Context) -> Result<ToolResult> {
    if ctx.is_aborted() { return Err(Error::Forbidden("bash invocation aborted".into())); }
    let started = Instant::now();
    #[cfg(windows)]
    let shell = crate::harness::env::nodejs::get_shell_config(None)
        .map_err(|error| Error::Storage(error.message))?.shell;
    #[cfg(not(windows))]
    let shell = "bash";
    let mut command = Command::new(shell);
    command.args(["-c", &args.command]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    if let Some(cwd) = args.cwd { command.current_dir(cwd); }
    let mut child = command.spawn().map_err(io_error)?;
    #[cfg(windows)]
    let pid = child.id().ok_or_else(|| Error::InvalidShape("bash child PID absent".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| Error::InvalidShape("bash stdout pipe absent".into()))?;
    let stderr = child.stderr.take().ok_or_else(|| Error::InvalidShape("bash stderr pipe absent".into()))?;
    // Convert before polling either pipe: no Tokio blocking read may be started.
    #[cfg(windows)]
    let stdout = windows_pipe(stdout.into_owned_handle().map_err(io_error)?);
    #[cfg(windows)]
    let stderr = windows_pipe(stderr.into_owned_handle().map_err(io_error)?);
    let signal = ctx.abort_signal();
    let cancelled = async { match signal { Some(signal) => signal.cancelled().await, None => std::future::pending::<()>().await } };
    let completion = async {
        let (((), ()), status) = futures::future::try_join(
            futures::future::try_join(pipe(stdout, stream), pipe(stderr, stream)),
            async { child.wait().await.map_err(io_error) },
        ).await?;
        Ok::<_, Error>(status)
    };
    let outcome = tokio::select! {
        biased;
        () = cancelled => Err(Error::Forbidden("bash invocation aborted".into())),
        result = completion => result,
    };
    let status = match outcome {
        Ok(status) => status,
        Err(error) => {
            #[cfg(windows)]
            let descendants = tokio::task::spawn_blocking(move || kill_windows_descendants(pid))
                .await.map_err(|error| Error::Storage(format!("bash process tree cleanup: {error}")))
                .and_then(|result| result);
            // Always reap the owned child, including when descendant cleanup fails.
            if child.try_wait().map_err(io_error)?.is_none() { child.kill().await.map_err(io_error)?; }
            child.wait().await.map_err(io_error)?;
            #[cfg(windows)]
            descendants?;
            return Err(error);
        }
    };
    let details = BashDetails { exit_code: status.code(), signal: exit_signal(status), ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX) };
    Ok(ToolResult { is_error: Some(details.exit_code != Some(0)), details: Some(to_stored(&details)?.as_json().clone()), ..ToolResult::default() })
}
fn exit_signal(status: std::process::ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|signal| match signal {
            1 => "SIGHUP".into(), 2 => "SIGINT".into(), 3 => "SIGQUIT".into(), 4 => "SIGILL".into(), 6 => "SIGABRT".into(),
            8 => "SIGFPE".into(), 9 => "SIGKILL".into(), 11 => "SIGSEGV".into(), 13 => "SIGPIPE".into(), 14 => "SIGALRM".into(), 15 => "SIGTERM".into(),
            signal => format!("SIG{signal}"),
        })
    }
    #[cfg(not(unix))]
    { let _status = status; None }
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;
