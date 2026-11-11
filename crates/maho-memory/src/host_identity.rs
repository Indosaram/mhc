//! The machine this process is running on.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-support-host.md`. Lock records store a host name so a
//! contender can tell "same machine, dead process, safe to reclaim" from "another machine, might be
//! alive". A host name that is wrong in the empty direction stops every reclaim; one that is wrong in
//! the `unknown` direction makes an owner permanently unreclaimable. Both are worse than a name that is
//! merely imprecise.

use crate::lock_record::LockHost;

pub const UNKNOWN_HOST: &str = "unknown";

/// Best-effort host name. Never empty: lock-record validation rejects an empty host, so an
/// unidentifiable machine degrades to [`UNKNOWN_HOST`] instead of producing an invalid record.
///
/// The kernel's own name wins over environment variables. A supervisor started without `HOSTNAME` and
/// the child it spawns with one would otherwise disagree about "this machine" and refuse to reclaim
/// each other's dead locks.
pub fn hostname() -> String {
    if let Some(name) = kernel_hostname() {
        return name;
    }
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(name) = std::env::var(key) {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    UNKNOWN_HOST.to_string()
}

/// Whether a recorded host name was written by the machine now reporting `current`.
///
/// [`UNKNOWN_HOST`] matches: it is the sentinel written before the kernel name was consulted, so a
/// record carrying it could be ours. Treating it as foreign would make those dead owners
/// unreclaimable forever, which is worse than the under-claim it allows — a liveness check still runs
/// before any reclaim.
pub fn same_machine_for(recorded: &str, current: &str) -> bool {
    recorded == current || recorded == UNKNOWN_HOST
}

pub fn same_machine(recorded: &str) -> bool {
    same_machine_for(recorded, &hostname())
}

/// The real machine, for callers that need a [`LockHost`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLockHost;

impl LockHost for SystemLockHost {
    fn process_id(&self) -> u32 {
        std::process::id()
    }

    fn process_start(&self, pid: u32) -> Option<String> {
        process_start(pid)
    }

    fn hostname(&self) -> String {
        hostname()
    }

    fn nonce(&self) -> String {
        crate::random_ids::random_uuid()
    }

    fn now(&self) -> String {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    fn os_file_lock_available(&self) -> bool {
        true
    }
}

/// A start-time token that distinguishes a live process from a later process that reused its pid.
///
/// Linux exposes it as `stat` field 22 (clock ticks since boot). Anything else returns `None`, and the
/// record stores `unavailable`: a caller must read that as "cannot prove the owner is gone", never as
/// "gone".
fn process_start(pid: u32) -> Option<String> {
    proc_stat_field(pid, 22).map(|ticks| format!("proc-start:{ticks}"))
}

/// Field `index` (1-based, as `proc(5)` numbers them) of `/proc/<pid>/stat`.
///
/// Field 2 is the executable name in parentheses and may itself contain spaces or parentheses, so the
/// split starts after the *last* `)` rather than tokenizing the whole line.
fn proc_stat_field(pid: u32, index: usize) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm
        .split(' ')
        .nth(index.checked_sub(3)?)
        .map(str::to_string)
}

fn kernel_hostname() -> Option<String> {
    for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(contents) = std::fs::read_to_string(path) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostname_is_never_empty() {
        assert!(!hostname().is_empty());
    }

    #[test]
    fn a_hostname_that_the_kernel_reports_is_not_the_placeholder() {
        if kernel_hostname().is_some() {
            assert_ne!(hostname(), UNKNOWN_HOST);
        }
    }

    #[test]
    fn the_hostname_is_stable_across_calls() {
        assert_eq!(hostname(), hostname());
    }

    #[test]
    fn this_host_matches_itself() {
        assert!(same_machine(&hostname()));
    }

    #[test]
    fn the_legacy_unknown_sentinel_matches_any_host() {
        assert!(same_machine_for(UNKNOWN_HOST, "any-host"));
        assert!(same_machine(UNKNOWN_HOST));
    }

    #[test]
    fn another_host_does_not_match() {
        assert!(!same_machine_for("other-machine", "this-machine"));
    }

    #[test]
    fn the_system_host_reports_a_live_process() {
        let host = SystemLockHost;
        assert_eq!(host.process_id(), std::process::id());
        assert_eq!(host.hostname(), hostname());
        assert!(host.os_file_lock_available());
        assert!(!host.now().is_empty());
        assert_ne!(host.nonce(), host.nonce());
    }

    #[test]
    fn a_process_start_token_is_available_for_this_process_on_linux() {
        if cfg!(target_os = "linux") {
            let token = SystemLockHost.process_start(std::process::id());
            assert!(token.is_some(), "own /proc/<pid>/stat must be readable");
            assert!(token.unwrap().starts_with("proc-start:"));
        }
    }

    #[test]
    fn a_start_token_is_unavailable_for_a_pid_that_cannot_exist() {
        assert!(SystemLockHost.process_start(u32::MAX).is_none());
    }

    #[test]
    fn a_comm_containing_spaces_and_parentheses_does_not_shift_the_field() {
        let line = format!(
            "42 (a b) c) {}",
            (3..40).map(|n| n.to_string()).collect::<Vec<_>>().join(" ")
        );
        let after_comm = line.rsplit_once(") ").unwrap().1;
        let field_22 = after_comm.split(' ').nth(19).unwrap();
        assert_eq!(field_22, "22");
    }
}
