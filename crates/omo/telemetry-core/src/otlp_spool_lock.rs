use crate::otlp_spool_names::lock_name;
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::{Mutex, OnceLock, PoisonError},
};
#[derive(Debug, Clone)]
pub struct LockOwner {
    pub pid: u32,
    pub boot_id: String,
}
static HELD: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
fn held() -> &'static Mutex<HashSet<String>> {
    HELD.get_or_init(|| Mutex::new(HashSet::new()))
}
fn busy(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
    ) || (cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied)
}
pub fn try_lock_sync(dir: &Path, source: &str, owner: &LockOwner) -> io::Result<Option<String>> {
    let path = dir.join(lock_name(source));
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(f) => f,
        Err(e) if busy(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let token = format!("{} {} {}", owner.pid, owner.boot_id, uuid::Uuid::new_v4());
    if let Err(error) = file.write_all(token.as_bytes()) {
        drop(file);
        return match fs::remove_file(path) {
            Ok(()) => Err(error),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(error),
            Err(e) => Err(io::Error::new(
                error.kind(),
                format!("{error}; lock cleanup failed: {e}"),
            )),
        };
    }
    drop(file);
    held()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(token.clone());
    Ok(Some(token))
}
pub async fn try_lock(dir: &Path, source: &str, owner: &LockOwner) -> io::Result<Option<String>> {
    let (dir, source, owner) = (dir.to_owned(), source.to_owned(), owner.clone());
    tokio::task::spawn_blocking(move || try_lock_sync(&dir, &source, &owner))
        .await
        .map_err(io::Error::other)?
}
pub fn holds_lock_sync(dir: &Path, source: &str, token: &str) -> io::Result<bool> {
    match fs::read_to_string(dir.join(lock_name(source))) {
        Ok(s) => Ok(s == token),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
pub async fn holds_lock(dir: &Path, source: &str, token: &str) -> io::Result<bool> {
    match tokio::fs::read_to_string(dir.join(lock_name(source))).await {
        Ok(s) => Ok(s == token),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
pub fn unlock(
    dir: &Path,
    source: &str,
    token: &str,
    on_left: Option<&(dyn Fn(&io::Error) + Send + Sync)>,
) {
    held()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(token);
    let result = holds_lock_sync(dir, source, token).and_then(|ours| {
        if ours {
            fs::remove_file(dir.join(lock_name(source)))
        } else {
            Ok(())
        }
    });
    if let Err(e) = result
        && e.kind() != io::ErrorKind::NotFound
            && let Some(report) = on_left {
                report(&e);
            }
}
pub struct LockLimits<'a> {
    pub lease_ms: u64,
    pub max_age_ms: u64,
    pub is_pid_alive: &'a dyn Fn(u32) -> bool,
    pub owner: &'a LockOwner,
}
pub fn is_abandoned_lock(
    dir: &Path,
    name: &str,
    age_ms: u64,
    limits: &LockLimits<'_>,
) -> io::Result<bool> {
    if age_ms < limits.lease_ms {
        return Ok(false);
    }
    if age_ms > limits.max_age_ms {
        return Ok(true);
    }
    let text = match fs::read_to_string(dir.join(name)) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut parts = text.split(' ');
    let Some(pid) = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|p| *p > 0)
    else {
        return Ok(true);
    };
    let boot = parts.next();
    if pid == limits.owner.pid {
        return Ok(boot != Some(limits.owner.boot_id.as_str())
            || !held()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&text));
    }
    Ok(!(limits.is_pid_alive)(pid))
}
pub fn throw_after_roll_back(dir: &Path, from: &str, to: &str, error: io::Error) -> io::Error {
    let result = match fs::symlink_metadata(dir.join(from)) {
        Ok(_) => fs::remove_file(dir.join(to)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::rename(dir.join(to), dir.join(from)),
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => error,
        Err(e) if e.kind() == io::ErrorKind::NotFound => error,
        Err(e) => io::Error::new(error.kind(), format!("{error}; spool rollback failed: {e}")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn competing_lock_and_replaced_owner_are_guarded() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let owner = LockOwner {
            pid: 1,
            boot_id: "a".repeat(32),
        };
        let token = try_lock_sync(dir.path(), "source", &owner)?.expect("first lock");
        assert!(try_lock_sync(dir.path(), "source", &owner)?.is_none());
        fs::write(dir.path().join("source.lock"), "replacement")?;
        assert!(!holds_lock_sync(dir.path(), "source", &token)?);
        unlock(dir.path(), "source", &token, None);
        assert_eq!(
            fs::read_to_string(dir.path().join("source.lock"))?,
            "replacement"
        );
        Ok(())
    }
    #[test]
    fn own_live_token_is_not_abandoned() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let owner = LockOwner {
            pid: 1,
            boot_id: "a".repeat(32),
        };
        let token = try_lock_sync(dir.path(), "source", &owner)?.expect("lock");
        let limits = LockLimits {
            lease_ms: 5,
            max_age_ms: 100,
            is_pid_alive: &|_| true,
            owner: &owner,
        };
        assert!(!is_abandoned_lock(dir.path(), "source.lock", 10, &limits)?);
        held()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&token);
        assert!(is_abandoned_lock(dir.path(), "source.lock", 10, &limits)?);
        Ok(())
    }
}
