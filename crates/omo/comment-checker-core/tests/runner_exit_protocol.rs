use std::io;

use comment_checker_core::*;

struct FakeProcess {
    exit_code: Option<i32>,
    stderr: String,
    delivered: bool,
}

impl SpawnProcess for FakeProcess {
    fn write_stdin(&mut self, _input: &str) -> io::Result<()> {
        if self.delivered {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "write EPIPE"))
        }
    }

    fn end_stdin(&mut self) -> io::Result<()> {
        if self.delivered {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "write EPIPE"))
        }
    }

    fn take_stdout(&mut self) -> ByteStream {
        Box::pin(tokio::io::empty())
    }

    fn take_stderr(&mut self) -> ByteStream {
        Box::pin(io::Cursor::new(self.stderr.clone().into_bytes()))
    }

    fn exited(&mut self) -> ExitFuture<'_> {
        let code = self.exit_code;
        Box::pin(async move {
            match code {
                Some(c) => Ok(c),
                None => Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "spawn /fake/comment-checker EACCES",
                )),
            }
        })
    }

    fn kill(&mut self, _signal: SpawnSignal) -> io::Result<()> {
        Ok(())
    }
}

async fn run(exit_code: i32, stderr: &str, delivered: bool) -> CheckResult {
    let process = FakeProcess {
        exit_code: Some(exit_code),
        stderr: stderr.to_string(),
        delivered,
    };
    run_comment_checker(
        &RunCommentCheckerInput {
            binary_path: Some("/fake/comment-checker".to_string()),
            hook_input: HookInput {
                session_id: "s".to_string(),
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
            spawn: &move |_args| {
                Ok(Box::new(FakeProcess {
                    exit_code: process.exit_code,
                    stderr: process.stderr.clone(),
                    delivered: process.delivered,
                }))
            },
            exists_sync: &|_path| true,
            timeout_ms: None,
            kill_grace_ms: None,
        },
    )
    .await
    .expect("run_comment_checker")
}

#[tokio::test]
async fn given_the_checker_exits_0_when_run_then_it_is_a_clean_result_without_a_failure() {
    let result = run(0, "", true).await;
    assert_eq!(
        result,
        CheckResult {
            has_comments: false,
            message: String::new(),
            failure: None,
        }
    );
}

#[tokio::test]
async fn given_the_checker_exits_2_when_run_then_its_stderr_is_the_comment_feedback() {
    let result = run(2, "found a comment
", true).await;
    assert_eq!(
        result,
        CheckResult {
            has_comments: true,
            message: "found a comment
".to_string(),
            failure: None,
        }
    );
}

#[tokio::test]
async fn given_the_checker_exits_outside_the_protocol_status_dll_not_found() {
    let result = run(3221225781_i64 as i32, "", true).await;
    assert!(!result.has_comments);
    assert_eq!(result.message, "");
    assert_eq!(
        result.failure,
        Some(CheckFailure {
            exit_code: Some(3221225781_i64 as i32),
            stderr: String::new(),
        })
    );
}

#[tokio::test]
async fn given_the_checker_exits_outside_the_protocol_generic_failure() {
    let result = run(1, "exec format error
", true).await;
    assert!(!result.has_comments);
    assert_eq!(result.message, "");
    assert_eq!(
        result.failure,
        Some(CheckFailure {
            exit_code: Some(1),
            stderr: "exec format error".to_string(),
        })
    );
}

#[tokio::test]
async fn given_a_checker_that_cannot_start_exits_before_reading_its_input_when_run_then_the_failure_is_still_exposed()
{
    let result = run(3221225781_i64 as i32, "", false).await;
    assert_eq!(
        result,
        CheckResult {
            has_comments: false,
            message: String::new(),
            failure: Some(CheckFailure {
                exit_code: Some(3221225781_i64 as i32),
                stderr: String::new(),
            }),
        }
    );
}

#[tokio::test]
async fn given_the_spawn_fails_after_it_returned_eacces_when_run_then_the_start_failure_is_exposed()
{
    let not_started = FakeProcess {
        exit_code: None,
        stderr: String::new(),
        delivered: false,
    };
    let result = run_comment_checker(
        &RunCommentCheckerInput {
            binary_path: Some("/fake/comment-checker".to_string()),
            hook_input: HookInput {
                session_id: "s".to_string(),
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
            spawn: &move |_args| {
                Ok(Box::new(FakeProcess {
                    exit_code: not_started.exit_code,
                    stderr: not_started.stderr.clone(),
                    delivered: not_started.delivered,
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
            failure: Some(CheckFailure {
                exit_code: None,
                stderr: "spawn /fake/comment-checker EACCES".to_string(),
            }),
        }
    );
}

#[tokio::test]
async fn given_a_checker_that_exits_0_before_reading_its_input_when_run_then_it_is_a_clean_result_without_a_failure()
{
    let result = run(0, "", false).await;
    assert_eq!(
        result,
        CheckResult {
            has_comments: false,
            message: String::new(),
            failure: None,
        }
    );
}
