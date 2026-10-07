//! Real-process integration for `run_comment_checker` (SC-U1).
//!
//! The crate's `SpawnProcess` seam is injected and the production spawner lives in the consumer
//! crate; this file adds a test-only adapter over a real `tokio::process::Child` so the runner's
//! stdin/exit/CRLF/timeout contract is exercised against an actual process, without duplicating
//! the production spawner.
#![cfg(unix)]

use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use comment_checker_core::ByteStream;
use comment_checker_core::CheckResult;
use comment_checker_core::ExitFuture;
use comment_checker_core::HookInput;
use comment_checker_core::HookToolInput;
use comment_checker_core::RunCommentCheckerInput;
use comment_checker_core::RunCommentCheckerOptions;
use comment_checker_core::SpawnProcess;
use comment_checker_core::SpawnSignal;
use comment_checker_core::run_comment_checker;
use pretty_assertions::assert_eq;
use tokio::io::AsyncWriteExt;
use tokio::process::Child;
use tokio::process::ChildStderr;
use tokio::process::ChildStdout;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

type StdinMessage = Option<String>;

struct RealProcess {
    child: Option<Child>,
    stdin: Option<mpsc::UnboundedSender<StdinMessage>>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    exit_notify: Option<oneshot::Sender<()>>,
    signals: Arc<Mutex<Vec<SpawnSignal>>>,
}

impl SpawnProcess for RealProcess {
    fn write_stdin(&mut self, input: &str) -> io::Result<()> {
        self.stdin
            .as_ref()
            .ok_or_else(|| io::Error::other("stdin already ended"))?
            .send(Some(input.to_owned()))
            .map_err(|_| io::Error::other("stdin writer stopped"))
    }

    fn end_stdin(&mut self) -> io::Result<()> {
        if let Some(stdin) = self.stdin.take() {
            let _sent = stdin.send(None);
        }
        Ok(())
    }

    fn take_stdout(&mut self) -> ByteStream {
        Box::pin(self.stdout.take().expect("stdout is taken once"))
    }

    fn take_stderr(&mut self) -> ByteStream {
        Box::pin(self.stderr.take().expect("stderr is taken once"))
    }

    fn exited(&mut self) -> ExitFuture<'_> {
        let child = self.child.as_mut().expect("child is alive until kill");
        Box::pin(async move { Ok(child.wait().await?.code().unwrap_or(-1)) })
    }

    fn kill(&mut self, signal: SpawnSignal) -> io::Result<()> {
        self.signals
            .lock()
            .map_err(|_| io::Error::other("signal log poisoned"))?
            .push(signal);
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let Some(pid) = child.id() else {
            return Ok(());
        };
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
        let exit_notify = self.exit_notify.take();
        tokio::spawn(async move {
            let _status = child.wait().await;
            if let Some(exit_notify) = exit_notify {
                let _sent = exit_notify.send(());
            }
        });
        Ok(())
    }
}

struct RealHarness {
    directory: tempfile::TempDir,
    binary: PathBuf,
    signals: Arc<Mutex<Vec<SpawnSignal>>>,
    exit: Mutex<Option<oneshot::Receiver<()>>>,
}

