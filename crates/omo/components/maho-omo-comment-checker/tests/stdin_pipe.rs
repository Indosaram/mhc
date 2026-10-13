#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use comment_checker_core::{CheckResult, HookInput, HookToolInput, RunCommentCheckerInput};
use maho_omo_comment_checker::runner::default_run_comment_checker;
use tempfile::tempdir;

const PAYLOAD_SIZES: &[usize] = &[1_000, 100_000];
const ROUNDS_PER_SIZE: usize = 3;

fn write_shell_checker(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("test assertion");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("test assertion");
    path
}

#[tokio::test]
async fn given_checker_binary_exiting_without_reading_stdin_when_checked_then_host_survives_and_clean()
{
    let temp = tempdir().expect("test assertion");
    let binary = write_shell_checker(temp.path(), "dead-checker", "exit 0");

    for size in PAYLOAD_SIZES {
        for _ in 0..ROUNDS_PER_SIZE {
            let result = default_run_comment_checker(&RunCommentCheckerInput {
                binary_path: Some(binary.to_string_lossy().into_owned()),
                hook_input: HookInput {
                    session_id: "session-1".into(),
                    tool_name: "write".into(),
                    transcript_path: "".into(),
                    cwd: ".".into(),
                    hook_event_name: "PostToolUse".into(),
                    tool_input: HookToolInput {
                        file_path: Some("src/plan.md".into()),
                        content: Some("x".repeat(*size)),
                        old_string: None,
                        new_string: None,
                        edits: None,
                    },
                    tool_response: None,
                },
                custom_prompt: None,
            })
            .await
            .expect("check should succeed");

            assert_eq!(
                result,
                CheckResult {
                    has_comments: false,
                    message: String::new(),
                    failure: None,
                }
            );
        }
    }
}

#[tokio::test]
async fn given_checker_reading_all_stdin_and_reporting_comments_when_checked_then_exit_2_feedback_arrives()
{
    let temp = tempdir().expect("test assertion");
    let binary = write_shell_checker(
        temp.path(),
        "reporting-checker",
        "cat >/dev/null\nprintf 'COMMENT/DOCSTRING DETECTED\n' >&2\nexit 2",
    );

    for size in PAYLOAD_SIZES {
        for _ in 0..ROUNDS_PER_SIZE {
            let result = default_run_comment_checker(&RunCommentCheckerInput {
                binary_path: Some(binary.to_string_lossy().into_owned()),
                hook_input: HookInput {
                    session_id: "session-1".into(),
                    tool_name: "write".into(),
                    transcript_path: "".into(),
                    cwd: ".".into(),
                    hook_event_name: "PostToolUse".into(),
                    tool_input: HookToolInput {
                        file_path: Some("src/plan.md".into()),
                        content: Some("x".repeat(*size)),
                        old_string: None,
                        new_string: None,
                        edits: None,
                    },
                    tool_response: None,
                },
                custom_prompt: None,
            })
            .await
            .expect("check should succeed");

            assert_eq!(
                result,
                CheckResult {
                    has_comments: true,
                    message: "COMMENT/DOCSTRING DETECTED
".to_string(),
                    failure: None,
                }
            );
        }
    }
}
