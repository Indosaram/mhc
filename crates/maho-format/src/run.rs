//! Spec §5: binary lookup, running a formatter command, line counts, and sharing one run per path.
use std::collections::HashMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::OnceCell;

/// Result of formatting one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Unchanged,
    /// The file changed on disk; `formatter` is the name shown to the model.
    Formatted {
        formatter: String,
        added: usize,
        removed: usize,
    },
    /// The formatter binary was not found, could not be spawned, or waiting on it failed.
    Missing,
}

/// How a command ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    Success,
    Failure,
    TimedOut,
    /// Not spawnable, or waiting on it failed.
    Broken,
}

/// Process environment of the engine: where binaries are looked up, the user's home, and counters
/// of the processes spawned (observable by callers and tests).
#[derive(Debug, Default)]
pub struct Host {
    /// Replaces `PATH` when set.
    pub search_path: Option<Vec<PathBuf>>,
    /// Replaces the user's home directory when set.
    pub home: Option<PathBuf>,
    processes: AtomicUsize,
    formatter_runs: AtomicUsize,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Executable suffixes tried after the bare name.
fn suffixes() -> Vec<String> {
    let mut list = vec![String::new()];
    if cfg!(windows) {
        let path_ext = std::env::var("PATHEXT").unwrap_or_default();
        for ext in path_ext
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(str::to_ascii_lowercase)
            .chain([".exe", ".cmd", ".bat"].map(str::to_owned))
        {
            if !list.contains(&ext) {
                list.push(ext);
            }
        }
    }
    list
}

impl Host {
    pub fn new(search_path: Option<Vec<PathBuf>>, home: Option<PathBuf>) -> Self {
        Self {
            search_path,
            home,
            ..Self::default()
        }
    }

    /// Every child process spawned (formatters, `git`, Bun probes).
    pub fn processes(&self) -> usize {
        self.processes.load(Ordering::SeqCst)
    }

    /// Formatter commands spawned on a file.
    pub fn formatter_runs(&self) -> usize {
        self.formatter_runs.load(Ordering::SeqCst)
    }

    /// The user's home directory.
    pub fn home_dir(&self) -> Option<PathBuf> {
        self.home.clone().or_else(|| {
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        })
    }

    /// Project-local tool directories from `cwd` upwards (the LSP installer's bases), then `PATH`.
    pub fn resolve_binary(&self, cwd: &Path, program: &str) -> Option<PathBuf> {
        let mut directories: Vec<PathBuf> = cwd
            .ancestors()
            .flat_map(maho_lsp::config::additional_path_bases)
            .collect();
        match &self.search_path {
            Some(paths) => directories.extend(paths.iter().cloned()),
            None => {
                let path: OsString = std::env::var_os("PATH")
                    .or_else(|| std::env::var_os("Path"))
                    .unwrap_or_default();
                directories.extend(std::env::split_paths(&path));
            }
        }
        let suffixes = suffixes();
        directories.iter().find_map(|directory| {
            suffixes
                .iter()
                .map(|suffix| directory.join(format!("{program}{suffix}")))
                .find(|candidate| candidate.is_file())
        })
    }

    /// Runs `program args` in `cwd` with all stdio discarded; killed after `timeout`.
    pub async fn run(&self, program: &Path, args: &[OsString], cwd: &Path, timeout: Duration) -> Exit {
        let mut command = command_for(program, args);
        command
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        self.processes.fetch_add(1, Ordering::SeqCst);
        let Ok(mut child) = command.spawn() else {
            return Exit::Broken;
        };
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) if status.success() => Exit::Success,
            Ok(Ok(_)) => Exit::Failure,
            Ok(Err(_)) => Exit::Broken,
            Err(_) => {
                let _ = child.kill().await;
                Exit::TimedOut
            }
        }
    }

    /// Spec §5 steps 2-5: the CLI formatter `program args file` on `file`.
    pub async fn format_with_cli(
        &self,
        name: &str,
        program: &str,
        args: &[&str],
        file: &Path,
        cwd: &Path,
        timeout: Duration,
    ) -> RunOutcome {
        let Some(binary) = self.resolve_binary(cwd, program) else {
            return RunOutcome::Missing;
        };
        let Ok(before) = std::fs::read(file) else {
            return RunOutcome::Unchanged;
        };
        let mut argv: Vec<OsString> = args.iter().map(OsString::from).collect();
        argv.push(file.as_os_str().to_owned());
        self.formatter_runs.fetch_add(1, Ordering::SeqCst);
        match self.run(&binary, &argv, cwd, timeout).await {
            Exit::Broken => RunOutcome::Missing,
            Exit::Failure | Exit::TimedOut => {
                restore(file, &before);
                RunOutcome::Unchanged
            }
            Exit::Success => match std::fs::read(file) {
                Ok(after) if after != before => {
                    let (added, removed) = line_delta(&before, &after);
                    RunOutcome::Formatted {
                        formatter: name.to_owned(),
                        added,
                        removed,
                    }
                }
                _ => RunOutcome::Unchanged,
            },
        }
    }
}

fn restore(file: &Path, before: &[u8]) {
    if std::fs::read(file).ok().as_deref() != Some(before) {
        let _ = std::fs::write(file, before);
    }
}

#[cfg(windows)]
fn command_for(program: &Path, args: &[OsString]) -> tokio::process::Command {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let shim = program
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|ext| ext == "cmd" || ext == "bat");
    let mut command = if shim {
        let mut command = tokio::process::Command::new("cmd.exe");
        command.arg("/d").arg("/c").arg(program);
        command
    } else {
        tokio::process::Command::new(program)
    };
    command.args(args).creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(not(windows))]
fn command_for(program: &Path, args: &[OsString]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command.args(args);
    command
}

/// Number of `\n`-separated segments (a trailing newline adds one).
pub fn line_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte == b'\n').count() + 1
}

/// `(added, removed)` lines between two contents.
pub fn line_delta(before: &[u8], after: &[u8]) -> (usize, usize) {
    let (before, after) = (line_count(before), line_count(after));
    (after.saturating_sub(before), before.saturating_sub(after))
}

type Slot = Arc<OnceCell<RunOutcome>>;

/// One in-flight run per absolute path: concurrent callers share the first caller's result; a
/// cancelled run lets the next caller run its own.
#[derive(Debug, Default)]
pub struct SharedRuns {
    slots: Mutex<HashMap<PathBuf, Slot>>,
}

impl SharedRuns {
    pub async fn run<F>(&self, path: &Path, work: F) -> RunOutcome
    where
        F: Future<Output = RunOutcome>,
    {
        let slot = Arc::clone(lock(&self.slots).entry(path.to_path_buf()).or_default());
        let outcome = slot.get_or_init(|| work).await.clone();
        let mut slots = lock(&self.slots);
        if slots.get(path).is_some_and(|current| Arc::ptr_eq(current, &slot)) {
            slots.remove(path);
        }
        outcome
    }

    /// Paths with a run in flight (or abandoned by a cancelled caller).
    pub fn pending(&self) -> usize {
        lock(&self.slots).len()
    }
}
