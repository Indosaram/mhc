//! Port of senpi packages/agent/src/harness/env/windows-process-tree.ts.

pub use super::windows_process_probe::{
    ProcessRow as WindowsProcessRow, TreeKillPlan as WindowsTreeKillPlan,
    TreeKillRefusal as WindowsTreeKillRefusal,
};

/// Kept for parity with the upstream constant. The listing itself is now a Win32 ToolHelp
/// snapshot ([`super::windows_process_probe::process_rows`]), not this PowerShell script.
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

pub fn windows_tree_kill_plan(
    rows: &[WindowsProcessRow],
    root_pid: u32,
    self_pid: u32,
) -> WindowsTreeKillPlan {
    super::windows_process_probe::tree_kill_plan(rows, root_pid, self_pid)
}

/// The WMI listing this replaces launched PowerShell and ran a COM query per tree kill; a ToolHelp
/// snapshot answers the same question in-process.
pub fn list_windows_process_rows_sync(_timeout_ms: u64) -> Option<Vec<WindowsProcessRow>> {
    let rows = super::windows_process_probe::process_rows();
    (!rows.is_empty()).then_some(rows)
}

/// Last resort after every taskkill launcher fails: terminate only the requested PID.
/// Like Node's process.kill on Windows, this requests TerminateProcess with exit code 1
/// without waiting for exit or touching descendants. The guard closes the native handle.
#[cfg(windows)]
pub fn terminate_windows_process(pid: u32) -> winsafe::SysResult<()> {
    winsafe::HPROCESS::OpenProcess(winsafe::co::PROCESS::TERMINATE, false, pid)?.TerminateProcess(1)
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

/// Kill a known tree through `TerminateProcess` directly, so the common path never pays for a
/// `taskkill.exe` launch; surviving pids (open refused) fall back to the launcher.
pub fn kill_known_tree(root_pid: u32) -> usize {
    let rows = super::windows_process_probe::process_rows();
    let plan = windows_tree_kill_plan(&rows, root_pid, std::process::id());
    if plan.kill.is_empty() {
        return 0;
    }
    let terminated = super::windows_process_probe::terminate_all(&plan.kill);
    let remaining: Vec<u32> = plan
        .kill
        .iter()
        .copied()
        .filter(|pid| super::windows_process_probe::is_process_alive(*pid))
        .collect();
    if remaining.is_empty() {
        return terminated;
    }
    let mut args = vec!["/F".to_owned()];
    for pid in remaining {
        args.extend(["/PID".to_owned(), pid.to_string()]);
    }
    let rendered: Vec<&str> = args.iter().map(String::as_str).collect();
    let _ = super::nodejs::run_command_bounded(
        "taskkill.exe",
        &rendered,
        std::time::Duration::from_secs(5),
        16 * 1024 * 1024,
    );
    terminated
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

    #[test]
    fn newer_child_of_a_recycled_parent_is_still_adopted() {
        // Given: a live child created after its parent.
        let rows = parse_windows_process_tree_rows("10 1 100 bash.exe\n30 10 200 child.exe");
        // When: the tree is planned.
        let plan = windows_tree_kill_plan(&rows, 10, 99);
        // Then: the child is killed with its parent.
        assert_eq!(plan.kill, [10, 30]);
    }
}
