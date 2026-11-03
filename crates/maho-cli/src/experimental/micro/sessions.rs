//! Port of senpi `experimental/micro/sessions.ts` (pin 903b8336).
//!
//! Micro sessions live under `<agentDir>/experimental/micro-sessions/<cwd-hash>/`. Each session is a
//! Pico3 `JsonlStorage` directory named `<13-digit ms>-<uuid>`. A filesystem lock (the
//! proper-lockfile `<path>.lock` directory, here `maho_core::lockfile_policy`) keeps two processes
//! from owning one session. senpi's function is async over `node:fs/promises`; this is the blocking
//! `std` equivalent, and callers on an async runtime run it on a blocking pool.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use maho_core::file_lock::{FileLockLostError, HeldFileLock, OnLostCallback};
use maho_core::lockfile_policy::{LockGuard, acquire_host_lock_sync};
use sha2::{Digest, Sha256};

/// senpi `MicroSessionLocation`. `release()` drops the session lock.
pub struct MicroSessionLocation {
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub created: bool,
    lock: Option<LockGuard>,
    held: Option<HeldFileLock>,
}

impl MicroSessionLocation {
    /// senpi `release()`.
    pub fn release(&mut self) {
        self.lock.take();
    }

    /// senpi `assertHeld()`: fails with `FileLockLostError` once another process reclaimed the
    /// session lock.
    pub fn assert_held(&self) -> Result<(), FileLockLostError> {
        self.held.as_ref().map_or(Ok(()), HeldFileLock::assert_held)
    }

    /// A shareable `assert_held` for the lock-gated storage.
    pub fn held_lock(&self) -> Option<HeldFileLock> {
        self.held.clone()
    }
}

fn cwd_key(cwd: &Path) -> String {
    let digest = Sha256::digest(cwd.to_string_lossy().as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..24]
        .to_owned()
}

/// `/^\d{13}-[0-9a-f-]{36}$/u`.
fn is_session_dir_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 50
        && bytes[..13].iter().all(u8::is_ascii_digit)
        && bytes[13] == b'-'
        && bytes[14..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte) || *byte == b'-')
}

/// senpi `selectSession`: a new session, or the newest session for this cwd, without opening any
/// JSONL logs. `on_lost` fires once if another process reclaims the session lock.
pub fn select_session(
    cwd_input: &Path,
    continue_session: bool,
    on_lost: OnLostCallback,
) -> Result<MicroSessionLocation, String> {
    select_session_in(
        Path::new(&maho_core::config::get_agent_dir()),
        cwd_input,
        continue_session,
        on_lost,
    )
}

/// `select_session` with the agent directory injected.
pub fn select_session_in(
    agent_dir: &Path,
    cwd_input: &Path,
    continue_session: bool,
    on_lost: OnLostCallback,
) -> Result<MicroSessionLocation, String> {
    let absolute = std::path::absolute(cwd_input).map_err(|error| error.to_string())?;
    let cwd = fs::canonicalize(&absolute).map_err(|error| error.to_string())?;
    let root = agent_dir
        .join("experimental")
        .join("micro-sessions")
        .join(cwd_key(&cwd));
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;

    let mut created = false;
    let path = if continue_session {
        let mut names = Vec::new();
        for entry in fs::read_dir(&root).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                continue;
            }
            if let Some(name) = entry.file_name().to_str()
                && is_session_dir_name(name)
            {
                names.push(name.to_owned());
            }
        }
        names.sort();
        let Some(newest) = names.pop() else {
            return Err(format!("No micro session exists for {}", cwd.display()));
        };
        root.join(newest)
    } else {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis();
        let path = root.join(format!("{now:013}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).map_err(|error| error.to_string())?;
        created = true;
        path
    };

    // `lockWithLossHandling(path, { realpath: false, retries: 0, onLost })`: one attempt, no waiting.
    let lock = acquire_host_lock_sync(&path.to_string_lossy(), 0)
        .map_err(|_| format!("Micro session is already open: {}", path.display()))?;
    let held = lock.held_lock();
    if let Some(held) = &held {
        held.set_on_lost(on_lost);
    }
    let id = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(MicroSessionLocation {
        id,
        path,
        cwd,
        created,
        lock: Some(lock),
        held,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_dir_names_follow_the_upstream_pattern() {
        assert!(is_session_dir_name(
            "0001760000000-0f3a1b2c-1234-4abc-8def-0123456789ab"
        ));
        assert!(!is_session_dir_name(
            "0001760000000-0f3a1b2c-1234-4abc-8def-0123456789ab.lock"
        ));
        assert!(!is_session_dir_name(
            "176000000000-0f3a1b2c-1234-4abc-8def-0123456789ab"
        ));
        assert!(!is_session_dir_name(
            "0001760000000-0F3A1B2C-1234-4ABC-8DEF-0123456789AB"
        ));
    }

    #[test]
    fn new_sessions_are_keyed_by_cwd_and_continue_picks_the_newest() {
        let agent = tempfile::tempdir().expect("agent dir");
        let cwd = tempfile::tempdir().expect("cwd");

        let missing = select_session_in(agent.path(), cwd.path(), true, None)
            .err()
            .expect("no session yet");
        assert!(missing.starts_with("No micro session exists for "));

        let mut first = select_session_in(agent.path(), cwd.path(), false, None).expect("first");
        assert!(first.created);
        assert!(first.path.is_dir());
        assert_eq!(
            first.cwd,
            fs::canonicalize(cwd.path()).expect("canonical cwd")
        );
        let key = cwd_key(&first.cwd);
        assert_eq!(key.len(), 24);
        assert_eq!(
            first.path.parent().expect("root"),
            agent
                .path()
                .join("experimental")
                .join("micro-sessions")
                .join(&key)
        );
        first.release();

        // A later directory name sorts after the first one.
        let root = first.path.parent().expect("root").to_path_buf();
        let newer = root.join("9999999999999-ffffffff-ffff-4fff-8fff-ffffffffffff");
        fs::create_dir(&newer).expect("newer session");

        let mut continued =
            select_session_in(agent.path(), cwd.path(), true, None).expect("continue");
        assert!(!continued.created);
        assert_eq!(continued.path, newer);
        assert_eq!(
            continued.id,
            "9999999999999-ffffffff-ffff-4fff-8fff-ffffffffffff"
        );

        let busy = select_session_in(agent.path(), cwd.path(), true, None)
            .err()
            .expect("locked session");
        assert!(busy.starts_with(&format!(
            "Micro session is already open: {}",
            newer.display()
        )));

        continued.release();
        let mut reopened = select_session_in(agent.path(), cwd.path(), true, None).expect("reopen");
        assert_eq!(reopened.path, newer);
        reopened.release();
    }
}
