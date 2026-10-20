use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const PROCESS_CRASH_RECORD_LIMIT: usize = 50;
pub const PROCESS_MARKER_HEARTBEAT_MS: u64 = 60_000;

fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file(path: &Path, append: bool) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn millis(time: SystemTime) -> io::Result<u64> {
    u64::try_from(
        time.duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis(),
    )
    .map_err(io::Error::other)
}

fn crash_record(raw: &str, heartbeat: SystemTime) -> Option<Value> {
    let marker: Value = serde_json::from_str(raw).ok()?;
    let kind = marker.get("kind")?.as_str()?;
    let started = marker.get("startedAt")?.as_u64()?;
    let version = marker.get("senpiVersion")?.as_str()?;
    let at = millis(heartbeat).ok()?;
    let timestamp = maho_ext_goal::format::iso_timestamp(at / 1000)?;
    let timestamp = timestamp.replace(".000Z", &format!(".{:03}Z", at % 1000));
    let mut record = json!({"at":timestamp,"kind":kind,"detection":"unclean_exit","uptimeMs":at.saturating_sub(started),"senpiVersion":version});
    for field in ["bunVersion", "productVersion"] {
        if let Some(value) = marker.get(field).and_then(Value::as_str) {
            record[field] = json!(value);
        }
    }
    Some(record)
}

fn append_record(dir: &Path, record: &Value) -> io::Result<()> {
    private_dir(dir)?;
    let path = dir.join("crashes.jsonl");
    writeln!(private_file(&path, true)?, "{record}")?;
    let raw = fs::read_to_string(&path)?;
    let lines: Vec<_> = raw.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.len() > PROCESS_CRASH_RECORD_LIMIT {
        writeln!(
            private_file(&path, false)?,
            "{}",
            lines[lines.len() - PROCESS_CRASH_RECORD_LIMIT..].join("\n")
        )?;
    }
    Ok(())
}

pub fn sweep_dead_process_markers(
    agent: &Path,
    self_pid: u32,
    is_alive: impl Fn(u32) -> bool,
) -> usize {
    let dir = agent.join("process-crashes");
    let live = dir.join("live");
    let entries = match fs::read_dir(&live) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return 0,
        Err(error) => {
            report_error(&error);
            return 0;
        }
    };
    let mut count = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report_error(&error);
                continue;
            }
        };
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|pid| pid.parse::<u32>().ok())
            .filter(|pid| *pid != 0 && *pid != self_pid)
        else {
            continue;
        };
        if is_alive(pid) {
            continue;
        }
        let claimed = live.join(format!("{}.{}.sweep", name.to_string_lossy(), self_pid));
        match fs::rename(entry.path(), &claimed) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                report_error(&error);
                continue;
            }
        }
        let result = (|| -> io::Result<bool> {
            let raw = fs::read_to_string(&claimed)?;
            let heartbeat = fs::metadata(&claimed)?.modified()?;
            let Some(record) = crash_record(&raw, heartbeat) else {
                return Ok(false);
            };
            append_record(&dir, &record)?;
            Ok(true)
        })();
        match result {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(error) => report_error(&error),
        }
        if let Err(error) = fs::remove_file(&claimed) {
            report_error(&error);
        }
    }
    count
}

fn report_error(error: &io::Error) {
    let _ = writeln!(io::stderr(), "Process crash evidence: {error}");
}

fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Ok(()) | Err(nix::errno::Errno::EPERM) => true,
            Err(nix::errno::Errno::ESRCH) => false,
            Err(error) => {
                report_error(&io::Error::other(error));
                true
            }
        }
    }
    #[cfg(windows)]
    {
        match std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
        {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                    line.split(',')
                        .nth(1)
                        .is_some_and(|value| value.trim_matches('"') == pid.to_string())
                })
            }
            Ok(_) => true,
            Err(error) => {
                report_error(&error);
                true
            }
        }
    }
}

