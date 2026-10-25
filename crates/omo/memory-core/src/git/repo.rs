//! High-level Git repository facade orchestrating transactions, locks, and history.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::config_lock::{with_git_lock_retry, with_serialized_git_config_mutation};
use super::errors::GitError;
use super::exec::{GitExec, GitExecOptions, GitExecResult, system_git_exec};
use super::path_state::GitPathStateStore;
use super::porcelain::describe_dirty_markdown_encoding_issues;
use super::repo_arguments::{
    author_flags, command_error, normalize_pathspecs, normalize_seed_path,
};
use super::repo_log::{parse_log_output, parse_log_output_with_paths};
use super::repo_status::assert_no_unrelated_changes;
use super::repo_tree::{parse_cat_file_batch, parse_ls_tree_blobs, parse_ls_tree_sized};
use super::repo_types::{
    GitCommitAuthor, GitCommitResult, GitHookInstaller, GitLogOptions, GitMemoryRepoOptions,
    GitMergeOptions, GitTreeBlobEntry, GitTreeSizedEntry, InitializeGitRepoOptions, MemoryCommit,
};
use super::worktree_mutation_queue::with_serialized_git_worktree_mutation;

const INITIAL_COMMIT: &str = "Initialize memory repository";
const EMPTY_INITIAL_COMMIT: &str = "Initialize empty memory repository";
const GIT_TIMEOUT_MS: u64 = 30_000;

/// High-level Git repository for memory storage operations.
#[derive(Clone)]
pub struct GitMemoryRepo {
    pub dir: PathBuf,
    pub agent_id: String,
    pub path_state: GitPathStateStore,
    exec: Arc<dyn GitExec>,
    hook_installer: Option<GitHookInstaller>,
    commit_times: Arc<super::repo_commit_times::PathCommitTimes>,
}

impl GitMemoryRepo {
    pub fn new(options: impl Into<GitMemoryRepoOptions>) -> Result<Self, GitError> {
        let mut opts = options.into();
        opts.dir = crate::support::paths::strip_verbatim_prefix(&opts.dir);
        let exec = opts.exec.unwrap_or_else(system_git_exec);
        let path_state = GitPathStateStore::new(&opts.dir, Arc::clone(&exec));
        Ok(Self {
            dir: opts.dir,
            agent_id: opts.agent_id,
            path_state,
            exec,
            hook_installer: opts.install_hooks,
            commit_times: Arc::default(),
        })
    }

    /// The process runner this repository shells out through.
    pub fn exec(&self) -> Arc<dyn GitExec> {
        Arc::clone(&self.exec)
    }

    pub fn open(dir: impl Into<PathBuf>, agent_id: impl Into<String>) -> Result<Self, GitError> {
        Self::new(GitMemoryRepoOptions::new(dir, agent_id))
    }

    pub fn init(
        &self,
        options: impl Into<Option<InitializeGitRepoOptions>>,
    ) -> Result<String, GitError> {
        let opts = options.into().unwrap_or_default();
        fs::create_dir_all(&self.dir).map_err(GitError::Io)?;

        if !self.dir.join(".git").exists() {
            with_git_lock_retry(|| self.git(&["init".to_string()]))?;
            with_git_lock_retry(|| {
                self.git(&[
                    "symbolic-ref".to_string(),
                    "HEAD".to_string(),
                    "refs/heads/main".to_string(),
                ])
            })?;
        }

        if let Some(installer) = opts.install_hooks.as_ref().or(self.hook_installer.as_ref()) {
            installer(&self.dir)?;
        }

        let author_name = opts
            .author_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Omo Agent");

        self.ensure_identity(author_name)?;

        if let Some(current_head) = self.head()? {
            return Ok(current_head);
        }

        let mut paths = Vec::new();
        for seed in &opts.seed_files {
            let rel = normalize_seed_path(&seed.relative_path)?;
            let full = self.dir.join(&rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).map_err(GitError::Io)?;
            }
            fs::write(&full, &seed.content).map_err(GitError::Io)?;
            paths.push(rel);
        }

        let author = GitCommitAuthor {
            agent_id: self.agent_id.clone(),
            author_name: author_name.to_string(),
            author_email: None,
        };

        if !paths.is_empty() {
            self.stage(&paths)?;
            if self.has_path_changes(&paths)? {
                return Ok(self.commit_staged(INITIAL_COMMIT, &author)?.sha);
            }
        }

        super::repo_lock_sweep::with_stranded_git_lock_recovery(&self.dir, || {
            let mut argv = vec!["commit".to_string(), "--allow-empty".to_string()];
            argv.extend(author_flags(&author));
            argv.push("-m".to_string());
            argv.push(EMPTY_INITIAL_COMMIT.to_string());
            self.git(&argv)
        })?;

