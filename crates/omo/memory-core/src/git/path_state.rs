//! Path state store capturing stage-zero index and worktree file identities.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use super::errors::GitError;
use super::exec::{GitExec, GitExecOptions, GitExecResult};
use super::path_state_files::{
    WorktreeDeletionReservation, assert_safe_parents, discard_moved_worktree_file,
    move_worktree_file, remove_worktree_file, reserve_moved_worktree_deletion,
    restore_claimed_worktree_file, restore_moved_worktree_file, unsupported_worktree,
    write_worktree_file,
};
use super::path_state_index::write_index_if_identity as write_index_conditionally;
use super::repo_arguments::command_error;

const GIT_TIMEOUT_MS: u64 = 30_000;

/// Stage-zero index entry identity consisting of mode and Git object ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitIndexIdentity {
    pub mode: String,
    pub oid: String,
}

/// Worktree file identity consisting of permission mode and Git object ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWorktreeFileIdentity {
    pub mode: u32,
    pub oid: String,
}

/// Worktree identity, either an existing file or missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitWorktreeIdentity {
    File(GitWorktreeFileIdentity),
    Missing,
}

/// Full path state capturing both index and worktree file identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPathState {
    pub index: Option<GitIndexIdentity>,
    pub worktree: GitWorktreeIdentity,
}

/// Store for querying and updating index and worktree path states.
#[derive(Clone)]
pub struct GitPathStateStore {
    dir: PathBuf,
    exec: Arc<dyn GitExec>,
    ownership_loss_recovery_path: Arc<Mutex<Option<PathBuf>>>,
}

impl GitPathStateStore {
    pub fn new(dir: impl Into<PathBuf>, exec: Arc<dyn GitExec>) -> Self {
        Self {
            dir: dir.into(),
            exec,
            ownership_loss_recovery_path: Arc::new(Mutex::new(None)),
        }
    }

    pub fn capture(&self, path: &str) -> Result<GitPathState, GitError> {
        let normalized = normalize_git_path(path)?;
        Ok(GitPathState {
            index: self.capture_index(&normalized)?,
            worktree: self.capture_worktree(&normalized)?,
        })
    }

    pub fn capture_all(
        &self,
        paths: &[impl AsRef<str>],
    ) -> Result<BTreeMap<String, GitPathState>, GitError> {
        let mut unique = BTreeSet::new();
        let mut ordered = Vec::new();
        for p in paths {
            let norm = normalize_git_path(p.as_ref())?;
            if unique.insert(norm.clone()) {
                ordered.push(norm);
            }
        }

        let mut results = BTreeMap::new();
        for path in ordered {
            let state = self.capture(&path)?;
            results.insert(path, state);
        }
        Ok(results)
    }

    pub fn hash_worktree_blob(&self, content: &str, write: bool) -> Result<String, GitError> {
        let mut argv = Vec::new();
        if write {
            argv.push("-w".to_string());
        }
        argv.push("--no-filters".to_string());
        argv.push("--stdin".to_string());
        self.hash_blob(&argv, content.as_bytes())
    }

    pub fn hash_index_blob(
        &self,
        path: &str,
        content: &str,
        write: bool,
    ) -> Result<String, GitError> {
        let normalized = normalize_git_path(path)?;
        let mut argv = Vec::new();
        if write {
            argv.push("-w".to_string());
        }
        argv.push(format!("--path={normalized}"));
        argv.push("--stdin".to_string());
        self.hash_blob(&argv, content.as_bytes())
    }

    pub fn read_blob(&self, oid: &str) -> Result<String, GitError> {
        assert_oid(oid)?;
        let res = self.git(
            &["cat-file".to_string(), "blob".to_string(), oid.to_string()],
            None,
        )?;
        Ok(res.stdout)
    }

