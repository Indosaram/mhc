//! Process probes backed by the live host process, for a session that runs scheduled-prompt passes
//! itself (the CLI's `NativeProbes` lives in the CLI crate; this one serves builtin extensions).
use super::{Result, ScheduleError, runner_lease::*, store::RunnerIdentity};
use std::sync::Arc;

pub type NowFn = Arc<dyn Fn() -> u64 + Send + Sync>;

pub struct NativeProbes {
    owner: RunnerIdentity,
    boot: u64,
    now: NowFn,
}
impl NativeProbes {
    pub fn new(now: NowFn) -> Result<Self> {
        let (boot, start) = native_identity().map_err(ScheduleError::Invalid)?;
        Ok(Self {
            owner: RunnerIdentity {
                pid: std::process::id(),
                process_started_at_ms: start,
            },
            boot,
            now,
        })
    }
}
#[async_trait::async_trait]
impl ProcessProbes for NativeProbes {
    fn own_identity(&self) -> RunnerIdentity {
        self.owner.clone()
    }
    fn boot_at_ms(&self) -> u64 {
        self.boot
    }
    fn now(&self) -> u64 {
        (self.now)()
    }
    fn pid_alive(&self, pid: u32) -> Result<bool> {
        Ok(native_pid_alive(pid))
    }
    fn delivery_group_alive(&self, pid: u32) -> Result<bool> {
        #[cfg(unix)]
        {
            let pid =
                i32::try_from(pid).map_err(|error| ScheduleError::Invalid(error.to_string()))?;
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), None) {
                Ok(()) | Err(nix::errno::Errno::EPERM) => Ok(true),
                Err(nix::errno::Errno::ESRCH) => Ok(false),
                Err(error) => Err(ScheduleError::Invalid(error.to_string())),
            }
        }
        #[cfg(not(unix))]
        {
            Ok(native_pid_alive(pid))
        }
    }
    async fn process_start_ms(&self, pid: u32) -> Result<Option<u64>> {
        if pid == self.owner.pid {
            return Ok(Some(self.owner.process_started_at_ms));
        }
        let boot = self.boot;
        tokio::task::spawn_blocking(move || read_process_start_ms(pid, boot))
            .await
            .map_err(|error| ScheduleError::Invalid(error.to_string()))?
    }
}
impl crate::session_holder_records::SessionHolderProbes for NativeProbes {}
pub(crate) fn read_process_start_ms(pid: u32, boot: u64) -> Result<Option<u64>> {
    #[cfg(target_os = "linux")]
    {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let ticks = stat
            .rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .and_then(|value| value.parse::<u64>().ok());
        let hz = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
            .map_err(|error| ScheduleError::Invalid(error.to_string()))?
            .and_then(|hz| u64::try_from(hz).ok())
            .filter(|hz| *hz > 0)
            .ok_or_else(|| ScheduleError::Invalid("cannot read clock ticks".into()))?;
        Ok(ticks.map(|ticks| boot + ticks * 1000 / hz))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = boot;
        let run = |program: &str, args: &[&str]| -> Option<String> {
            let output = std::process::Command::new(program)
                .args(args)
                .env("LC_ALL", "C")
                .env("LANG", "C")
                .output()
                .ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        };
        let Some(text) = run("ps", &["-o", "lstart=", "-p", &pid.to_string()]) else {
            return Ok(None);
        };
        Ok(run("date", &["-j", "-f", "%a %b %e %T %Y", &text, "+%s"])
            .and_then(|seconds| seconds.parse::<u64>().ok())
            .map(|seconds| seconds * 1000))
    }
    #[cfg(windows)]
    {
        let _ = boot;
        let script = format!(
            "[DateTimeOffset](Get-Process -Id {pid} -ErrorAction Stop).StartTime | ForEach-Object {{$_.ToUnixTimeMilliseconds()}}"
        );
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
            .flatten())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (pid, boot);
        Ok(None)
    }
}
