//! Port of `packages/machine-ops/src/process/scopes.ts`.

use crate::errors::MachineOpError;
use crate::process::exec::KillTreeFn;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Environment variable every exec/spawn child of an instance with a scope registry carries.
pub const SCOPE_ENV: &str = "SENPI_MACHINE_SCOPE";
/// Set on the runtime's own processes, which a sweep never signals.
pub const RUNTIME_ENV: &str = "OMO_MACHINE_RUNTIME";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SweepStep {
    pub signal: &'static str,
    pub wait_ms: u64,
}

/// HUP, then TERM, then KILL, each followed by its wait (herdr sweep ladder).
pub const SWEEP_LADDER: &[SweepStep] = &[
    SweepStep {
        signal: "SIGHUP",
        wait_ms: 500,
    },
    SweepStep {
        signal: "SIGTERM",
        wait_ms: 500,
    },
    SweepStep {
        signal: "SIGKILL",
        wait_ms: 2000,
    },
];

const SWEEP_POLL_MS: u64 = 25;

pub type StartTimeLookup =
    Arc<dyn Fn(u32) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

pub fn new_scope_token() -> String {
    let mut buf = [0u8; 16];
    let _ = getrandom::fill(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn alive(target: i32) -> bool {
    #[cfg(unix)]
    {
        use nix::errno::Errno;
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        match kill(Pid::from_raw(target), None) {
            Ok(()) => true,
            Err(Errno::ESRCH) => false,
            Err(_) => true,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        false
    }
}

fn read_start_time_sync(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close_paren = stat.rfind(')')?;
        let after = stat.get(close_paren + 2..)?;
        after.split(' ').nth(22 - 3).map(str::to_owned)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

fn default_start_time_of(pid: u32) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> {
    if cfg!(target_os = "linux") {
        let val = read_start_time_sync(pid);
        return Box::pin(async move { val });
    }
    if cfg!(windows) {
        return Box::pin(async { None });
    }
    Box::pin(async move {
        let output = tokio::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if text.is_empty() { None } else { Some(text) }
    })
}

fn send_signal(target: i32, sig_name: &str) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let Ok(sig) = parse_signal(sig_name) else {
            return;
        };
        let _ = kill(Pid::from_raw(target), sig);
        let _ = Signal::SIGKILL;
    }
    #[cfg(not(unix))]
    {
        let _ = (target, sig_name);
    }
}

#[cfg(unix)]
fn parse_signal(name: &str) -> Result<nix::sys::signal::Signal, ()> {
    use nix::sys::signal::Signal;
    match name {
        "SIGHUP" => Ok(Signal::SIGHUP),
        "SIGINT" => Ok(Signal::SIGINT),
        "SIGQUIT" => Ok(Signal::SIGQUIT),
        "SIGTERM" => Ok(Signal::SIGTERM),
        "SIGKILL" => Ok(Signal::SIGKILL),
        "SIGUSR1" => Ok(Signal::SIGUSR1),
        "SIGUSR2" => Ok(Signal::SIGUSR2),
        other => other.parse::<Signal>().map_err(|_| ()),
    }
}

async fn scan_environments(tokens: &HashSet<String>) -> Vec<u32> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = tokens;
        Vec::new()
    }
    #[cfg(target_os = "linux")]
    {
        if tokens.is_empty() {
            return Vec::new();
        }
        let Ok(mut rd) = tokio::fs::read_dir("/proc").await else {
            return Vec::new();
        };
        let own_pid = std::process::id();
        let mut candidates = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name();
            if let Ok(pid) = name.to_string_lossy().parse::<u32>()
                && pid != own_pid
            {
                candidates.push(pid);
            }
        }
        let mut members = Vec::new();
        let scope_prefix = format!("{SCOPE_ENV}=");
        let runtime_marker = format!("{RUNTIME_ENV}=1");
        for pid in candidates {
            let Ok(bytes) = tokio::fs::read(format!("/proc/{pid}/environ")).await else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let vars: Vec<&str> = text.split('\0').collect();
            if vars.contains(&runtime_marker.as_str()) {
                continue;
            }
            for var in vars {
                if let Some(tok) = var.strip_prefix(&scope_prefix)
                    && tokens.contains(tok)
                {
                    members.push(pid);
                    break;
                }
            }
        }
        members
    }
}

fn group_target(pid: u32) -> i32 {
    let raw = i32::try_from(pid).unwrap_or(i32::MAX);
    if cfg!(windows) { raw } else { -raw }
}

type RecordedStart = tokio::sync::watch::Receiver<Option<Option<String>>>;

/// Scope tokens: the token file is written under `<stateDir>/machine/scopes/` before the child spawns.
pub struct ScopeRegistry {
    dir: PathBuf,
    groups: Mutex<HashMap<String, HashMap<u32, RecordedStart>>>,
    kill_tree: KillTreeFn,
    start_time_of: StartTimeLookup,
}

