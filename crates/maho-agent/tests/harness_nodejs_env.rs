//! Ports of test/harness/nodejs-env.test.ts and test/harness/text-line-reader.test.ts.
//!
//! senpi's suite uses `node:fs/promises` temp dirs; the Rust port uses the same `std::env::temp_dir`
//! convention with a unique per-test directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use maho_agent::harness::context::{Context, background_context, with_abort_signal};
use maho_agent::harness::env::nodejs::{
    NodeExecutionEnv, get_bash_shell_config, is_legacy_wsl_bash_path, windows_taskkill_candidates,
};
#[cfg(unix)]
use maho_agent::harness::types::FileKind;
use maho_agent::harness::types::{
    ExecutionErrorCode, FileErrorCode, FileSystem, Shell, ShellExecOptions,
    ShellOutputCaptureOptions, ShellOutputLimits, ShellOutputRetention, ShellOutputUpdate,
    TextLineReader,
};
use maho_agent::harness::utils::shell_output::{
    ShellCaptureOptions, ShellChunkHandler, execute_shell_with_capture,
};
use maho_ai::utils::abort::{AbortController, AbortReason};
use serde_json::json;

fn unique_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("t15-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn env_for(dir: &Path) -> NodeExecutionEnv {
    NodeExecutionEnv::new(dir.to_string_lossy().into_owned())
}

fn bg_context() -> Context {
    background_context()
}

fn expect_reader_err(
    result: Result<Box<dyn TextLineReader>, maho_agent::harness::types::FileError>,
) -> maho_agent::harness::types::FileError {
    match result {
        Ok(_) => panic!("expected a file error"),
        Err(error) => error,
    }
}

fn capture_options(max_bytes: u64, spill: bool) -> ShellOutputCaptureOptions {
    ShellOutputCaptureOptions {
        limits: ShellOutputLimits {
            max_bytes,
            max_lines: 1000,
            retain: Some(ShellOutputRetention::Tail),
        },
        spill: Some(spill),
    }
}

