//! Host identity: the port of `node:os`'s `hostname()`.

use std::env;
use std::fs;

/// Best-effort host name, matching Node's `os.hostname()` fallback behaviour.
///
/// Lock records store this value so a contender can tell "same machine" from
/// "another machine"; an empty result degrades to `unknown` rather than an
/// empty string, because lock-record validation rejects empty host names.
pub fn hostname() -> String {
    // The OS's own name wins: environment variables come from whichever parent spawned the process, so a
    // supervisor started without `HOSTNAME` and its child started with one would otherwise disagree about
    // "this machine" and refuse to reclaim each other's dead locks.
    if let Some(name) = system_hostname() {
        return name;
    }
    if let Ok(name) = env::var("HOSTNAME") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(name) = env::var("COMPUTERNAME") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    for candidate in ["/etc/hostname", "/proc/sys/kernel/hostname"] {
        if let Ok(contents) = fs::read_to_string(candidate) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    "unknown".to_string()
}

/// `None` only when the OS refuses to report a name (the caller falls back to the env vars).
fn system_hostname() -> Option<String> {
    let name = gethostname::gethostname();
    let name = name.to_string_lossy();
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Whether a recorded host name was written by this machine.
///
/// `unknown` is the sentinel older macOS builds wrote before [`hostname`] consulted the OS, so a record
/// carrying it could have come from here: treating it as foreign would make those dead owners
/// unreclaimable forever, which is worse than the under-claim it allows (a liveness check still runs).
pub fn same_machine(recorded: &str) -> bool {
    same_machine_for(recorded, &hostname())
}

/// Whether a recorded host name was written by the machine now reporting `current`.
///
/// Split out from [`same_machine`] so callers holding their own view of the local host name (the
/// reconciliation port) share one comparison rule instead of repeating the sentinel handling.
pub fn same_machine_for(recorded: &str, current: &str) -> bool {
    recorded == current || recorded == "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_is_never_empty_and_never_the_placeholder() {
        let name = hostname();
        assert!(!name.is_empty());
        // Lock records compare this value across processes; `unknown` on every macOS host makes a
        // contender unable to prove a dead same-machine owner, so it must not reach a real machine.
        if system_hostname().is_some() {
            assert_ne!(name, "unknown");
        }
    }

    #[test]
    fn same_machine_accepts_this_host_and_the_legacy_placeholder() {
        assert!(same_machine(&hostname()));
        assert!(same_machine("unknown"));
        assert!(!same_machine("some-other-host"));
    }
}
