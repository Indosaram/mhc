//! Port of `packages/machine-ops/src/process/oom-score.ts` (`senpi#7056`).

/// Linux `oom_score_adj` for every command a machine exec starts: the maximum, so under memory
/// pressure the kernel's OOM killer picks a runaway command before the long-lived host that ran it.
pub const EXEC_CHILD_OOM_SCORE_ADJ: &str = "1000";

/// Marks `pid` as the preferred OOM victim on Linux; a no-op elsewhere. Best effort and never fails
/// exec startup: the write lands just after spawn so children forked later inherit it, and a child
/// that already exited (`ENOENT`/`ESRCH`) or locked-down `/proc` (`EACCES`) is left as is.
pub fn prefer_exec_child_as_oom_victim(pid: u32) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::fs::write(
            format!("/proc/{pid}/oom_score_adj"),
            EXEC_CHILD_OOM_SCORE_ADJ,
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
    }
}
