//! Raises the open-file soft limit to what the hard limit allows, as the Node and Bun runtimes senpi
//! runs on do at startup. macOS starts processes at a soft limit of 256: a long-lived interactive
//! session that keeps sockets, kqueues and git pipes open reaches it, and then the TUI's hidden
//! diagnostic log can no longer be opened, so those diagnostics are written over the screen.

/// A conservative ceiling for the raised soft limit: `RLIM_INFINITY` is both absurd as a soft limit
/// and not accepted everywhere, so aim below it and let the halving loop absorb a refusal.
#[cfg(unix)]
const CEILING: u64 = 1 << 20;

/// Raises `RLIMIT_NOFILE` toward the hard limit; the largest value the kernel accepts wins. A
/// failure keeps the inherited limit.
#[cfg(unix)]
pub fn raise_open_file_limit() {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
    let Ok((soft, hard)) = getrlimit(Resource::RLIMIT_NOFILE) else {
        return;
    };
    let mut target = hard.min(CEILING);
    while target > soft {
        if setrlimit(Resource::RLIMIT_NOFILE, target, hard).is_ok() {
            return;
        }
        target /= 2;
    }
}

#[cfg(not(unix))]
pub fn raise_open_file_limit() {}

#[cfg(all(test, unix))]
mod tests {
    /// A soft limit low enough to leave guaranteed headroom below the ceiling, yet high enough
    /// that a test running in parallel in this process can still open every file it needs: the
    /// limit is process-wide, so lowering it must never provoke `EMFILE` elsewhere.
    const LOWERED_SOFT: u64 = 65_536;

    #[test]
    fn the_soft_limit_rises_toward_the_hard_limit_and_never_drops() {
        use nix::sys::resource::{Resource, getrlimit, setrlimit};
        let (before, hard) = getrlimit(Resource::RLIMIT_NOFILE).expect("getrlimit");
        super::raise_open_file_limit();
        let (after, after_hard) = getrlimit(Resource::RLIMIT_NOFILE).expect("getrlimit");
        assert!(after >= before, "{after} < {before}");
        assert_eq!(after_hard, hard);
        // The checks above hold even for a body that never calls `setrlimit`, and an inherited
        // soft limit already at the ceiling leaves no headroom to observe a rise. Lower it below
        // the ceiling first so the rise is measured rather than skipped; the previous soft limit
        // is restored afterwards so the process is left as it was found.
        let ceiling = hard.min(super::CEILING);
        if ceiling <= LOWERED_SOFT {
            return;
        }
        if setrlimit(Resource::RLIMIT_NOFILE, LOWERED_SOFT, hard).is_err() {
            return;
        }
        super::raise_open_file_limit();
        let (raised, _) = getrlimit(Resource::RLIMIT_NOFILE).expect("getrlimit");
        let restore = setrlimit(Resource::RLIMIT_NOFILE, after, hard);
        assert!(
            raised > LOWERED_SOFT,
            "soft limit stayed at {raised} below ceiling {ceiling}"
        );
        assert!(
            restore.is_ok(),
            "could not restore the soft limit to {after}"
        );
    }
}
