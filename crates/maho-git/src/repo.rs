//! A thin, synchronous `git` command runner with the repository queries the isolation backend and
//! the git tools need. Process handling follows the MIT oh-my-pi `pi-iso` rcopy helpers
//! (`git_capture`, `git_apply_with_program`: stdin written while stderr is drained on a thread);
//! the query set mirrors the parts of oh-my-pi's `pi-vcs` facade that its task isolation and
//! agentic-commit tools call (`diffText`, `numstat`, `changedFiles`, `lsFiles`, `logSubjects`,
//! `readTree`/`writeTree` on a temporary index, `applyPatch`).
use std::ffi::OsStr;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A failed git invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitError {
    /// `git` is not on `PATH`.
    Missing,
    /// The directory is not inside a git work tree.
    NotARepository(PathBuf),
    /// The command ran and failed.
    Failed {
        command: String,
        code: i32,
        stderr: String,
    },
    /// Spawning or talking to the process failed.
    Io(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("`git` is not on PATH"),
            Self::NotARepository(dir) => write!(f, "not a git repository: {}", dir.display()),
            Self::Failed {
                command,
                code,
                stderr,
            } => {
                let detail = stderr.trim();
                if detail.is_empty() {
                    write!(f, "{command} (exit {code})")
                } else {
                    write!(f, "{command} (exit {code}): {detail}")
                }
            }
            Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for GitError {}

pub type GitResult<T> = Result<T, GitError>;

/// One `git diff --numstat` row; binary files have no counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumstatEntry {
    pub path: String,
    pub added: Option<u64>,
    pub removed: Option<u64>,
}

/// Options of [`Git::apply`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ApplyOptions {
    /// Apply to the index only (`--cached`).
    pub cached: bool,
    /// Also update the index (`--index`).
    pub index: bool,
    /// Fall back to a three-way merge (`--3way`).
    pub three_way: bool,
    /// Apply in reverse (`--reverse`).
    pub reverse: bool,
    /// Only check (`--check`).
    pub check: bool,
}

/// `git` bound to a working directory and, optionally, a temporary index file.
#[derive(Clone, Debug)]
pub struct Git {
    dir: PathBuf,
    index_file: Option<PathBuf>,
}

