use std::{process::Stdio, time::Instant};
use serde::{Deserialize, Serialize};
use tokio::{io::{AsyncRead, AsyncReadExt}, process::Command};
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
async fn pipe(mut pipe: impl AsyncRead + Unpin, stream: &dyn Fn(&[u8]) -> Result<()>) -> Result<()> {
    let mut chunk = [0_u8; 8192];
    loop {
        let count = pipe.read(&mut chunk).await.map_err(io_error)?;
        if count == 0 { return Ok(()); }
        stream(&chunk[..count])?;
    }
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
            tokio::task::spawn_blocking(move || {
                let env = std::env::vars().collect();
                crate::harness::env::nodejs::kill_windows_process_tree(
                    pid,
                    &crate::harness::env::nodejs::windows_taskkill_candidates(&env),
                );
            }).await.map_err(|error| Error::Storage(format!("bash process tree cleanup: {error}")))?;
            if child.try_wait().map_err(io_error)?.is_none() { child.kill().await.map_err(io_error)?; }
            child.wait().await.map_err(io_error)?;
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