impl RealHarness {
    fn new(script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("fixture directory");
        let binary = directory.path().join("checker");
        std::fs::write(&binary, script).expect("write checker fixture");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
            .expect("mark checker executable");
        Self {
            directory,
            binary,
            signals: Arc::new(Mutex::new(Vec::new())),
            exit: Mutex::new(None),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    async fn run(&self, custom_prompt: Option<&str>, timeout_ms: Option<u64>) -> io::Result<CheckResult> {
        let (exit_tx, exit_rx) = oneshot::channel();
        *self.exit.lock().expect("exit slot") = Some(exit_rx);
        let binary = self.binary.clone();
        let signals = Arc::clone(&self.signals);
        let exit_slot = Arc::new(Mutex::new(Some(exit_tx)));
        let spawn = move |args: &[String]| -> io::Result<Box<dyn SpawnProcess>> {
            let mut command = tokio::process::Command::new(&binary);
            command
                .args(&args[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = command.spawn()?;
            let stdin = child.stdin.take().expect("child stdin");
            let stdout = child.stdout.take().expect("child stdout");
            let stderr = child.stderr.take().expect("child stderr");
            let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<StdinMessage>();
            tokio::spawn(async move {
                let mut stdin = stdin;
                while let Some(message) = stdin_rx.recv().await {
                    match message {
                        Some(payload) => {
                            if stdin.write_all(payload.as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        None => {
                            let _shutdown = stdin.shutdown().await;
                            break;
                        }
                    }
                }
            });
            let exit_notify = exit_slot
                .lock()
                .map_err(|_| io::Error::other("exit slot poisoned"))?
                .take();
            Ok(Box::new(RealProcess {
                child: Some(child),
                stdin: Some(stdin_tx),
                stdout: Some(stdout),
                stderr: Some(stderr),
                exit_notify,
                signals: Arc::clone(&signals),
            }))
        };
        let exists = |path: &str| Path::new(path).exists();
        let options = RunCommentCheckerOptions {
            spawn: &spawn,
            exists_sync: &exists,
            timeout_ms,
            kill_grace_ms: None,
        };
        run_comment_checker(&input(&self.binary, custom_prompt), &options).await
    }

    async fn child_exited(&self) -> bool {
        let receiver = self.exit.lock().expect("exit slot").take();
        match receiver {
            Some(receiver) => tokio::time::timeout(Duration::from_secs(5), receiver).await.is_ok(),
            None => false,
        }
    }
}

fn input(binary: &Path, custom_prompt: Option<&str>) -> RunCommentCheckerInput {
    RunCommentCheckerInput {
        hook_input: HookInput {
            session_id: "ses_real".to_string(),
            tool_name: "Write".to_string(),
            transcript_path: "/tmp/t.jsonl".to_string(),
            cwd: "/repo".to_string(),
            hook_event_name: "PostToolUse".to_string(),
            tool_input: HookToolInput {
                file_path: Some("src/a.ts".to_string()),
                content: Some("// hi\n".to_string()),
                ..HookToolInput::default()
            },
            tool_response: None,
        },
        binary_path: Some(binary.to_string_lossy().into_owned()),
        custom_prompt: custom_prompt.map(str::to_string),
    }
}

#[tokio::test]
async fn real_checker_receives_hook_json_and_reports_exit_two() {
    let harness = RealHarness::new(
        "#!/bin/sh\ncat > \"$0.stdin\"\nprintf 'COMMENT\\r\\nline two\\r\\n' >&2\nexit 2\n",
    );

    let result = harness.run(None, None).await.expect("run");

    assert_eq!(
        result,
        CheckResult {
            has_comments: true,
            message: "COMMENT\nline two\n".to_string(),
        }
    );
    let recorded = std::fs::read_to_string(harness.path("checker.stdin")).expect("recorded stdin");
    let payload: serde_json::Value = serde_json::from_str(&recorded).expect("hook json");
    assert_eq!(payload["session_id"], "ses_real");
    assert_eq!(
        payload["tool_input"],
        serde_json::json!({ "file_path": "src/a.ts", "content": "// hi\n" })
    );
}

#[tokio::test]
async fn real_checker_receives_the_check_and_prompt_arguments() {
    let harness = RealHarness::new(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\ncat > /dev/null\nexit 0\n",
    );

    let result = harness.run(Some("be strict"), None).await.expect("run");

    assert_eq!(result, CheckResult::default());
    assert_eq!(
        std::fs::read_to_string(harness.path("checker.args")).expect("recorded args"),
        "check\n--prompt\nbe strict\n"
    );
}

#[tokio::test]
async fn real_checker_timeout_sends_sigterm_and_reaps_the_child() {
    let harness = RealHarness::new("#!/bin/sh\ncat > /dev/null\nexec sleep 30\n");

    let result = harness.run(None, Some(200)).await.expect("run");

    assert_eq!(result, CheckResult::default());
    assert_eq!(*harness.signals.lock().expect("signals"), vec![SpawnSignal::Sigterm]);
    assert!(
        harness.child_exited().await,
        "the timed-out child must be reaped by the SIGTERM, not orphaned"
    );
}

#[tokio::test]
async fn real_checker_drains_large_stderr_without_deadlock() {
    let harness = RealHarness::new(
        "#!/bin/sh\ncat > /dev/null\ni=0\nwhile [ $i -lt 20000 ]; do printf 'line\\r\\n'; i=$((i+1)); done >&2\nexit 2\n",
    );

    let result = harness.run(None, None).await.expect("run");

    assert!(result.has_comments);
    assert_eq!(result.message.matches('\n').count(), 20000);
    assert!(!result.message.contains('\r'));
}
