//! Recall corpus provider: committed memory files from HEAD, excluding only the root `system/` tree
//! (pin `recall/provider.ts`).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};

use super::bm25_persist::{RecallTermErrorHandler, RecallTermPersistence};
use crate::fs::resilient;
use crate::git::{GitError, GitMemoryRepo};
use crate::memfs::parse_memory_file;

/// One recallable memory document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallDocument {
    pub path: String,
    pub description: String,
    pub body: String,
}

/// A corpus revision with its documents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallCorpus {
    pub revision: Option<String>,
    pub documents: Vec<RecallDocument>,
}

const RESERVED_SYSTEM_TREE: &str = "system/";

/// True for a `.md` path outside the reserved root `system/` tree.
///
/// The exclusion is the reserved ROOT tree only: `reference/system/deploy.md` is ordinary user memory.
pub fn is_recall_candidate_path(path: &str) -> bool {
    path.ends_with(".md") && !path.starts_with(RESERVED_SYSTEM_TREE)
}

/// Loads the corpus at HEAD; a repository without a commit yields an empty corpus.
pub fn load_recall_corpus(repo: &GitMemoryRepo) -> Result<RecallCorpus, GitError> {
    let Some(revision) = repo.head()? else {
        return Ok(RecallCorpus::default());
    };
    load_corpus_at_revision(repo, &revision, &BTreeMap::new()).map(|(corpus, _)| corpus)
}

/// A parsed file keyed by the blob it came from; `None` records an unparseable blob.
type LoadedBlob = Option<RecallDocument>;
type LoadedBlobs = BTreeMap<String, (String, LoadedBlob)>;

fn load_corpus_at_revision(
    repo: &GitMemoryRepo,
    revision: &str,
    previous: &LoadedBlobs,
) -> Result<(RecallCorpus, LoadedBlobs), GitError> {
    let entries: Vec<_> = repo
        .ls_tree_blobs(Some(revision))?
        .into_iter()
        .filter(|entry| is_recall_candidate_path(&entry.path))
        .collect();
    let wanted: Vec<String> = entries
        .iter()
        .filter(|entry| previous.get(&entry.path).map(|(oid, _)| oid) != Some(&entry.oid))
        .map(|entry| entry.oid.clone())
        .collect();
    let blobs = repo.read_blobs(&wanted)?;

    let mut loaded = BTreeMap::new();
    let mut documents = Vec::new();
    for entry in &entries {
        let reused = previous
            .get(&entry.path)
            .filter(|(oid, _)| oid == &entry.oid)
            .map(|(_, document)| document.clone());
        let document = match reused {
            Some(document) => document,
            None => {
                let Some(content) = blobs.get(&entry.oid) else {
                    return Err(GitError::Other(format!(
                        "git cat-file did not return blob {} for {}",
                        entry.oid, entry.path
                    )));
                };
                parse_recall_document(&entry.path, content)
            }
        };
        loaded.insert(entry.path.clone(), (entry.oid.clone(), document.clone()));
        if let Some(document) = document {
            documents.push(document);
        }
    }
    documents.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((
        RecallCorpus {
            revision: Some(revision.to_string()),
            documents,
        },
        loaded,
    ))
}

fn parse_recall_document(path: &str, content: &str) -> LoadedBlob {
    // Fail-closed: files without valid frontmatter are silently skipped.
    parse_memory_file(content)
        .ok()
        .map(|parsed| RecallDocument {
            path: path.to_string(),
            description: parsed.frontmatter.description,
            body: parsed.body,
        })
}

/// Resolves the corpus revision; the default reads HEAD through the repository.
pub type RecallHeadResolver =
    Arc<dyn Fn(&GitMemoryRepo) -> Result<Option<String>, GitError> + Send + Sync>;

/// Builds the persisted-terms store of one repository directory on the first corpus load, so a host
/// that never persists pays nothing. Without it every new process tokenizes the corpus.
pub type RecallTermStoreLoader = Arc<
    dyn Fn(&Path, RecallTermErrorHandler) -> Result<RecallTermPersistence, std::io::Error>
        + Send
        + Sync,
>;

/// Options for [`RecallCorpusCache`].
#[derive(Default, Clone)]
pub struct RecallCorpusCacheOptions {
    /// HEAD resolution seam; the default spawns `git rev-parse` through the repo.
    pub head: Option<RecallHeadResolver>,
    /// Share one corpus per memory repository with every other shared cache in this process. The
    /// shared state lives while any shared cache that loaded the repository is alive.
    pub shared: bool,
    /// Told when the persisted recall terms cannot be read or saved; recall itself is unaffected.
    pub on_term_store_error: Option<RecallTermErrorHandler>,
    /// Loads the persisted-terms store on the first corpus load.
    pub term_store: Option<RecallTermStoreLoader>,
}