    pub fn set_index(&self, path: &str, identity: &GitIndexIdentity) -> Result<(), GitError> {
        let normalized = normalize_git_path(path)?;
        assert_index_identity(identity)?;
        self.git(
            &[
                "update-index".to_string(),
                "--add".to_string(),
                "--cacheinfo".to_string(),
                identity.mode.clone(),
                identity.oid.clone(),
                normalized,
            ],
            None,
        )?;
        Ok(())
    }

    pub fn write_index_if_identity(
        &self,
        path: &str,
        expected: Option<&GitIndexIdentity>,
        next: Option<&GitIndexIdentity>,
    ) -> Result<bool, GitError> {
        let normalized = normalize_git_path(path)?;
        if let Some(next_id) = next {
            assert_index_identity(next_id)?;
        }
        write_index_conditionally(
            &self.dir,
            self.exec.as_ref(),
            &normalized,
            expected,
            next,
            || self.capture_index(&normalized),
        )
    }

    pub fn remove_index(&self, path: &str) -> Result<(), GitError> {
        let normalized = normalize_git_path(path)?;
        self.git(
            &[
                "update-index".to_string(),
                "--force-remove".to_string(),
                "--".to_string(),
                normalized,
            ],
            None,
        )?;
        Ok(())
    }

    pub fn write_worktree(
        &self,
        path: &str,
        identity: &GitWorktreeFileIdentity,
    ) -> Result<(), GitError> {
        let normalized = normalize_git_path(path)?;
        assert_worktree_identity(identity)?;
        let content = self.read_blob(&identity.oid)?;
        write_worktree_file(&self.dir, &normalized, &content, identity.mode, false)?;
        Ok(())
    }

    pub fn write_worktree_if_identity(
        &self,
        path: &str,
        expected: &GitWorktreeIdentity,
        next: &GitWorktreeIdentity,
    ) -> Result<bool, GitError> {
        *self.ownership_loss_recovery_path.lock().unwrap() = None;
        let normalized = normalize_git_path(path)?;

        if let GitWorktreeIdentity::Missing = expected {
            if let GitWorktreeIdentity::File(_) = self.capture_worktree(&normalized)? {
                return Ok(false);
            }
            return match next {
                GitWorktreeIdentity::Missing => Ok(true),
                GitWorktreeIdentity::File(file_id) => {
                    assert_worktree_identity(file_id)?;
                    let content = self.read_blob(&file_id.oid)?;
                    write_worktree_file(&self.dir, &normalized, &content, file_id.mode, true)
                }
            };
        }

        let moved = match move_worktree_file(&self.dir, &normalized)? {
            Some(m) => m,
            None => return Ok(false),
        };

        let rollback = |moved_path: &Path| -> Result<(), GitError> {
            if !restore_moved_worktree_file(&self.dir, &normalized, moved_path)? {
                discard_moved_worktree_file(moved_path)?;
            }
            Ok(())
        };

        let current = match self.capture_moved_worktree(&moved) {
            Ok(c) => GitWorktreeIdentity::File(c),
            Err(err) => {
                rollback(&moved)?;
                return Err(err);
            }
        };

        if &current != expected {
            rollback(&moved)?;
            return Ok(false);
        }

        match next {
            GitWorktreeIdentity::Missing => {
                let reservation = match self.reserve_worktree_deletion(&normalized, &moved)? {
                    Some(r) => r,
                    None => {
                        discard_moved_worktree_file(&moved)?;
                        return Ok(false);
                    }
                };
                let finalizer = match reservation.verify()? {
                    Some(f) => f,
                    None => {
                        discard_moved_worktree_file(&moved)?;
                        return Ok(false);
                    }
                };
                match finalizer.finish()? {
                    Ok(true) => Ok(true),
                    Ok(false) => {
                        discard_moved_worktree_file(&moved)?;
                        Ok(false)
                    }
                    Err(recovered) => {
                        discard_moved_worktree_file(&moved)?;
                        *self.ownership_loss_recovery_path.lock().unwrap() = Some(recovered);
                        Ok(false)
                    }
                }
            }
            GitWorktreeIdentity::File(file_id) => {
                assert_worktree_identity(file_id)?;
                let content = match self.read_blob(&file_id.oid) {
                    Ok(c) => c,
                    Err(err) => {
                        rollback(&moved)?;
                        return Err(err);
                    }
                };
                let published =
                    match write_worktree_file(&self.dir, &normalized, &content, file_id.mode, true)
                    {
                        Ok(p) => p,
                        Err(err) => {
                            rollback(&moved)?;
                            return Err(err);
                        }
                    };
                discard_moved_worktree_file(&moved)?;
                Ok(published)
            }
        }
    }