impl ScopeRegistry {
    pub fn new(
        state_dir: PathBuf,
        kill_tree: KillTreeFn,
        start_time_of: Option<StartTimeLookup>,
    ) -> Self {
        Self {
            dir: state_dir.join("machine").join("scopes"),
            groups: Mutex::new(HashMap::new()),
            kill_tree,
            start_time_of: start_time_of.unwrap_or_else(|| Arc::new(default_start_time_of)),
        }
    }

    pub fn token_path(&self, token: &str) -> PathBuf {
        self.dir.join(format!("{token}.json"))
    }

    pub async fn record(
        &self,
        token: &str,
        principal: &str,
        command: &str,
    ) -> Result<(), MachineOpError> {
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|err| MachineOpError::from_io(&err, Some("mkdir"), Some(&self.dir)))?;
        let token_path = self.token_path(token);
        let payload = format!(
            "{}\n",
            json!({
                "token": token,
                "createdAtMs": now_ms(),
                "principal": principal,
                "command": command,
            })
        );
        tokio::fs::write(&token_path, payload.as_bytes())
            .await
            .map_err(|err| MachineOpError::from_io(&err, Some("open"), Some(&token_path)))?;
        Ok(())
    }

    pub fn attach(&self, token: &str, pid: u32) {
        let lookup = Arc::clone(&self.start_time_of);
        let (tx, rx) = tokio::sync::watch::channel(None);
        tokio::spawn(async move {
            let started = lookup(pid).await;
            let _ = tx.send(Some(started));
        });
        let mut groups = self
            .groups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        groups.entry(token.to_owned()).or_default().insert(pid, rx);
    }

    pub async fn release(&self, token: &str) {
        let _ = tokio::fs::remove_file(self.token_path(token)).await;
        let mut groups = self
            .groups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(leaders) = groups.get_mut(token) {
            leaders.retain(|&pid, _| alive(group_target(pid)));
            if leaders.is_empty() {
                groups.remove(token);
            }
        }
    }

    async fn read_recorded(rx: &mut RecordedStart) -> Option<String> {
        loop {
            if let Some(val) = rx.borrow_and_update().clone() {
                return val;
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }

    async fn same_group(&self, pid: u32, mut recorded: RecordedStart) -> bool {
        let raw_pid = i32::try_from(pid).unwrap_or(i32::MAX);
        if !alive(raw_pid) {
            return true;
        }
        let Some(then) = Self::read_recorded(&mut recorded).await else {
            return false;
        };
        let now = (self.start_time_of)(pid).await;
        now.as_deref() == Some(then.as_str())
    }

    async fn targets(&self, tokens: &HashSet<String>) -> (Vec<u32>, Vec<u32>) {
        let snapshot: Vec<(String, u32, RecordedStart)> = {
            let groups = self
                .groups
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut out = Vec::new();
            for token in tokens {
                if let Some(leaders) = groups.get(token) {
                    for (&pid, rx) in leaders {
                        out.push((token.clone(), pid, rx.clone()));
                    }
                }
            }
            out
        };

        let mut group_pids = Vec::new();
        let mut stale: Vec<(String, u32)> = Vec::new();
        for (token, pid, rx) in snapshot {
            if !alive(group_target(pid)) {
                continue;
            }
            if self.same_group(pid, rx).await {
                group_pids.push(pid);
            } else {
                stale.push((token, pid));
            }
        }
        if !stale.is_empty() {
            let mut groups = self
                .groups
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for (token, pid) in stale {
                if let Some(leaders) = groups.get_mut(&token) {
                    leaders.remove(&pid);
                }
            }
        }

        let pids: Vec<u32> = scan_environments(tokens)
            .await
            .into_iter()
            .filter(|&pid| alive(i32::try_from(pid).unwrap_or(i32::MAX)))
            .collect();
        (group_pids, pids)
    }

    pub async fn sweep(&self, token_list: &[String]) {
        let tokens: HashSet<String> = token_list.iter().cloned().collect();
        for step in SWEEP_LADDER {
            let (groups, pids) = self.targets(&tokens).await;
            if groups.is_empty() && pids.is_empty() {
                break;
            }
            if cfg!(windows) {
                if step.signal == "SIGKILL" {
                    for pid in groups.iter().chain(pids.iter()) {
                        (self.kill_tree)(*pid);
                    }
                }
            } else {
                for &pgid in &groups {
                    send_signal(-(i32::try_from(pgid).unwrap_or(i32::MAX)), step.signal);
                }
                for &pid in &pids {
                    send_signal(i32::try_from(pid).unwrap_or(i32::MAX), step.signal);
                }
            }
            let deadline = Instant::now() + Duration::from_millis(step.wait_ms);
            loop {
                let any_live = groups.iter().any(|&p| alive(group_target(p)))
                    || pids
                        .iter()
                        .any(|&p| alive(i32::try_from(p).unwrap_or(i32::MAX)));
                if !any_live || Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(SWEEP_POLL_MS)).await;
            }
        }
        for token in &tokens {
            {
                let mut groups = self
                    .groups
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                groups.remove(token);
            }
            let _ = tokio::fs::remove_file(self.token_path(token)).await;
        }
    }
}
