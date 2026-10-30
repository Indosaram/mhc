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
    let (done, finished) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
            finished.recv_timeout(std::time::Duration::from_millis(INTERNAL_KEY_DEADLINE_MS))
        {
            eprintln!(
                "senpi rpc host: internal_key_unavailable: no key within {INTERNAL_KEY_DEADLINE_MS}ms"
            );
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            );
        }
    });
    move || {
        let _ = done.send(());
    }
}

#[cfg(unix)]
fn bootstrap(
    env: &HashMap<String, String>,
) -> Result<Option<InternalHostBootstrap>, InternalBootstrapError> {
    use std::os::fd::IntoRawFd;
    if env.get(INTERNAL_KEY_STDIN_ENV).map(String::as_str) != Some("1") {
        return Ok(None);
    }
    let unavailable = || InternalBootstrapError("internal_key_unavailable".into());
    let mut key = [0u8; 32];
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(INTERNAL_KEY_DEADLINE_MS);
    let read_finished = arm_key_deadline();
    let mut offset = 0;
    let mut lifetime_fd = None;
    let mut failure = None;
    while offset < key.len() {
        if std::time::Instant::now() >= deadline {
            failure = Some(unavailable());
            break;
        }
        match rustix::io::read(rustix::stdio::stdin(), &mut key[offset..]) {
            Ok(0) => {
                failure = Some(unavailable());
                break;
            }
            Ok(count) => offset += count,
            Err(rustix::io::Errno::AGAIN) | Err(rustix::io::Errno::INTR) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => {
                failure = Some(unavailable());
                break;
            }
        }
    }
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
}
