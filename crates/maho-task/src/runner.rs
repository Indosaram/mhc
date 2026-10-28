//! How a child runs. [`TaskRunner`] is the interface; [`ProcessRunner`] runs the child as a
//! non-interactive `mhc -p` process of the current executable, with its own session (so a later run
//! of the same task resumes it), its final answer on stdout redirected to the task's output file
//! and stderr to its log. Behavior spec: `docs/cleanroom/specs/task.md` §3.
use crate::settings::{DEPTH_ENV, TASK_ID_ENV};
use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Cancellation shared between a run and whoever may stop it.
#[derive(Clone, Debug)]
pub struct CancelToken(tokio::sync::watch::Sender<bool>);

impl Default for CancelToken {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
}

impl CancelToken {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    /// Resolves once cancelled (immediately when already cancelled).
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

/// Everything one run needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunSpec {
    pub task_id: String,
    pub session_id: String,
    pub cwd: PathBuf,
    /// The user message of this run (the task, or the follow-up of a resume).
    pub prompt: String,
    /// File whose contents are appended to the child's system prompt.
    pub system_prompt_file: Option<PathBuf>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    /// Tool allowlist (`None` = the child's defaults).
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Vec<String>,
    /// Depth of the child (parent depth + 1).
    pub depth: u32,
    pub output_path: PathBuf,
    pub log_path: PathBuf,
    pub sessions_dir: PathBuf,
}

/// How a run ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOutcome {
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    /// The child could not be started (or waited on).
    pub error: Option<String>,
}

impl RunOutcome {
    pub fn succeeded(&self) -> bool {
        !self.cancelled && self.error.is_none() && self.exit_code == Some(0)
    }
}

/// Runs children. `on_start` receives the child's pid once it is running.
pub trait TaskRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        spec: &'a RunSpec,
        cancel: CancelToken,
        on_start: Box<dyn FnOnce(Option<u32>) + Send + 'a>,
    ) -> BoxFuture<'a, RunOutcome>;
}

/// Runs each child as `<exe> -p ...`.
#[derive(Clone, Debug)]
pub struct ProcessRunner {
    pub exe: PathBuf,
    /// Arguments placed before the generated ones (tests, wrappers).
    pub leading_args: Vec<OsString>,
}

impl ProcessRunner {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            leading_args: Vec::new(),
        }
    }

    /// The current executable.
    ///
    /// # Errors
    /// When the executable path cannot be determined.
    pub fn current_exe() -> std::io::Result<Self> {
        std::env::current_exe().map(Self::new)
    }

    /// The child's command-line arguments (after `leading_args`).
    pub fn args(spec: &RunSpec) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "-p".into(),
            "--mode".into(),
            "text".into(),
            "--session-dir".into(),
            spec.sessions_dir.clone().into_os_string(),
            "--session-id".into(),
            spec.session_id.clone().into(),
            "--name".into(),
            spec.task_id.clone().into(),
        ];
        if let Some(model) = &spec.model {
            args.push("--model".into());
            args.push(model.into());
        }
        if let Some(thinking) = &spec.thinking {
            args.push("--thinking".into());
            args.push(thinking.into());
        }
        if let Some(file) = &spec.system_prompt_file {
            args.push("--append-system-prompt".into());
            args.push(file.clone().into_os_string());
        }
        if let Some(tools) = &spec.tools {
            args.push("--tools".into());
            args.push(tools.join(",").into());
        }
        if !spec.exclude_tools.is_empty() {
            args.push("--exclude-tools".into());
            args.push(spec.exclude_tools.join(",").into());
        }
        args.push("--".into());
        args.push(spec.prompt.clone().into());
        args
    }
}

fn open(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create(path)
}

