//! Recall corpus provider: committed memory files from HEAD, excluding only the root `system/` tree
//! (pin `recall/provider.ts`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

fn load_corpus_at_revision(
    repo: &GitMemoryRepo,
    revision: &str,
    previous: &BTreeMap<String, (String, LoadedBlob)>,
) -> Result<(RecallCorpus, BTreeMap<String, (String, LoadedBlob)>), GitError> {
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
    parse_memory_file(content).ok().map(|parsed| RecallDocument {
        path: path.to_string(),
        description: parsed.frontmatter.description,
        body: parsed.body,
    })
}

/// Resolves the corpus revision; the default reads HEAD through the repository.
pub type RecallHeadResolver =
    Arc<dyn Fn(&GitMemoryRepo) -> Result<Option<String>, GitError> + Send + Sync>;

/// Options for [`RecallCorpusCache`].
#[derive(Default)]
pub struct RecallCorpusCacheOptions {
    /// HEAD resolution seam; the default spawns `git rev-parse` through the repo.
    pub head: Option<RecallHeadResolver>,
}

struct RecallCorpusCacheEntry {
    revision: Option<String>,
    corpus: RecallCorpus,
}

/// Caches the corpus keyed by HEAD sha; a moved HEAD invalidates the entry. A moved HEAD re-reads
/// only the blobs that changed since the last successful load of the same repository.
pub struct RecallCorpusCache {
    entry: Option<RecallCorpusCacheEntry>,
    probe: Option<(PathBuf, String)>,
    blobs: Option<(PathBuf, BTreeMap<String, (String, LoadedBlob)>)>,
    resolve_head: RecallHeadResolver,
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
            .unwrap_or_else(|| Arc::new(|repo: &GitMemoryRepo| repo.head()));
        Self {
            entry: None,
            probe: None,
            blobs: None,
            resolve_head,
        }
    }

    /// Loads the corpus at HEAD, reusing the cached entry while the HEAD fingerprint holds.
    pub fn load(&mut self, repo: &GitMemoryRepo) -> Result<RecallCorpus, GitError> {
        let signature = head_signature(&repo.dir);
        if let Some(entry) = &self.entry
            && let Some(signature) = &signature
            && self
                .probe
                .as_ref()
                .is_some_and(|(dir, probe)| dir == &repo.dir && probe == signature)
        {
            return Ok(entry.corpus.clone());
        }

        let revision = (self.resolve_head)(repo)?;
        let probe = signature.map(|signature| (repo.dir.clone(), signature));
        if let Some(entry) = &self.entry
            && entry.revision == revision
        {
            self.probe = probe;
            return Ok(entry.corpus.clone());
        }

        let corpus = match &revision {
            None => RecallCorpus::default(),
            Some(revision) => self.load_revision(repo, revision)?,
        };
        self.entry = Some(RecallCorpusCacheEntry {
            revision,
            corpus: corpus.clone(),
        });
        self.probe = probe;
        Ok(corpus)
    }

    pub fn clear(&mut self) {
        self.entry = None;
        self.probe = None;
        self.blobs = None;
    }

    fn load_revision(&mut self, repo: &GitMemoryRepo, revision: &str) -> Result<RecallCorpus, GitError> {
        let previous = match &self.blobs {
            Some((dir, loaded)) if dir == &repo.dir => loaded.clone(),
            _ => BTreeMap::new(),
        };
        let (corpus, loaded) = load_corpus_at_revision(repo, revision, &previous)?;
        self.blobs = Some((repo.dir.clone(), loaded));
        Ok(corpus)
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
    let head = resilient::read_to_string(&head_path).ok()?.trim().to_string();
    let mut parts = vec![
        format!("head={head}"),
        format!("head-stat={}:{}", stat_mtime_ms(&head_stat), head_stat.len()),
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