type SharedCache = Arc<Mutex<RecallCorpusCache>>;

static SHARED_CACHES: LazyLock<Mutex<HashMap<PathBuf, Weak<Mutex<RecallCorpusCache>>>>> =
    LazyLock::new(Mutex::default);

/// One repository reached through a symlink or a trailing slash is still one corpus; keying by the
/// spelled path would give each spelling its own copy. The resilient filesystem boundary resolves it.
fn shared_key(dir: &Path) -> PathBuf {
    resilient::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn acquire_shared_cache(key: &Path, options: &RecallCorpusCacheOptions) -> SharedCache {
    let mut caches = SHARED_CACHES.lock().unwrap_or_else(PoisonError::into_inner);
    caches.retain(|_, weak| weak.strong_count() > 0);
    if let Some(live) = caches.get(key).and_then(Weak::upgrade) {
        return live;
    }
    let cache = Arc::new(Mutex::new(RecallCorpusCache::new(
        RecallCorpusCacheOptions {
            shared: false,
            ..options.clone()
        },
    )));
    caches.insert(key.to_path_buf(), Arc::downgrade(&cache));
    cache
}

/// The corpus one cache holds, kept outside the cache value so a moved cache still reports it.
#[derive(Default)]
struct CacheAccounting {
    held: Mutex<Option<Arc<RecallCorpus>>>,
}

impl CacheAccounting {
    fn hold(&self, corpus: Arc<RecallCorpus>) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(corpus);
    }

    fn release(&self) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn held(&self) -> Option<Arc<RecallCorpus>> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Every cache in the process, read only when a memory report is taken.
static LIVE_CACHES: LazyLock<Mutex<Vec<Weak<CacheAccounting>>>> = LazyLock::new(Mutex::default);

fn register_live_cache(accounting: &Arc<CacheAccounting>) {
    let mut caches = LIVE_CACHES.lock().unwrap_or_else(PoisonError::into_inner);
    caches.retain(|weak| weak.strong_count() > 0);
    caches.push(Arc::downgrade(accounting));
}

/// The recall layer's figures in the on-demand memory report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecallMemoryFigures {
    /// Caches holding a parsed corpus; a shared corpus counts once however many sessions hold it.
    pub corpus_caches: usize,
    pub documents: usize,
    /// UTF-16 size of every held document's path, description and body.
    pub document_text_bytes: usize,
    /// Term id and count arrays the BM25 index keeps for those documents.
    pub term_array_bytes: usize,
    /// Terms in the process-wide BM25 dictionary.
    pub vocabulary_terms: usize,
}

/// Reads the live caches' figures. Nothing is kept alive for the report, and a corpus several caches
/// share is counted once.
pub fn recall_memory_figures() -> RecallMemoryFigures {
    let live: Vec<Arc<CacheAccounting>> = {
        let mut caches = LIVE_CACHES.lock().unwrap_or_else(PoisonError::into_inner);
        caches.retain(|weak| weak.strong_count() > 0);
        caches.iter().filter_map(Weak::upgrade).collect()
    };
    memory_figures_for_caches(live)
}

fn memory_figures_for_caches(live: Vec<Arc<CacheAccounting>>) -> RecallMemoryFigures {
    let mut corpora: Vec<Arc<RecallCorpus>> = Vec::new();
    let mut corpus_caches = 0usize;
    for accounting in &live {
        let Some(corpus) = accounting.held() else {
            continue;
        };
        corpus_caches += 1;
        if corpora.iter().any(|held| Arc::ptr_eq(held, &corpus)) {
            continue;
        }
        corpora.push(corpus);
    }
    let mut documents = 0usize;
    let mut document_text_bytes = 0usize;
    for corpus in &corpora {
        for document in &corpus.documents {
            documents += 1;
            document_text_bytes += 2 * (document.path.encode_utf16().count()
                + document.description.encode_utf16().count()
                + document.body.encode_utf16().count());
        }
    }
    let terms = super::bm25::recall_term_figures(
        corpora
            .iter()
            .flat_map(|corpus| corpus.documents.iter()),
    );
    RecallMemoryFigures {
        corpus_caches,
        documents,
        document_text_bytes,
        term_array_bytes: terms.term_array_bytes,
        vocabulary_terms: terms.vocabulary_terms,
    }
}