        self.require_head()
    }

    pub fn clean_check(&self) -> Result<(), GitError> {
        if let Some(installer) = &self.hook_installer {
            installer(&self.dir)?;
        }
        let porcelain = self.status(&[] as &[&str])?;
        if porcelain.trim().is_empty() {
            return Ok(());
        }
        Err(GitError::DirtyRepo {
            porcelain: porcelain.clone(),
            encoding_diagnostics: describe_dirty_markdown_encoding_issues(&self.dir, &porcelain),
        })
    }

    pub fn commit_write(
        &self,
        paths: &[impl AsRef<str>],
        reason: &str,
        author: &GitCommitAuthor,
    ) -> Result<GitCommitResult, GitError> {
        if let Some(installer) = &self.hook_installer {
            installer(&self.dir)?;
        }
        let normalized = normalize_pathspecs(paths);
        if normalized.is_empty() {
            return Err(GitError::NoEffectiveChanges { paths: normalized });
        }

        assert_no_unrelated_changes(&self.dir, &normalized, || self.status(&[] as &[&str]))?;
        self.stage(&normalized)?;
        // The hook installer and the full-tree unrelated-change assertion already ran above; only
        // the pathspec change check remains before committing.
        if !self.has_path_changes(&normalized)? {
            return Err(GitError::NoEffectiveChanges { paths: normalized });
        }
        self.commit_staged(reason, author)
    }

    pub fn commit_prepared(
        &self,
        paths: &[impl AsRef<str>],
        reason: &str,
        author: &GitCommitAuthor,
    ) -> Result<GitCommitResult, GitError> {
        if let Some(installer) = &self.hook_installer {
            installer(&self.dir)?;
        }
        let normalized = normalize_pathspecs(paths);
        if normalized.is_empty() {
            return Err(GitError::NoEffectiveChanges { paths: normalized });
        }

        assert_no_unrelated_changes(&self.dir, &normalized, || self.status(&[] as &[&str]))?;
        if !self.has_path_changes(&normalized)? {
            return Err(GitError::NoEffectiveChanges { paths: normalized });
        }
        self.commit_staged(reason, author)
    }

    pub fn status(&self, paths: &[impl AsRef<str>]) -> Result<String, GitError> {
        let normalized = normalize_pathspecs(paths);
        let mut argv = vec![
            "-c".to_string(),
            "core.quotePath=false".to_string(),
            "status".to_string(),
            "--porcelain".to_string(),
            "--untracked-files=all".to_string(),
        ];
        if !normalized.is_empty() {
            argv.push("--".to_string());
            argv.extend(normalized);
        }
        let res = self.git(&argv)?;
        Ok(res.stdout)
    }

    pub fn head(&self) -> Result<Option<String>, GitError> {
        let res = self.git_result(&[
            "rev-parse".to_string(),
            "--verify".to_string(),
            "HEAD".to_string(),
        ])?;
        if res.code != 0 {
            return Ok(None);
        }
        let trimmed = res.stdout.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }

    pub fn head_commit_timestamp(&self) -> Result<Option<i64>, GitError> {
        let mut argv = super::stable_log_output::stable_log_output();
        argv.extend([
            "show".into(),
            "-s".into(),
            "--format=%ct".into(),
            "HEAD".into(),
        ]);
        let res = self.git_result(&argv)?;
        if res.code != 0 {
            return Ok(None);
        }
        let trimmed = res.stdout.trim();
        Ok(trimmed.parse::<i64>().ok().filter(|&ts| ts >= 0))
    }

    pub fn path_commit_times(&self, revision: &str) -> Result<BTreeMap<String, i64>, GitError> {
        let present = self.ls_tree(Some(revision), None)?;
        self.commit_times
            .at(&self.dir, revision, &present, &|args, timeout_ms| {
                let result = self
                    .exec
                    .run(
                        args,
                        &GitExecOptions {
                            cwd: self.dir.clone(),
                            timeout_ms,
                            ..Default::default()
                        },
                    )
                    .map_err(GitError::Io)?;
                if result.code != 0 && args.first().is_none_or(|arg| arg != "merge-base") {
                    return Err(command_error(args, &result));
                }
                Ok(result)
            })
    }

    pub fn ls_tree(
        &self,
        revision: Option<&str>,
        path: Option<&str>,
    ) -> Result<Vec<String>, GitError> {
        let rev = revision.unwrap_or("HEAD");
        let mut argv = vec![
            "ls-tree".to_string(),
            "-r".to_string(),
            "--name-only".to_string(),
            "-z".to_string(),
            rev.to_string(),
        ];
        if let Some(p) = path {
            argv.push("--".to_string());
            argv.push(p.to_string());
        }
        let res = self.git(&argv)?;
        Ok(res
            .stdout
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect())
    }

    pub fn show(&self, revision: &str, path: &str) -> Result<String, GitError> {
        let res = self.git(&["show".to_string(), format!("{revision}:{path}")])?;
        Ok(res.stdout)
    }

    /// Lists `ls-tree -r -l -z` entries with their byte sizes.
    pub fn ls_tree_sized(
        &self,
        revision: Option<&str>,
    ) -> Result<Vec<GitTreeSizedEntry>, GitError> {
        let rev = revision.unwrap_or("HEAD");
        let res = self.git(&[
            "ls-tree".to_string(),
            "-r".to_string(),
            "-l".to_string(),
            "-z".to_string(),
            rev.to_string(),
        ])?;
        Ok(parse_ls_tree_sized(&res.stdout))
    }

    /// Lists `ls-tree -r -z` blob entries with their object ids.
    pub fn ls_tree_blobs(&self, revision: Option<&str>) -> Result<Vec<GitTreeBlobEntry>, GitError> {
        let rev = revision.unwrap_or("HEAD");
        let res = self.git(&[
            "ls-tree".to_string(),
            "-r".to_string(),
            "-z".to_string(),
            rev.to_string(),
        ])?;
        Ok(parse_ls_tree_blobs(&res.stdout))
    }

    /// Reads every requested blob through one `git cat-file --batch` process.
    ///
    /// Object ids git reports missing are absent from the returned map. Blob content is decoded as
    /// UTF-8, which is lossless for the memory repository's UTF-8 markdown contract.
    pub fn read_blobs(&self, oids: &[String]) -> Result<BTreeMap<String, String>, GitError> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut unique: Vec<&str> = Vec::new();
        for oid in oids {
            if seen.insert(oid.as_str()) {
                unique.push(oid.as_str());
            }
        }
        if unique.is_empty() {
            return Ok(BTreeMap::new());
        }
        let argv = vec!["cat-file".to_string(), "--batch".to_string()];
        let stdin = format!("{}\n", unique.join("\n")).into_bytes();
        let res = self.git_with_stdin(&argv, &stdin)?;
        if res.code != 0 {
            return Err(command_error(&argv, &res));
        }
        parse_cat_file_batch(res.stdout.as_bytes())
    }

    fn git_with_stdin(&self, argv: &[String], stdin: &[u8]) -> Result<GitExecResult, GitError> {
        let mut env = BTreeMap::new();
        env.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
        let opts = GitExecOptions {
            cwd: self.dir.clone(),
            timeout_ms: GIT_TIMEOUT_MS,
            env,
            stdin: Some(stdin.to_vec()),
        };
        self.exec.run(argv, &opts).map_err(GitError::Io)
    }

    pub fn log(&self, options: Option<&GitLogOptions>) -> Result<Vec<MemoryCommit>, GitError> {
        let include_paths = options.is_some_and(|o| o.include_paths);
        let mut argv = super::stable_log_output::stable_log_output();
        if include_paths {
            argv.extend(["-c".into(), "log.showRoot=false".into()]);
        }
        argv.extend([
            "log".into(),
            "--format=%x1e%H%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%cI".into(),
        ]);
        if include_paths {
            argv.extend([
                "--name-only".into(),
                "-z".into(),
                "--no-renames".into(),
                "--full-diff".into(),
            ]);
        }
        if let Some(limit) = options.and_then(|o| o.limit) {
            argv.push("-n".to_string());
            argv.push(limit.to_string());
        }
        if let Some(grep) = options.and_then(|o| o.grep.as_deref()) {
            argv.push("--fixed-strings".to_string());
            argv.push(format!("--grep={grep}"));
        }
        if let Some(range) = options.and_then(|o| o.range.as_deref()) {
            argv.push(range.to_string());
        }
        if let Some(paths) = options.and_then(|o| o.paths.as_ref())
            && !paths.is_empty()
        {
            argv.push("--".to_string());
            argv.extend(paths.clone());
        }

        let res = self.git(&argv)?;
        Ok(if include_paths {
            parse_log_output_with_paths(&res.stdout)
        } else {
            parse_log_output(&res.stdout)
        })
    }

    pub fn worktree_add(
        &self,
        path: impl AsRef<Path>,
        branch: &str,
        start_point: Option<&str>,
    ) -> Result<(), GitError> {
        let clean_path = crate::support::paths::strip_verbatim_prefix(path.as_ref());
        let path_str = clean_path.to_string_lossy().into_owned();
        let start = start_point.unwrap_or("HEAD");
        with_serialized_git_worktree_mutation(&self.dir, || {
            with_git_lock_retry(|| {
                self.git(&[
                    "worktree".to_string(),
                    "add".to_string(),
                    "-b".to_string(),
                    branch.to_string(),
                    path_str.clone(),
                    start.to_string(),
                ])
            })
        })?;
        Ok(())
    }

    pub fn worktree_remove(&self, path: impl AsRef<Path>, force: bool) -> Result<(), GitError> {
        let clean_path = crate::support::paths::strip_verbatim_prefix(path.as_ref());
        let path_str = clean_path.to_string_lossy().into_owned();
        with_serialized_git_worktree_mutation(&self.dir, || {
            with_git_lock_retry(|| {
                let mut argv = vec!["worktree".to_string(), "remove".to_string()];
                if force {
                    argv.push("--force".to_string());
                }
                argv.push(path_str.clone());
                self.git(&argv)
            })
        })?;
        Ok(())
    }

    pub fn merge(
        &self,
        ref_name: &str,
        options: Option<&GitMergeOptions>,
    ) -> Result<String, GitError> {
        let mut argv = vec!["merge".to_string()];
        let no_ff = options.and_then(|o| o.no_ff).unwrap_or(true);
        if no_ff {
            argv.push("--no-ff".to_string());
        }
        if let Some(msg) = options.and_then(|o| o.message.as_deref()) {
            argv.push("-m".to_string());
            argv.push(msg.to_string());
        }
        argv.push(ref_name.to_string());

        super::repo_lock_sweep::with_stranded_git_lock_recovery(&self.dir, || self.git(&argv))?;
        self.require_head()
    }

    pub fn config_get(&self, key: &str) -> Result<Option<String>, GitError> {
        let res = self.git_result(&[
            "config".to_string(),
            "--local".to_string(),
            "--get".to_string(),
            key.to_string(),
        ])?;
        if res.code == 1 {
            return Ok(None);
        }
        if res.code != 0 {
            return Err(command_error(
                &[
                    "config".to_string(),
                    "--local".to_string(),
                    "--get".to_string(),
                    key.to_string(),
                ],
                &res,
            ));
        }
        let trimmed = res.stdout.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed.to_string()))
        }
    }

    pub fn config_set(&self, key: &str, value: &str) -> Result<(), GitError> {
        with_serialized_git_config_mutation(&self.dir, || {
            self.git(&[
                "config".to_string(),
                "--local".to_string(),
                key.to_string(),
                value.to_string(),
            ])
        })?;
        Ok(())
    }

    fn ensure_identity(&self, author_name: &str) -> Result<(), GitError> {
        super::repo_config_defaults::ensure_repo_config_defaults(
            &self.dir,
            &self.agent_id,
            author_name,
            |key| self.config_get(key),
            |key, value| self.config_set(key, value),
            &|args, input| match input {
                Some(input) => self.git_with_stdin(args, input),
                None => self.git_result(args),
            },
        )
    }

    fn stage(&self, paths: &[String]) -> Result<(), GitError> {
        super::repo_lock_sweep::with_stranded_git_lock_recovery(&self.dir, || {
            let mut argv = vec!["add".to_string(), "-A".to_string(), "--".to_string()];
            argv.extend_from_slice(paths);
            self.git(&argv)
        })?;
        Ok(())
    }

    fn has_path_changes(&self, paths: &[String]) -> Result<bool, GitError> {
        let status = self.status(paths)?;
        Ok(!status.trim().is_empty())
    }

    fn commit_staged(
        &self,
        reason: &str,
        author: &GitCommitAuthor,
    ) -> Result<GitCommitResult, GitError> {
        let landed = super::commit_outcome::commit_judged_by_outcome(
            || {
                super::repo_lock_sweep::with_stranded_git_lock_recovery(&self.dir, || {
                    let mut argv = vec!["commit".to_string()];
                    argv.extend(author_flags(author));
                    argv.push("-m".to_string());
                    argv.push(reason.to_string());
                    self.git(&argv)
                })
            },
            || self.head(),
            |args| self.git_result(args),
            reason,
        )?;
        Ok(GitCommitResult {
            committed: true,
            sha: match landed {
                Some(sha) => sha,
                None => self.require_head()?,
            },
        })
    }

    fn require_head(&self) -> Result<String, GitError> {
        self.head()?
            .ok_or_else(|| GitError::Other("Memory repository has no HEAD commit".to_string()))
    }

    fn git(&self, argv: &[String]) -> Result<GitExecResult, GitError> {
        let res = self.git_result(argv)?;
        if res.code != 0 {
            return Err(command_error(argv, &res));
        }
        Ok(res)
    }

    fn git_result(&self, argv: &[String]) -> Result<GitExecResult, GitError> {
        let mut env = BTreeMap::new();
        env.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
        let opts = GitExecOptions {
            cwd: self.dir.clone(),
            timeout_ms: GIT_TIMEOUT_MS,
            env,
            stdin: None,
        };
        self.exec.run(argv, &opts).map_err(GitError::Io)
    }
}
