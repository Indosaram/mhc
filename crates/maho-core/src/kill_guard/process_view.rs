//! Port of `packages/coding-agent/src/core/kill-guard/process-view.ts` (senpi#5349).

use super::types::{
    KillGuardContext, KillGuardShell, KillGuardVantage, ProcessRow, ProcessView, SelectResult,
};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The bound on one table read and on one dry run (DESIGN-12096 §4').
const KILL_GUARD_DEADLINE_MS: u64 = 2_000;

#[derive(Clone, Debug)]
pub struct SpawnOutput {
    pub status: Option<i32>,
    pub stdout: String,
}

/// A synchronous spawn the judge may use.
pub type KillGuardSpawnSync =
    Arc<dyn Fn(&str, &[String], u64) -> Option<SpawnOutput> + Send + Sync>;

pub fn system_spawn_sync() -> KillGuardSpawnSync {
    Arc::new(|file, args, timeout_ms| {
        let mut child = Command::new(file)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let pid = child.id();
        let mut stdout_pipe = child.stdout.take();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut buf = String::new();
            if let Some(pipe) = stdout_pipe.as_mut() {
                let _ = pipe.read_to_string(&mut buf);
            }
            let status = child.wait().ok().and_then(|s| s.code());
            let _ = tx.send(SpawnOutput {
                status,
                stdout: buf,
            });
        });
        match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
            Ok(output) => Some(output),
            Err(_) => {
                #[cfg(unix)]
                {
                    if let Ok(raw_pid) = i32::try_from(pid) {
                        let _ = nix::sys::signal::kill(
                            nix::unistd::Pid::from_raw(raw_pid),
                            nix::sys::signal::Signal::SIGKILL,
                        );
                    }
                }
                #[cfg(not(unix))]
                let _ = pid;
                None
            }
        }
    })
}

/// `ps -A -o pid=,ppid=,pgid=,sess=,uid=,comm=,args=`; macOS pads `comm` and may put spaces in it, so it is display only.
pub fn parse_ps_rows(text: &str) -> Vec<ProcessRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid_s), Some(ppid_s), Some(pgid_s), Some(sid_s), Some(uid_s)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(pgid), Ok(sid), Ok(uid)) = (
            pid_s.parse::<i64>(),
            ppid_s.parse::<i64>(),
            pgid_s.parse::<i64>(),
            sid_s.parse::<i64>(),
            uid_s.parse::<u32>(),
        ) else {
            continue;
        };
        // Find the rest of the line after the 5th column (matching PS_ROW regex).
        let Some(rest) = rest_after_five_columns(line) else {
            continue;
        };
        let trimmed = rest.trim();
        let first = trimmed.split_whitespace().next().unwrap_or("");
        let comm = first.rsplit('/').next().unwrap_or(first).to_owned();
        rows.push(ProcessRow {
            pid,
            ppid,
            pgid,
            sid,
            uid,
            comm,
            args: trimmed.to_owned(),
        });
    }
    rows
}

fn rest_after_five_columns(line: &str) -> Option<&str> {
    let mut idx = 0usize;
    let bytes = line.as_bytes();
    for _ in 0..5 {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            return None;
        }
        while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
    }
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    Some(&line[idx..])
}

#[cfg(target_os = "linux")]
fn read_proc_rows(deadline: Instant) -> Option<Vec<ProcessRow>> {
    use std::os::unix::fs::MetadataExt;
    let entries = std::fs::read_dir("/proc").ok()?;
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.is_empty() || !name_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if Instant::now() > deadline {
            return None;
        }
        let Ok(pid) = name_str.parse::<i64>() else {
            continue;
        };
        let proc_dir = entry.path();
        let Ok(stat) = std::fs::read_to_string(proc_dir.join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        if close <= open {
            continue;
        }
        let Some(after) = stat.get(close + 2..) else {
            continue;
        };
        let fields: Vec<&str> = after.split(' ').collect();
        let (Some(ppid), Some(pgid), Some(sid)) = (
            fields.get(1).and_then(|s| s.parse::<i64>().ok()),
            fields.get(2).and_then(|s| s.parse::<i64>().ok()),
            fields.get(3).and_then(|s| s.parse::<i64>().ok()),
        ) else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&proc_dir) else {
            continue;
        };
        let uid = meta.uid();
        let comm = stat[open + 1..close].to_owned();
        let args = std::fs::read(proc_dir.join("cmdline"))
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .replace('\0', " ")
                    .trim()
                    .to_owned()
            })
            .unwrap_or_default();
        rows.push(ProcessRow {
            pid,
            ppid,
            pgid,
            sid,
            uid,
            comm,
            args,
        });
    }
    (!rows.is_empty()).then_some(rows)
}

