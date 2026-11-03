//! Per-real-path move queue and cross-process holder admission fence.
use crate::session_sidecar_store::serialize_by_key;
use std::future::Future;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// A caller's check that runs inside the move queue before anything is read; an error refuses the step.
/// Upstream lets it return a promise; every Rust guard is an in-memory check, so it stays synchronous and
/// does no I/O on the move path.
pub type MoveGuard<'a> = &'a dyn Fn() -> Result<(), String>;

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

/// Synchronous form, for callers outside the move path (no I/O beyond `canonicalize`).
pub fn real_session_path(path: &Path) -> PathBuf {
    let absolute = absolute(path);
    // Canonicalize the deepest existing ancestor so the result is identical before and after the
    // path (or its parent directories) is created; otherwise symlinked temp roots (macOS /var) and
    // Windows short names / verbatim prefixes make the same target spell two different ways.
    let mut tail = Vec::new();
    let mut base = absolute.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            return tail.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                base = parent;
            }
            _ => return absolute,
        }
    }
}

/// senpi `realSessionPath`: resolved with asynchronous I/O; never fails.
pub async fn real_session_path_async(path: &Path) -> PathBuf {
    let absolute = absolute(path);
    let mut tail = Vec::new();
    let mut base = absolute.clone();
    loop {
        if let Ok(real) = tokio::fs::canonicalize(&base).await {
            return tail
                .iter()
                .rev()
                .fold(real, |acc: PathBuf, part| acc.join(part));
        }
        match (base.parent().map(Path::to_owned), base.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_owned());
                base = parent;
            }
            _ => return absolute,
        }
    }
}

/// Resolutions of one path settle one after another, in call order, so two steps asked for in order enter the
/// per-file queue in that order even though resolving the real path is asynchronous. Each path has its own
/// chain, so one stuck realpath never delays another file's; a chain is dropped once it is idle.
pub async fn ordered_real_path(path: &Path) -> PathBuf {
    let key = format!("session-move-realpath:{}", absolute(path).display());
    serialize_by_key(&key, real_session_path_async(path)).await
}

fn lock_key(resolved: &Path) -> String {
    format!("session-move:{}", resolved.display())
}

pub async fn move_lock_key(file: &Path) -> String {
    lock_key(&ordered_real_path(file).await)
}

struct AdmissionFence(PathBuf);
impl Drop for AdmissionFence {
    fn drop(&mut self) {
        // Drop cannot await; unlinking one marker is a single metadata operation.
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Runs the sender's steps of a move (export, finish) one at a time per session file, across processes too.
/// `guard` runs first inside the queue, so a caller's own check sees the state the step will act on.
pub async fn with_session_move_lock<T>(
    file: &Path,
    step: impl Future<Output = Result<T, String>>,
    guard: Option<MoveGuard<'_>>,
) -> Result<T, String> {
    let path = ordered_real_path(file).await;
    serialize_by_key(&lock_key(&path), async {
        if let Some(guard) = guard {
            guard()?;
        }
        let values = super::entries(&path).await;
        let Some(id) = super::ownership::header_session_id(&values) else {
            return step.await;
        };
        let directory = crate::schedule::runner_lease::session_holders_dir(&path, id)
            .map_err(|e| e.to_string())?;
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|e| e.to_string())?;
        let lock_path = directory.join("move-admission");
        let lock = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .await
            .map_err(|e| e.to_string())?
            .into_std()
            .await;
        lock.try_lock()
            .map_err(|e| format!("session move lock busy: {e}"))?;
        let marker = directory.join("move.lock");
        let mut output = tokio::fs::File::create(&marker)
            .await
            .map_err(|e| e.to_string())?;
        output
            .write_all(format!("{{\"pid\":{}}}\n", std::process::id()).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        drop(output);
        let _fence = AdmissionFence(marker);
        let mut records = tokio::fs::read_dir(&directory)
            .await
            .map_err(|e| e.to_string())?;
        while let Some(record) = records.next_entry().await.map_err(|e| e.to_string())? {
            let name = record.file_name().to_string_lossy().into_owned();
            if let Some(pid) = name
                .strip_suffix(".json")
                .and_then(|name| name.parse::<u32>().ok())
                && crate::schedule::runner_lease::native_pid_alive(pid)
            {
                return Err(format!("ESESSIONHELD: {} is held by {pid}", file.display()));
            }
        }
        let result = step.await;
        drop(_fence);
        drop(lock);
        result
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn async_and_sync_real_paths_agree_before_and_after_creation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("a/b.jsonl");
        assert_eq!(
            real_session_path_async(&missing).await,
            real_session_path(&missing)
        );
        std::fs::create_dir_all(missing.parent().expect("parent")).expect("dir");
        std::fs::write(&missing, "").expect("write");
        assert_eq!(
            real_session_path_async(&missing).await,
            real_session_path(&missing)
        );
        assert_eq!(
            move_lock_key(&missing).await,
            format!("session-move:{}", real_session_path(&missing).display())
        );
    }

    #[tokio::test]
    async fn resolutions_of_one_path_settle_in_call_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("a.jsonl");
        std::fs::write(&file, "").expect("write");
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let first = {
            let order = std::sync::Arc::clone(&order);
            let file = file.clone();
            async move {
                ordered_real_path(&file).await;
                order.lock().expect("order").push(1);
            }
        };
        let second = {
            let order = std::sync::Arc::clone(&order);
            let file = file.clone();
            async move {
                ordered_real_path(&file).await;
                order.lock().expect("order").push(2);
            }
        };
        tokio::join!(first, second);
        assert_eq!(*order.lock().expect("order"), vec![1, 2]);
    }
}
