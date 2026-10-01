use std::{io,process::{Command,Stdio}};
use comment_checker_core::*;
struct Process { child:tokio::process::Child,input:String }
impl SpawnProcess for Process {
    fn write_stdin(&mut self,input:&str)->io::Result<()> { self.input.push_str(input);Ok(()) }
    fn end_stdin(&mut self)->io::Result<()> { Ok(()) }
    fn take_stdout(&mut self)->ByteStream { match self.child.stdout.take() { Some(stdout)=>Box::pin(stdout),None=>Box::pin(tokio::io::empty()) } }
    fn take_stderr(&mut self)->ByteStream { match self.child.stderr.take() { Some(stderr)=>Box::pin(stderr),None=>Box::pin(tokio::io::empty()) } }
    fn exited(&mut self)->ExitFuture<'_> { Box::pin(async { use tokio::io::AsyncWriteExt; if let Some(mut stdin)=self.child.stdin.take() { stdin.write_all(self.input.as_bytes()).await?;stdin.shutdown().await?; } Ok(self.child.wait().await?.code().unwrap_or(1)) }) }
    fn kill(&mut self,signal:SpawnSignal)->io::Result<()> {
        let Some(pid)=self.child.id() else { return Ok(()); };
        let signal=match signal { SpawnSignal::Sigterm=>"-TERM",SpawnSignal::Sigkill=>"-KILL" };
        let status=Command::new("kill").args([signal,&pid.to_string()]).status()?;
        if status.success() { Ok(()) } else { Err(io::Error::other("failed to signal checker")) }
    }
}
fn spawn(args:&[String])->io::Result<Box<dyn SpawnProcess>> {
    let Some((command,args))=args.split_first() else { return Err(io::Error::other("comment-checker command is required")); };
    let child=tokio::process::Command::new(command).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn()?;
    Ok(Box::new(Process{child,input:String::new()}))
}
pub async fn default_run_comment_checker(input:&RunCommentCheckerInput)->io::Result<CheckResult> {
    run_comment_checker(input,&RunCommentCheckerOptions{spawn:&spawn,exists_sync:&|p|std::path::Path::new(p).exists(),timeout_ms:None,kill_grace_ms:None}).await
}