struct RecallCorpusCacheEntry {
    revision: Option<String>,
    corpus: Arc<RecallCorpus>,
}

/// Caches the corpus keyed by HEAD sha; a moved HEAD invalidates the entry. A moved HEAD re-reads
/// only the blobs that changed since the last successful load of the same repository.
pub struct RecallCorpusCache {
    entry: Option<RecallCorpusCacheEntry>,
    probe: Option<(PathBuf, String)>,
    blobs: Option<(PathBuf, LoadedBlobs)>,
    terms: Option<Arc<RecallTermPersistence>>,
    /// Every repository's persisted terms this cache attached, so `settled()` covers a save started
    /// before a switch.
    attached: Vec<Arc<RecallTermPersistence>>,
    resolve_head: RecallHeadResolver,
    options: RecallCorpusCacheOptions,
    /// Shared mode only: the process-wide caches this instance keeps alive, one per repository.
    held: HashMap<PathBuf, SharedCache>,
    /// The corpus this cache holds, reported to the memory report without keeping the cache alive.
    accounting: Arc<CacheAccounting>,
}

impl Default for RecallCorpusCache {
    fn default() -> Self {
        Self::new(RecallCorpusCacheOptions::default())
    }
}

impl RecallCorpusCache {
    pub fn new(options: RecallCorpusCacheOptions) -> Self {
        let resolve_head: RecallHeadResolver = options
            .head
            .clone()
            .unwrap_or_else(|| Arc::new(|repo: &GitMemoryRepo| repo.head()));
        let accounting = Arc::new(CacheAccounting::default());
        register_live_cache(&accounting);
        Self {
            entry: None,
            probe: None,
            blobs: None,
            terms: None,
            attached: Vec::new(),
            resolve_head,
            options,
            held: HashMap::new(),
            accounting,
        }
    }

    /// The parsed documents this cache holds itself, `None` before its first load (memory report only).
    pub fn held_documents(&self) -> Option<Vec<RecallDocument>> {
        self.accounting
            .held()
            .map(|corpus| corpus.documents.clone())
    }

    /// Loads the corpus at HEAD, reusing the cached entry while the HEAD fingerprint holds.
    ///
    /// The shared corpus keeps one stable documents slice per revision, so the BM25 index cache keyed
    /// by that slice is reused across calls.
    pub fn load(&mut self, repo: &GitMemoryRepo) -> Result<Arc<RecallCorpus>, GitError> {
        if self.options.shared {
            let cache = self.shared_for(&repo.dir);
            return cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .load(repo);
        }
        let signature = head_signature(&repo.dir);
        if let Some(entry) = &self.entry
            && let Some(signature) = &signature
            && self
                .probe
                .as_ref()
                .is_some_and(|(dir, probe)| dir == &repo.dir && probe == signature)
        {
            return Ok(Arc::clone(&entry.corpus));
        }

        let revision = (self.resolve_head)(repo)?;
        let probe = signature.map(|signature| (repo.dir.clone(), signature));
        if let Some(entry) = &self.entry
            && entry.revision == revision
        {
            self.probe = probe;
            return Ok(Arc::clone(&entry.corpus));
        }

        let corpus = Arc::new(match &revision {
            None => RecallCorpus::default(),
            Some(revision) => self.load_revision(repo, revision)?,
        });
        if let Some(previous) = &self.entry {
            super::corpus_derived::unregister(&previous.corpus);
        }
        super::corpus_derived::register(&corpus);
        if revision.is_some() {
            self.accounting.hold(Arc::clone(&corpus));
        }
        self.attach_terms(repo, &corpus);
        self.entry = Some(RecallCorpusCacheEntry {
            revision,
            corpus: Arc::clone(&corpus),
        });
        self.probe = probe;
        Ok(corpus)
    }

    /// Blocks until no persisted-terms save started by this cache, or a shared cache it holds, is
    /// running.
    pub fn settled(&self) {
        for terms in &self.attached {
            terms.settled();
        }
        for cache in self.held.values() {
            cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .settled();
        }
    }

    pub fn clear(&mut self) {
        self.held.clear();
        if let Some(previous) = self.entry.take() {
            super::corpus_derived::unregister(&previous.corpus);
        }
        self.probe = None;
        self.blobs = None;
        self.accounting.release();
    }

