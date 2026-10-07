use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use memory_core::locks::{
    acquire_lock, create_lock_record, release_lock, AcquireLockOptions, CreateLockRecordOptions,
};

use crate::backend::{error_text, IsolationError, Result};
use crate::git::command::{git_text, run_git, str_args, GitOptions};
use crate::git::delta::DeltaPatchResult;
use crate::git::synthetic_tree::parse_diff_git_line_paths;
use crate::util::{dirname, lock, resolve_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeKind {
    Applied,
    AlreadyApplied,
    NotApplied,
    BranchMerged,
    BranchMergeFailed,
    NoChanges,
    Retained,
}

impl MergeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MergeKind::Applied => "applied",
            MergeKind::AlreadyApplied => "already-applied",
            MergeKind::NotApplied => "not-applied",
            MergeKind::BranchMerged => "branch-merged",
            MergeKind::BranchMergeFailed => "branch-merge-failed",
            MergeKind::NoChanges => "no-changes",
            MergeKind::Retained => "retained",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NestedFailure {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MergeState {
    pub changes_applied: bool,
    pub kind: MergeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_failed: Option<Vec<NestedFailure>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl MergeState {
    pub fn new(kind: MergeKind, changes_applied: bool) -> Self {
        MergeState {
            changes_applied,
            kind,
            partial: None,
            nested_failed: None,
            branch_name: None,
            conflict: None,
            manual_command: None,
            warning: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IsolationMergeResult {
    #[serde(flatten)]
    pub state: MergeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_patch_paths: Option<Vec<String>>,
    pub summary_path: String,
    pub files_changed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockEvent {
    Waiting,
    Acquired,
    Released,
}

pub type LockHook = Arc<dyn Fn(LockEvent, &Path) + Send + Sync>;

#[derive(Clone)]
pub struct ArtifactOptions {
    pub id: String,
    pub artifacts_dir: PathBuf,
    pub lock_hook: Option<LockHook>,
}

pub fn task_branch(id: &str) -> Result<String> {
    if !is_valid_task_id(id) {
        return Err(IsolationError::other("Invalid isolation task id"));
    }
    Ok(format!("omo/task/{id}"))
}

fn is_valid_task_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return false;
    }
    if id.contains("..") || id.ends_with('.') || id.ends_with(".lock") {
        return false;
    }
    true
}

pub fn nested_path(root: &Path, relative_path: &str) -> Result<PathBuf> {
    let root_resolved = resolve_path(root);
    let path = resolve_path(&root_resolved.join(relative_path));
    let below = path != root_resolved && path.starts_with(&root_resolved);
    if Path::new(relative_path).is_absolute() || !below {
        return Err(IsolationError::other(format!(
            "Invalid nested repository path: {relative_path}"
        )));
    }
    // Lexical containment is not filesystem containment: an existing component can
    // be a symlink out of the root, redirecting every later mutation. Validate the
    // canonical location of the deepest existing ancestor of the target.
    let anchor = deepest_existing_canonical(&root_resolved)?;
    let existing = deepest_existing_canonical(&path)?;
    if existing != anchor && !existing.starts_with(&anchor) {
        return Err(IsolationError::other(format!(
            "Nested repository path escapes the repository root through a symlink: {relative_path}"
        )));
    }
    Ok(path)
}

fn deepest_existing_canonical(path: &Path) -> Result<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        match std::fs::canonicalize(&current) {
            Ok(value) => return Ok(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = dirname(&current);
                if parent == current {
                    return Err(error.into());
                }
                current = parent;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn queue_for(path: &Path) -> Arc<Mutex<()>> {
    static QUEUES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let queues = QUEUES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = lock(queues);
    Arc::clone(
        guard
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

pub fn with_repo_lock<T>(
    repo_root: &Path,
    f: &dyn Fn() -> Result<T>,
    hook: Option<&LockHook>,
) -> Result<T> {
    let common_dir = git_text(
        repo_root,
        &str_args(&["rev-parse", "--path-format=absolute", "--git-common-dir"]),
    )?;
    let path = PathBuf::from(common_dir).join("omo-isolation-merge.lock");
    if let Some(hook) = hook {
        hook(LockEvent::Waiting, &path);
    }
    let queue = queue_for(&path);
    let _guard = lock(&queue);
    let record = create_lock_record("omo-isolation-merge", CreateLockRecordOptions::default())
        .map_err(|error| IsolationError::other(format!("{error:?}")))?;
    acquire_lock(
        &path,
        &record,
        &AcquireLockOptions {
            wait_timeout_ms: Some(60_000),
            ..Default::default()
        },
    )
    .map_err(|error| IsolationError::other(format!("{error:?}")))?;
    let result = (|| {
        if let Some(hook) = hook {
            hook(LockEvent::Acquired, &path);
        }
        f()
    })();
    let _ = release_lock(&path, record.nonce.as_str());
    if let Some(hook) = hook {
        hook(LockEvent::Released, &path);
    }
    result
}

#[derive(Debug, Clone)]
pub struct WrittenArtifacts {
    pub patch_path: PathBuf,
    pub nested_patch_paths: Vec<PathBuf>,
    pub summary_path: PathBuf,
    pub files_changed: usize,
}

impl WrittenArtifacts {
    pub fn base_result(&self, state: MergeState) -> IsolationMergeResult {
        IsolationMergeResult {
            state,
            patch_path: Some(self.patch_path.to_string_lossy().into_owned()),
            nested_patch_paths: Some(
                self.nested_patch_paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
            ),
            summary_path: self.summary_path.to_string_lossy().into_owned(),
            files_changed: self.files_changed,
        }
    }
}

pub fn write_artifacts(
    delta: &DeltaPatchResult,
    options: &ArtifactOptions,
) -> Result<WrittenArtifacts> {
    task_branch(&options.id)?;
    let dir = resolve_path(&options.artifacts_dir.join("isolation").join(&options.id));
    assert_no_symlink_components(&dir, &options.artifacts_dir)?;
    std::fs::create_dir_all(&dir)?;
    let patch_path = dir.join("root.patch");
    write_file_owned(&patch_path, &delta.root_patch)?;
    let mut nested_patch_paths: Vec<PathBuf> = Vec::new();
    let nested_root = dir.join("nested");
    std::fs::create_dir_all(&nested_root)?;
    for nested in &delta.nested_patches {
        let path = nested_path(&nested_root, &format!("{}.patch", nested.relative_path))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_file_owned(&path, &nested.patch)?;
        nested_patch_paths.push(path);
    }
    let mut files: HashSet<String> = HashSet::new();
    for path in delta.root_patch.split('\n').flat_map(parse_diff_git_line_paths) {
        files.insert(path);
    }
    for nested in &delta.nested_patches {
        for path in nested.patch.split('\n').flat_map(parse_diff_git_line_paths) {
            files.insert(format!("{}/{}", nested.relative_path, path));
        }
    }
    Ok(WrittenArtifacts {
        patch_path,
        nested_patch_paths,
        summary_path: dir.join("isolation-summary.md"),
        files_changed: files.len(),
    })
}

/// A pre-existing symlink on the owned artifact path would redirect every write.
/// Only components at or below the caller's artifacts directory are ours to judge:
/// system-level symlinked prefixes (such as /var on macOS) are not redirects.
fn assert_no_symlink_components(path: &Path, base: &Path) -> Result<()> {
    let resolved_path = resolve_path(path);
    let resolved_base = resolve_path(base);
    let relative = resolved_path
        .strip_prefix(&resolved_base)
        .unwrap_or(&resolved_path);
    let mut prefix = resolved_base;
    for part in relative.components() {
        prefix = prefix.join(part.as_os_str());
        match std::fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(IsolationError::other(format!(
                    "Artifact path component is a symlink: {}",
                    prefix.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Owned outputs are created without following a pre-existing final symlink.
fn write_file_owned(path: &Path, data: &str) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(data.as_bytes())?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let info = std::fs::symlink_metadata(path)?;
            if !info.is_file() {
                return Err(IsolationError::other(format!(
                    "Artifact path is not a regular file: {}",
                    path.display()
                )));
            }
            std::fs::write(path, data)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub fn summarize(result: IsolationMergeResult) -> Result<IsolationMergeResult> {
    let json = serde_json::to_string_pretty(&result)
        .map_err(|error| IsolationError::other(error.to_string()))?;
    std::fs::write(
        &result.summary_path,
        format!(
            "# Isolation merge: {}\n\n```json\n{json}\n```\n",
            result.state.kind.as_str()
        ),
    )?;
    Ok(result)
}

pub fn stash_push(repo_root: &Path) -> Result<Option<String>> {
    let status = run_git(
        &str_args(&["status", "--porcelain", "--untracked-files=all"]),
        &GitOptions::new(repo_root.to_path_buf()),
    )?;
    if status.stdout.is_empty() {
        return Ok(None);
    }
    let read_tip = || -> Result<String> {
        let mut options = GitOptions::new(repo_root.to_path_buf());
        options.allowed_exit_codes = Some(vec![0, 1]);
        let output = run_git(
            &str_args(&["rev-parse", "--verify", "--quiet", "refs/stash"]),
            &options,
        )?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let before = read_tip()?;
    run_git(
        &str_args(&[
            "stash",
            "push",
            "--include-untracked",
            "-m",
            "omo-task-merge",
        ]),
        &GitOptions::new(repo_root.to_path_buf()),
    )?;
    let created = read_tip()?;
    // A no-op push (dirt only inside a submodule, for example) leaves the stack
    // unchanged; the caller must not restore and drop an unrelated user stash.
    Ok(if !created.is_empty() && created != before {
        Some(created)
    } else {
        None
    })
}

pub fn stash_pop(repo_root: &Path, stash: &str) -> Result<Option<String>> {
    // Locate OUR entry by identity: whatever sits on top when we get here belongs
    // to someone else and must survive the merge untouched.
    let list = git_text(repo_root, &str_args(&["stash", "list", "--format=%H"]))?;
    let Some(index) = list.split('\n').position(|sha| sha == stash) else {
        return Ok(None);
    };
    let reference = format!("stash@{{{index}}}");
    let mut options = GitOptions::new(repo_root.to_path_buf());
    options.allowed_exit_codes = Some(vec![0, 1]);
    let applied = run_git(
        &str_args(&["stash", "apply", "--index", &reference]),
        &options,
    )?;
    if applied.code != 0 {
        let detail = if applied.stderr.is_empty() {
            String::from_utf8_lossy(&applied.stdout).into_owned()
        } else {
            applied.stderr.clone()
        };
        return Ok(Some(format!(
            "stash restore failed; stash entry preserved: {detail}"
        )));
    }
    let dropped = run_git(&str_args(&["stash", "drop", &reference]), &options)?;
    if dropped.code != 0 {
        let detail = if dropped.stderr.is_empty() {
            String::from_utf8_lossy(&dropped.stdout).into_owned()
        } else {
            dropped.stderr.clone()
        };
        return Ok(Some(format!("stash applied but not dropped: {detail}")));
    }
    Ok(None)
}

pub fn merge_error_text(error: &IsolationError) -> String {
    error_text(error)
}
