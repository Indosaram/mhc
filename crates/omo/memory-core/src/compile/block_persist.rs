//! Repository-local derived string cache. Never stores data in the working tree.
use crate::fs::resilient as fs;
use crate::recall::bm25_persist_gate::{is_recall_term_save_excluded, track_recall_term_save};
use crate::support::sha256::sha256_hex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};
pub const MEMORY_BLOCK_STORE_PREFIX: &str = "omo-memory-block-";
const FORMAT_VERSION: u32 = 1;
const KEEP_ENTRIES: usize = 32;
const STALE_TEMP: Duration = Duration::from_secs(600);
pub type StoreErrorHandler = Arc<dyn Fn(String) + Send + Sync>;
#[derive(Serialize, Deserialize)]
struct StoredEntry {
    version: u32,
    key: String,
    value: String,
}
fn entry_name(key: &str) -> String {
    format!(
        "{MEMORY_BLOCK_STORE_PREFIX}{}.json",
        sha256_hex(key.as_bytes())
    )
}
fn decode(text: &str, key: &str) -> Option<String> {
    let entry: StoredEntry = serde_json::from_str(text).ok()?;
    (entry.version == FORMAT_VERSION && entry.key == key).then_some(entry.value)
}
pub struct CompiledBlockStore {
    pub dir: PathBuf,
    on_error: StoreErrorHandler,
    writes: Mutex<Vec<JoinHandle<()>>>,
}
impl CompiledBlockStore {
    pub fn new(dir: impl Into<PathBuf>, on_error: StoreErrorHandler) -> Self {
        Self {
            dir: dir.into(),
            on_error,
            writes: Mutex::new(Vec::new()),
        }
    }
    fn git_dir(&self) -> Option<PathBuf> {
        git_dir(&self.dir)
    }
    pub fn get(&self, key: &str) -> Option<String> {
        let path = self.git_dir()?.join(entry_name(key));
        match fs::read_to_string(&path) {
            Ok(text) => decode(&text, key),
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    (self.on_error)(e.to_string());
                }
                None
            }
        }
    }
    pub fn put(&self, key: String, value: String) {
        if is_recall_term_save_excluded(&self.dir) {
            return;
        }
        // Register before spawning: restores must await every requested write, including queued ones.
        let guard = track_recall_term_save(&self.dir);
        let dir = self.dir.clone();
        let on_error = self.on_error.clone();
        let handle = std::thread::spawn(move || {
            let _guard = guard;
            if let Err(e) = write(&dir, key, value) {
                on_error(e.to_string());
            }
        });
        self.writes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(handle);
    }
    pub fn settled(&self) {
        loop {
            let handles =
                std::mem::take(&mut *self.writes.lock().unwrap_or_else(|e| e.into_inner()));
            if handles.is_empty() {
                return;
            }
            for handle in handles {
                if handle.join().is_err() {
                    (self.on_error)("compiled store writer panicked".into());
                }
            }
        }
    }
}
fn git_dir(dir: &Path) -> Option<PathBuf> {
    let git = dir.join(".git");
    fs::metadata(&git).ok()?.is_dir().then_some(git)
}
fn write(dir: &Path, key: String, value: String) -> std::io::Result<()> {
    let Some(git) = git_dir(dir) else {
        return Ok(());
    };
    if is_recall_term_save_excluded(dir) {
        return Ok(());
    }
    let path = git.join(entry_name(&key));
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let operation = (|| {
        use std::io::Write;
        let bytes = serde_json::to_vec(&StoredEntry {
            version: FORMAT_VERSION,
            key,
            value,
        })?;
        let mut file = fs::create_exclusive(&temp)?;
        file.write_all(&bytes)?;
        drop(file);
        if is_recall_term_save_excluded(dir) {
            fs::remove_file(&temp)?;
            return Ok(());
        }
        fs::rename(&temp, &path)?;
        prune(&git)
    })();
    if operation.is_err() {
        match fs::remove_file(&temp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    operation
}
fn prune(git: &Path) -> std::io::Result<()> {
    let mut entries = Vec::new();
    for name in fs::read_dir_names(git)? {
        if !name.starts_with(MEMORY_BLOCK_STORE_PREFIX) {
            continue;
        }
        let path = git.join(&name);
        let Ok(info) = fs::metadata(&path) else {
            continue;
        };
        let modified = info.modified()?;
        if name.ends_with(".tmp") {
            if SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
                > STALE_TEMP
            {
                remove_if_present(&path)?;
            }
        } else {
            entries.push((modified, path));
        }
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, path) in entries.into_iter().skip(KEEP_ENTRIES) {
        remove_if_present(&path)?;
    }
    Ok(())
}
fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_corruption_restore_and_git_pointer() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(&t.path().join(".git")).unwrap();
        let store = CompiledBlockStore::new(t.path(), Arc::new(|e| panic!("{e}")));
        store.put("key".into(), "value".into());
        store.settled();
        assert_eq!(store.get("key").as_deref(), Some("value"));
        fs::write(&t.path().join(".git").join(entry_name("key")), b"bad json").unwrap();
        assert!(store.get("key").is_none());
        crate::recall::bm25_persist_gate::with_recall_term_saves_excluded(t.path(), || {
            store.put("excluded".into(), "no".into());
        });
        store.settled();
        assert!(store.get("excluded").is_none());
        let t2 = tempfile::tempdir().unwrap();
        fs::write(&t2.path().join(".git"), b"gitdir: elsewhere").unwrap();
        let store = CompiledBlockStore::new(t2.path(), Arc::new(|e| panic!("{e}")));
        store.put("key".into(), "value".into());
        store.settled();
        assert!(store.get("key").is_none());
    }
}