    pub fn consume_ownership_loss_recovery_path(&self) -> Option<PathBuf> {
        self.ownership_loss_recovery_path.lock().unwrap().take()
    }

    pub fn reserve_worktree_deletion(
        &self,
        path: &str,
        moved: &Path,
    ) -> Result<Option<WorktreeDeletionReservation>, GitError> {
        let store = self.clone();
        let target_path = path.to_string();
        reserve_moved_worktree_deletion(&self.dir, path, moved, move |claimed| {
            store.restore_worktree_claim(&target_path, claimed)
        })
    }

    pub fn restore_worktree_claim(
        &self,
        path: &str,
        claimed: &Path,
    ) -> Result<Result<(), PathBuf>, GitError> {
        restore_claimed_worktree_file(&self.dir, path, claimed)
    }

    pub fn remove_worktree(&self, path: &str) -> Result<(), GitError> {
        let normalized = normalize_git_path(path)?;
        remove_worktree_file(&self.dir, &normalized)
    }

    fn capture_index(&self, path: &str) -> Result<Option<GitIndexIdentity>, GitError> {
        let argv = vec![
            "--literal-pathspecs".to_string(),
            "ls-files".to_string(),
            "--stage".to_string(),
            "-z".to_string(),
            "--".to_string(),
            path.to_string(),
        ];
        let res = self.git(&argv, None)?;
        let records: Vec<&str> = res.stdout.split('\0').filter(|s| !s.is_empty()).collect();
        if records.is_empty() {
            return Ok(None);
        }

        let mut parsed = Vec::new();
        for rec in &records {
            parsed.push(parse_index_record(rec)?);
        }

        if parsed.iter().any(|r| r.stage != "0") {
            return Err(GitError::PathState(format!(
                "Git path is unmerged and cannot be recovered safely: {path}"
            )));
        }
        if parsed.len() != 1 || parsed[0].path != path {
            return Err(GitError::PathState(format!(
                "Git path did not resolve to one stage-zero index entry: {path}"
            )));
        }

        let record = &parsed[0];
        if record.mode != "100644" && record.mode != "100755" {
            return Err(GitError::PathState(format!(
                "Unsupported Git index mode {} for path: {path}",
                record.mode
            )));
        }

        Ok(Some(GitIndexIdentity {
            mode: record.mode.clone(),
            oid: record.oid.clone(),
        }))
    }

