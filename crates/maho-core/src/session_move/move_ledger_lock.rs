//! Held-lock tracking for cross-process move record writes.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
#[derive(Default)]
pub struct MoveRecordLocks {
    held: Mutex<HashSet<PathBuf>>,
}
pub struct HeldMoveRecord<'a> {
    locks: &'a MoveRecordLocks,
    path: PathBuf,
    _file: std::fs::File,
}
impl Drop for HeldMoveRecord<'_> {
    fn drop(&mut self) {
        self.locks
            .held
            .lock()
            .expect("move locks")
            .remove(&self.path);
    }
}
impl MoveRecordLocks {
    pub fn assert_held(&self, path: &Path) -> Result<(), String> {
        if self.held.lock().expect("move locks").contains(path) {
            Ok(())
        } else {
            Err(format!("Move record {} is not locked", path.display()))
        }
    }
    pub async fn hold(&self, path: &Path) -> Result<HeldMoveRecord<'_>, String> {
        let lock_path = PathBuf::from(format!("{}.record-lock", path.display()));
        let file = tokio::task::spawn_blocking(move || -> Result<std::fs::File, String> {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(lock_path)
                .map_err(|e| e.to_string())?;
            file.lock().map_err(|e| e.to_string())?;
            Ok(file)
        })
        .await
        .map_err(|e| e.to_string())??;
        self.held
            .lock()
            .expect("move locks")
            .insert(path.to_owned());
        Ok(HeldMoveRecord {
            locks: self,
            path: path.to_owned(),
            _file: file,
        })
    }
}