#[cfg(not(target_os = "linux"))]
fn read_proc_rows(_deadline: Instant) -> Option<Vec<ProcessRow>> {
    None
}

fn read_ps_rows(spawn_sync: &KillGuardSpawnSync) -> Option<Vec<ProcessRow>> {
    let args = [
        "-A".to_owned(),
        "-o".to_owned(),
        "pid=,ppid=,pgid=,sess=,uid=,comm=,args=".to_owned(),
    ];
    let result = spawn_sync("ps", &args, KILL_GUARD_DEADLINE_MS)?;
    if result.status != Some(0) {
        return None;
    }
    let rows = parse_ps_rows(&result.stdout);
    (!rows.is_empty()).then_some(rows)
}

fn parse_pids(text: &str) -> Vec<i64> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|line| line.parse::<i64>().ok())
        .collect()
}

/// The production view: `/proc` on Linux (falling back to `ps`), `ps` elsewhere; `None` when neither answers.
pub fn create_process_view(platform: &str, spawn_sync: KillGuardSpawnSync) -> Option<ProcessView> {
    if platform == "win32" {
        return None;
    }
    let mut rows = if platform == "linux" {
        read_proc_rows(Instant::now() + Duration::from_millis(KILL_GUARD_DEADLINE_MS))
    } else {
        None
    };
    if rows.is_none() {
        rows = read_ps_rows(&spawn_sync);
    }
    let rows = rows?;
    let select = Arc::new(move |args: &[String]| -> SelectResult {
        let Some(result) = spawn_sync("pgrep", args, KILL_GUARD_DEADLINE_MS) else {
            return SelectResult::Err {
                reason: "pgrep did not finish".to_owned(),
            };
        };
        let Some(status) = result.status else {
            return SelectResult::Err {
                reason: "pgrep did not finish".to_owned(),
            };
        };
        if status == 1 {
            return SelectResult::Ok { pids: Vec::new() };
        }
        if status != 0 {
            return SelectResult::Err {
                reason: format!("pgrep exited {status}"),
            };
        }
        SelectResult::Ok {
            pids: parse_pids(&result.stdout),
        }
    });
    Some(ProcessView { rows, select })
}

#[derive(Clone)]
pub struct KillGuardContextOptions {
    pub platform: String,
    pub agent_pid: i64,
    pub agent_ppid: Option<i64>,
    pub vantage: KillGuardVantage,
    pub shell: KillGuardShell,
    pub spawn_sync: KillGuardSpawnSync,
}

/// A context whose view is read at most once, and only when a rule needs the table.
pub fn kill_guard_context(options: KillGuardContextOptions) -> KillGuardContext {
    let platform = options.platform.clone();
    let spawn_sync = Arc::clone(&options.spawn_sync);
    let cached: Arc<Mutex<Option<Option<ProcessView>>>> = Arc::new(Mutex::new(None));
    let view = Arc::new(move || -> Option<ProcessView> {
        let mut guard = cached.lock().ok()?;
        if let Some(existing) = guard.as_ref() {
            return existing.clone();
        }
        let loaded = create_process_view(&platform, Arc::clone(&spawn_sync));
        *guard = Some(loaded.clone());
        loaded
    });
    KillGuardContext {
        platform: options.platform,
        agent_pid: options.agent_pid,
        agent_ppid: options.agent_ppid,
        vantage: options.vantage,
        shell: options.shell,
        view,
    }
}