    fn capture_worktree(&self, path: &str) -> Result<GitWorktreeIdentity, GitError> {
        assert_safe_parents(&self.dir, path)?;
        let full = self.dir.join(path);
        let meta = match fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(GitWorktreeIdentity::Missing);
            }
            Err(err) => return Err(GitError::Io(err)),
        };

        if meta.file_type().is_symlink() {
            return Err(unsupported_worktree(path, "symlink"));
        }
        if !meta.is_file() {
            return Err(unsupported_worktree(path, "non-file"));
        }

        let content = fs::read_to_string(&full).map_err(GitError::Io)?;
        let oid = self.hash_worktree_blob(&content, true)?;
        let mode = worktree_mode(&meta);
        Ok(GitWorktreeIdentity::File(GitWorktreeFileIdentity {
            mode,
            oid,
        }))
    }

    fn capture_moved_worktree(&self, moved: &Path) -> Result<GitWorktreeFileIdentity, GitError> {
        let meta = match fs::symlink_metadata(moved) {
            Ok(m) => m,
            Err(err) => return Err(GitError::Io(err)),
        };
        if meta.file_type().is_symlink() {
            return Err(unsupported_worktree(&moved.to_string_lossy(), "symlink"));
        }
        if !meta.is_file() {
            return Err(unsupported_worktree(&moved.to_string_lossy(), "non-file"));
        }
        let content = fs::read_to_string(moved).map_err(GitError::Io)?;
        let oid = self.hash_worktree_blob(&content, true)?;
        let mode = worktree_mode(&meta);
        Ok(GitWorktreeFileIdentity { mode, oid })
    }

    fn hash_blob(&self, argv: &[String], stdin: &[u8]) -> Result<String, GitError> {
        let mut full_argv = vec!["hash-object".to_string()];
        full_argv.extend_from_slice(argv);
        let res = self.git(&full_argv, Some(stdin))?;
        let oid = res.stdout.trim().to_string();
        assert_oid(&oid)?;
        Ok(oid)
    }

    fn git(&self, argv: &[String], stdin: Option<&[u8]>) -> Result<GitExecResult, GitError> {
        let mut env = BTreeMap::new();
        env.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
        let opts = GitExecOptions {
            cwd: self.dir.clone(),
            timeout_ms: GIT_TIMEOUT_MS,
            env,
            stdin: stdin.map(|s| s.to_vec()),
        };
        let res = self.exec.run(argv, &opts)?;
        if res.code != 0 {
            return Err(command_error(argv, &res));
        }
        Ok(res)
    }
}

pub fn normalize_git_path(path: &str) -> Result<String, GitError> {
    let normalized = path.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').collect();

    let has_win_drive = normalized.len() >= 3
        && normalized.as_bytes()[0].is_ascii_alphabetic()
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[2] == b'/';

    if normalized.is_empty()
        || normalized.contains('\0')
        || normalized.starts_with('/')
        || has_win_drive
        || segments.iter().any(|part| {
            part.is_empty() || *part == "." || *part == ".." || part.eq_ignore_ascii_case(".git")
        })
    {
        return Err(GitError::PathState(format!(
            "Invalid repository-relative Git path: {path}"
        )));
    }
    Ok(normalized)
}

struct ParsedIndexRecord {
    mode: String,
    oid: String,
    stage: String,
    path: String,
}

fn parse_index_record(record: &str) -> Result<ParsedIndexRecord, GitError> {
    let Some((meta, path)) = record.split_once('\t') else {
        return Err(GitError::PathState(
            "Git returned a malformed index entry".to_string(),
        ));
    };
    let parts: Vec<&str> = meta.split_whitespace().collect();
    if parts.len() != 3 {
        return Err(GitError::PathState(
            "Git returned a malformed index entry".to_string(),
        ));
    }
    Ok(ParsedIndexRecord {
        mode: parts[0].to_string(),
        oid: parts[1].to_string(),
        stage: parts[2].to_string(),
        path: path.to_string(),
    })
}

fn assert_oid(oid: &str) -> Result<(), GitError> {
    if oid.is_empty() || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(GitError::PathState(format!("Invalid Git object ID: {oid}")));
    }
    Ok(())
}

fn assert_index_identity(identity: &GitIndexIdentity) -> Result<(), GitError> {
    if identity.mode != "100644" && identity.mode != "100755" {
        return Err(GitError::PathState(format!(
            "Unsupported Git index mode: {}",
            identity.mode
        )));
    }
    assert_oid(&identity.oid)
}

fn assert_worktree_identity(identity: &GitWorktreeFileIdentity) -> Result<(), GitError> {
    if identity.mode > 0o777 {
        return Err(GitError::PathState(
            "Invalid worktree file identity".to_string(),
        ));
    }
    assert_oid(&identity.oid)
}

fn worktree_mode(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        meta.mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o644
    }
}
