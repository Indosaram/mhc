//! SyncRepo over the memory repo's git, through memory-core's own exec boundary. The sync commit
//! is built with plumbing so it never depends on a clean tree: the tree is HEAD plus exactly the
//! rendered paths, assembled in a private index; the commit is authored `OmO Sync`; HEAD moves by
//! compare-and-swap (`update-ref <new> <old>`), so an agent commit landing meanwhile is kept and
//! the sync is rebuilt on top of it; finally only the committed paths' index entries are synced.
//! A person's uncaptured edits elsewhere stay dirty and unstaged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::super::error::EngineResult;
use super::super::ports::SyncRepo;
use crate::fs::resilient;
use crate::git::exec::{GitExec, GitExecOptions, system_git_exec};
use crate::support::random::random_id;

const SYNC_AUTHOR_NAME: &str = "OmO Sync";
const SYNC_AUTHOR_EMAIL: &str = "sync@omo.invalid";
const TIMEOUT_MS: u64 = 60_000;
const CAS_ATTEMPTS: usize = 5;
/// Reconciliation looks for its trailer among recent history only.
const TRAILER_SCAN: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCommitError(pub String);

impl std::fmt::Display for SyncCommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SyncCommitError {}

pub struct GitSyncRepoOptions {
    pub root: PathBuf,
    pub exec: Option<Arc<dyn GitExec>>,
}

pub struct GitSyncRepo {
    root: PathBuf,
    git_dir: PathBuf,
    exec: Arc<dyn GitExec>,
}

pub fn create_git_sync_repo(options: GitSyncRepoOptions) -> GitSyncRepo {
    let git_dir = options.root.join(".git");
    GitSyncRepo {
        root: options.root,
        git_dir,
        exec: options.exec.unwrap_or_else(system_git_exec),
    }
}

fn args(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|a| (*a).to_string()).collect()
}

impl GitSyncRepo {
    fn options(&self, env: BTreeMap<String, String>, stdin: Option<Vec<u8>>) -> GitExecOptions {
        GitExecOptions {
            cwd: self.root.clone(),
            timeout_ms: TIMEOUT_MS,
            env,
            stdin,
        }
    }

    fn run(
        &self,
        argv: &[String],
        env: BTreeMap<String, String>,
        stdin: Option<&str>,
    ) -> EngineResult<String> {
        let result = self.exec.run(
            argv,
            &self.options(env, stdin.map(|s| s.as_bytes().to_vec())),
        )?;
        if result.code != 0 {
            let command = argv.first().map_or("", String::as_str);
            return Err(SyncCommitError(format!(
                "git {command} failed ({}): {}",
                result.code,
                result.stderr.trim()
            ))
            .into());
        }
        Ok(result.stdout.trim().to_string())
    }

    fn head_id(&self) -> EngineResult<Option<String>> {
        let result = self.exec.run(
            &args(&["rev-parse", "--verify", "-q", "HEAD"]),
            &self.options(BTreeMap::new(), None),
        )?;
        Ok((result.code == 0).then(|| result.stdout.trim().to_string()))
    }

    fn tree_onto(&self, parent: Option<&str>, paths: &[String]) -> EngineResult<String> {
        let dir = self.git_dir.join("omo-sync");
        let suffix: String = random_id().chars().take(12).collect();
        let index = dir.join(format!("index.{suffix}"));
        let env = BTreeMap::from([(
            "GIT_INDEX_FILE".to_string(),
            index.to_string_lossy().into_owned(),
        )]);
        resilient::create_dir_all(&dir)?;
        let built = (|| -> EngineResult<String> {
            match parent {
                None => self.run(&args(&["read-tree", "--empty"]), env.clone(), None)?,
                Some(p) => self.run(&args(&["read-tree", p]), env.clone(), None)?,
            };
            // A path on disk is staged as is; a missing one is removed from the index if tracked and
            // is a no-op otherwise. `git add` on a path in neither HEAD nor the working tree exits
            // 128, which once wedged a device that received a create and a delete of a file it
            // never had.
            let (present, missing): (Vec<&String>, Vec<&String>) = paths
                .iter()
                .partition(|p| resilient::exists(&self.root.join(p.as_str())));
            if !present.is_empty() {
                let mut argv = args(&["add", "-A", "--"]);
                argv.extend(present.into_iter().cloned());
                self.run(&argv, env.clone(), None)?;
            }
            if !missing.is_empty() {
                let mut argv = args(&["rm", "--cached", "-q", "--ignore-unmatch", "--"]);
                argv.extend(missing.into_iter().cloned());
                self.run(&argv, env.clone(), None)?;
            }
            self.run(&args(&["write-tree"]), env.clone(), None)
        })();
        let _ = resilient::remove_file(&index);
        built
    }
}

impl SyncRepo for GitSyncRepo {
    fn root(&self) -> &Path {
        &self.root
    }

    fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    fn head(&self) -> EngineResult<Option<String>> {
        self.head_id()
    }

    fn head_tree(&self) -> EngineResult<String> {
        self.run(&args(&["rev-parse", "HEAD^{tree}"]), BTreeMap::new(), None)
    }

    fn stage_tree(&self, paths: &[String]) -> EngineResult<String> {
        self.tree_onto(self.head_id()?.as_deref(), paths)
    }

    fn commit(&self, paths: &[String], message: &str) -> EngineResult<String> {
        let author = BTreeMap::from([
            ("GIT_AUTHOR_NAME".to_string(), SYNC_AUTHOR_NAME.to_string()),
            (
                "GIT_AUTHOR_EMAIL".to_string(),
                SYNC_AUTHOR_EMAIL.to_string(),
            ),
            (
                "GIT_COMMITTER_NAME".to_string(),
                SYNC_AUTHOR_NAME.to_string(),
            ),
            (
                "GIT_COMMITTER_EMAIL".to_string(),
                SYNC_AUTHOR_EMAIL.to_string(),
            ),
        ]);
        for _ in 0..CAS_ATTEMPTS {
            let parent = self.head_id()?;
            let tree = self.tree_onto(parent.as_deref(), paths)?;
            let mut argv = args(&["commit-tree", &tree]);
            if let Some(p) = &parent {
                argv.extend(args(&["-p", p]));
            }
            argv.extend(args(&["-F", "-"]));
            let commit = self.run(&argv, author.clone(), Some(message))?;
            let old = parent.unwrap_or_else(|| "0".repeat(40));
            let moved = self.exec.run(
                &args(&["update-ref", "-m", "omo-sync", "HEAD", &commit, &old]),
                &self.options(BTreeMap::new(), None),
            )?;
            if moved.code != 0 {
                continue;
            }
            if !paths.is_empty() {
                let mut reset = args(&["reset", "-q", "HEAD", "--"]);
                reset.extend(paths.iter().cloned());
                self.run(&reset, BTreeMap::new(), None)?;
            }
            return Ok(commit);
        }
        Err(SyncCommitError(format!(
            "HEAD kept moving; the sync commit was not recorded after {CAS_ATTEMPTS} attempts"
        ))
        .into())
    }

    fn has_sync_commit(&self, range: &str) -> EngineResult<bool> {
        if self.head_id()?.is_none() {
            return Ok(false);
        }
        let trailers = self.run(
            &args(&[
                "log",
                &format!("-{TRAILER_SCAN}"),
                "--format=%(trailers:key=Sync-Seq,valueonly)",
                "HEAD",
            ]),
            BTreeMap::new(),
            None,
        )?;
        Ok(trailers.split('\n').any(|line| line == range))
    }
}

#[cfg(test)]
#[path = "git_sync_repo_tests.rs"]
mod tests;
