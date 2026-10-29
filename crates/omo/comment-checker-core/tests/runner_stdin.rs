use std::io;
use std::sync::{Arc, Mutex};

use comment_checker_core::*;

struct StdinFailingProcess {
    signals: Arc<Mutex<Vec<SpawnSignal>>>,
    stderr: String,
}

impl SpawnProcess for StdinFailingProcess {
    fn write_stdin(&mut self, _input: &str) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "write EPIPE"))
    }

    fn end_stdin(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "write EPIPE"))
    }

    fn take_stdout(&mut self) -> ByteStream {
        Box::pin(tokio::io::empty())
    }

    fn take_stderr(&mut self) -> ByteStream {
        Box::pin(io::Cursor::new(self.stderr.clone().into_bytes()))
    }

    fn exited(&mut self) -> ExitFuture<'_> {
        Box::pin(async move { Ok(2) })
    }

    fn kill(&mut self, signal: SpawnSignal) -> io::Result<()> {
        self.signals.lock().expect("lock signals").push(signal);
        Ok(())
    }
}

#[tokio::test]
async fn given_stdin_delivery_fails_when_checker_still_exits_2_then_child_is_killed_and_result_is_empty()
{
    let signals = Arc::new(Mutex::new(Vec::new()));
    let captured = signals.clone();
    let result = run_comment_checker(
        &RunCommentCheckerInput {
            binary_path: Some("/checker".to_string()),
            hook_input: HookInput {
                session_id: "session-1".to_string(),
                tool_name: "write".to_string(),
                transcript_path: String::new(),
                cwd: ".".to_string(),
                hook_event_name: "PostToolUse".to_string(),
                tool_input: HookToolInput {
                    file_path: Some("src/a.ts".to_string()),
                    content: Some("x".to_string()),
                    old_string: None,
                    new_string: None,
                    edits: None,
                },
                tool_response: None,
            },
            custom_prompt: None,
        },
        &RunCommentCheckerOptions {
            spawn: &move |_args| {
                Ok(Box::new(StdinFailingProcess {
                    signals: captured.clone(),
                    stderr: "partial input looked like a comment".to_string(),
                }))
            },
            exists_sync: &|_path| true,
            timeout_ms: None,
            kill_grace_ms: None,
        },
    )
    .await
    .expect("run_comment_checker");

    assert_eq!(
        result,
        CheckResult {
            has_comments: false,
            message: String::new(),
            failure: None,
        }
    );
    assert_eq!(*signals.lock().expect("lock signals"), vec![SpawnSignal::Sigkill]);
}

#[tokio::test]
async fn given_spawn_fails_synchronously_when_checker_runs_then_no_comments_reported_and_start_failure_exposed()
{
    let result = run_comment_checker(
        &RunCommentCheckerInput {
            binary_path: Some("/checker".to_string()),
            hook_input: HookInput {
                session_id: "session-1".to_string(),
                tool_name: "write".to_string(),
                transcript_path: String::new(),
                cwd: ".".to_string(),
                hook_event_name: "PostToolUse".to_string(),
                tool_input: HookToolInput::default(),
                tool_response: None,
            },
            custom_prompt: None,
        },
        &RunCommentCheckerOptions {
            spawn: &|_args| {
                Err(io::Error::new(io::ErrorKind::InvalidInput, "spawn EINVAL"))
            },
            exists_sync: &|_path| true,
            timeout_ms: None,
            kill_grace_ms: None,
        },
    )
    .await
    .expect("run_comment_checker");

    assert_eq!(
        result,
        CheckResult {
            has_comments: false,
            message: String::new(),
            failure: Some(CheckFailure {
                exit_code: None,
                stderr: "spawn EINVAL".to_string(),
            }),
        }
    );
}
