//! coding-agent/src/core/extensions/builtin/schedule/runner-lease.ts.
use super::{Result, ScheduleError, store::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::UNIX_EPOCH,
};

pub const RUNNER_HEARTBEAT_STALE_MS: u64 = 120_000;
pub const RUNNER_HEARTBEAT_INTERVAL_MS: u64 = 30_000;
pub const DEFAULT_MAX_DELIVERY_MS: u64 = 900_000;

#[async_trait::async_trait]
pub trait ProcessProbes: Sync {
    fn own_identity(&self) -> RunnerIdentity;
    fn boot_at_ms(&self) -> u64;
    fn now(&self) -> u64;
    fn pid_alive(&self, pid: u32) -> Result<bool>;
    fn delivery_group_alive(&self, pid: u32) -> Result<bool>;
    async fn process_start_ms(&self, pid: u32) -> Result<Option<u64>>;
}
pub fn same_process_start(left: u64, right: u64) -> bool {
    left.abs_diff(right) <= 3_000
}
fn same_boot(left: u64, right: u64) -> bool {
    left.abs_diff(right) <= 120_000
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveRunner {
    pub pid: u32,
    pub process_started_at_ms: u64,
    pub beat_at: u64,
    #[serde(default)]
    pub watch: bool,
    pub exec: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Holder {
    pub(crate) pid: u32,
    pub(crate) boot_at_ms: u64,
    pub(crate) process_started_at_ms: u64,
}

fn native_identity() -> std::result::Result<(u64, u64), String> {
    static IDENTITY: OnceLock<std::result::Result<(u64, u64), String>> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        #[cfg(target_os = "linux")]
        {
            let stat = fs::read_to_string("/proc/stat").map_err(|error| error.to_string())?;
            let boot = stat.lines().find_map(|line| line.strip_prefix("btime ").and_then(|value| value.parse::<u64>().ok()))
                .and_then(|seconds| seconds.checked_mul(1000)).ok_or("cannot read boot instant")?;
            let stat = fs::read_to_string("/proc/self/stat").map_err(|error| error.to_string())?;
            let ticks = stat.rsplit_once(") ").and_then(|(_, fields)| fields.split_whitespace().nth(19))
                .and_then(|ticks| ticks.parse::<u64>().ok()).ok_or("cannot read process start")?;
            let hz = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK).map_err(|error| error.to_string())?
                .and_then(|hz| u64::try_from(hz).ok()).filter(|hz| *hz > 0).ok_or("cannot read clock ticks")?;
            let start = ticks.checked_mul(1000).and_then(|ticks| boot.checked_add(ticks / hz)).ok_or("process start overflow")?;
            Ok((boot, start / 1000 * 1000))
        }
        #[cfg(target_os = "macos")]
        {
            let run = |program: &str, args: &[&str]| -> std::result::Result<String, String> {
                let output = std::process::Command::new(program).args(args).env("LC_ALL", "C").output().map_err(|error| error.to_string())?;
                if !output.status.success() { return Err(format!("{program}: {}", output.status)); }
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            };
            let boot = run("/usr/sbin/sysctl", &["-n", "kern.boottime"])?;
            let boot = boot.split("sec = ").nth(1).and_then(|tail| tail.split(',').next()).and_then(|value| value.trim().parse::<u64>().ok())
                .and_then(|seconds| seconds.checked_mul(1000)).ok_or("cannot read boot instant")?;
            let start = run("/bin/ps", &["-o", "lstart=", "-p", &std::process::id().to_string()])?;
            let start = run("/bin/date", &["-j", "-f", "%a %b %e %T %Y", &start, "+%s"])?;
            let start = start.parse::<u64>().ok().and_then(|seconds| seconds.checked_mul(1000)).ok_or("cannot read process start")?;
            Ok((boot, start))
        }
        #[cfg(windows)]
        {
            let script = format!("$b=[DateTimeOffset](Get-CimInstance Win32_OperatingSystem).LastBootUpTime; $p=[DateTimeOffset](Get-Process -Id {}).StartTime; Write-Output ($b.ToUnixTimeMilliseconds().ToString() + ',' + $p.ToUnixTimeMilliseconds().ToString())", std::process::id());
            let output = std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().map_err(|error| error.to_string())?;
            if !output.status.success() { return Err(format!("process identity: {}", output.status)); }
            let text = String::from_utf8_lossy(&output.stdout);
            let (boot, start) = text.trim().split_once(',').ok_or("invalid process identity")?;
            Ok((boot.parse::<u64>().map_err(|error| error.to_string())?, start.parse::<u64>().map_err(|error| error.to_string())? / 1000 * 1000))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        { Err("session holder identity is unsupported on this platform".into()) }
    }).clone()
}

