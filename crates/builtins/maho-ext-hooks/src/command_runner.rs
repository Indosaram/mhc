use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration,Instant};
use tokio::io::{AsyncRead,AsyncReadExt,AsyncWriteExt};
use crate::output_bounds::*;
use crate::plugin_loader::select_hook_command_for_platform;
use crate::safety::{build_hook_environment,resolve_hook_timeout_seconds};
use crate::types::{ExecutableHookHandler,SupportedHookEvent};
use maho_tools::definition::AbortSignal;

pub struct CommandHookRunOptions<'a> {
    pub cwd:&'a Path,
    pub env_passthrough:&'a [String],
    pub output_policy:Option<&'a HookOutputPolicy>,
    pub signal:Option<&'a AbortSignal>,
    pub source_env:Option<&'a BTreeMap<String,String>>,
}
pub struct CommandHookRunResult {
    pub command:String,pub cwd:String,pub stdout:String,pub stderr:String,
    pub exit_code:Option<i32>,pub signal:Option<String>,pub timed_out:bool,pub aborted:bool,
    pub duration_ms:f64,pub output_safety:HookOutputSafetyMetadata,pub timeout_seconds:f64,
}

pub async fn run_command_hook(handler:&ExecutableHookHandler,input:&serde_json::Value,options:CommandHookRunOptions<'_>)->std::io::Result<CommandHookRunResult> {
    let platform=if cfg!(windows) {"win32"} else {"linux"};
    let command=select_hook_command_for_platform(handler,platform);let start=Instant::now();
    let timeout_seconds=resolve_hook_timeout_seconds(handler).map_err(std::io::Error::other)?;
    if options.signal.is_some_and(AbortSignal::is_aborted) {return build_result(command,&options,start,None,None,false,true,timeout_seconds,Captured::default(),Captured::default());}
    let event:SupportedHookEvent=serde_json::from_value(input.get("event").cloned().unwrap_or(serde_json::Value::Null)).map_err(std::io::Error::other)?;
    let inherited=std::env::vars().collect::<BTreeMap<_,_>>();let env=build_hook_environment(handler,event,options.source_env.unwrap_or(&inherited),options.env_passthrough);
    let mut process=if cfg!(windows) {let mut process=tokio::process::Command::new("cmd.exe");process.args(["/d","/s","/c",command]);process} else {let mut process=tokio::process::Command::new("/bin/sh");process.args(["-c",command]);process};
    process.current_dir(options.cwd).env_clear().envs(env).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    process.process_group(0);
    let mut child=process.spawn()?;let pid=child.id();
    let stdout=child.stdout.take().ok_or_else(||std::io::Error::other("hook stdout unavailable"))?;
    let stderr=child.stderr.take().ok_or_else(||std::io::Error::other("hook stderr unavailable"))?;
    let stdout_limit=options.output_policy.and_then(|p|p.max_stdout_bytes).unwrap_or(DEFAULT_STDOUT_LIMIT_BYTES);
    let stderr_limit=options.output_policy.and_then(|p|p.max_stderr_bytes).unwrap_or(DEFAULT_STDERR_LIMIT_BYTES);
    let out=tokio::spawn(capture_stream(stdout,stdout_limit));let err=tokio::spawn(capture_stream(stderr,stderr_limit));
    if let Some(mut stdin)=child.stdin.take() {stdin.write_all(&serde_json::to_vec(input).map_err(std::io::Error::other)?).await?;stdin.shutdown().await?;}
    let timeout=tokio::time::sleep(Duration::try_from_secs_f64(timeout_seconds).map_err(std::io::Error::other)?);tokio::pin!(timeout);
    let cancel=async {match options.signal {Some(signal)=>signal.cancelled().await,None=>std::future::pending::<()>().await}};tokio::pin!(cancel);
    let (status,timed_out,aborted)=tokio::select! {
        status=child.wait()=>(status?,false,false),
        _=&mut timeout=>{kill_child_tree(&mut child,pid).await?;(child.wait().await?,true,false)},
        _=&mut cancel=>{kill_child_tree(&mut child,pid).await?;(child.wait().await?,false,true)},
    };
    let stdout=out.await.map_err(std::io::Error::other)??;let stderr=err.await.map_err(std::io::Error::other)??;
    #[cfg(unix)]
    let signal={use std::os::unix::process::ExitStatusExt;status.signal().map(|signal|format!("SIG{signal}"))};
    #[cfg(not(unix))]
    let signal=None;
    build_result(command,&options,start,if timed_out||aborted {None} else {status.code()},signal,timed_out,aborted,timeout_seconds,stdout,stderr)
}

async fn kill_child_tree(child:&mut tokio::process::Child,pid:Option<u32>)->std::io::Result<()> {
    #[cfg(unix)]
    if let Some(pid)=pid {
        let pid=i32::try_from(pid).map_err(std::io::Error::other)?;
        match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid),nix::sys::signal::Signal::SIGKILL) {Ok(())=>{},Err(nix::errno::Errno::ESRCH)=>{},Err(error)=>return Err(std::io::Error::other(error))}
        return Ok(());
    }
    child.start_kill()
}