pub struct ProcessLifetime {
    marker: PathBuf,
    heartbeat: tokio::task::JoinHandle<()>,
}

impl Drop for ProcessLifetime {
    fn drop(&mut self) {
        self.heartbeat.abort();
        if let Err(error) = fs::remove_file(&self.marker) {
            if error.kind() != io::ErrorKind::NotFound {
                report_error(&error);
            }
        }
    }
}

pub fn record_process_lifetime(
    agent: &Path,
    kind: &str,
    supervised: bool,
) -> Option<ProcessLifetime> {
    sweep_dead_process_markers(agent, std::process::id(), is_alive);
    if supervised {
        return None;
    }
    let live = agent.join("process-crashes/live");
    let marker = live.join(format!("{}.json", std::process::id()));
    let result = (|| -> io::Result<()> {
        private_dir(&agent.join("process-crashes"))?;
        private_dir(&live)?;
        let version = env!("CARGO_PKG_VERSION");
        let product =
            maho_core::config::display_version(option_env!("MHC_BUILD_VERSION").unwrap_or(version));
        let mut value = json!({"kind":kind,"pid":std::process::id(),"startedAt":millis(SystemTime::now())?,"senpiVersion":version});
        if product != version {
            value["productVersion"] = json!(product);
        }
        writeln!(private_file(&marker, false)?, "{value}")
    })();
    if let Err(error) = result {
        report_error(&error);
        return None;
    }
    let file = marker.clone();
    let heartbeat = tokio::spawn(async move {
        let period = Duration::from_millis(PROCESS_MARKER_HEARTBEAT_MS);
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        loop {
            ticks.tick().await;
            let result = fs::OpenOptions::new()
                .write(true)
                .open(&file)
                .and_then(|file| file.set_modified(SystemTime::now()));
            if let Err(error) = result {
                report_error(&error);
            }
        }
    });
    Some(ProcessLifetime { marker, heartbeat })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sweeps_dead_peers_once_and_bounds_records() {
        let dir = tempfile::tempdir().expect("directory");
        let live = dir.path().join("process-crashes/live");
        private_dir(&live).expect("live directory");
        for pid in 1..=55 {
            fs::write(
                live.join(format!("{pid}.json")),
                json!({"kind":"print","startedAt":0,"senpiVersion":"test"}).to_string(),
            )
            .expect("marker");
        }
        assert_eq!(
            sweep_dead_process_markers(dir.path(), 100, |pid| pid == 55),
            54
        );
        assert_eq!(
            sweep_dead_process_markers(dir.path(), 100, |pid| pid == 55),
            0
        );
        assert!(live.join("55.json").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("process-crashes/crashes.jsonl"))
                .expect("records")
                .lines()
                .count(),
            PROCESS_CRASH_RECORD_LIMIT
        );
    }
    #[tokio::test(start_paused = true)]
    async fn clean_disposal_removes_marker_and_supervised_process_never_creates_one() {
        let dir = tempfile::tempdir().expect("directory");
        let lifetime = record_process_lifetime(dir.path(), "print", false).expect("lifetime");
        let path = lifetime.marker.clone();
        assert!(path.exists());
        drop(lifetime);
        assert!(!path.exists());
        assert!(record_process_lifetime(dir.path(), "rpc", true).is_none());
        assert!(!path.exists());
    }
    #[test]
    fn record_uses_last_heartbeat_and_runtime_identity() {
        let raw = json!({"kind":"json","startedAt":1000,"senpiVersion":"engine","productVersion":"product"}).to_string();
        let record = crash_record(&raw, UNIX_EPOCH + Duration::from_millis(2123)).expect("record");
        assert_eq!(record["at"], "1970-01-01T00:00:02.123Z");
        assert_eq!(record["uptimeMs"], 1123);
        assert_eq!(record["productVersion"], "product");
        assert_eq!(record["detection"], "unclean_exit");
    }
}
