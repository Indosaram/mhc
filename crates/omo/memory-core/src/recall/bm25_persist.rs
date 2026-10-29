//! Persisted recall terms (pin `recall/bm25-persist.ts`): a new process takes each note's BM25 terms
//! from a store in the memory repo's git dir instead of tokenizing the whole corpus again. The store is
//! a derived cache and never a source of truth:
//!   - keyed by the SHA-1 of the exact text a note's terms are made from, under a fingerprint of the
//!     tokenizer, so a stored entry is either the note's terms or ignored; ranking is unchanged;
//!   - lives in `.git/`, so it is never committed or synced;
//!   - a missing, unreadable, corrupt or foreign store reads as empty and is rewritten;
//!   - written atomically (temp file + rename) after a build that had to tokenize, off the prompt path.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, SystemTime};

use super::bm25::{
    RECALL_TERM_ACCESS, RecallTermAccess, StoredDocumentTerms, document_key,
    on_recall_documents_encoded, recall_document_terms, sha1_digest,
};
use super::bm25_persist_gate::{is_recall_term_save_excluded, track_recall_term_save};
use super::bm25_store::{RecallTermStore, decode_recall_term_store, encode_recall_term_store};
use super::bm25_terms_transfer::{seed_recall_document_terms, snapshot_recall_document_terms};
use super::provider::{RecallCorpus, RecallDocument};
use crate::fs::resilient;

pub use super::bm25_persist_gate::with_recall_term_saves_excluded;

pub const RECALL_TERM_STORE_NAME: &str = "omo-recall-bm25-terms.bin";

/// Bump with any change to how recall turns text into terms; the probes below may not exercise it.
const TOKENIZER_VERSION: u32 = 1;
/// A temp file this old belongs to a save whose process died before its rename.
const STALE_TEMP_MS: u64 = 10 * 60_000;

/// Texts whose terms cover every tokenizer rule: CJK runs and bigrams, stand-alone Han characters,
/// NFD Hangul and full-width Latin under NFKC, and English suffix folding.
const FINGERPRINT_PROBES: [&str; 3] = [
    "Deploying the rollback checklists; libraries, classes, nationalization 2026",
    "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af} 데이터베이스를 배포했다 ＡＢＣ１２３",
    "東京の記憶管理 データベース 中文检索 ラーメン",
];

/// Receives every error the persisted store hits; recall itself is unaffected.
pub type RecallTermErrorHandler = Arc<dyn Fn(&std::io::Error) + Send + Sync>;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn tokenizer_fingerprint() -> &'static str {
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        let probes: Vec<serde_json::Value> = FINGERPRINT_PROBES
            .iter()
            .map(|body| {
                let terms = recall_document_terms(&RecallDocument {
                    path: String::new(),
                    description: String::new(),
                    body: (*body).to_string(),
                });
                serde_json::json!({ "terms": terms.terms, "length": terms.length })
            })
            .collect();
        let text =
            serde_json::json!({ "version": TOKENIZER_VERSION, "probes": probes }).to_string();
        hex(&sha1_digest(text.as_bytes()))
    })
}

fn is_missing(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::NotFound
}

/// The store path, or `None` when `.git` is not a directory (a worktree or submodule checkout).
fn store_path(dir: &Path) -> Option<PathBuf> {
    let git_dir = dir.join(".git");
    match resilient::metadata(&git_dir) {
        Ok(metadata) if metadata.is_dir() => Some(git_dir.join(RECALL_TERM_STORE_NAME)),
        _ => None,
    }
}

#[derive(Default)]
struct SaveState {
    writing: bool,
    queued: Option<Weak<RecallCorpus>>,
}

