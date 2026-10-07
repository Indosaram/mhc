//! Session recall ledger (latest `recall/ledger.ts`).
//!
//! Tracks which memory paths were already surfaced in a session so the same hint never repeats. One
//! JSON file per session under the ledger directory; reads fail closed (missing or malformed yields
//! an empty set) and writes are atomic `.tmp` -> rename at mode `0o600`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::fs::resilient;
use crate::support::time::now_iso;

/// Ledger file schema version.
pub const RECALL_LEDGER_VERSION: u32 = 1;

const SESSION_FILENAME_MAX_LENGTH: usize = 80;
const MAX_CACHED_LEDGERS: usize = 32;

static LEDGER_DISK_READS: AtomicU64 = AtomicU64::new(0);

/// Diagnostic: session-ledger files parsed from disk since process start (asserted in tests).
pub fn recall_ledger_disk_reads() -> u64 {
    LEDGER_DISK_READS.load(Ordering::Relaxed)
}

/// One path marked as surfaced, with the content hash recorded at that moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallSurfacedEntry {
    pub path: String,
    pub hash: String,
}

/// One surfaced path's recorded hash and timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallLedgerEntry {
    pub hash: String,
    pub at: String,
}

/// The versioned per-session ledger file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallLedgerFile {
    pub version: u32,
    pub surfaced: BTreeMap<String, RecallLedgerEntry>,
}

impl Default for RecallLedgerFile {
    fn default() -> Self {
        Self {
            version: RECALL_LEDGER_VERSION,
            surfaced: BTreeMap::new(),
        }
    }
}

/// Windows-safe, colon-free session filename component. Path separators, glob metacharacters and
/// control characters collapse to dashes; degenerate results (`.`, `..`, empty) fall back to
/// `session`.
pub fn sanitize_session_filename(session_id: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in session_id.chars() {
        let safe = ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-';
        if safe {
            if pending_dash {
                out.push('-');
                pending_dash = false;
            }
            out.push(ch);
        } else {
            pending_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let mut sliced: String = trimmed.chars().take(SESSION_FILENAME_MAX_LENGTH).collect();
    while sliced.ends_with('-') {
        sliced.pop();
    }
    if sliced.is_empty() || sliced == "." || sliced == ".." {
        return "session".to_string();
    }
    sliced
}

#[derive(Clone)]
struct LedgerCacheEntry {
    mtime_ms: f64,
    size: u64,
    file: RecallLedgerFile,
}

static LEDGER_CACHE: Mutex<Option<Vec<(String, LedgerCacheEntry)>>> = Mutex::new(None);

fn cache_get(path: &str) -> Option<LedgerCacheEntry> {
    let guard = LEDGER_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .as_ref()?
        .iter()
        .find(|(key, _)| key == path)
        .map(|(_, entry)| entry.clone())
}

fn cache_remove(path: &str) {
    let mut guard = LEDGER_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entries) = guard.as_mut() {
        entries.retain(|(key, _)| key != path);
    }
}

fn remember_ledger(path: &str, mtime_ms: f64, size: u64, file: RecallLedgerFile) {
    let mut guard = LEDGER_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entries = guard.get_or_insert_with(Vec::new);
    entries.retain(|(key, _)| key != path);
    entries.push((
        path.to_string(),
        LedgerCacheEntry {
            mtime_ms,
            size,
            file,
        },
    ));
    while entries.len() > MAX_CACHED_LEDGERS {
        entries.remove(0);
    }
}

/// Per-session recall ledger backed by one JSON file under `dir`.
pub struct RecallLedger {
    dir: PathBuf,
}

impl RecallLedger {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The set of paths already surfaced in this session.
    pub fn surfaced_paths(&self, session_id: &str) -> BTreeSet<String> {
        self.read(session_id).surfaced.into_keys().collect()
    }

    /// Marks paths as surfaced, merging into the existing file and updating the parse cache.
    pub fn mark_surfaced(
        &self,
        session_id: &str,
        entries: &[RecallSurfacedEntry],
    ) -> std::io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let current = self.read(session_id);
        let mut surfaced = current.surfaced;
        let at = now_iso();
        for entry in entries {
            surfaced.insert(
                entry.path.clone(),
                RecallLedgerEntry {
                    hash: entry.hash.clone(),
                    at: at.clone(),
                },
            );
        }

        let target = self.session_file_path(session_id);
        resilient::create_dir_all(&self.dir)?;
        let temporary = temporary_path(&target);
        let written = RecallLedgerFile {
            version: RECALL_LEDGER_VERSION,
            surfaced,
        };
        let body = format!("{}\n", serde_json::to_string_pretty(&written)?);
        write_private(&temporary, body.as_bytes())?;
        resilient::rename(&temporary, &target)?;

        let key = path_key(&target);
        match resilient::metadata(&target) {
            Ok(meta) => remember_ledger(&key, stat_mtime_ms(&meta), meta.len(), written),
            Err(_) => cache_remove(&key),
        }
        Ok(())
    }

    fn session_file_path(&self, session_id: &str) -> PathBuf {
        self.dir
            .join(format!("{}.json", sanitize_session_filename(session_id)))
    }

    fn read(&self, session_id: &str) -> RecallLedgerFile {
        let target = self.session_file_path(session_id);
        let key = path_key(&target);
        let (mtime_ms, size) = match resilient::metadata(&target) {
            Ok(meta) => (stat_mtime_ms(&meta), meta.len()),
            Err(_) => {
                cache_remove(&key);
                return RecallLedgerFile::default();
            }
        };
        if let Some(cached) = cache_get(&key)
            && cached.mtime_ms == mtime_ms
            && cached.size == size
        {
            return cached.file;
        }

        // The stat is taken BEFORE the read: a write landing in between yields content newer than the
        // recorded stat, which the next stat comparison re-reads.
        let raw = match resilient::read_to_string(&target) {
            Ok(raw) => {
                LEDGER_DISK_READS.fetch_add(1, Ordering::Relaxed);
                raw
            }
            Err(_) => {
                cache_remove(&key);
                return RecallLedgerFile::default();
            }
        };
        let file = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .as_ref()
            .and_then(parse_ledger_value)
            .unwrap_or_default();
        remember_ledger(&key, mtime_ms, size, file.clone());
        file
    }
}

fn parse_ledger_value(value: &serde_json::Value) -> Option<RecallLedgerFile> {
    let record = value.as_object()?;
    if record.get("version")?.as_u64()? != u64::from(RECALL_LEDGER_VERSION) {
        return None;
    }
    let surfaced = record.get("surfaced")?.as_object()?;
    let mut map = BTreeMap::new();
    for (path, entry) in surfaced {
        let entry = entry.as_object()?;
        let hash = entry
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let at = entry
            .get("at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        map.insert(path.clone(), RecallLedgerEntry { hash, at });
    }
    Some(RecallLedgerFile {
        version: RECALL_LEDGER_VERSION,
        surfaced: map,
    })
}

fn temporary_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session".to_string());
    target.with_file_name(format!("{name}.tmp-{}", std::process::id()))
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn stat_mtime_ms(meta: &std::fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Writes a private (mode `0o600`) file, mirroring `writeFile(path, data, { mode: 0o600 })`.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
