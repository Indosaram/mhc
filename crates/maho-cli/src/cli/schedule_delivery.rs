use maho_core::schedule::{Result, ScheduleError, runner_lease::*, types::ScheduledJob};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledPromptEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: String,
    pub session_id: String,
    pub session_file: Option<String>,
    pub cwd: String,
    pub prompt: String,
    pub message: String,
    pub due_at: u64,
    pub fired_at: u64,
    pub every_ms: Option<u64>,
    pub fire_count: u64,
}
#[async_trait::async_trait]
pub trait Delivery: Sync {
    async fn should_defer(
        &self,
        job: &ScheduledJob,
        probes: &dyn ProcessProbes,
    ) -> Result<Option<String>>;
    async fn deliver(
        &self,
        event: &ScheduledPromptEvent,
        lock: &mut DeliveryLock,
        probes: &dyn ProcessProbes,
        timeout_ms: u64,
    ) -> std::result::Result<(), String>;
}
pub struct ProcessDelivery {
    pub exec: Option<String>,
    pub executable: PathBuf,
}

pub fn moved_job_paths(job: &ScheduledJob) -> ScheduledJob {
    ScheduledJob {
        session_file: job.session_file.as_deref().map(maho_core::session_manager::resolve_moved_session_path),
        cwd: maho_core::session_manager::resolve_moved_session_path(&job.cwd),
        ..job.clone()
    }
}

#[async_trait::async_trait]
impl Delivery for ProcessDelivery {
    async fn should_defer(
        &self,
        job: &ScheduledJob,
        probes: &dyn ProcessProbes,
    ) -> Result<Option<String>> {
        if self.exec.is_some() {
            return Ok(None);
        }
        let Some(file) = &job.session_file else {
            return Ok(None);
        };
        if !Path::new(file).try_exists()? {
            return Ok(None);
        }
        let pids = live_session_holder_pids(Path::new(file), &job.session_id, probes).await?;
        if pids.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "session is open in pid {}; waiting until it closes",
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
    async fn deliver(
        &self,
        event: &ScheduledPromptEvent,
        lock: &mut DeliveryLock,
        probes: &dyn ProcessProbes,
        timeout_ms: u64,
    ) -> std::result::Result<(), String> {
        self.run(event, lock, probes, timeout_ms)
            .await
            .map_err(|error| error.to_string())
    }
}
impl ProcessDelivery {
    async fn run(
        &self,
        event: &ScheduledPromptEvent,
        lock: &mut DeliveryLock,
        probes: &dyn ProcessProbes,
        timeout_ms: u64,
    ) -> Result<()> {
        let (program, args, cwd, input) = if let Some(exec) = &self.exec {
            #[cfg(unix)]
            let shell = (PathBuf::from("/bin/sh"), vec!["-c".into(), exec.clone()]);
            #[cfg(windows)]
            let shell = (
                std::env::var_os("ComSpec")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("cmd.exe")),
                vec!["/d".into(), "/s".into(), "/c".into(), exec.clone()],
            );
            (
                shell.0,
                shell.1,
                std::env::current_dir()?,
                format!("{}\n", serde_json::to_string(event)?),
            )
        } else {
            if !Path::new(&event.cwd).try_exists()? {
                return Err(ScheduleError::Invalid(format!(
                    "working directory no longer exists: {}",
                    event.cwd
                )));
            }
            if let Some(file) = &event.session_file
                && !Path::new(file).try_exists()?
            {
                return Err(ScheduleError::Invalid(format!(
                    "session file no longer exists: {file}"
                )));
            }
            (
                self.executable.clone(),
                vec![
                    "-p".into(),
                    "--session".into(),
                    event
                        .session_file
                        .clone()
                        .unwrap_or_else(|| event.session_id.clone()),
                    event.message.clone(),
                ],
                PathBuf::from(&event.cwd),
                String::new(),
            )
        };
        #[cfg(unix)]
        let mut command = {
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .args(["-c", "IFS= read -r _ || exit 75; exec \"$0\" \"$@\""])
                .arg(&program)
                .args(&args);
            command.process_group(0);
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = tokio::process::Command::new(&program);
            if let Some(exec) = &self.exec {
                command.raw_arg(format!("/d /s /c \"{exec}\""));
            } else {
                command.args(&args);
            }
            command
        };
        command
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("SENPI_SCHEDULE_ID", &event.id)
            .env("SENPI_SCHEDULE_SESSION_ID", &event.session_id)
            .env(
                "SENPI_SCHEDULE_SESSION_FILE",
                event.session_file.as_deref().unwrap_or(""),
            )
            .env("SENPI_SCHEDULE_CWD", &event.cwd);
        let mut child = command.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| ScheduleError::Invalid("delivery process has no pid".into()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| ScheduleError::Invalid("delivery stderr unavailable".into()))?;
        let collect = async move {
            let mut tail = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = stderr.read(&mut chunk).await?;
                if count == 0 {
                    break;
                }
                tail.extend_from_slice(&chunk[..count]);
                if tail.len() > 8000 {
                    tail.drain(..tail.len() - 8000);
                }
            }
            Ok::<_, std::io::Error>(
                String::from_utf8_lossy(&tail)
                    .chars()
                    .rev()
                    .take(2000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>(),
            )
        };
        let mut collected = tokio::spawn(collect);
        let delivery = async {
            if let Err(error) = lock.attach_delivery(pid, probes.now()) {
                kill_tree(pid).await?;
                child.wait().await?;
                return Err(error);
            }
            let work = async {
                let mut stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| ScheduleError::Invalid("delivery stdin unavailable".into()))?;
                #[cfg(unix)]
                stdin.write_all(b"go\n").await?;
                match stdin.write_all(input.as_bytes()).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
                    Err(error) => return Err(ScheduleError::Io(error)),
                }
                drop(stdin);
                Ok::<_, ScheduleError>(child.wait().await?)
            };
            let status = work.await?;
            let tail = (&mut collected)
                .await
                .map_err(|error| ScheduleError::Invalid(error.to_string()))??;
            Ok::<_, ScheduleError>((status, tail))
        };
        match tokio::time::timeout(Duration::from_millis(timeout_ms), delivery).await {
            Ok(Ok((status, _))) if status.success() => Ok(()),
            Ok(Ok((status, tail))) => {
                Err(ScheduleError::Invalid(format!("{status}: {}", tail.trim())))
            }
            Ok(Err(error)) => {
                kill_tree(pid).await?;
                child.wait().await?;
                collected.abort();
                Err(error)
            }
            Err(_) => {
                kill_tree(pid).await?;
                child.wait().await?;
                collected
                    .await
                    .map_err(|error| ScheduleError::Invalid(error.to_string()))??;
                Err(ScheduleError::Invalid(format!(
                    "delivery timed out after {}s",
                    (timeout_ms + 500) / 1000
                )))
            }
        }
    }
}
async fn kill_tree(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use nix::{
            sys::signal::{Signal, kill},
            unistd::Pid,
        };
        let pid = i32::try_from(pid).map_err(|error| ScheduleError::Invalid(error.to_string()))?;
        match kill(Pid::from_raw(-pid), Signal::SIGKILL) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
            Err(error) => return Err(ScheduleError::Invalid(error.to_string())),
        }
    }
    #[cfg(windows)]
    {
        let status = tokio::process::Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .await?;
        if !status.success() {
            return Err(ScheduleError::Invalid(format!("taskkill failed: {status}")));
        }
    }
    Ok(())
}