impl TaskRunner for ProcessRunner {
    fn run<'a>(
        &'a self,
        spec: &'a RunSpec,
        cancel: CancelToken,
        on_start: Box<dyn FnOnce(Option<u32>) + Send + 'a>,
    ) -> BoxFuture<'a, RunOutcome> {
        Box::pin(async move {
            let failed = |error: String| RunOutcome {
                error: Some(error),
                ..RunOutcome::default()
            };
            let (stdout, stderr) = match (open(&spec.output_path), open(&spec.log_path)) {
                (Ok(out), Ok(err)) => (out, err),
                (Err(error), _) | (_, Err(error)) => {
                    return failed(format!("cannot create the task output files: {error}"));
                }
            };
            if let Err(error) = std::fs::create_dir_all(&spec.sessions_dir) {
                return failed(format!("cannot create the task session directory: {error}"));
            }
            let mut command = tokio::process::Command::new(&self.exe);
            command
                .args(&self.leading_args)
                .args(Self::args(spec))
                .current_dir(&spec.cwd)
                .env(DEPTH_ENV, spec.depth.to_string())
                .env(TASK_ID_ENV, &spec.task_id)
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                // A background child outlives a parent that exits; its output stays on disk.
                .kill_on_drop(false);
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    return failed(format!(
                        "cannot start {}: {error}",
                        self.exe.display()
                    ));
                }
            };
            on_start(child.id());
            tokio::select! {
                status = child.wait() => match status {
                    Ok(status) => RunOutcome { exit_code: status.code(), ..RunOutcome::default() },
                    Err(error) => failed(format!("waiting for the child failed: {error}")),
                },
                () = cancel.cancelled() => {
                    let _ = child.start_kill();
                    let status = child.wait().await.ok();
                    RunOutcome {
                        exit_code: status.and_then(|status| status.code()),
                        cancelled: true,
                        error: None,
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(dir: &std::path::Path) -> RunSpec {
        RunSpec {
            task_id: "T".into(),
            session_id: "sid".into(),
            cwd: dir.to_path_buf(),
            prompt: "-starts with a dash".into(),
            system_prompt_file: Some(dir.join("p.md")),
            model: Some("a/b".into()),
            thinking: Some("low".into()),
            tools: Some(vec!["read".into(), "grep".into()]),
            exclude_tools: vec!["bash".into()],
            depth: 1,
            output_path: dir.join("T.md"),
            log_path: dir.join("T.log"),
            sessions_dir: dir.join("sessions"),
        }
    }

    #[test]
    fn builds_print_mode_arguments() {
        let dir = std::path::Path::new("/s");
        let args: Vec<String> = ProcessRunner::args(&spec(dir))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // Path arguments are real OS paths, so build them with the platform's separator.
        let sessions = dir.join("sessions").to_string_lossy().into_owned();
        let prompt_file = dir.join("p.md").to_string_lossy().into_owned();
        assert_eq!(
            args,
            [
                "-p", "--mode", "text", "--session-dir", &sessions, "--session-id", "sid",
                "--name", "T", "--model", "a/b", "--thinking", "low", "--append-system-prompt",
                &prompt_file, "--tools", "read,grep", "--exclude-tools", "bash", "--",
                "-starts with a dash"
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_and_cancels_a_child() {
        let dir = tempfile::tempdir().expect("tmp");
        let script = dir.path().join("child.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nfor last; do :; done\nif [ \"$last\" = sleep ]; then sleep 30; fi\necho \"depth=$MHC_TASK_DEPTH id=$MHC_TASK_ID last=$last\"\necho log >&2\n",
        )
        .expect("script");
        let mut perms = std::fs::metadata(&script).expect("meta").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script, perms).expect("chmod");
        let runner = ProcessRunner::new(&script);
        let mut run = spec(dir.path());
        run.prompt = "hello".into();
        let outcome = runner
            .run(&run, CancelToken::default(), Box::new(|pid| assert!(pid.is_some())))
            .await;
        assert!(outcome.succeeded(), "{outcome:?}");
        let out = std::fs::read_to_string(&run.output_path).expect("out");
        assert_eq!(out.trim(), "depth=1 id=T last=hello");
        assert_eq!(std::fs::read_to_string(&run.log_path).expect("log").trim(), "log");

        run.prompt = "sleep".into();
        let cancel = CancelToken::default();
        let stopper = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            stopper.cancel();
        });
        let started = std::time::Instant::now();
        let outcome = runner.run(&run, cancel, Box::new(|_| {})).await;
        assert!(outcome.cancelled);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[tokio::test]
    async fn missing_executable_is_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let runner = ProcessRunner::new(dir.path().join("missing"));
        let outcome = runner
            .run(&spec(dir.path()), CancelToken::default(), Box::new(|_| {}))
            .await;
        assert!(outcome.error.is_some_and(|e| e.contains("cannot start")));
    }
}