impl Git {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            index_file: None,
        }
    }

    /// The same repository with `GIT_INDEX_FILE` pointing at `index`.
    #[must_use]
    pub fn with_index(&self, index: impl Into<PathBuf>) -> Self {
        Self {
            dir: self.dir.clone(),
            index_file: Some(index.into()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.dir)
            .args(["-c", "core.quotepath=false", "-c", "color.ui=false"])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match &self.index_file {
            Some(index) => {
                command.env("GIT_INDEX_FILE", index);
            }
            None => {
                command.env_remove("GIT_INDEX_FILE");
            }
        }
        command
    }

    /// Runs `git <args>` and returns stdout; a non-zero exit is an error.
    ///
    /// # Errors
    /// Spawn failure or non-zero exit.
    pub fn run(&self, args: &[&str]) -> GitResult<Vec<u8>> {
        self.run_with_input(args, None)
    }

    /// Like [`Self::run`] with `input` written to stdin (stderr is drained concurrently so a large
    /// input cannot deadlock the child, as in oh-my-pi `git_apply_with_program`).
    ///
    /// # Errors
    /// Spawn failure or non-zero exit.
    pub fn run_with_input(&self, args: &[&str], input: Option<&[u8]>) -> GitResult<Vec<u8>> {
        let mut command = self.command(args);
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                GitError::Missing
            } else {
                GitError::Io(format!(
                    "spawn git {}: {error}",
                    args.first().unwrap_or(&"")
                ))
            }
        })?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| GitError::Io("git: stdout not piped".into()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| GitError::Io("git: stderr not piped".into()))?;
        let err_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        let out_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            bytes
        });
        let write_result = match (input, child.stdin.take()) {
            (Some(bytes), Some(mut stdin)) => stdin.write_all(bytes),
            _ => Ok(()),
        };
        let status = child
            .wait()
            .map_err(|error| GitError::Io(format!("wait git: {error}")))?;
        let out = out_reader.join().unwrap_or_default();
        let err = err_reader.join().unwrap_or_default();
        if status.success() {
            write_result.map_err(|error| GitError::Io(format!("write to git: {error}")))?;
            return Ok(out);
        }
        Err(GitError::Failed {
            command: format!("git {}", args.join(" ")),
            code: status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&err).into_owned(),
        })
    }

    /// stdout as (lossy) UTF-8.
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn text(&self, args: &[&str]) -> GitResult<String> {
        self.run(args)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Trimmed single-line output.
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn line(&self, args: &[&str]) -> GitResult<String> {
        self.text(args).map(|text| text.trim().to_owned())
    }

    /// Top level of the work tree containing `dir`.
    ///
    /// # Errors
    /// [`GitError::NotARepository`] outside a work tree.
    pub fn repo_root(&self) -> GitResult<PathBuf> {
        match self.line(&["rev-parse", "--show-toplevel"]) {
            Ok(root) if !root.is_empty() => Ok(PathBuf::from(root)),
            Ok(_) | Err(GitError::Failed { .. }) => Err(GitError::NotARepository(self.dir.clone())),
            Err(error) => Err(error),
        }
    }

    /// Whether `dir` is inside a git work tree.
    pub fn is_repository(&self) -> bool {
        self.line(&["rev-parse", "--is-inside-work-tree"])
            .is_ok_and(|answer| answer == "true")
    }

    /// The commit `HEAD` names, `None` on an unborn branch.
    pub fn head(&self) -> Option<String> {
        self.line(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
            .ok()
            .filter(|sha| !sha.is_empty())
    }

    /// Absolute path of the common git dir.
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn common_dir(&self) -> GitResult<PathBuf> {
        let dir = PathBuf::from(self.line(&["rev-parse", "--git-common-dir"])?);
        Ok(if dir.is_absolute() {
            dir
        } else {
            self.dir.join(dir)
        })
    }

    /// `git diff [--cached] [-- files]` as text (no colour, binary patches when `binary`).
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn diff_text(&self, cached: bool, binary: bool, files: &[&str]) -> GitResult<String> {
        let mut args = vec!["diff", "--no-color", "--no-ext-diff"];
        if cached {
            args.push("--cached");
        }
        if binary {
            args.push("--binary");
        }
        if !files.is_empty() {
            args.push("--");
            args.extend_from_slice(files);
        }
        self.text(&args)
    }

    /// Bytes of `git diff --binary --full-index <from> <to>` between two trees or commits.
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn diff_trees(&self, from: &str, to: &str) -> GitResult<Vec<u8>> {
        self.run(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--binary",
            "--full-index",
            from,
            to,
        ])
    }

    /// Paths with staged (`cached`) or unstaged changes.
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn changed_files(&self, cached: bool) -> GitResult<Vec<String>> {
        let mut args = vec!["diff", "--name-only", "-z", "--no-ext-diff"];
        if cached {
            args.push("--cached");
        }
        Ok(split_nul(&self.run(&args)?))
    }

    /// `git diff --numstat` rows (renames reported under their new path).
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn numstat(&self, cached: bool) -> GitResult<Vec<NumstatEntry>> {
        let mut args = vec!["diff", "--numstat", "-z", "--no-renames", "--no-ext-diff"];
        if cached {
            args.push("--cached");
        }
        let out = self.run(&args)?;
        Ok(split_nul(&out)
            .into_iter()
            .filter_map(|row| {
                let mut parts = row.splitn(3, '\t');
                let added = parts.next()?;
                let removed = parts.next()?;
                let path = parts.next()?.to_owned();
                Some(NumstatEntry {
                    path,
                    added: added.parse().ok(),
                    removed: removed.parse().ok(),
                })
            })
            .collect())
    }

    /// Untracked, non-ignored paths (`ls-files --others --exclude-standard`).
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn untracked(&self) -> GitResult<Vec<String>> {
        Ok(split_nul(&self.run(&[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
        ])?))
    }

    /// Subjects of the last `count` commits (empty on an unborn branch).
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn log_subjects(&self, count: usize) -> GitResult<Vec<String>> {
        if self.head().is_none() {
            return Ok(Vec::new());
        }
        let limit = format!("-n{count}");
        Ok(self
            .text(&["log", &limit, "--format=%s"])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// `git apply --binary` of `patch` with `options`.
    ///
    /// # Errors
    /// The patch does not apply (or does not check).
    pub fn apply(&self, patch: &[u8], options: ApplyOptions) -> GitResult<()> {
        let mut args = vec!["apply", "--binary", "--whitespace=nowarn"];
        if options.cached {
            args.push("--cached");
        }
        if options.index {
            args.push("--index");
        }
        if options.three_way {
            args.push("--3way");
        }
        if options.reverse {
            args.push("--reverse");
        }
        if options.check {
            args.push("--check");
        }
        args.push("-");
        self.run_with_input(&args, Some(patch)).map(|_| ())
    }

    /// Writes the tree of `HEAD` (or the empty tree on an unborn branch) plus every change in the
    /// work tree, ignored files excepted, through a temporary index; returns the tree id. The real
    /// index and work tree are not touched.
    ///
    /// # Errors
    /// Any git step fails or the temporary index cannot be created.
    pub fn snapshot_tree(&self, scratch: &Path) -> GitResult<String> {
        std::fs::create_dir_all(scratch)
            .map_err(|error| GitError::Io(format!("create {}: {error}", scratch.display())))?;
        let index = scratch.join(format!("index-{}-{}", std::process::id(), unique()));
        let temp = self.with_index(&index);
        let result = (|| {
            if self.head().is_some() {
                temp.run(&["read-tree", "HEAD"])?;
            } else {
                temp.run(&["read-tree", "--empty"])?;
            }
            temp.run(&["add", "-A", "--", "."])?;
            temp.line(&["write-tree"])
        })();
        let _ = std::fs::remove_file(&index);
        result
    }

    /// Tree id of `base` (a tree-ish) with `patch` applied, through a temporary index; tries a
    /// plain apply, then `--3way`.
    ///
    /// # Errors
    /// The patch applies neither way.
    pub fn patched_tree(&self, base: &str, patch: &[u8], scratch: &Path) -> GitResult<String> {
        std::fs::create_dir_all(scratch)
            .map_err(|error| GitError::Io(format!("create {}: {error}", scratch.display())))?;
        let mut last = None;
        for three_way in [false, true] {
            let index = scratch.join(format!("index-{}-{}", std::process::id(), unique()));
            let temp = self.with_index(&index);
            let result = (|| {
                temp.run(&["read-tree", base])?;
                if !patch.is_empty() {
                    temp.apply(
                        patch,
                        ApplyOptions {
                            cached: true,
                            three_way,
                            ..ApplyOptions::default()
                        },
                    )?;
                }
                temp.line(&["write-tree"])
            })();
            let _ = std::fs::remove_file(&index);
            match result {
                Ok(tree) => return Ok(tree),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| GitError::Io("patch did not apply".into())))
    }

    /// Runs `git <args>` with a fallback author/committer identity (`mhc <mhc@localhost>`) when the
    /// repository has none configured, so isolation commits work in fresh environments.
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn run_committing(&self, args: &[&str]) -> GitResult<Vec<u8>> {
        if self.has_identity() {
            return self.run(args);
        }
        let mut full = vec!["-c", "user.name=mhc", "-c", "user.email=mhc@localhost"];
        full.extend_from_slice(args);
        self.run(&full)
    }

    /// Creates a commit object of `tree` on `parent` and returns its id (no ref moves).
    ///
    /// # Errors
    /// See [`Self::run`].
    pub fn commit_tree(&self, tree: &str, parent: &str, message: &str) -> GitResult<String> {
        let out = self.run_committing(&["commit-tree", tree, "-p", parent, "-m", message])?;
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    }

    /// Whether the repository has a user identity for commits.
    pub fn has_identity(&self) -> bool {
        self.line(&["config", "user.email"])
            .is_ok_and(|email| !email.is_empty())
    }
}

fn unique() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    nanos ^ (u128::from(COUNTER.fetch_add(1, Ordering::Relaxed)) << 64)
}

/// Splits NUL-terminated `-z` output into strings.
pub fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}
