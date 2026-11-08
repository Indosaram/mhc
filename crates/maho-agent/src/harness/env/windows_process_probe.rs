//! Win32 process probes shared by the tree killets.
//!
//! `tasklist.exe` and `powershell.exe`/`Get-CimInstance` cost hundreds of milliseconds cold and are
//! called from 25 ms polling loops; `OpenProcess` and a ToolHelp snapshot answer the same questions
//! in microseconds. Non-Windows targets get inert fallbacks.

use std::collections::{BTreeMap, BTreeSet};

/// `created_at` is the Win32 creation time in 100 ns units (0 when unreadable), which is what
/// distinguishes a live child from a descendant whose parent pid was recycled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: u32,
    pub created_at: u128,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeKillRefusal {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeKillPlan {
    pub kill: Vec<u32>,
    pub refused: Vec<TreeKillRefusal>,
}

const PROTECTED_IMAGES: [&str; 9] = [
    "system",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "svchost.exe",
];

/// Whether `pid` names a live process.
///
/// An access-denied open means the process exists under another user, which is alive (the
/// `kill(pid, 0)` EPERM rule).
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        match probe(pid) {
            Some(observed) => observed.alive,
            None => false,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

/// Terminate `pid` and nothing else; `Ok(false)` when it was already gone.
pub fn terminate_process(pid: u32) -> Result<bool, String> {
    #[cfg(windows)]
    {
        if pid == 0 {
            return Ok(false);
        }
        if !is_process_alive(pid) {
            return Ok(false);
        }
        terminate_now(pid).map(|()| true)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Ok(false)
    }
}

/// Terminate every pid in `pids` (skipping 0/1 and failures); returns how many were signalled.
pub fn terminate_all(pids: &[u32]) -> usize {
    pids.iter()
        .filter(|pid| **pid > 1 && terminate_process(**pid).unwrap_or(false))
        .count()
}

/// One ToolHelp snapshot of every process, not a WMI/PowerShell query.
pub fn process_rows() -> Vec<ProcessRow> {
    #[cfg(windows)]
    {
        use winsafe::{HPROCESSLIST, co};

        // SAFETY: winsafe wraps CreateToolhelp32Snapshot with a CloseHandle guard.
        let Ok(mut list) = HPROCESSLIST::CreateToolhelp32Snapshot(co::TH32CS::SNAPPROCESS, None)
        else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for entry in list.iter_processes() {
            let Ok(entry) = entry else { break };
            rows.push(ProcessRow {
                pid: entry.th32ProcessID,
                parent_pid: entry.th32ParentProcessID,
                created_at: creation_time(entry.th32ProcessID),
                name: entry.szExeFile(),
            });
        }
        rows
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// One process's creation time in 100 ns units, or 0 when it cannot be read.
///
/// Only the Windows snapshot reads it, so the non-Windows build has nothing to link here.
#[cfg(windows)]
fn creation_time(pid: u32) -> u128 {
    probe(pid).map_or(0, |observed| observed.created_at)
}

/// Descendants of `root` (including `root`) from one snapshot.
///
/// A child is only adopted when its parent is in the snapshot and not newer than it, so a recycled
/// parent pid cannot drag an unrelated process into the tree.
pub fn process_tree(root: u32) -> Vec<u32> {
    let rows = process_rows();
    if rows.is_empty() {
        return vec![root];
    }
    let by_pid: BTreeMap<u32, &ProcessRow> = rows.iter().map(|row| (row.pid, row)).collect();
    let mut tree = vec![root];
    let mut members = BTreeSet::from([root]);
    loop {
        let before = tree.len();
        for row in &rows {
            if members.contains(&row.pid) || !members.contains(&row.parent_pid) {
                continue;
            }
            if by_pid
                .get(&row.parent_pid)
                .is_some_and(|parent| row.created_at >= parent.created_at)
            {
                members.insert(row.pid);
                tree.push(row.pid);
            }
        }
        if tree.len() == before {
            return tree;
        }
    }
}

/// The root and every process that genuinely descends from it, minus protected OS images and
/// ancestors of `self_pid`.
///
/// If any protected image or self-ancestor was reached, the walk is untrustworthy, so only the root
/// stays eligible (and nothing when the root itself is one of them).
pub fn tree_kill_plan(rows: &[ProcessRow], root_pid: u32, self_pid: u32) -> TreeKillPlan {
    let by_pid: BTreeMap<_, _> = rows.iter().map(|row| (row.pid, row)).collect();
    if !by_pid.contains_key(&root_pid) {
        return TreeKillPlan {
            kill: Vec::new(),
            refused: Vec::new(),
        };
    }
    let mut tree = vec![root_pid];
    let mut members = BTreeSet::from([root_pid]);
    loop {
        let before = tree.len();
        for row in rows {
            if members.contains(&row.pid) || !members.contains(&row.parent_pid) {
                continue;
            }
            if by_pid
                .get(&row.parent_pid)
                .is_some_and(|parent| row.created_at >= parent.created_at)
            {
                members.insert(row.pid);
                tree.push(row.pid);
            }
        }
        if tree.len() == before {
            break;
        }
    }
    let mut ancestors = BTreeSet::from([self_pid]);
    let mut current = by_pid.get(&self_pid).copied();
    while let Some(row) = current {
        let Some(parent) = by_pid.get(&row.parent_pid).copied() else {
            break;
        };
        if ancestors.contains(&parent.pid) || parent.created_at > row.created_at {
            break;
        }
        ancestors.insert(parent.pid);
        current = Some(parent);
    }
    let refused: Vec<_> = tree
        .iter()
        .filter_map(|pid| {
            let name = &by_pid.get(pid)?.name;
            let protected = PROTECTED_IMAGES
                .iter()
                .any(|image| name.to_ascii_lowercase() == *image);
            (protected || ancestors.contains(pid)).then(|| TreeKillRefusal {
                pid: *pid,
                name: name.clone(),
            })
        })
        .collect();
    let kill = if refused.is_empty() {
        tree
    } else if refused.iter().any(|entry| entry.pid == root_pid) {
        Vec::new()
    } else {
        vec![root_pid]
    };
    TreeKillPlan { kill, refused }
}

#[cfg(windows)]
struct Observed {
    alive: bool,
    created_at: u128,
}

#[cfg(windows)]
fn probe(pid: u32) -> Option<Observed> {
    use winsafe::{HPROCESS, co};

    if pid == 0 {
        return None;
    }
    let process = match HPROCESS::OpenProcess(
        co::PROCESS::QUERY_LIMITED_INFORMATION | co::PROCESS::SYNCHRONIZE,
        false,
        pid,
    ) {
        Ok(process) => process,
        Err(co::ERROR::ACCESS_DENIED) => {
            return Some(Observed {
                alive: true,
                created_at: 0,
            });
        }
        Err(_) => {
            return Some(Observed {
                alive: false,
                created_at: 0,
            });
        }
    };
    let alive = process.WaitForSingleObject(Some(0)) == Ok(co::WAIT::TIMEOUT);
    let created_at = process.GetProcessTimes().map_or(0, |(creation, _, _, _)| {
        (u128::from(creation.dwHighDateTime) << 32) | u128::from(creation.dwLowDateTime)
    });
    Some(Observed { alive, created_at })
}

#[cfg(windows)]
fn terminate_now(pid: u32) -> Result<(), String> {
    use winsafe::{HPROCESS, co};

    HPROCESS::OpenProcess(co::PROCESS::TERMINATE, false, pid)
        .and_then(|process| process.TerminateProcess(1))
        .map_err(|error| format!("TerminateProcess({pid}): {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &[(u32, u32, u128, &str)]) -> Vec<ProcessRow> {
        text.iter()
            .map(|(pid, parent_pid, created_at, name)| ProcessRow {
                pid: *pid,
                parent_pid: *parent_pid,
                created_at: *created_at,
                name: (*name).into(),
            })
            .collect()
    }

    #[test]
    fn recycled_parent_pid_does_not_adopt_older_processes() {
        let rows = rows(&[
            (10, 1, 200, "bash.exe"),
            (20, 10, 100, "old.exe"),
            (30, 10, 201, "child.exe"),
        ]);
        assert_eq!(tree_kill_plan(&rows, 10, 99).kill, [10, 30]);
    }

    #[test]
    fn protected_descendant_restricts_the_kill_to_root() {
        let rows = rows(&[(10, 1, 100, "bash.exe"), (20, 10, 200, "Svchost.EXE")]);
        let plan = tree_kill_plan(&rows, 10, 99);
        assert_eq!(plan.kill, [10]);
        assert_eq!(plan.refused[0].pid, 20);
    }

    #[test]
    fn self_ancestor_root_is_refused() {
        let rows = rows(&[(10, 1, 100, "bash.exe"), (20, 10, 200, "mhc.exe")]);
        assert!(tree_kill_plan(&rows, 10, 20).kill.is_empty());
    }

    #[test]
    fn missing_root_kills_nothing() {
        let rows = rows(&[(20, 10, 200, "child.exe")]);
        assert!(tree_kill_plan(&rows, 10, 99).kill.is_empty());
    }
}