#[derive(Debug)]
struct HolderRecord {
    path: PathBuf,
    mine: String,
}
fn holder_records() -> &'static Mutex<HashMap<PathBuf, Weak<HolderRecord>>> {
    static RECORDS: OnceLock<Mutex<HashMap<PathBuf, Weak<HolderRecord>>>> = OnceLock::new();
    RECORDS.get_or_init(|| Mutex::new(HashMap::new()))
}
impl Drop for HolderRecord {
    fn drop(&mut self) {
        let _records = holder_records()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = (|| -> Result<()> {
            if read_lease_text(&self.path)?.as_deref() == Some(self.mine.as_str()) {
                unlink_if_present(&self.path)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!(
                "cannot release session holder {}: {error}",
                self.path.display()
            );
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SessionHold {
    _record: Arc<HolderRecord>,
}

/// The host endpoint a holder record names (senpi `SessionHolderEndpoint`), so a foreign process can
/// tell which host generation holds the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionHolderEndpoint {
    pub host_instance: Option<String>,
    pub host_socket: Option<String>,
    pub host_daemon_dir: Option<String>,
}

pub(crate) fn hold_session_file(
    session_file: &Path,
    session_id: &str,
    cwd: &str,
    expect_existing: bool,
) -> Result<SessionHold> {
    hold_session_file_with_endpoint(session_file, session_id, cwd, expect_existing, None)
}

pub(crate) fn hold_session_file_with_endpoint(
    session_file: &Path,
    session_id: &str,
    cwd: &str,
    expect_existing: bool,
    endpoint: Option<&SessionHolderEndpoint>,
) -> Result<SessionHold> {
    let (boot, start) = native_identity().map_err(ScheduleError::Invalid)?;
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ScheduleError::Invalid(error.to_string()))?
            .as_millis(),
    )
    .map_err(|error| ScheduleError::Invalid(error.to_string()))?;
    hold_session_file_with_identity(
        session_file,
        session_id,
        cwd,
        expect_existing,
        Holder {
            pid: std::process::id(),
            boot_at_ms: boot,
            process_started_at_ms: start,
        },
        now,
        endpoint,
    )
}

fn hold_session_file_with_identity(
    session_file: &Path,
    session_id: &str,
    cwd: &str,
    expect_existing: bool,
    identity: Holder,
    now: u64,
    endpoint: Option<&SessionHolderEndpoint>,
) -> Result<SessionHold> {
    let directory = session_holders_dir(session_file, session_id)?;
    ensure_dir(&directory)?;
    let admission = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(directory.join("move-admission"))?;
    admission.try_lock_shared().map_err(|error| ScheduleError::Invalid(format!("session is being moved: {error}")))?;
    let path = directory.join(format!("{}.json", identity.pid));
    let mut records = holder_records()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    records.retain(|_, record| record.strong_count() > 0);
    let record = match records.get(&path).and_then(Weak::upgrade) {
        Some(record) => record,
        None => {
            ensure_dir(&directory)?;
            let mut record_json = serde_json::json!({"pid":identity.pid,"bootAtMs":identity.boot_at_ms,"processStartedAtMs":identity.process_started_at_ms,"cwd":cwd,"token":uuid::Uuid::new_v4().to_string()});
            if let (Some(endpoint), Some(object)) = (endpoint, record_json.as_object_mut()) {
                for (key, value) in [
                    ("host_instance", &endpoint.host_instance),
                    ("host_socket", &endpoint.host_socket),
                    ("host_daemon_dir", &endpoint.host_daemon_dir),
                ] {
                    if let Some(value) = value {
                        object.insert(key.to_owned(), serde_json::Value::String(value.clone()));
                    }
                }
            }
            let mine = serde_json::to_string(&record_json)?;
            write_atomic(&path, &mine)?;
            let record = Arc::new(HolderRecord {
                path: path.clone(),
                mine,
            });
            records.insert(path, Arc::downgrade(&record));
            record
        }
    };
    drop(records);
    let hold = SessionHold { _record: record };
    if let Some(raw) = read_lease_text(&directory.join("move.lock"))? {
        let _age = lock_age(&directory.join("move.lock"), now)?;
        let pid = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|value| value["pid"].as_u64())
            .and_then(|pid| u32::try_from(pid).ok());
        if pid.is_none_or(native_pid_alive) {
            return Err(ScheduleError::Invalid("session is being moved".into()));
        }
    }
    if expect_existing && !session_file.try_exists()? {
        return Err(ScheduleError::Invalid(format!(
            "session file was moved: {}",
            session_file.display()
        )));
    }
    Ok(hold)
}

pub(crate) fn native_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Ok(()) | Err(nix::errno::Errno::EPERM) => true,
            Err(nix::errno::Errno::ESRCH) => false,
            Err(_) => true,
        }
    }
    #[cfg(windows)]
    {
        std::process::Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{exit 0}} else {{exit 1}}")]).status().map_or(true, |status| status.success())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

