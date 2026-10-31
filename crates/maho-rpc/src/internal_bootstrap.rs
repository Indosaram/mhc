//! The engine's stdin key bootstrap (senpi `internal-bootstrap.ts`, senpi#3795): a supervised POSIX host reads the
//! 32-byte memory-only internal key its supervisor wrote once to stdin, then replaces fd 0 with /dev/null so no
//! descendant inherits the key pipe. A launcher that keeps stdin open without sending the key would leave the read
//! blocked forever, so a separate thread enforces the 2 s bound and SIGKILLs the process when it expires.
//!
//! It must run before anything else touches stdin. The CLI entry calls `init_internal_host_bootstrap` and removes
//! `SENPI_RPC_INTERNAL_KEY_STDIN` from the process environment (descendants must never bootstrap again); this crate
//! cannot write the process environment.
use std::collections::HashMap;
use std::sync::OnceLock;

pub const INTERNAL_KEY_STDIN_ENV: &str = "SENPI_RPC_INTERNAL_KEY_STDIN";
pub const INTERNAL_KEY_DEADLINE_MS: u64 = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalHostBootstrap {
    pub key: [u8; 32],
    /// The reopened stdin (macOS lifetime binding); `None` when the reopen is unavailable (Linux socketpair stdin).
    pub lifetime_fd: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InternalBootstrapError(pub String);

/// Arms the deadline on its own thread; the returned closure reports the read finished.
#[cfg(unix)]
fn arm_key_deadline() -> impl FnOnce() {
    arm_key_deadline_after(
        std::time::Duration::from_millis(INTERNAL_KEY_DEADLINE_MS),
        || {
            eprintln!(
                "senpi rpc host: internal_key_unavailable: no key within {INTERNAL_KEY_DEADLINE_MS}ms"
            );
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            );
        },
    )
    .0
}

/// The deadline watcher with its bound and expiry action injected; also returns the watcher thread.
#[cfg(unix)]
fn arm_key_deadline_after(
    bound: std::time::Duration,
    expire: impl FnOnce() + Send + 'static,
) -> (impl FnOnce(), std::thread::JoinHandle<()>) {
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let watcher = std::thread::spawn(move || {
        if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = finished.recv_timeout(bound) {
            expire();
        }
    });
    (
        move || {
            let _ = done.send(());
        },
        watcher,
    )
}

/// Reads the 32-byte key with the read, clock and retry pause injected. Fails on EOF, a read error, or the clock
/// reaching the deadline before the key is complete.
#[cfg(unix)]
fn read_key(
    mut read: impl FnMut(&mut [u8]) -> Result<usize, rustix::io::Errno>,
    mut now: impl FnMut() -> std::time::Instant,
    mut pause: impl FnMut(),
) -> Result<[u8; 32], InternalBootstrapError> {
    let unavailable = || InternalBootstrapError("internal_key_unavailable".into());
    let mut key = [0u8; 32];
    let deadline = now() + std::time::Duration::from_millis(INTERNAL_KEY_DEADLINE_MS);
    let mut offset = 0;
    while offset < key.len() {
        if now() >= deadline {
            return Err(unavailable());
        }
        match read(&mut key[offset..]) {
            Ok(0) => return Err(unavailable()),
            Ok(count) => offset += count,
            Err(rustix::io::Errno::AGAIN) | Err(rustix::io::Errno::INTR) => pause(),
            Err(_) => return Err(unavailable()),
        }
    }
    Ok(key)
}

