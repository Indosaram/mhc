//! Port of senpi packages/agent/src/harness/env/windows-process-tree.ts.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsProcessRow {
    pub pid: u32,
    pub parent_pid: u32,
    pub created_at: u128,
    pub name: String,
}

pub const WINDOWS_PROCESS_TREE_ROWS: &str = r#"Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -ne $PID } | ForEach-Object {
$created = if ($_.CreationDate) { $_.CreationDate.ToFileTimeUtc() } else { 0 }
"{0} {1} {2} {3}" -f $_.ProcessId, $_.ParentProcessId, $created, $_.Name }"#;

pub fn parse_windows_process_tree_rows(stdout: &str) -> Vec<WindowsProcessRow> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let parent_pid = fields.next()?.parse().ok()?;
            let created = fields.next()?;
            if pid == 0 || created.is_empty() || !created.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            Some(WindowsProcessRow {
                pid,
                parent_pid,
                created_at: created.parse().ok()?,
                name: fields.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsTreeKillRefusal {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsTreeKillPlan {
    pub kill: Vec<u32>,
    pub refused: Vec<WindowsTreeKillRefusal>,
}

pub fn windows_tree_kill_plan(
    rows: &[WindowsProcessRow],
    root_pid: u32,
    self_pid: u32,
) -> WindowsTreeKillPlan {
    let by_pid: BTreeMap<_, _> = rows.iter().map(|row| (row.pid, row)).collect();
    if !by_pid.contains_key(&root_pid) {
        return WindowsTreeKillPlan {
            kill: Vec::new(),
            refused: Vec::new(),
        };
    }
    // Keep insertion order, matching the upstream Set and thus its explicit PID argument order.
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
            let protected = matches!(
                name.to_ascii_lowercase().as_str(),
                "system"
                    | "smss.exe"
                    | "csrss.exe"
                    | "wininit.exe"
                    | "winlogon.exe"
                    | "services.exe"
                    | "lsass.exe"
                    | "lsaiso.exe"
                    | "svchost.exe"
            );
            (protected || ancestors.contains(pid)).then(|| WindowsTreeKillRefusal {
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
    WindowsTreeKillPlan { kill, refused }
}

pub fn list_windows_process_rows_sync(timeout_ms: u64) -> Option<Vec<WindowsProcessRow>> {
    #[cfg(windows)]
    {
        let output = super::nodejs::run_command_bounded(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                WINDOWS_PROCESS_TREE_ROWS,
            ],
            std::time::Duration::from_millis(timeout_ms),
            16 * 1024 * 1024,
        )?;
        if !output.status.success() {
            return None;
        }
        let rows = parse_windows_process_tree_rows(std::str::from_utf8(&output.stdout).ok()?);
        (!rows.is_empty()).then_some(rows)
    }
    #[cfg(not(windows))]
    {
        let _ = timeout_ms;
        None
    }
}

/// Last resort after every taskkill launcher fails: terminate only the requested PID.
/// Like Node's process.kill on Windows, this requests TerminateProcess with exit code 1
/// without waiting for exit or touching descendants. The guard closes the native handle.
#[cfg(windows)]
pub fn terminate_windows_process(pid: u32) -> winsafe::SysResult<()> {
    let process = winsafe::HPROCESS::OpenProcess(winsafe::co::PROCESS::TERMINATE, false, pid)?;
    process.TerminateProcess(1)
}

pub fn windows_tree_kill_args(
    root_pid: u32,
    rows: Option<&[WindowsProcessRow]>,
    self_pid: u32,
) -> Vec<String> {
    let Some(rows) = rows else {
        return vec![
            "/F".into(),
            "/T".into(),
            "/PID".into(),
            root_pid.to_string(),
        ];
    };
    let plan = windows_tree_kill_plan(rows, root_pid, self_pid);
    if plan.kill.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["/F".into()];
    for pid in plan.kill {
        args.extend(["/PID".into(), pid.to_string()]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn direct_termination_works_without_owned_child_registration() {
        use std::process::{Command, Stdio};
        use winsafe::{HPROCESS, co};

        // Given: a process blocked on an open stdin pipe, absent from OWNED_CHILDREN.
        let mut child = Command::new("cmd.exe")
            .args(["/D", "/Q", "/C", "set /p value="])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn unregistered process");
        // Acquire the exit-event handle before termination; no sleeps or polling.
        let exited = HPROCESS::OpenProcess(co::PROCESS::SYNCHRONIZE, false, child.id());

        // When: the native fallback receives only the arbitrary PID.
        let terminated = terminate_windows_process(child.id());
        let signaled = exited.and_then(|process| process.WaitForSingleObject(Some(5000)));

        // Then: the process exit event fires and Node's termination code is preserved.
        // Clean up before assertions even when the fallback or bounded wait fails.
        let cleanup = child.kill();
        let status = child.wait().expect("reap unregistered process");
        assert!(terminated.is_ok(), "native termination: {terminated:?}");
        assert_eq!(signaled, Ok(co::WAIT::OBJECT_0));
        assert_eq!(status.code(), Some(1), "cleanup: {cleanup:?}");
    }

    #[test]
    fn recycled_parent_pid_does_not_adopt_older_processes() {
        // Given: PID 10 has been reused after PID 20 was created.
        let rows = parse_windows_process_tree_rows(
            "10 1 200 bash.exe\n20 10 100 old.exe\n30 10 201 child.exe",
        );
        // When: the checked tree is converted to explicit arguments.
        let args = windows_tree_kill_args(10, Some(&rows), 99);
        // Then: the unrelated older process is not killed.
        assert_eq!(args, ["/F", "/PID", "10", "/PID", "30"]);
    }

    #[test]
    fn protected_descendant_restricts_the_kill_to_root() {
        // Given: the walk adopts a protected image.
        let rows = parse_windows_process_tree_rows("10 1 100 bash.exe\n20 10 200 Svchost.EXE");
        // When: the tree is planned.
        let plan = windows_tree_kill_plan(&rows, 10, 99);
        // Then: only the root is eligible.
        assert_eq!(plan.kill, [10]);
        assert_eq!(plan.refused[0].pid, 20);
    }

    #[test]
    fn self_ancestor_root_is_refused() {
        // Given: the requested root is an ancestor of the executing process.
        let rows = parse_windows_process_tree_rows("10 1 100 bash.exe\n20 10 200 mhc.exe");
        // When: arguments are built with that self PID.
        let args = windows_tree_kill_args(10, Some(&rows), 20);
        // Then: no process is killed.
        assert!(args.is_empty());
    }

    #[test]
    fn malformed_rows_are_skipped() {
        // Given: invalid IDs and timestamps alongside one valid row.
        let text = "0 1 1 zero\n1 nope 2 bad\n2 1 -1 bad\n3 1 300 good name.exe";
        // When: the listing is parsed.
        let rows = parse_windows_process_tree_rows(text);
        // Then: only the valid row survives.
        assert_eq!(
            rows,
            [WindowsProcessRow {
                pid: 3,
                parent_pid: 1,
                created_at: 300,
                name: "good name.exe".into()
            }]
        );
    }
}
