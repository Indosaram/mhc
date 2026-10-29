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
            &self.options(env.clone(), stdin.map(|s| s.as_bytes().to_vec())),
        )?;
        if result.code != 0 {
            if result
                .stderr
                .to_ascii_lowercase()
                .contains("unable to create")
                && result.stderr.contains(".lock")
            {
                if recover_stranded_locks(&self.root, &self.git_dir)? {
                    return self.run(argv, env, stdin);
                }
                return Err(stranded_lock(&self.git_dir, &result.stderr));
            }
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
                if moved
                    .stderr
                    .to_ascii_lowercase()
                    .contains("unable to create")
                    && moved.stderr.contains(".lock")
                {
                    if recover_stranded_locks(&self.root, &self.git_dir)? {
                        continue;
                    }
                    return Err(stranded_lock(&self.git_dir, &moved.stderr));
                }
                continue;
            }
            return Ok(commit);
        }
        Err(SyncCommitError(format!(
            "HEAD kept moving; the sync commit was not recorded after {CAS_ATTEMPTS} attempts"
        ))
        .into())
    }

    fn sync_index(&self, paths: &[String]) -> EngineResult<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut reset = args(&["reset", "-q", "HEAD", "--"]);
        reset.extend(paths.iter().cloned());
        self.run(&reset, BTreeMap::new(), None)?;
        Ok(())
    }

    fn has_sync_commit(&self, range: &str) -> EngineResult<bool> {
        if self.head_id()?.is_none() {
            return Ok(false);
        }
        let trailers = self.run(
            &args(&[
                "-c",
                "log.showSignature=false",
                "-c",
                "log.decorate=false",
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

fn stranded_lock(git_dir: &Path, stderr: &str) -> super::super::error::EngineError {
    let paths = stderr
        .split('\'')
        .filter(|part| part.ends_with(".lock"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    super::super::error::EngineError::StrandedLock(if paths.is_empty() {
        vec![git_dir.join("index.lock").to_string_lossy().into_owned()]
    } else {
        paths
    })
}

fn recover_stranded_locks(root: &Path, git_dir: &Path) -> EngineResult<bool> {
    use crate::locks::{is_lock_owner_proven_dead, memory_writer_lock_path, parse_lock_record};
    let Some(identity) = root.parent().filter(|p| {
        root.file_name().is_some_and(|n| n == "repo")
            && p.parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "agents")
    }) else {
        return Ok(false);
    };
    let lock = memory_writer_lock_path(&identity.join("runtime/locks"));
    let Some(current) = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|raw| parse_lock_record(&raw))
    else {
        return Ok(false);
    };
    if current.pid != std::process::id() {
        return Ok(false);
    }
    let Some(previous) = crate::locks::acquire::proven_dead_takeover_for(&lock, &current.nonce)
    else {
        return Ok(false);
    };
    if !is_lock_owner_proven_dead(&previous) {
        return Ok(false);
    }
    let mut candidates = Vec::new();
    fn collect(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let meta = entry.file_type()?;
            if recursive && meta.is_dir() {
                collect(&entry.path(), true, out)?;
            } else if meta.is_file() && entry.file_name().to_string_lossy().ends_with(".lock") {
                out.push(entry.path());
            }
        }
        Ok(())
    }
    for name in ["index.lock", "HEAD.lock", "packed-refs.lock", "config.lock"] {
        let path = git_dir.join(name);
        if path.is_file() {
            candidates.push(path);
        }
    }
    if git_dir.join("refs").is_dir() {
        collect(&git_dir.join("refs"), true, &mut candidates)?;
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let before: Vec<_> = candidates
            .iter()
            .map(|p| std::fs::symlink_metadata(p).map(|m| (m.ino(), m.mtime(), m.mtime_nsec())))
            .collect::<Result<_, _>>()?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        let Ok(processes) = std::fs::read_dir("/proc") else {
            return Ok(false);
        };
        for process in processes {
            let Ok(process) = process else {
                return Ok(false);
            };
            let Some(pid) = process
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == std::process::id() {
                continue;
            }
            let proc = process.path();
            let Ok(meta) = std::fs::metadata(&proc) else {
                continue;
            };
            if meta.uid() != std::fs::metadata("/proc/self")?.uid() {
                continue;
            }
            let cwd = match std::fs::read_link(proc.join("cwd")) {
                Ok(path) => path,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Ok(false),
            };
            if cwd.starts_with(root) {
                return Ok(false);
            }
            let Ok(fds) = std::fs::read_dir(proc.join("fd")) else {
                return Ok(false);
            };
            for fd in fds {
                let Ok(fd) = fd else {
                    return Ok(false);
                };
                if std::fs::read_link(fd.path())
                    .ok()
                    .is_some_and(|p| candidates.contains(&p))
                {
                    return Ok(false);
                }
            }
        }
        for (path, snapshot) in candidates.iter().zip(before) {
            let m = std::fs::symlink_metadata(path)?;
            if (m.ino(), m.mtime(), m.mtime_nsec()) != snapshot {
                return Ok(false);
            }
        }
        for path in &candidates {
            std::fs::remove_file(path)?;
        }
        Ok(!candidates.is_empty())
    }
    #[cfg(not(target_os = "linux"))]
    Ok(false)
}