#[cfg(unix)]
fn bootstrap(
    env: &HashMap<String, String>,
) -> Result<Option<InternalHostBootstrap>, InternalBootstrapError> {
    use std::os::fd::IntoRawFd;
    if env.get(INTERNAL_KEY_STDIN_ENV).map(String::as_str) != Some("1") {
        return Ok(None);
    }
    let read_finished = arm_key_deadline();
    let mut lifetime_fd = None;
    let read = read_key(
        |buffer| rustix::io::read(rustix::stdio::stdin(), buffer),
        std::time::Instant::now,
        || std::thread::sleep(std::time::Duration::from_millis(5)),
    );
    let (key, failure) = match read {
        Ok(key) => (key, None),
        Err(error) => ([0u8; 32], Some(error)),
    };
    if failure.is_none() {
        // close-on-exec: descendants inherit only the restored /dev/null, never this lifetime fd.
        match std::fs::File::open("/dev/fd/0") {
            Ok(file) => lifetime_fd = Some(file.into_raw_fd()),
            // Linux socketpair stdin returns ENXIO. Every reopen failure has the same safe fallback.
            Err(_) => eprintln!(
                "senpi rpc host: stdin lifetime reopen unavailable; using supervisor identity poll"
            ),
        }
    }
    read_finished();
    // Runs on success and failure alike: descendants must inherit /dev/null, never the key pipe. Fatal by design:
    // a host whose fd 0 is not /dev/null would hand the key pipe to every descendant.
    let restored = std::fs::File::open("/dev/null")
        .and_then(|null| rustix::stdio::dup2_stdin(&null).map_err(std::io::Error::from));
    if restored.is_err() {
        return Err(InternalBootstrapError(
            "internal_key_unavailable: cannot restore stdin".into(),
        ));
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    Ok(Some(InternalHostBootstrap { key, lifetime_fd }))
}

#[cfg(not(unix))]
fn bootstrap(
    _env: &HashMap<String, String>,
) -> Result<Option<InternalHostBootstrap>, InternalBootstrapError> {
    Ok(None)
}

static BOOTSTRAP: OnceLock<Option<InternalHostBootstrap>> = OnceLock::new();

/// Runs the bootstrap once, before the CLI graph can read stdin. An error is fatal for the host: the caller exits.
pub fn init_internal_host_bootstrap(
    env: &HashMap<String, String>,
) -> Result<(), InternalBootstrapError> {
    if BOOTSTRAP.get().is_some() {
        return Ok(());
    }
    let result = bootstrap(env)?;
    let _ = BOOTSTRAP.set(result);
    Ok(())
}

/// senpi `getInternalHostBootstrap`: the key and lifetime descriptor, when this host was launched with one.
pub fn get_internal_host_bootstrap() -> Option<&'static InternalHostBootstrap> {
    BOOTSTRAP.get().and_then(Option::as_ref)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_host_without_the_stdin_marker_does_not_bootstrap() {
        assert_eq!(bootstrap(&HashMap::new()), Ok(None));
        let env = HashMap::from([(INTERNAL_KEY_STDIN_ENV.to_owned(), "0".to_owned())]);
        assert_eq!(bootstrap(&env), Ok(None));
    }

    /// A scripted stdin on a fake clock: each step advances the clock, then reads that many 7 bytes (or fails).
    #[cfg(unix)]
    fn scripted(
        steps: Vec<(u64, Result<usize, rustix::io::Errno>)>,
    ) -> (Result<[u8; 32], InternalBootstrapError>, usize) {
        let start = std::time::Instant::now();
        let clock = std::cell::Cell::new(start);
        let mut steps = steps.into_iter();
        let mut pauses = 0;
        let result = read_key(
            |buffer| {
                let (advance_ms, step) = steps.next().expect("read past the script");
                clock.set(clock.get() + std::time::Duration::from_millis(advance_ms));
                step.map(|count| {
                    buffer[..count].fill(7);
                    count
                })
            },
            || clock.get(),
            || pauses += 1,
        );
        (result, pauses)
    }

    #[cfg(unix)]
    #[test]
    fn assembles_a_fragmented_key_across_eagain_and_eintr() {
        let (result, pauses) = scripted(vec![
            (0, Ok(10)),
            (0, Err(rustix::io::Errno::AGAIN)),
            (1999, Err(rustix::io::Errno::INTR)),
            (0, Ok(22)),
        ]);
        assert_eq!(result, Ok([7; 32]));
        assert_eq!(pauses, 2);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_an_eof_or_a_read_error_before_the_whole_key() {
        for last in [Ok(0), Err(rustix::io::Errno::BADF)] {
            let (result, _) = scripted(vec![(0, Ok(31)), (0, last)]);
            assert_eq!(result.expect_err("refused").0, "internal_key_unavailable");
        }
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_key_still_incomplete_at_the_2_s_deadline() {
        // The clock reaches the deadline during a retry; the next read is never attempted.
        let (result, pauses) = scripted(vec![
            (0, Ok(16)),
            (INTERNAL_KEY_DEADLINE_MS, Err(rustix::io::Errno::AGAIN)),
        ]);
        assert_eq!(result.expect_err("refused").0, "internal_key_unavailable");
        assert_eq!(pauses, 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_deadline_watcher_fires_only_when_the_read_never_finishes() {
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = fired.clone();
        let (finished, watcher) =
            arm_key_deadline_after(std::time::Duration::from_secs(3600), move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
        finished();
        watcher.join().expect("watcher");
        assert!(!fired.load(std::sync::atomic::Ordering::SeqCst));

        let flag = fired.clone();
        let (_unfinished, watcher) = arm_key_deadline_after(std::time::Duration::ZERO, move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        watcher.join().expect("watcher");
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
    }
}
