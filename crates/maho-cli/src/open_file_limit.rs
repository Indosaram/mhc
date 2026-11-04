//! Raises the open-file soft limit to what the hard limit allows, as the Node and Bun runtimes senpi
//! runs on do at startup. macOS starts processes at a soft limit of 256: a long-lived interactive
//! session that keeps sockets, kqueues and git pipes open reaches it, and then the TUI's hidden
//! diagnostic log can no longer be opened, so those diagnostics are written over the screen.

/// Raises `RLIMIT_NOFILE` toward the hard limit; the largest value the kernel accepts wins. A
/// failure keeps the inherited limit.
#[cfg(unix)]
pub fn raise_open_file_limit() {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
    /// macOS rejects `RLIM_INFINITY` and anything above `kern.maxfilesperproc`; start below that
    /// bound and halve until the kernel accepts.
    const CEILING: u64 = 1 << 20;
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
    #[test]
    fn the_soft_limit_never_drops() {
        use nix::sys::resource::{Resource, getrlimit};
        let (before, hard) = getrlimit(Resource::RLIMIT_NOFILE).expect("getrlimit");
        super::raise_open_file_limit();
        let (after, after_hard) = getrlimit(Resource::RLIMIT_NOFILE).expect("getrlimit");
        assert!(after >= before, "{after} < {before}");
        assert_eq!(after_hard, hard);
    }
}
