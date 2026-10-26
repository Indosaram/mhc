//! Per-real-path move queue and cross-process holder admission fence.
use crate::session_sidecar_store::serialize_by_key;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn real_session_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
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
pub fn move_lock_key(file: &Path) -> String {
    format!("session-move:{}", real_session_path(file).display())
}
struct AdmissionFence(PathBuf);
impl Drop for AdmissionFence {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
pub async fn with_session_move_lock<T>(
    file: &Path,
    step: impl Future<Output = Result<T, String>>,
    guard: Option<&dyn Fn() -> Result<(), String>>,
) -> Result<T, String> {
    let path = real_session_path(file);
    serialize_by_key(&move_lock_key(&path), async {
        if let Some(guard) = guard {
            guard()?;
        }
        let values = super::entries(&path);
        let Some(id) = super::ownership::header_session_id(&values) else {
            return step.await;
        };
        let directory = crate::schedule::runner_lease::session_holders_dir(&path, id)
            .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let lock_path = directory.join("move-admission");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .map_err(|e| e.to_string())?;
        lock.try_lock()
            .map_err(|e| format!("session move lock busy: {e}"))?;
        let marker = directory.join("move.lock");
        let mut output = std::fs::File::create(&marker).map_err(|e| e.to_string())?;
        writeln!(output, "{{\"pid\":{}}}", std::process::id()).map_err(|e| e.to_string())?;
        let _fence = AdmissionFence(marker);
        for record in std::fs::read_dir(&directory).map_err(|e| e.to_string())? {
            let record = record.map_err(|e| e.to_string())?;
            let name = record.file_name().to_string_lossy().into_owned();
            if let Some(pid) = name
                .strip_suffix(".json")
                .and_then(|name| name.parse::<u32>().ok())
                && crate::schedule::runner_lease::native_pid_alive(pid) {
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