#[derive(Default)]
struct Captured {text:String,total_bytes:usize,kept_bytes:usize}
async fn capture_stream(mut stream:impl AsyncRead+Unpin,limit:usize)->std::io::Result<Captured> {
    let mut captured=Captured::default();let mut buffer=[0;8192];
    loop {let bytes=stream.read(&mut buffer).await?;if bytes==0 {break;}captured.total_bytes+=bytes;
        let keep=bytes.min(limit.saturating_sub(captured.kept_bytes));captured.text.push_str(&String::from_utf8_lossy(&buffer[..keep]));captured.kept_bytes+=keep;
    }Ok(captured)
}

#[expect(clippy::too_many_arguments,reason="mirrors upstream result builder's complete exit and capture record")]
fn build_result(command:&str,options:&CommandHookRunOptions<'_>,start:Instant,exit_code:Option<i32>,signal:Option<String>,timed_out:bool,aborted:bool,timeout_seconds:f64,stdout:Captured,stderr:Captured)->std::io::Result<CommandHookRunResult> {
    let stdout=apply_hook_output_safety(HookStream::Stdout,&stdout.text,options.output_policy,Some(CaptureMetadata {original_bytes:stdout.total_bytes,truncated:stdout.total_bytes>stdout.kept_bytes}))?;
    let stderr=apply_hook_output_safety(HookStream::Stderr,&stderr.text,options.output_policy,Some(CaptureMetadata {original_bytes:stderr.total_bytes,truncated:stderr.total_bytes>stderr.kept_bytes}))?;
    Ok(CommandHookRunResult {command:command.to_owned(),cwd:options.cwd.to_string_lossy().into_owned(),stdout:stdout.text,stderr:stderr.text,exit_code,signal,timed_out,aborted,duration_ms:start.elapsed().as_secs_f64()*1000.0,output_safety:HookOutputSafetyMetadata {stdout:stdout.safety,stderr:stderr.safety},timeout_seconds})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_hook_config;
    use crate::types::{HookSourceMetadata,HookSourceScope,HookDiscoveryTiming};
    use serde_json::json;
    fn handler(command:&str)->ExecutableHookHandler {parse_hook_config(&json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":command}]}]}}),&HookSourceMetadata {scope:HookSourceScope::Project,source_path:"/repo/hooks.json".to_owned(),display_order:1,discovered_at:HookDiscoveryTiming::PreSession,plugin_root:None,manifest_path:None,plugin_env:None}).executable_handlers.remove(0)}
    fn options(path:&Path)->CommandHookRunOptions<'_> {CommandHookRunOptions {cwd:path,env_passthrough:&[],output_policy:None,signal:None,source_env:None}}
    #[tokio::test] async fn stdin_stdout_stderr_exit_metadata()->std::io::Result<()> {let dir=tempfile::tempdir()?;let result=run_command_hook(&handler("cat; printf stderr-marker >&2; exit 7"),&json!({"event":"PreToolUse","toolName":"Bash"}),options(dir.path())).await?;assert_eq!(serde_json::from_str::<serde_json::Value>(&result.stdout).map_err(std::io::Error::other)?,json!({"event":"PreToolUse","toolName":"Bash"}));assert_eq!(result.stderr,"stderr-marker");assert_eq!(result.exit_code,Some(7));assert!(!result.aborted && !result.timed_out);Ok(())}
    #[tokio::test] async fn invalid_timeout_prevents_spawn() {let mut hook=handler("exit 0");hook.config.timeout=Some(0.0);assert!(run_command_hook(&hook,&json!({"event":"SessionStart"}),options(Path::new("/tmp"))).await.is_err());}
    #[tokio::test] async fn timeout_kills_process_group()->std::io::Result<()> {let mut hook=handler("while :; do :; done");hook.config.timeout=Some(0.02);let result=run_command_hook(&hook,&json!({"event":"SessionStart"}),options(Path::new("/tmp"))).await?;assert!(result.timed_out);assert_eq!(result.exit_code,None);Ok(())}
    #[tokio::test] async fn already_aborted_never_spawns()->std::io::Result<()> {let signal=AbortSignal::default();signal.abort();let mut opts=options(Path::new("/tmp"));opts.signal=Some(&signal);let result=run_command_hook(&handler("exit 9"),&json!({"event":"SessionStart"}),opts).await?;assert!(result.aborted);assert_eq!(result.exit_code,None);Ok(())}
    #[tokio::test] async fn capture_caps_before_concatenation()->std::io::Result<()> {let captured=capture_stream(&vec![b'x';4096][..],128).await?;assert_eq!(captured.total_bytes,4096);assert_eq!(captured.kept_bytes,128);assert_eq!(captured.text.len(),128);Ok(())}
    #[test] fn windows_command_selection() {let mut hook=handler("posix");hook.config.command_windows=Some("windows".to_owned());assert_eq!(select_hook_command_for_platform(&hook,"win32"),"windows");assert_eq!(select_hook_command_for_platform(&hook,"darwin"),"posix");}
}