    fn shared_for(&mut self, dir: &Path) -> SharedCache {
        let key = shared_key(dir);
        if let Some(cache) = self.held.get(&key) {
            return Arc::clone(cache);
        }
        let cache = acquire_shared_cache(&key, &self.options);
        self.held.insert(key, Arc::clone(&cache));
        cache
    }

    /// Attaches the repository's persisted terms to a freshly loaded corpus: seeds it from the store
    /// the first time and saves again after a later build that tokenized.
    fn attach_terms(&mut self, repo: &GitMemoryRepo, corpus: &Arc<RecallCorpus>) {
        if self.terms.as_ref().map(|terms| terms.dir()) != Some(repo.dir.as_path()) {
            self.terms = self.persistence_for(&repo.dir);
        }
        if let Some(terms) = &self.terms {
            terms.attach(corpus);
        }
    }

    fn persistence_for(&mut self, dir: &Path) -> Option<Arc<RecallTermPersistence>> {
        let loader = self.options.term_store.as_ref()?;
        let on_error: RecallTermErrorHandler = self
            .options
            .on_term_store_error
            .clone()
            .unwrap_or_else(|| Arc::new(|_| {}));
        match loader(dir, Arc::clone(&on_error)) {
            Ok(persistence) => {
                let persistence = Arc::new(persistence);
                self.attached.push(Arc::clone(&persistence));
                Some(persistence)
            }
            Err(error) => {
                on_error(&error);
                None
            }
        }
    }

    fn load_revision(
        &mut self,
        repo: &GitMemoryRepo,
        revision: &str,
    ) -> Result<RecallCorpus, GitError> {
        // PERF-DIVERGENCE(recall/provider.ts): upstream copies the previous blob map before reloading; this port moves it out and restores it if the load fails, avoiding a deep clone of every body. The cache ends up identical.
        let (previous, reused) = match self.blobs.take() {
            Some((dir, loaded)) if dir == repo.dir => (loaded, true),
            other => {
                self.blobs = other;
                (BTreeMap::new(), false)
            }
        };
        match load_corpus_at_revision(repo, revision, &previous) {
            Ok((corpus, loaded)) => {
                self.blobs = Some((repo.dir.clone(), loaded));
                Ok(corpus)
            }
            Err(error) => {
                if reused {
                    self.blobs = Some((repo.dir.clone(), previous));
                }
                Err(error)
            }
        }
    }
}

impl Drop for RecallCorpusCache {
    fn drop(&mut self) {
        if let Some(entry) = &self.entry {
            super::corpus_derived::unregister(&entry.corpus);
        }
    }
}

/// Filesystem fingerprint of what `git rev-parse --verify HEAD` would read; `None` means
/// "cannot be fingerprinted" and always falls open to the git call.
fn head_signature(dir: &Path) -> Option<String> {
    let git_dir = dir.join(".git");
    let git_dir_metadata = resilient::metadata(&git_dir).ok()?;
    if !git_dir_metadata.is_dir() {
        return None;
    }
    let head_path = git_dir.join("HEAD");
    let head_stat = resilient::metadata(&head_path).ok()?;
    let head = resilient::read_to_string(&head_path)
        .ok()?
        .trim()
        .to_string();
    let mut parts = vec![
        format!("head={head}"),
        format!(
            "head-stat={}:{}",
            stat_mtime_ms(&head_stat),
            head_stat.len()
        ),
    ];
    if let Some(ref_name) = head.strip_prefix("ref:") {
        let ref_name = ref_name.trim();
        if !is_valid_ref_name(ref_name) {
            return None;
        }
        let ref_path = ref_name
            .split('/')
            .fold(git_dir.clone(), |path, part| path.join(part));
        parts.push(format!("ref={}", file_signature(&ref_path)));
        parts.push(format!(
            "packed={}",
            file_signature(&git_dir.join("packed-refs"))
        ));
    }
    Some(parts.join("|"))
}

fn is_valid_ref_name(ref_name: &str) -> bool {
    let Some(rest) = ref_name.strip_prefix("refs/") else {
        return false;
    };
    !rest.is_empty()
        && ref_name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/'))
}

fn file_signature(path: &Path) -> String {
    match resilient::metadata(path) {
        Ok(metadata) => format!("{}:{}", stat_mtime_ms(&metadata), metadata.len()),
        Err(_) => "absent".to_string(),
    }
}

fn stat_mtime_ms(metadata: &std::fs::Metadata) -> f64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod tests;
