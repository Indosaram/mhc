use std::io;
use std::process::Stdio;

use comment_checker_core::*;
use tokio::io::AsyncWriteExt;

struct Process {
    child: tokio::process::Child,
    input: String,
}

impl SpawnProcess for Process {
    fn write_stdin(&mut self, input: &str) -> io::Result<()> {
        self.input.push_str(input);
        Ok(())
    }

    fn end_stdin(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn take_stdout(&mut self) -> ByteStream {
        match self.child.stdout.take() {
            Some(stdout) => Box::pin(stdout),
            None => Box::pin(tokio::io::empty()),
        }
    }

    fn take_stderr(&mut self) -> ByteStream {
        match self.child.stderr.take() {
            Some(stderr) => Box::pin(stderr),
            None => Box::pin(tokio::io::empty()),
        }
    }

    fn exited(&mut self) -> ExitFuture<'_> {
        Box::pin(async {
            if let Some(mut stdin) = self.child.stdin.take() {
                let _ = stdin.write_all(self.input.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
            let status = self.child.wait().await?;
            Ok(status.code().unwrap_or(1))
        })
    }

    fn kill(&mut self, signal: SpawnSignal) -> io::Result<()> {
        let Some(pid) = self.child.id() else {
            return Ok(());
        };
        let signal = match signal {
            SpawnSignal::Sigterm => "-TERM",
            SpawnSignal::Sigkill => "-KILL",
        };
        #[cfg(unix)]
        let sent = {
            let sig = if signal == "-TERM" {
                rustix::process::Signal::TERM
            } else {
                rustix::process::Signal::KILL
            };
            i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
                .is_some_and(|pid| rustix::process::kill_process(pid, sig).is_ok())
        };
        #[cfg(not(unix))]
        let sent = std::process::Command::new("kill")
            .args([signal, &pid.to_string()])
            .status()?
            .success();
        if sent {
            Ok(())
        } else {
            Err(io::Error::other("failed to signal checker"))
        }
    }
}

fn spawn(args: &[String]) -> io::Result<Box<dyn SpawnProcess>> {
    let Some((command, args)) = args.split_first() else {
        return Err(io::Error::other("comment-checker command is required"));
    };
    let child = tokio::process::Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    Ok(Box::new(Process {
        child,
        input: String::new(),
    }))
}

pub async fn default_run_comment_checker(
    input: &RunCommentCheckerInput,
) -> io::Result<CheckResult> {
    run_comment_checker(
        input,
        &RunCommentCheckerOptions {
            spawn: &spawn,
            exists_sync: &|p| std::path::Path::new(p).exists(),
            timeout_ms: None,
            kill_grace_ms: None,
        },
    )
    .await
}
