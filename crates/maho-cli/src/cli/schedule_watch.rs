use super::{
    schedule_delivery::ProcessDelivery,
    schedule_runner::{RunDueOptions, RunDueResult, run_due_jobs},
};
use maho_core::schedule::{Result, ScheduleError, runner_lease::*, store::*};
use notify::Watcher;
use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub struct RunOptions {
    pub watch: bool,
    pub exec: Option<String>,
    pub poll_seconds: u64,
    pub timeout_seconds: u64,
    pub concurrency: usize,
}
pub fn write_line(value: &serde_json::Value) -> Result<()> {
    use std::io::Write;
    writeln!(std::io::stdout().lock(), "{value}")?;
    Ok(())
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}
pub struct NativeProbes {
    owner: RunnerIdentity,
    boot: u64,
}
impl NativeProbes {
    pub async fn new() -> Result<Self> {
        let pid = std::process::id();
        let start = read_process_start_ms(pid).await?.ok_or_else(|| {
            ScheduleError::Invalid("could not determine runner process start identity".into())
        })?;
        let boot = boot_at_ms().await?;
        Ok(Self {
            owner: RunnerIdentity {
                pid,
                process_started_at_ms: start / 1000 * 1000,
            },
            boot,
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
        now_ms()
    }
    fn pid_alive(&self, pid: u32) -> Result<bool> {
        is_alive(pid, false)
    }
    fn delivery_group_alive(&self, pid: u32) -> Result<bool> {
        is_alive(pid, cfg!(unix))
    }
    async fn process_start_ms(&self, pid: u32) -> Result<Option<u64>> {
        read_process_start_ms(pid).await
    }
}
fn is_alive(pid: u32, group: bool) -> Result<bool> {
    #[cfg(unix)]
    {
        use nix::{sys::signal::kill, unistd::Pid};
        let pid = i32::try_from(pid).map_err(|error| ScheduleError::Invalid(error.to_string()))?;
        match kill(Pid::from_raw(if group { -pid } else { pid }), None) {
            Ok(()) | Err(nix::errno::Errno::EPERM) => Ok(true),
            Err(nix::errno::Errno::ESRCH) => Ok(false),
            Err(error) => Err(ScheduleError::Invalid(error.to_string())),
        }
    }
    #[cfg(windows)]
    {
        let _ = group;
        Ok(maho_rpc::host_reservations::process_is_live(pid))
    }
}
#[cfg(any(target_os = "macos", windows))]
async fn probe_command(command: &str, args: &[String]) -> Result<Option<String>> {
    let mut command = tokio::process::Command::new(command);
    command
        .args(args)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|error| ScheduleError::Invalid(error.to_string()))??;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).trim().into()))
}
pub async fn read_process_start_ms(pid: u32) -> Result<Option<u64>> {
    #[cfg(target_os = "linux")]
    {
        let stat = match tokio::fs::read_to_string(format!("/proc/{pid}/stat")).await {
            Ok(stat) => stat,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            return Ok(None);
        };
        let Some(ticks) = fields
            .split_whitespace()
            .nth(19)
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return Ok(None);
        };
        Ok(Some(linux_boot_ms().await? + ticks * 10))
    }
    #[cfg(target_os = "macos")]
    {
        let Some(text) = probe_command(
            "ps",
            &["-o".into(), "lstart=".into(), "-p".into(), pid.to_string()],
        )
        .await?
        else {
            return Ok(None);
        };
        let date = parse_local_process_start(&text).await?;
        Ok(date)
    }
    #[cfg(windows)]
    {
        let script = format!(
            "[DateTimeOffset](Get-Process -Id {pid} -ErrorAction Stop).StartTime | ForEach-Object {{$_.ToUnixTimeMilliseconds()}}"
        );
        Ok(probe_command(
            "powershell",
            &[
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script,
            ],
        )
        .await?
        .and_then(|text| text.parse().ok()))
    }
}
#[cfg(target_os = "linux")]
async fn linux_boot_ms() -> Result<u64> {
    let stat = tokio::fs::read_to_string("/proc/stat").await?;
    stat.lines()
        .find_map(|line| {
            line.strip_prefix("btime ")
                .and_then(|value| value.parse::<u64>().ok())
        })
        .map(|seconds| seconds * 1000)
        .ok_or_else(|| ScheduleError::Invalid("/proc/stat has no boot instant".into()))
}
#[cfg(target_os = "macos")]
async fn parse_local_process_start(text: &str) -> Result<Option<u64>> {
    let args = [
        "-j".into(),
        "-f".into(),
        "%a %b %e %T %Y".into(),
        text.into(),
        "+%s".into(),
    ];
    Ok(probe_command("date", &args)
        .await?
        .and_then(|text| text.parse::<u64>().ok())
        .map(|seconds| seconds * 1000))
}
async fn boot_at_ms() -> Result<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_boot_ms().await
    }
    #[cfg(target_os = "macos")]
    {
        let text = probe_command("sysctl", &["-n".into(), "kern.boottime".into()])
            .await?
            .ok_or_else(|| ScheduleError::Invalid("cannot read boot instant".into()))?;
        let seconds = text
            .split("sec = ")
            .nth(1)
            .and_then(|tail| tail.split(',').next())
            .and_then(|seconds| seconds.trim().parse::<u64>().ok())
            .ok_or_else(|| ScheduleError::Invalid("invalid boot instant".into()))?;
        Ok(seconds * 1000)
    }
    #[cfg(windows)]
    {
        let script = "[DateTimeOffset](Get-CimInstance Win32_OperatingSystem).LastBootUpTime | ForEach-Object {$_.ToUnixTimeMilliseconds()}";
        probe_command(
            "powershell",
            &[
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script.into(),
            ],
        )
        .await?
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| ScheduleError::Invalid("cannot read boot instant".into()))
    }
}
#[derive(Default)]
struct Reporter {
    deferred: HashMap<String, String>,
    invalid: HashMap<String, String>,
}
impl Reporter {
    fn report(&mut self, result: &RunDueResult) -> Result<bool> {
        let mut ok = true;
        let mut still_invalid = std::collections::HashSet::new();
        for event in &result.events {
            let id = event["id"].as_str().unwrap_or("").to_owned();
            match event["event"].as_str() {
                Some("deferred") => {
                    let reason = event["reason"].as_str().unwrap_or("").to_owned();
                    if self.deferred.insert(id, reason.clone()).as_ref() == Some(&reason) {
                        continue;
                    }
                }
                Some("invalid") => {
                    let file = event["file"].as_str().unwrap_or("").to_owned();
                    still_invalid.insert(file.clone());
                    let error = event["error"].as_str().unwrap_or("").to_owned();
                    if self.invalid.insert(file, error.clone()).as_ref() == Some(&error) {
                        continue;
                    }
                }
                Some("fired") => {
                    self.deferred.remove(&id);
                    if event["outcome"] == "failed" {
                        ok = false;
                    }
                }
                Some("error") => ok = false,
                _ => {}
            }
            write_line(event)?;
        }
        self.invalid.retain(|file, _| still_invalid.contains(file));
        Ok(ok)
    }
}
async fn guarded_pass<D: super::schedule_delivery::Delivery + ?Sized>(options: &RunDueOptions<'_, D>) -> RunDueResult {
    match run_due_jobs(options).await {
        Ok(result) => result,
        Err(error) => RunDueResult {
            events: vec![serde_json::json!({"event":"error","error":error.to_string()})],
            next_due_at: None,
        },
    }
}
pub async fn run_passes(
    dir: &Path,
    options: RunOptions,
    probes: &dyn ProcessProbes,
) -> Result<i32> {
    let delivery = ProcessDelivery {
        exec: options.exec.clone(),
        executable: std::env::current_exe()?,
    };
    let started = probes.now();
    write_runner_lease(dir, probes, started, options.watch, options.exec.as_deref())?;
    let pass_options = RunDueOptions {
        dir,
        probes,
        delivery: &delivery,
        concurrency: options.concurrency,
        delivery_timeout_ms: options.timeout_seconds * 1000,
    };
    let heartbeat = async {
        let mut timer = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(RUNNER_HEARTBEAT_INTERVAL_MS),
            Duration::from_millis(RUNNER_HEARTBEAT_INTERVAL_MS),
        );
        let mut failing = false;
        loop {
            timer.tick().await;
            match write_runner_lease(dir, probes, started, options.watch, options.exec.as_deref()) {
                Ok(()) => failing = false,
                Err(error) => {
                    if !failing
                        && let Err(error) = write_line(
                            &serde_json::json!({"event":"lease_error","error":error.to_string()}),
                        )
                    {
                        break Err::<(), ScheduleError>(error);
                    }
                    failing = true;
                }
            }
        }
    };
    let run = async {
        let mut reporter = Reporter::default();
        if !options.watch {
            return Ok(if reporter.report(&guarded_pass(&pass_options).await)? {
                0
            } else {
                1
            });
        }
        ensure_dir(&dir.join("pending"))?;
        let (wake, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = wake.send(event);
        })
        .map_err(|error| ScheduleError::Invalid(error.to_string()))?;
        watcher
            .watch(&dir.join("pending"), notify::RecursiveMode::NonRecursive)
            .map_err(|error| ScheduleError::Invalid(error.to_string()))?;
        let stop = stop_signal()?;
        tokio::pin!(stop);
        write_line(
            &serde_json::json!({"event":"watching","pid":probes.own_identity().pid,"dir":dir,"pollSeconds":options.poll_seconds}),
        )?;
        loop {
            let result = guarded_pass(&pass_options).await;
            reporter.report(&result)?;
            let wait = options.poll_seconds.saturating_mul(1000).min(
                result
                    .next_due_at
                    .map_or(u64::MAX, |due| due.saturating_sub(probes.now())),
            );
            tokio::select! {
                signal = &mut stop => { signal?; break; },
                event = events.recv() => {
                    match event {
                        Some(Ok(_)) => {},
                        Some(Err(error)) => return Err(ScheduleError::Invalid(error.to_string())),
                        None => return Err(ScheduleError::Invalid("pending watcher closed".into())),
                    }
                    while let Ok(event) = events.try_recv() { event.map_err(|error| ScheduleError::Invalid(error.to_string()))?; }
                },
                () = tokio::time::sleep(Duration::from_millis(wait)) => {},
            }
        }
        write_line(&serde_json::json!({"event":"stopped","pid":probes.own_identity().pid}))?;
        Ok(0)
    };
    let result =
        tokio::select! { result = run => result, result = heartbeat => result.map(|()| 1) };
    remove_runner_lease(dir, &probes.own_identity())?;
    result
}
fn stop_signal() -> Result<std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>>>>> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        Ok(Box::pin(async move {
            tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {} }
            Ok(())
        }))
    }
    #[cfg(windows)]
    {
        Ok(Box::pin(async {
            tokio::signal::ctrl_c().await?;
            Ok(())
        }))
    }
}