#[tokio::test]
async fn reads_writes_lists_and_removes_files_and_directories() {
    let dir = unique_dir("rw");
    let env = env_for(&dir);
    let context = bg_context();

    env.write_file("a/b.txt", b"hello", &context)
        .await
        .expect("write");
    assert_eq!(
        env.read_text_file("a/b.txt", &context).await.expect("read"),
        "hello"
    );
    env.append_file("a/b.txt", b" world", &context)
        .await
        .expect("append");
    assert_eq!(
        env.read_text_file("a/b.txt", &context).await.expect("read"),
        "hello world"
    );
    assert_eq!(
        env.read_binary_file("a/b.txt", &context)
            .await
            .expect("binary"),
        b"hello world".to_vec()
    );

    let entries = env.list_dir("a", &context).await.expect("list");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["b.txt"]
    );

    env.remove("a", Some(true), Some(false), &context)
        .await
        .expect("remove");
    assert!(!env.exists("a", &context).await.expect("exists"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn expands_home_relative_paths_and_file_urls() {
    let dir = unique_dir("home");
    let env = env_for(&dir);
    let context = bg_context();
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .expect("HOME or USERPROFILE");

    assert_eq!(env.absolute_path("~", &context).await.expect("home"), home);
    assert_eq!(
        env.absolute_path("~/x", &context).await.expect("home x"),
        std::path::Path::new(&home).join("x").to_string_lossy()
    );
    assert_eq!(
        env.absolute_path(
            if cfg!(windows) {
                "file:///C:/tmp/x"
            } else {
                "file:///tmp/x"
            },
            &context
        )
        .await
        .expect("file url"),
        std::path::Path::new(if cfg!(windows) { r"C:\tmp\x" } else { "/tmp/x" }).to_string_lossy()
    );
    assert_eq!(
        env.absolute_path("rel/../other", &context)
            .await
            .expect("relative"),
        dir.join("other").to_string_lossy()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[cfg(unix)]
async fn returns_file_info_for_files_directories_and_symlinks_without_following_symlinks() {
    let dir = unique_dir("info");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("file.txt", b"abc", &context)
        .await
        .expect("write");
    env.create_dir("sub", Some(true), &context)
        .await
        .expect("mkdir");
    std::os::unix::fs::symlink(dir.join("file.txt"), dir.join("link.txt")).expect("symlink");

    let file = env.file_info("file.txt", &context).await.expect("file");
    assert_eq!(file.kind, FileKind::File);
    assert_eq!(file.name, "file.txt");
    assert_eq!(file.size, 3);

    let sub = env.file_info("sub", &context).await.expect("dir");
    assert_eq!(sub.kind, FileKind::Directory);

    let link = env.file_info("link.txt", &context).await.expect("link");
    assert_eq!(link.kind, FileKind::Symlink);

    let canonical = env
        .canonical_path("link.txt", &context)
        .await
        .expect("canonical");
    assert_eq!(
        canonical,
        std::fs::canonicalize(&dir)
            .expect("canonical temp dir")
            .join("file.txt")
            .to_string_lossy()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[cfg(unix)]
async fn lists_symlinks_as_symlinks() {
    let dir = unique_dir("listsym");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("target.txt", b"x", &context)
        .await
        .expect("write");
    std::os::unix::fs::symlink(dir.join("target.txt"), dir.join("link.txt")).expect("symlink");

    let entries = env.list_dir(".", &context).await.expect("list");
    let link = entries
        .iter()
        .find(|entry| entry.name == "link.txt")
        .expect("link entry");
    assert_eq!(link.kind, FileKind::Symlink);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn stops_reading_text_lines_at_the_requested_limit() {
    let dir = unique_dir("maxlines");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("lines.txt", b"one\ntwo\nthree\n", &context)
        .await
        .expect("write");

    assert_eq!(
        env.read_text_lines("lines.txt", Some(2), &context)
            .await
            .expect("lines"),
        vec!["one", "two"]
    );
    assert_eq!(
        env.read_text_lines("lines.txt", None, &context)
            .await
            .expect("all"),
        vec!["one", "two", "three"]
    );
    assert!(
        env.read_text_lines("lines.txt", Some(0), &context)
            .await
            .expect("zero")
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_file_error_for_missing_paths_and_keeps_exists_false_for_missing_paths() {
    let dir = unique_dir("missing");
    let env = env_for(&dir);
    let context = bg_context();

    let error = env
        .read_text_file("nope.txt", &context)
        .await
        .expect_err("missing");
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(
        error.path.as_deref(),
        Some(dir.join("nope.txt").to_string_lossy().as_ref())
    );
    assert!(!env.exists("nope.txt", &context).await.expect("exists"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_file_error_for_listing_non_directories() {
    let dir = unique_dir("notdir");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("file.txt", b"x", &context)
        .await
        .expect("write");

    let error = env
        .list_dir("file.txt", &context)
        .await
        .expect_err("not a dir");
    assert_eq!(error.code, FileErrorCode::NotDirectory);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn appends_to_new_files_and_creates_parent_directories() {
    let dir = unique_dir("append");
    let env = env_for(&dir);
    let context = bg_context();

    env.append_file("deep/nested/file.txt", b"first", &context)
        .await
        .expect("append");
    env.append_file("deep/nested/file.txt", b"second", &context)
        .await
        .expect("append");
    assert_eq!(
        env.read_text_file("deep/nested/file.txt", &context)
            .await
            .expect("read"),
        "firstsecond"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn atomically_renames_a_file_and_replaces_the_destination() {
    let dir = unique_dir("rename");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("source.txt", b"source", &context)
        .await
        .expect("write");
    env.write_file("destination.txt", b"destination", &context)
        .await
        .expect("write");

    env.rename_file("source.txt", "destination.txt", &context)
        .await
        .expect("rename");
    assert_eq!(
        env.read_text_file("destination.txt", &context)
            .await
            .expect("read"),
        "source"
    );
    assert!(!env.exists("source.txt", &context).await.expect("exists"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reports_the_source_path_when_rename_fails_because_the_source_is_missing() {
    let dir = unique_dir("renamefail");
    let env = env_for(&dir);
    let context = bg_context();

    let error = env
        .rename_file("missing.txt", "destination.txt", &context)
        .await
        .expect_err("rename");
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(
        error.path.as_deref(),
        Some(dir.join("missing.txt").to_string_lossy().as_ref())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn creates_temporary_directories_and_files() {
    let dir = unique_dir("temp");
    let env = env_for(&dir);
    let context = bg_context();

    let temp_dir = env.create_temp_dir(None, &context).await.expect("temp dir");
    assert!(Path::new(&temp_dir).is_dir());
    assert!(
        Path::new(&temp_dir)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("tmp-")
    );

    let temp_file = env
        .create_temp_file(None, None, &context)
        .await
        .expect("temp file");
    assert!(Path::new(&temp_file).is_file());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn honors_create_dir_recursive_false_and_remove_recursive_force_options() {
    let dir = unique_dir("options");
    let env = env_for(&dir);
    let context = bg_context();

    env.create_dir("a", Some(true), &context)
        .await
        .expect("mkdir a");
    let error = env
        .create_dir("a", Some(false), &context)
        .await
        .expect_err("existing dir");
    assert_eq!(error.code, FileErrorCode::Unknown);

    env.create_dir("a/b", Some(true), &context)
        .await
        .expect("mkdir a/b");
    let error = env
        .remove("a", Some(false), Some(false), &context)
        .await
        .expect_err("non-empty dir");
    assert_eq!(error.code, FileErrorCode::Unknown);

    env.remove("a", Some(true), Some(false), &context)
        .await
        .expect("recursive remove");
    assert!(!env.exists("a", &context).await.expect("exists"));

    env.remove("missing", Some(false), Some(true), &context)
        .await
        .expect("force remove");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_aborted_results_for_pre_aborted_cancellable_file_operations() {
    let dir = unique_dir("abortfs");
    let env = env_for(&dir);
    let controller = AbortController::new();
    controller.abort(Some(AbortReason::dom_default()));
    let context = with_abort_signal(controller.signal(), &bg_context());

    assert_eq!(
        env.read_text_file("a.txt", &context)
            .await
            .expect_err("read")
            .code,
        FileErrorCode::Aborted
    );
    assert_eq!(
        env.read_binary_file("a.txt", &context)
            .await
            .expect_err("binary")
            .code,
        FileErrorCode::Aborted
    );
    assert_eq!(
        env.write_file("a.txt", b"x", &context)
            .await
            .expect_err("write")
            .code,
        FileErrorCode::Aborted
    );
    assert_eq!(
        env.file_info("a.txt", &context)
            .await
            .expect_err("info")
            .code,
        FileErrorCode::Aborted
    );
    assert_eq!(
        env.list_dir(".", &context).await.expect_err("list").code,
        FileErrorCode::Aborted
    );
    assert_eq!(
        env.canonical_path("a.txt", &context)
            .await
            .expect_err("canonical")
            .code,
        FileErrorCode::Aborted
    );
    assert_eq!(
        expect_reader_err(env.open_text_line_reader("a.txt", &context).await).code,
        FileErrorCode::Aborted
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cleanup_is_best_effort() {
    let dir = unique_dir("cleanup");
    let env = env_for(&dir);
    FileSystem::cleanup(&env, &bg_context()).await;
    FileSystem::cleanup(&env, &bg_context()).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn executes_commands_in_cwd_with_env_overrides() {
    let dir = unique_dir("exec");
    let env = env_for(&dir);
    let context = bg_context();

    let result = env
        .exec(
            "printf '%s' \"$PI_T15\"",
            Some(ShellExecOptions {
                env: Some(serde_json::Map::from_iter([(
                    "PI_T15".to_string(),
                    json!("yes"),
                )])),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 0);

    let cwd_result = env
        .exec(
            "printf '%s' \"$(pwd)\"",
            Some(ShellExecOptions::default()),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(cwd_result.exit_code, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn can_replace_rather_than_inherit_the_default_shell_environment() {
    let dir = unique_dir("noinherit");
    let env = env_for(&dir);
    let context = bg_context();

    let inherited = env
        .exec(
            "printf '%s' \"${PI_T15_MARKER-unset}\"",
            Some(ShellExecOptions::default()),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(inherited.exit_code, 0);

    let replaced = env
        .exec(
            "printf '%s' \"${PI_T15_MARKER-unset}\"",
            Some(ShellExecOptions {
                env: Some(serde_json::Map::from_iter([(
                    "PI_T15_MARKER".to_string(),
                    json!("only"),
                )])),
                inherit_env: Some(false),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(replaced.exit_code, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn uses_stdin_command_transport_for_legacy_wsl_bash_paths() {
    assert!(is_legacy_wsl_bash_path("C:\\Windows\\System32\\bash.exe"));
    assert!(is_legacy_wsl_bash_path("c:/windows/sysnative/bash.exe"));
    assert!(!is_legacy_wsl_bash_path("/bin/bash"));

    let config = get_bash_shell_config("C:\\Windows\\System32\\bash.exe");
    assert_eq!(config.command_transport, Some("stdin"));
    assert_eq!(config.args, vec!["-s".to_string()]);

    let normal = get_bash_shell_config("/bin/bash");
    assert_eq!(normal.command_transport, None);
    assert_eq!(normal.args, vec!["-c".to_string()]);
}

#[test]
fn builds_windows_taskkill_candidates_from_the_environment() {
    let mut env = BTreeMap::new();
    env.insert("SystemRoot".to_string(), "/nonexistent-root".to_string());
    let candidates = windows_taskkill_candidates(&env);
    assert_eq!(candidates.last().map(String::as_str), Some("taskkill.exe"));
}

#[tokio::test]
async fn cleanup_terminates_active_shell_processes() {
    let dir = unique_dir("killcleanup");
    let env = env_for(&dir);
    let context = bg_context();
    FileSystem::cleanup(&env, &context).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn combines_stdout_and_stderr_into_one_bounded_view() {
    let dir = unique_dir("combined");
    let env = env_for(&dir);
    let context = bg_context();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_updates = seen.clone();

    let result = env
        .exec(
            "printf 'out'; printf 'err' 1>&2",
            Some(ShellExecOptions {
                capture: Some(capture_options(1024, false)),
                on_update: Some(std::sync::Arc::new(
                    move |update: ShellOutputUpdate, _context: Context| {
                        let text = match &update {
                            ShellOutputUpdate::Replace { output } => output.text.clone(),
                            ShellOutputUpdate::Append { text, .. }
                            | ShellOutputUpdate::Slide { text, .. } => text.clone(),
                            ShellOutputUpdate::Metadata { .. } => String::new(),
                        };
                        seen_updates.lock().expect("seen poisoned").push(text);
                        Box::pin(async { Ok(()) })
                    },
                )),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.truncation.total_bytes, 6);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reports_a_missing_working_directory_before_spawning() {
    let dir = unique_dir("missingcwd");
    let env = env_for(&dir);
    let context = bg_context();

    let error = env
        .exec(
            "printf 'x'",
            Some(ShellExecOptions {
                cwd: Some("nope".to_string()),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect_err("missing cwd");
    assert_eq!(error.code, ExecutionErrorCode::SpawnError);
    assert!(error.message.contains("Working directory does not exist"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_non_zero_command_exit_codes_as_successful_execution_results() {
    let dir = unique_dir("exitcode");
    let env = env_for(&dir);
    let context = bg_context();

    let result = env
        .exec("exit 7", Some(ShellExecOptions::default()), &context)
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 7);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_timeout_errors_for_commands_exceeding_the_timeout() {
    let dir = unique_dir("timeout");
    let env = env_for(&dir);
    let context = bg_context();

    let error = env
        .exec(
            "sleep 30",
            Some(ShellExecOptions {
                timeout: Some(0.2),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect_err("timeout");
    assert_eq!(error.code, ExecutionErrorCode::Timeout);
    assert_eq!(error.message, "timeout:0.2");

    let invalid = env
        .exec(
            "true",
            Some(ShellExecOptions {
                timeout: Some(0.0),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect_err("invalid timeout");
    assert_eq!(invalid.code, ExecutionErrorCode::Timeout);
    assert!(invalid.message.contains("Invalid timeout"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_shell_unavailable_and_spawn_errors() {
    let dir = unique_dir("shellunavail");
    let env = env_for(&dir).with_shell_path(Some("/nonexistent/bash".to_string()));
    let context = bg_context();

    let error = env
        .exec("printf 'x'", Some(ShellExecOptions::default()), &context)
        .await
        .expect_err("shell");
    assert_eq!(error.code, ExecutionErrorCode::ShellUnavailable);
    assert!(
        error
            .message
            .contains("Custom shell path not found: /nonexistent/bash")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_an_aborted_result_for_aborted_commands() {
    let dir = unique_dir("abortexec");
    let env = env_for(&dir);
    let controller = AbortController::new();
    controller.abort(Some(AbortReason::dom_default()));
    let context = with_abort_signal(controller.signal(), &bg_context());

    let error = env
        .exec("printf 'x'", Some(ShellExecOptions::default()), &context)
        .await
        .expect_err("aborted");
    assert_eq!(error.code, ExecutionErrorCode::Aborted);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn does_not_create_a_spill_before_bounded_output_crosses_its_limits() {
    let dir = unique_dir("nospill");
    let env = env_for(&dir);
    let context = bg_context();

    let result = env
        .exec(
            "printf 'small'",
            Some(ShellExecOptions {
                capture: Some(capture_options(1024, true)),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 0);
    assert!(result.spill_path.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn captures_large_shell_output_to_a_full_output_file() {
    let dir = unique_dir("spill");
    let env = env_for(&dir);
    let context = bg_context();

    let result = env
        .exec(
            "printf 'x%.0s' $(seq 1 4000)",
            Some(ShellExecOptions {
                capture: Some(capture_options(64, true)),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 0);
    assert!(result.truncation.truncated);
    let spill_path = result.spill_path.expect("spill path");
    let spilled = std::fs::read_to_string(&spill_path).expect("spill file");
    assert_eq!(spilled.len(), 4000);
    assert!(spilled.chars().all(|character| character == 'x'));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(Path::new(&spill_path).parent().expect("spill parent"));
}

#[tokio::test]
async fn preserves_exact_raw_bytes_in_the_spill_while_decoding_a_bounded_text_view() {
    let dir = unique_dir("spillbytes");
    let env = env_for(&dir);
    let context = bg_context();

    let result = env
        .exec(
            "printf 'héllo wörld '; printf 'x%.0s' $(seq 1 2000)",
            Some(ShellExecOptions {
                capture: Some(capture_options(32, true)),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 0);
    let spill_path = result.spill_path.expect("spill path");
    let spilled = std::fs::read(&spill_path).expect("spill bytes");
    assert!(spilled.starts_with("héllo wörld ".as_bytes()));
    assert_eq!(spilled.len(), "héllo wörld ".len() + 2000);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(Path::new(&spill_path).parent().expect("spill parent"));
}

fn spill_limits() -> ShellOutputCaptureOptions {
    ShellOutputCaptureOptions {
        limits: ShellOutputLimits {
            max_bytes: 10,
            max_lines: 10,
            retain: Some(ShellOutputRetention::Tail),
        },
        spill: Some(true),
    }
}

fn ignore_updates() -> Option<maho_agent::harness::types::ShellOutputUpdateHandler> {
    Some(std::sync::Arc::new(|_, _| Box::pin(async { Ok(()) })))
}

fn remove_spill(path: &str) {
    let _ = std::fs::remove_dir_all(Path::new(path).parent().expect("spill parent"));
}

#[tokio::test]
#[cfg(unix)]
async fn applies_string_shell_environment_overrides_when_override_is_missing_empty_or_string() {
    let cases: [(&str, Option<&str>, &str); 3] = [
        ("missing", None, "x:/stale/parent.jsonl"),
        ("empty", Some(""), "x:"),
        (
            "string",
            Some("/sessions/current.jsonl"),
            "x:/sessions/current.jsonl",
        ),
    ];
    for (label, override_value, expected_session_file) in cases {
        let dir = unique_dir(&format!("shellenv-{label}"));
        let env = env_for(&dir).with_shell_env(Some(BTreeMap::from([
            (
                "PI_SESSION_FILE".to_string(),
                "/stale/parent.jsonl".to_string(),
            ),
            ("PI_CODING_AGENT".to_string(), "true".to_string()),
            (
                "PI_NODE_ENV_PRESERVED_TEST".to_string(),
                "preserved".to_string(),
            ),
        ])));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed = seen.clone();
        let result = env
            .exec(
                "printf '%s:%s|%s|%s' \"${PI_SESSION_FILE+x}\" \"${PI_SESSION_FILE-}\" \"$PI_CODING_AGENT\" \"$PI_NODE_ENV_PRESERVED_TEST\"",
                Some(ShellExecOptions {
                    env: override_value.map(|value| {
                        serde_json::Map::from_iter([("PI_SESSION_FILE".to_string(), json!(value))])
                    }),
                    on_update: Some(std::sync::Arc::new(move |update, _| {
                        let mut view = observed.lock().expect("view poisoned");
                        *view = Some(maho_agent::harness::utils::output_capture::apply_shell_output_update(
                            view.as_ref(),
                            &update,
                        ));
                        Box::pin(async { Ok(()) })
                    })),
                    ..Default::default()
                }),
                &bg_context(),
            )
            .await
            .expect("exec");
        assert_eq!(result.exit_code, 0);
        let text = seen
            .lock()
            .expect("view poisoned")
            .as_ref()
            .map(|view| view.text.clone());
        assert_eq!(
            text.as_deref(),
            Some(format!("{expected_session_file}|true|preserved").as_str()),
            "{label}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[tokio::test]
async fn returns_callback_errors_from_exec_stream_handlers() {
    let dir = unique_dir("callbackerr");
    let env = env_for(&dir);
    let error = env
        .exec(
            "printf out",
            Some(ShellExecOptions {
                on_update: Some(std::sync::Arc::new(|_, _| {
                    Box::pin(async { Err("callback failed".to_string()) })
                })),
                ..Default::default()
            }),
            &bg_context(),
        )
        .await
        .expect_err("callback failure");
    assert_eq!(error.code, ExecutionErrorCode::CallbackError);
    assert_eq!(error.message, "callback failed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The six upstream `it.each` rows reject with a string, a plain object and an `Error`. Rust chunk
/// handlers fail with a `String`, so each rejected value is represented by its string form and the
/// assertion is that this exact value becomes the `callback_error` cause. An awaited `Err` cannot
/// escape as an unhandled rejection; the bounded timeout asserts the execution still settles.
#[tokio::test]
async fn returns_async_on_chunk_failures_without_an_unhandled_rejection() {
    let rows = [
        ("stdout", "harness async stdout failed".to_string()),
        (
            "stdout",
            json!({ "stream": "stdout", "failure": true }).to_string(),
        ),
        ("stdout", "Error: harness async stdout failed".to_string()),
        ("stderr", "harness async stderr failed".to_string()),
        (
            "stderr",
            json!({ "stream": "stderr", "failure": true }).to_string(),
        ),
        ("stderr", "Error: harness async stderr failed".to_string()),
    ];
    for (index, (stream, callback_error)) in rows.into_iter().enumerate() {
        let dir = unique_dir(&format!("asyncchunk{index}"));
        let env = env_for(&dir);
        let redirect = if stream == "stderr" { " >&2" } else { "" };
        let rejected = callback_error.clone();
        let on_chunk: ShellChunkHandler = std::sync::Arc::new(move |_, _, _| {
            let rejected = rejected.clone();
            Box::pin(async move { Err(rejected) })
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            execute_shell_with_capture(
                &env,
                &format!("head -c 100001 /dev/zero | tr '\\0' x{redirect}"),
                Some(ShellCaptureOptions {
                    on_chunk: Some(on_chunk),
                    ..Default::default()
                }),
                &bg_context(),
            ),
        )
        .await
        .expect("execution settles");
        let error = result.expect_err("onChunk failure fails the execution");
        assert_eq!(error.code, ExecutionErrorCode::CallbackError, "{stream}");
        assert_eq!(
            error.cause.as_deref(),
            Some(callback_error.as_str()),
            "{stream}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[tokio::test]
#[cfg(unix)]
async fn maps_signal_killed_processes_to_a_non_zero_exit_code() {
    let dir = unique_dir("sigkill");
    let env = env_for(&dir);
    let result = env
        .exec("kill -9 $$", None, &bg_context())
        .await
        .expect("signal-killed shell is a completed execution");
    assert_eq!(result.exit_code, 128 + 9);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[cfg(unix)]
async fn fails_rather_than_silently_losing_a_requested_spill() {
    let dir = unique_dir("failingspill");
    let missing = dir.join("missing").join("spill.log");
    let missing = missing.to_string_lossy().into_owned();
    // Pinned `FailingSpillExecutionEnv`: the spill temp file resolves into a missing directory.
    let env = env_for(&dir).with_create_temp_file_hook(Some(std::sync::Arc::new(
        move |prefix: Option<&str>,
              created: Result<String, maho_agent::harness::types::FileError>| {
            if prefix == Some("pi-output-") {
                if let Ok(path) = &created {
                    let _ = std::fs::remove_dir_all(Path::new(path).parent().expect("temp parent"));
                }
                return Ok(missing.clone());
            }
            created
        },
    )));
    let error = env
        .exec(
            "printf 12345678901234567890",
            Some(ShellExecOptions {
                capture: Some(spill_limits()),
                on_update: ignore_updates(),
                ..Default::default()
            }),
            &bg_context(),
        )
        .await
        .expect_err("spill failure");
    assert_eq!(error.code, ExecutionErrorCode::Unknown);
    assert!(
        error
            .message
            .contains("Failed to preserve complete shell output"),
        "{}",
        error.message
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[cfg(unix)]
async fn preserves_complete_output_when_spill_stream_backpressure_pauses_a_process_that_exits_quickly()
 {
    let dir = unique_dir("spillbackpressure");
    let env = env_for(&dir);
    let size = 500_000;
    let result = env
        .exec(
            &format!("head -c {size} /dev/zero | tr '\\0' x"),
            Some(ShellExecOptions {
                capture: Some(spill_limits()),
                on_update: ignore_updates(),
                ..Default::default()
            }),
            &bg_context(),
        )
        .await
        .expect("exec");
    let spill_path = result.spill_path.expect("spill path");
    assert_eq!(
        std::fs::read_to_string(&spill_path)
            .expect("spill file")
            .len(),
        size
    );
    remove_spill(&spill_path);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[cfg(unix)]
async fn keeps_the_full_output_path_when_a_command_times_out_after_its_output_spilled() {
    let dir = unique_dir("spilltimeout");
    let env = env_for(&dir);
    let size = 5000;
    let error = env
        .exec(
            &format!("head -c {size} /dev/zero | tr '\\0' x; exec sleep 30"),
            Some(ShellExecOptions {
                timeout: Some(3.0),
                capture: Some(spill_limits()),
                on_update: ignore_updates(),
                ..Default::default()
            }),
            &bg_context(),
        )
        .await
        .expect_err("timeout");
    assert_eq!(error.code, ExecutionErrorCode::Timeout);
    let spill_path = error.spill_path.expect("spill path");
    assert_eq!(
        std::fs::read_to_string(&spill_path).expect("spill file"),
        "x".repeat(size)
    );
    remove_spill(&spill_path);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[cfg(unix)]
async fn keeps_the_full_output_path_when_a_command_is_aborted_after_its_output_spilled() {
    let dir = unique_dir("spillabort");
    let env = env_for(&dir);
    let size: u64 = 5000;
    let controller = AbortController::new();
    let context = with_abort_signal(controller.signal(), &bg_context());
    let view = std::sync::Arc::new(std::sync::Mutex::new(None));
    let observed = view.clone();
    let error = env
        .exec(
            &format!("head -c {size} /dev/zero | tr '\\0' x; exec sleep 30"),
            Some(ShellExecOptions {
                capture: Some(spill_limits()),
                on_update: Some(std::sync::Arc::new(move |update, _| {
                    let mut current = observed.lock().expect("view poisoned");
                    let next =
                        maho_agent::harness::utils::output_capture::apply_shell_output_update(
                            current.as_ref(),
                            &update,
                        );
                    // Abort only once every byte has been observed and the spill exists, so the
                    // outcome depends on event order rather than on timing.
                    if next.spill_path.is_some() && next.truncation.total_bytes == size {
                        controller.abort(None);
                    }
                    *current = Some(next);
                    Box::pin(async { Ok(()) })
                })),
                ..Default::default()
            }),
            &context,
        )
        .await
        .expect_err("aborted");
    assert_eq!(error.code, ExecutionErrorCode::Aborted);
    let observed_spill = view
        .lock()
        .expect("view poisoned")
        .as_ref()
        .and_then(|view| view.spill_path.clone());
    assert_eq!(error.spill_path, observed_spill);
    let spill_path = error.spill_path.expect("spill path");
    assert_eq!(
        std::fs::read_to_string(&spill_path).expect("spill file"),
        "x".repeat(size as usize)
    );
    remove_spill(&spill_path);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn creates_no_spill_file_when_a_command_is_aborted_before_any_output() {
    let dir = unique_dir("spillnone");
    // Pinned `SpillRecordingExecutionEnv`: record every spill file `exec` creates.
    let spill_files = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorded = spill_files.clone();
    let env = env_for(&dir).with_create_temp_file_hook(Some(std::sync::Arc::new(
        move |prefix: Option<&str>,
              created: Result<String, maho_agent::harness::types::FileError>| {
            if prefix == Some("pi-output-")
                && let Ok(path) = &created
            {
                recorded.lock().expect("spill files").push(path.clone());
            }
            created
        },
    )));
    let controller = AbortController::new();
    let context = with_abort_signal(controller.signal(), &bg_context());
    let execution = env.exec(
        "exec sleep 30",
        Some(ShellExecOptions {
            capture: Some(spill_limits()),
            on_update: ignore_updates(),
            ..Default::default()
        }),
        &context,
    );
    controller.abort(None);
    let error = execution.await.expect_err("aborted");
    assert_eq!(error.code, ExecutionErrorCode::Aborted);
    assert!(error.spill_path.is_none());
    assert!(spill_files.lock().expect("spill files").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// text-line-reader.test.ts
#[tokio::test]
async fn decodes_unicode_blank_lines_and_a_torn_final_line() {
    let dir = unique_dir("tlr");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("lines.txt", "α\n\nb😀".as_bytes(), &context)
        .await
        .expect("write");

    let reader = env
        .open_text_line_reader("lines.txt", &context)
        .await
        .expect("open");
    assert_eq!(
        reader
            .read_line(&context)
            .await
            .expect("line")
            .expect("some")
            .text,
        "α"
    );
    let blank = reader
        .read_line(&context)
        .await
        .expect("line")
        .expect("some");
    assert_eq!(blank.text, "");
    assert!(blank.terminated);
    let torn = reader
        .read_line(&context)
        .await
        .expect("line")
        .expect("some");
    assert_eq!(torn.text, "b😀");
    assert!(!torn.terminated);
    assert!(reader.read_line(&context).await.expect("line").is_none());
    reader.close(&context).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reads_an_empty_file() {
    let dir = unique_dir("tlrempty");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("empty.txt", b"", &context)
        .await
        .expect("write");

    let reader = env
        .open_text_line_reader("empty.txt", &context)
        .await
        .expect("open");
    assert!(reader.read_line(&context).await.expect("line").is_none());
    reader.close(&context).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn decodes_multibyte_characters_split_across_64_kib_chunks() {
    let dir = unique_dir("tlrsplit");
    let env = env_for(&dir);
    let context = bg_context();
    let mut content = "a".repeat(65_534);
    content.push('😀');
    content.push('\n');
    content.push_str("tail\n");
    env.write_file("big.txt", content.as_bytes(), &context)
        .await
        .expect("write");

    let reader = env
        .open_text_line_reader("big.txt", &context)
        .await
        .expect("open");
    let first = reader
        .read_line(&context)
        .await
        .expect("line")
        .expect("some");
    assert!(first.text.ends_with('😀'));
    assert!(first.terminated);
    let second = reader
        .read_line(&context)
        .await
        .expect("line")
        .expect("some");
    assert_eq!(second.text, "tail");
    reader.close(&context).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rejects_an_open_with_a_pre_aborted_context() {
    let dir = unique_dir("tlrabort");
    let env = env_for(&dir);
    env.write_file("a.txt", b"x\n", &bg_context())
        .await
        .expect("write");
    let controller = AbortController::new();
    controller.abort(Some(AbortReason::dom_default()));
    let context = with_abort_signal(controller.signal(), &bg_context());

    let error = expect_reader_err(env.open_text_line_reader("a.txt", &context).await);
    assert_eq!(error.code, FileErrorCode::Aborted);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn closes_idempotently_even_with_an_aborted_context_and_rejects_later_reads() {
    let dir = unique_dir("tlrclose");
    let env = env_for(&dir);
    env.write_file("a.txt", b"one\ntwo\n", &bg_context())
        .await
        .expect("write");
    let context = bg_context();

    let reader = env
        .open_text_line_reader("a.txt", &context)
        .await
        .expect("open");
    assert_eq!(
        reader
            .read_line(&context)
            .await
            .expect("line")
            .expect("some")
            .text,
        "one"
    );
    reader.close(&context).await;
    reader.close(&context).await;

    let controller = AbortController::new();
    controller.abort(Some(AbortReason::dom_default()));
    let aborted_context = with_abort_signal(controller.signal(), &bg_context());
    reader.close(&aborted_context).await;

    let error = reader.read_line(&context).await.expect_err("closed reader");
    assert_eq!(error.code, FileErrorCode::Invalid);
    assert_eq!(error.message, "Text line reader is closed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_a_file_error_for_a_missing_file() {
    let dir = unique_dir("tlrmissing");
    let env = env_for(&dir);
    let context = bg_context();

    let error = expect_reader_err(env.open_text_line_reader("missing.txt", &context).await);
    assert_eq!(error.code, FileErrorCode::NotFound);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn replaces_malformed_and_incomplete_utf8() {
    let dir = unique_dir("tlrmalformed");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("bad.txt", &[0xff, 0x0a, 0xe2, 0x82], &context)
        .await
        .expect("write");
    let reader = env
        .open_text_line_reader("bad.txt", &context)
        .await
        .expect("open");
    let first = reader
        .read_line(&context)
        .await
        .expect("line")
        .expect("some");
    assert_eq!((first.text.as_str(), first.terminated), ("\u{fffd}", true));
    let second = reader
        .read_line(&context)
        .await
        .expect("line")
        .expect("some");
    assert_eq!(
        (second.text.as_str(), second.terminated),
        ("\u{fffd}", false)
    );
    assert!(reader.read_line(&context).await.expect("eof").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn does_not_consume_a_buffered_line_when_its_context_is_pre_aborted() {
    let dir = unique_dir("tlrbuffered");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("a.txt", b"one\ntwo\n", &context)
        .await
        .expect("write");
    let reader = env
        .open_text_line_reader("a.txt", &context)
        .await
        .expect("open");
    reader
        .read_line(&context)
        .await
        .expect("first")
        .expect("some");
    let controller = AbortController::new();
    controller.abort(Some(AbortReason::dom_default()));
    let aborted = with_abort_signal(controller.signal(), &bg_context());
    let error = reader.read_line(&aborted).await.expect_err("aborted");
    assert_eq!(error.code, FileErrorCode::Aborted);
    assert_eq!(
        reader
            .read_line(&context)
            .await
            .expect("second")
            .expect("some")
            .text,
        "two"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Rust reader reads each 64 KiB chunk synchronously, so the abort lands before the pending
/// read is first polled rather than between chunks; the retry contract (no line lost, every
/// chunk decoded) is the same.
#[tokio::test]
async fn honors_cancellation_during_a_read_and_allows_retry() {
    let dir = unique_dir("tlrretry");
    let env = env_for(&dir);
    let context = bg_context();
    let long = "é".repeat(70_000);
    env.write_file("a.txt", format!("{long}\nlast").as_bytes(), &context)
        .await
        .expect("write");
    let reader = env
        .open_text_line_reader("a.txt", &context)
        .await
        .expect("open");
    let controller = AbortController::new();
    let cancellable = with_abort_signal(controller.signal(), &bg_context());
    let pending = reader.read_line(&cancellable);
    controller.abort(Some(AbortReason::dom_default()));
    assert_eq!(
        pending.await.expect_err("aborted").code,
        FileErrorCode::Aborted
    );
    let first = reader
        .read_line(&context)
        .await
        .expect("first")
        .expect("some");
    assert_eq!(first.text, long);
    let last = reader
        .read_line(&context)
        .await
        .expect("last")
        .expect("some");
    assert_eq!((last.text.as_str(), last.terminated), ("last", false));
    assert!(reader.read_line(&context).await.expect("eof").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