struct Inner {
    dir: PathBuf,
    on_error: RecallTermErrorHandler,
    access: RecallTermAccess,
    primed: Mutex<bool>,
    save: Mutex<SaveState>,
    settled: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One memory repository's persisted terms; one instance per repository directory.
pub struct RecallTermPersistence {
    inner: Arc<Inner>,
}

impl RecallTermPersistence {
    pub fn new(dir: &Path, on_error: RecallTermErrorHandler) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir: dir.to_path_buf(),
                on_error,
                access: RECALL_TERM_ACCESS,
                primed: Mutex::new(false),
                save: Mutex::new(SaveState::default()),
                settled: Condvar::new(),
            }),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// Seeds the corpus' terms from the store the first time this repository loads, and saves the
    /// terms again after any later build of the corpus that had to tokenize one of its documents.
    /// The corpus must be the one a `RecallCorpusCache` registered.
    pub fn attach(&self, corpus: &Arc<RecallCorpus>) {
        let inner = Arc::clone(&self.inner);
        let weak = Arc::downgrade(corpus);
        on_recall_documents_encoded(
            &corpus.documents,
            Arc::new(move || Inner::save(&inner, weak.clone())),
        );
        {
            let mut primed = lock(&self.inner.primed);
            if *primed {
                return;
            }
            *primed = true;
        }
        let Some(path) = store_path(&self.inner.dir) else {
            return;
        };
        let bytes = match resilient::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                if !is_missing(&error) {
                    (self.inner.on_error)(&error);
                }
                return;
            }
        };
        self.inner.seed(&corpus.documents, &bytes);
    }

    /// Blocks until no save is running or queued; saves never fail the caller.
    pub fn settled(&self) {
        let mut state = lock(&self.inner.save);
        while state.writing {
            state = self
                .inner
                .settled
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Inner {
    fn seed(&self, documents: &[RecallDocument], bytes: &[u8]) {
        let Some(store) = decode_recall_term_store(bytes) else {
            return;
        };
        if store.fingerprint != tokenizer_fingerprint() {
            return;
        }
        let mut entries: Vec<(&RecallDocument, StoredDocumentTerms)> = Vec::new();
        for document in documents {
            if let Some(stored) = store.documents.get(&document_key(document)) {
                entries.push((document, stored.clone()));
            }
        }
        seed_recall_document_terms(&self.access, &store.terms, entries);
    }

    fn save(self: &Arc<Self>, corpus: Weak<RecallCorpus>) {
        if is_recall_term_save_excluded(&self.dir) {
            return;
        }
        {
            let mut state = lock(&self.save);
            state.queued = Some(corpus);
            if state.writing {
                return;
            }
            state.writing = true;
        }
        let tracked = track_recall_term_save(&self.dir);
        let worker = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("recall-term-store-save".to_string())
            .spawn(move || {
                worker.drain();
                drop(tracked);
            });
        if let Err(error) = spawned {
            (self.on_error)(&error);
            self.finish();
        }
    }

    fn finish(&self) {
        lock(&self.save).writing = false;
        self.settled.notify_all();
    }

    fn drain(&self) {
        loop {
            let queued = {
                let mut state = lock(&self.save);
                match state.queued.take() {
                    Some(queued) => queued,
                    None => {
                        state.writing = false;
                        self.settled.notify_all();
                        return;
                    }
                }
            };
            if is_recall_term_save_excluded(&self.dir) {
                continue;
            }
            let Some(corpus) = queued.upgrade() else {
                continue;
            };
            if let Err(error) = self.write(&corpus.documents) {
                (self.on_error)(&error);
            }
        }
    }

    fn write(&self, documents: &[RecallDocument]) -> std::io::Result<()> {
        let snapshot = snapshot_recall_document_terms(&self.access, documents);
        let (Some(snapshot), Some(path)) = (snapshot, store_path(&self.dir)) else {
            return Ok(());
        };
        let mut keyed = BTreeMap::new();
        for (document, terms) in documents.iter().zip(snapshot.documents) {
            keyed.insert(document_key(document), terms);
        }
        let bytes = encode_recall_term_store(&RecallTermStore {
            fingerprint: tokenizer_fingerprint().to_string(),
            terms: snapshot.terms,
            documents: keyed,
        });
        if is_recall_term_save_excluded(&self.dir) {
            return Ok(());
        }
        sweep_stale_temps(&path)?;
        let temp = PathBuf::from(format!(
            "{}.{}.{}.tmp",
            path.display(),
            std::process::id(),
            unique_suffix()
        ));
        let landed = resilient::write(&temp, &bytes).and_then(|()| {
            if is_recall_term_save_excluded(&self.dir) {
                remove_quietly(&temp);
                return Ok(());
            }
            resilient::rename(&temp, &path)
        });
        if landed.is_err() {
            remove_quietly(&temp);
        }
        landed
    }
}

/// A random-enough UUID-shaped suffix: SHA-1 over the clock, a counter and the thread id.
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let seed = format!(
        "{nanos}:{}:{:?}",
        COUNTER.fetch_add(1, Ordering::Relaxed),
        std::thread::current().id()
    );
    let digest = hex(&sha1_digest(seed.as_bytes()));
    format!(
        "{}-{}-{}-{}-{}",
        &digest[0..8],
        &digest[8..12],
        &digest[12..16],
        &digest[16..20],
        &digest[20..32]
    )
}

fn remove_quietly(path: &Path) {
    let _ = resilient::remove_file(path);
}

/// Removes temp files of saves that died before their rename; a live save's temp file is younger.
fn sweep_stale_temps(path: &Path) -> std::io::Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(());
    };
    let prefix = format!("{}.", name.to_string_lossy());
    for entry in resilient::read_dir_names(dir)? {
        if !entry.starts_with(&prefix) || !entry.ends_with(".tmp") {
            continue;
        }
        let temp = dir.join(&entry);
        let Ok(metadata) = resilient::metadata(&temp) else {
            continue;
        };
        let age = metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok());
        if age.is_some_and(|age| age > Duration::from_millis(STALE_TEMP_MS)) {
            remove_quietly(&temp);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "bm25_persist_tests.rs"]
mod tests;
