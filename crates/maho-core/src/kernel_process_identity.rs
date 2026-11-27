//! Port of senpi packages/coding-agent/src/core/kernel-process-identity.ts.
//!
//! Who a pid is, as the kernel reports it: its parent and when it started. Never throws/panics.

use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelProcessIdentity {
    pub ppid: u32,
    pub started_at_ms: u64,
    /// The boot this start time is relative to (linux); None where the start time is a wall-clock stamp.
    pub boot_id: Option<String>,
    /// Exited but not yet reaped: the pid is still taken, but nothing runs under it.
    pub zombie: bool,
}

const LINUX_CLOCK_TICKS_PER_SECOND: u64 = 100;

static LINUX_BOOT_ID: OnceLock<Option<String>> = OnceLock::new();

pub async fn read_kernel_process_identity(pid: u32) -> Option<KernelProcessIdentity> {
    if pid == 0 {
        return None;
    }
    if cfg!(target_os = "linux") {
        return linux_identity(pid).await;
    }
    if cfg!(target_os = "macos") {
        return darwin_ps_identity(pid).await;
    }
    None
}

async fn read_linux_boot_id() -> Option<String> {
    let raw = tokio::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .await
        .ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

async fn linux_identity(pid: u32) -> Option<KernelProcessIdentity> {
    let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
        .await
        .ok()?;
    let boot_id = if let Some(cached) = LINUX_BOOT_ID.get() {
        cached.clone()
    } else {
        let read = read_linux_boot_id().await;
        let _ = LINUX_BOOT_ID.set(read.clone());
        read
    }?;
    parse_linux_stat(&stat, &boot_id)
}

/// Parses `/proc/<pid>/stat` fields after the closing `)` of comm.
pub fn parse_linux_stat(stat: &str, boot_id: &str) -> Option<KernelProcessIdentity> {
    let close_paren = stat.rfind(')')?;
    let after = stat.get(close_paren + 2..)?;
    let fields: Vec<&str> = after.split(' ').collect();
    let state = *fields.first()?;
    let ppid: u32 = fields.get(1)?.parse().ok()?;
    let start_ticks: u64 = fields.get(19)?.parse().ok()?;
    Some(KernelProcessIdentity {
        ppid,
        started_at_ms: start_ticks.saturating_mul(1000 / LINUX_CLOCK_TICKS_PER_SECOND),
        boot_id: Some(boot_id.to_owned()),
        zombie: state == "Z",
    })
}

/// Darwin `ps -o ppid=,stat=,lstart= -p <pid>` reader under `TZ=UTC`.
pub async fn darwin_ps_identity(pid: u32) -> Option<KernelProcessIdentity> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new("ps")
            .args(["-o", "ppid=,stat=,lstart=", "-p", &pid.to_string()])
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .env("TZ", "UTC")
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_darwin_ps_identity(&stdout)
}

/// One `ps -o ppid=,stat=,lstart=` line printed under `TZ=UTC`.
pub fn parse_darwin_ps_identity(stdout: &str) -> Option<KernelProcessIdentity> {
    let trimmed = stdout.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }
    let ppid: u32 = tokens[0].parse().ok()?;
    let stat = tokens[1];
    // lstart format under LC_ALL=C: "Mon Nov 23 12:34:56 2026"
    let date_str = tokens[2..].join(" ");
    let parsed = chrono::NaiveDateTime::parse_from_str(&date_str, "%a %b %e %H:%M:%S %Y")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(&date_str, "%a %b %d %H:%M:%S %Y"))
        .ok()?;
    let ms = parsed.and_utc().timestamp_millis();
    let started_at_ms = u64::try_from(ms).ok()?;
    Some(KernelProcessIdentity {
        ppid,
        started_at_ms,
        boot_id: None,
        zombie: stat.starts_with('Z'),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identifies_self_on_linux_and_rejects_pid_zero() {
        assert!(read_kernel_process_identity(0).await.is_none());
        #[cfg(target_os = "linux")]
        {
            let self_id = read_kernel_process_identity(std::process::id())
                .await
                .expect("self identity on linux");
            assert!(!self_id.zombie);
            assert!(self_id.boot_id.is_some());
        }
    }

    #[test]
    fn parses_linux_stat_with_spaces_in_comm_and_zombie_state() {
        let stat =
            "1234 (my worker (1)) Z 100 1234 1234 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 98765 0 0";
        let id = parse_linux_stat(stat, "boot-abc").expect("parsed");
        assert_eq!(id.ppid, 100);
        assert_eq!(id.started_at_ms, 987_650);
        assert_eq!(id.boot_id.as_deref(), Some("boot-abc"));
        assert!(id.zombie);
    }

    #[test]
    fn parses_darwin_ps_utc_lstart() {
        let out = "  42 Ss   Mon Nov 23 12:34:56 2026\n";
        let id = parse_darwin_ps_identity(out).expect("parsed");
        assert_eq!(id.ppid, 42);
        assert!(!id.zombie);
        assert!(id.boot_id.is_none());
        assert!(id.started_at_ms > 0);
    }
}