pub(crate) fn session_holders_dir(session_file: &Path, session_id: &str) -> Result<PathBuf> {
    let resolved = super::moved_paths::resolve(&session_file.to_string_lossy());
    let parent = Path::new(&resolved)
        .parent()
        .ok_or_else(|| ScheduleError::Invalid("session file has no parent".into()))?;
    Ok(parent
        .join("session-holders")
        .join(encode_session_id(session_id)))
}
pub(crate) enum LockState {
    Free,
    /// `unproven`: the holder pid is alive but its start time could not be read, so a caller with
    /// positive evidence that the holder's host generation ended may still reclaim it.
    Held(Option<Holder>, bool),
    Stale(String),
}
pub fn read_lease_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn temp_path(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(
        ".{}.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4(),
        extension
    ));
    PathBuf::from(name)
}
fn publish_exclusive(path: &Path, text: &str) -> Result<()> {
    let temp = temp_path(path, "tmp");
    let result = (|| -> Result<()> {
        private_file(&temp)?.write_all(text.as_bytes())?;
        match fs::hard_link(&temp, path) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                private_file(path)?.write_all(text.as_bytes())?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    })();
    unlink_if_present(&temp)?;
    result
}
fn lock_age(path: &Path, now: u64) -> Result<u64> {
    match fs::metadata(path) {
        Ok(info) => {
            let modified = info
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_err(|error| ScheduleError::Invalid(error.to_string()))?;
            let ms = u64::try_from(modified.as_millis())
                .map_err(|error| ScheduleError::Invalid(error.to_string()))?;
            Ok(now.saturating_sub(ms))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}
pub(crate) async fn reclaim_lock_state(
    path: &Path,
    probes: &dyn ProcessProbes,
) -> Result<LockState> {
    let Some(raw) = read_lease_text(path)? else {
        return Ok(LockState::Free);
    };
    let holder: Holder = match serde_json::from_str(&raw) {
        Ok(holder) => holder,
        Err(_) => {
            return Ok(if lock_age(path, probes.now())? > 30_000 {
                LockState::Stale(raw)
            } else {
                LockState::Held(None, false)
            });
        }
    };
    if !same_boot(holder.boot_at_ms, probes.boot_at_ms()) {
        return Ok(LockState::Stale(raw));
    }
    let own = probes.own_identity();
    if holder.pid == own.pid {
        return Ok(
            if same_process_start(holder.process_started_at_ms, own.process_started_at_ms) {
                LockState::Held(Some(holder), false)
            } else {
                LockState::Stale(raw)
            },
        );
    }
    if !probes.pid_alive(holder.pid)? {
        return Ok(LockState::Stale(raw));
    }
    match probes.process_start_ms(holder.pid).await? {
        Some(observed) if !same_process_start(holder.process_started_at_ms, observed) => {
            Ok(LockState::Stale(raw))
        }
        None => Ok(LockState::Held(Some(holder), true)),
        Some(_) => Ok(LockState::Held(Some(holder), false)),
    }
}
pub(crate) fn break_stale_lock(path: &Path, inspected: &str) -> Result<()> {
    let aside = temp_path(path, "broken");
    match fs::rename(path, &aside) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let moved = fs::read_to_string(&aside)?;
    if moved != inspected {
        match publish_exclusive(path, &moved) {
            Ok(()) => {}
            Err(ScheduleError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    unlink_if_present(&aside)
}
pub fn write_runner_lease(
    dir: &Path,
    probes: &dyn ProcessProbes,
    started_at: u64,
    watch: bool,
    exec: Option<&str>,
) -> Result<()> {
    ensure_dir(&runners_dir(dir))?;
    let owner = probes.own_identity();
    let value = serde_json::json!({"pid":owner.pid,"processStartedAtMs":owner.process_started_at_ms,
        "bootAtMs":probes.boot_at_ms(),"beatAt":probes.now(),"startedAt":started_at,"watch":watch,"exec":exec});
    write_atomic(
        &runners_dir(dir).join(format!("{}.json", owner.pid)),
        &serde_json::to_string(&value)?,
    )
}
pub fn remove_runner_lease(dir: &Path, owner: &RunnerIdentity) -> Result<()> {
    unlink_if_present(&runners_dir(dir).join(format!("{}.json", owner.pid)))
}
pub async fn live_runners(dir: &Path, probes: &dyn ProcessProbes) -> Result<Vec<LiveRunner>> {
    let mut runners = Vec::new();
    for name in names(&runners_dir(dir))? {
        if !name
            .strip_suffix(".json")
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let path = runners_dir(dir).join(name);
        match reclaim_lock_state(&path, probes).await? {
            LockState::Stale(raw) => break_stale_lock(&path, &raw)?,
            LockState::Held(Some(holder), _) => {
                if let Some(raw) = read_lease_text(&path)?
                    && let Ok(mut runner) = serde_json::from_str::<LiveRunner>(&raw)
                {
                    runner.pid = holder.pid;
                    runner.process_started_at_ms = holder.process_started_at_ms;
                    runners.push(runner);
                }
            }
            LockState::Free | LockState::Held(None, _) => {}
        }
    }
    Ok(runners)
}
pub async fn is_watch_runner_available(dir: &Path, probes: &dyn ProcessProbes) -> Result<bool> {
    Ok(live_runners(dir, probes).await?.iter().any(|runner| {
        runner.watch && probes.now().saturating_sub(runner.beat_at) <= RUNNER_HEARTBEAT_STALE_MS
    }))
}
pub fn is_owner_among(owner: &RunnerIdentity, runners: &[LiveRunner]) -> bool {
    runners.iter().any(|runner| {
        runner.pid == owner.pid
            && same_process_start(runner.process_started_at_ms, owner.process_started_at_ms)
    })
}
pub fn encode_session_id(id: &str) -> String {
    let mut encoded = String::new();
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
pub enum SessionDeliveryLock {
    Acquired(DeliveryLock),
    Held { held_by_pid: Option<u32> },
}
pub struct DeliveryLock {
    path: PathBuf,
    mine: String,
    identity: serde_json::Value,
}
impl DeliveryLock {
    pub fn attach_delivery(&mut self, pid: u32, now: u64) -> Result<()> {
        if read_lease_text(&self.path)?.as_deref() != Some(&self.mine) {
            return Err(ScheduleError::Invalid(
                "session delivery lock was lost".into(),
            ));
        }
        let mut next = self.identity.clone();
        next["deliveryPid"] = pid.into();
        next["deliveryStartedAtMs"] = (now / 1000 * 1000).into();
        let next = serde_json::to_string(&next)?;
        write_atomic(&self.path, &next)?;
        self.mine = next;
        Ok(())
    }
    pub fn release(self) -> Result<()> {
        if read_lease_text(&self.path)?.as_deref() == Some(&self.mine) {
            unlink_if_present(&self.path)?;
        }
        Ok(())
    }
}
async fn orphaned_delivery(raw: &str, age: u64, probes: &dyn ProcessProbes) -> Result<Option<u32>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    if !value["bootAtMs"]
        .as_u64()
        .is_some_and(|boot| same_boot(boot, probes.boot_at_ms()))
    {
        return Ok(None);
    }
    let pid = value["deliveryPid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok());
    let start = value["deliveryStartedAtMs"].as_u64();
    let (Some(pid), Some(start)) = (pid, start) else {
        return Ok(
            if value["gated"] == false
                && value["maxDeliveryMs"]
                    .as_u64()
                    .is_some_and(|max| age <= max)
            {
                value["pid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
            } else {
                None
            },
        );
    };
    if !probes.delivery_group_alive(pid)? {
        return Ok(None);
    }
    if !probes.pid_alive(pid)? {
        return Ok(Some(pid));
    }
    Ok(probes
        .process_start_ms(pid)
        .await?
        .filter(|observed| same_process_start(start, *observed))
        .map(|_| pid))
}
pub async fn acquire_session_delivery_lock(
    dir: &Path,
    session_id: &str,
    probes: &dyn ProcessProbes,
    gated: bool,
    max_delivery_ms: u64,
) -> Result<SessionDeliveryLock> {
    let path = dir
        .join("sessions")
        .join(format!("{}.lock", encode_session_id(session_id)));
    let owner = probes.own_identity();
    let identity = serde_json::json!({"pid":owner.pid,"bootAtMs":probes.boot_at_ms(),
        "processStartedAtMs":owner.process_started_at_ms,"gated":gated,"maxDeliveryMs":max_delivery_ms});
    let mine = serde_json::to_string(&identity)?;
    for _ in 0..3 {
        match publish_exclusive(&path, &mine) {
            Ok(()) => {
                return Ok(SessionDeliveryLock::Acquired(DeliveryLock {
                    path,
                    mine,
                    identity,
                }));
            }
            Err(ScheduleError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure_dir(&dir.join("sessions"))?;
                continue;
            }
            Err(ScheduleError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        match reclaim_lock_state(&path, probes).await? {
            LockState::Held(holder, _) => {
                return Ok(SessionDeliveryLock::Held {
                    held_by_pid: holder.map(|holder| holder.pid),
                });
            }
            LockState::Stale(raw) => {
                if let Some(pid) =
                    orphaned_delivery(&raw, lock_age(&path, probes.now())?, probes).await?
                {
                    return Ok(SessionDeliveryLock::Held {
                        held_by_pid: Some(pid),
                    });
                }
                break_stale_lock(&path, &raw)?;
            }
            LockState::Free => {}
        }
    }
    Ok(SessionDeliveryLock::Held { held_by_pid: None })
}

/// Lets plain `ProcessProbes` serve the holder-record reader (no host-generation evidence).
struct PlainHolderProbes<'a>(&'a dyn ProcessProbes);

#[async_trait::async_trait]
impl ProcessProbes for PlainHolderProbes<'_> {
    fn own_identity(&self) -> RunnerIdentity {
        self.0.own_identity()
    }
    fn boot_at_ms(&self) -> u64 {
        self.0.boot_at_ms()
    }
    fn now(&self) -> u64 {
        self.0.now()
    }
    fn pid_alive(&self, pid: u32) -> Result<bool> {
        self.0.pid_alive(pid)
    }
    fn delivery_group_alive(&self, pid: u32) -> Result<bool> {
        self.0.delivery_group_alive(pid)
    }
    async fn process_start_ms(&self, pid: u32) -> Result<Option<u64>> {
        self.0.process_start_ms(pid).await
    }
}
impl crate::session_holder_records::SessionHolderProbes for PlainHolderProbes<'_> {}

pub async fn live_session_holder_pids(
    session_file: &Path,
    session_id: &str,
    probes: &dyn ProcessProbes,
) -> Result<Vec<u32>> {
    let holders = crate::session_holder_records::live_session_holders(
        session_file,
        session_id,
        &PlainHolderProbes(probes),
    )
    .await?;
    Ok(holders.into_iter().map(|holder| holder.pid).collect())
}

#[cfg(test)]
mod holder_tests {
    use super::*;

    struct Probes;
    #[async_trait::async_trait]
    impl ProcessProbes for Probes {
        fn own_identity(&self) -> RunnerIdentity {
            RunnerIdentity {
                pid: 101,
                process_started_at_ms: 1_000_000,
            }
        }
        fn boot_at_ms(&self) -> u64 {
            100_000
        }
        fn now(&self) -> u64 {
            2_000_000
        }
        fn pid_alive(&self, pid: u32) -> Result<bool> {
            Ok(pid == 101)
        }
        fn delivery_group_alive(&self, _: u32) -> Result<bool> {
            Ok(false)
        }
        async fn process_start_ms(&self, pid: u32) -> Result<Option<u64>> {
            Ok((pid == 101).then_some(1_000_000))
        }
    }
    fn identity() -> Holder {
        Holder {
            pid: 101,
            boot_at_ms: 100_000,
            process_started_at_ms: 1_000_000,
        }
    }

    #[tokio::test]
    async fn holder_remains_until_last_same_process_owner_releases() -> Result<()> {
        // Given
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("session.jsonl");
        let first =
            hold_session_file_with_identity(&file, "session", "/work", false, identity(), 0, None)?;
        let second =
            hold_session_file_with_identity(&file, "session", "/work", false, identity(), 0, None)?;
        // When
        drop(first);
        // Then
        assert_eq!(
            live_session_holder_pids(&file, "session", &Probes).await?,
            vec![101]
        );
        drop(second);
        assert!(
            live_session_holder_pids(&file, "session", &Probes)
                .await?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn missing_existing_session_releases_failed_open_holder() -> Result<()> {
        // Given
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("missing.jsonl");
        // When
        let result =
            hold_session_file_with_identity(&file, "session", "/work", true, identity(), 0, None);
        // Then
        assert!(result.is_err());
        assert!(!session_holders_dir(&file, "session")?.join("101.json").exists());
        Ok(())
    }

    #[test]
    fn active_move_lock_prevents_holder_admission() -> Result<()> {
        // Given
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("session.jsonl");
        let holders = session_holders_dir(&file, "session")?;
        ensure_dir(&holders)?;
        write_atomic(
            &holders.join("move.lock"),
            &serde_json::json!({"pid": std::process::id()}).to_string(),
        )?;
        // When
        let result =
            hold_session_file_with_identity(&file, "session", "/work", false, identity(), 0, None);
        // Then
        assert!(result.is_err());
        assert!(!holders.join("101.json").try_exists()?);
        Ok(())
    }
}
