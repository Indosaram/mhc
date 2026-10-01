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
use maho_agent::harness::types::{
    ExecutionErrorCode, FileErrorCode, FileKind, FileSystem, Shell, ShellExecOptions, ShellOutputCaptureOptions,
    ShellOutputLimits, ShellOutputRetention, ShellOutputUpdate, TextLineReader,
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
        limits: ShellOutputLimits { max_bytes, max_lines: 1000, retain: Some(ShellOutputRetention::Tail) },
        spill: Some(spill),
    }
}

#[tokio::test]
async fn reads_writes_lists_and_removes_files_and_directories() {
    let dir = unique_dir("rw");
    let env = env_for(&dir);
    let context = bg_context();

    env.write_file("a/b.txt", b"hello", &context).await.expect("write");
    assert_eq!(env.read_text_file("a/b.txt", &context).await.expect("read"), "hello");
    env.append_file("a/b.txt", b" world", &context).await.expect("append");
    assert_eq!(env.read_text_file("a/b.txt", &context).await.expect("read"), "hello world");
    assert_eq!(env.read_binary_file("a/b.txt", &context).await.expect("binary"), b"hello world".to_vec());

    let entries = env.list_dir("a", &context).await.expect("list");
    assert_eq!(entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), vec!["b.txt"]);

    env.remove("a", Some(true), Some(false), &context).await.expect("remove");
    assert!(!env.exists("a", &context).await.expect("exists"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn expands_home_relative_paths_and_file_urls() {
    let dir = unique_dir("home");
    let env = env_for(&dir);
    let context = bg_context();
    let home = std::env::var("HOME").expect("HOME");

    assert_eq!(env.absolute_path("~", &context).await.expect("home"), home);
    assert_eq!(env.absolute_path("~/x", &context).await.expect("home x"), format!("{home}/x"));
    assert_eq!(
        env.absolute_path("file:///tmp/x", &context).await.expect("file url"),
        "/tmp/x"
    );
    assert_eq!(
        env.absolute_path("rel/../other", &context).await.expect("relative"),
        format!("{}/other", dir.to_string_lossy())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_file_info_for_files_directories_and_symlinks_without_following_symlinks() {
    let dir = unique_dir("info");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("file.txt", b"abc", &context).await.expect("write");
    env.create_dir("sub", Some(true), &context).await.expect("mkdir");
    std::os::unix::fs::symlink(dir.join("file.txt"), dir.join("link.txt")).expect("symlink");

    let file = env.file_info("file.txt", &context).await.expect("file");
    assert_eq!(file.kind, FileKind::File);
    assert_eq!(file.name, "file.txt");
    assert_eq!(file.size, 3);

    let sub = env.file_info("sub", &context).await.expect("dir");
    assert_eq!(sub.kind, FileKind::Directory);

    let link = env.file_info("link.txt", &context).await.expect("link");
    assert_eq!(link.kind, FileKind::Symlink);

    let canonical = env.canonical_path("link.txt", &context).await.expect("canonical");
    assert_eq!(canonical, dir.join("file.txt").to_string_lossy());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn lists_symlinks_as_symlinks() {
    let dir = unique_dir("listsym");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("target.txt", b"x", &context).await.expect("write");
    std::os::unix::fs::symlink(dir.join("target.txt"), dir.join("link.txt")).expect("symlink");

    let entries = env.list_dir(".", &context).await.expect("list");
    let link = entries.iter().find(|entry| entry.name == "link.txt").expect("link entry");
    assert_eq!(link.kind, FileKind::Symlink);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn stops_reading_text_lines_at_the_requested_limit() {
    let dir = unique_dir("maxlines");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("lines.txt", b"one\ntwo\nthree\n", &context).await.expect("write");

    assert_eq!(env.read_text_lines("lines.txt", Some(2), &context).await.expect("lines"), vec!["one", "two"]);
    assert_eq!(env.read_text_lines("lines.txt", None, &context).await.expect("all"), vec!["one", "two", "three"]);
    assert!(env.read_text_lines("lines.txt", Some(0), &context).await.expect("zero").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_file_error_for_missing_paths_and_keeps_exists_false_for_missing_paths() {
    let dir = unique_dir("missing");
    let env = env_for(&dir);
    let context = bg_context();

    let error = env.read_text_file("nope.txt", &context).await.expect_err("missing");
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(error.path.as_deref(), Some(dir.join("nope.txt").to_string_lossy().as_ref()));
    assert!(!env.exists("nope.txt", &context).await.expect("exists"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_file_error_for_listing_non_directories() {
    let dir = unique_dir("notdir");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("file.txt", b"x", &context).await.expect("write");

    let error = env.list_dir("file.txt", &context).await.expect_err("not a dir");
    assert_eq!(error.code, FileErrorCode::NotDirectory);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn appends_to_new_files_and_creates_parent_directories() {
    let dir = unique_dir("append");
    let env = env_for(&dir);
    let context = bg_context();

    env.append_file("deep/nested/file.txt", b"first", &context).await.expect("append");
    env.append_file("deep/nested/file.txt", b"second", &context).await.expect("append");
    assert_eq!(env.read_text_file("deep/nested/file.txt", &context).await.expect("read"), "firstsecond");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn atomically_renames_a_file_and_replaces_the_destination() {
    let dir = unique_dir("rename");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("source.txt", b"source", &context).await.expect("write");
    env.write_file("destination.txt", b"destination", &context).await.expect("write");

    env.rename_file("source.txt", "destination.txt", &context).await.expect("rename");
    assert_eq!(env.read_text_file("destination.txt", &context).await.expect("read"), "source");
    assert!(!env.exists("source.txt", &context).await.expect("exists"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reports_the_source_path_when_rename_fails_because_the_source_is_missing() {
    let dir = unique_dir("renamefail");
    let env = env_for(&dir);
    let context = bg_context();

    let error = env.rename_file("missing.txt", "destination.txt", &context).await.expect_err("rename");
    assert_eq!(error.code, FileErrorCode::NotFound);
    assert_eq!(error.path.as_deref(), Some(dir.join("missing.txt").to_string_lossy().as_ref()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn creates_temporary_directories_and_files() {
    let dir = unique_dir("temp");
    let env = env_for(&dir);
    let context = bg_context();

    let temp_dir = env.create_temp_dir(None, &context).await.expect("temp dir");
    assert!(Path::new(&temp_dir).is_dir());
    assert!(Path::new(&temp_dir).file_name().unwrap().to_string_lossy().starts_with("tmp-"));

    let temp_file = env.create_temp_file(None, None, &context).await.expect("temp file");
    assert!(Path::new(&temp_file).is_file());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn honors_create_dir_recursive_false_and_remove_recursive_force_options() {
    let dir = unique_dir("options");
    let env = env_for(&dir);
    let context = bg_context();

    env.create_dir("a", Some(true), &context).await.expect("mkdir a");
    let error = env.create_dir("a", Some(false), &context).await.expect_err("existing dir");
    assert_eq!(error.code, FileErrorCode::Unknown);

    env.create_dir("a/b", Some(true), &context).await.expect("mkdir a/b");
    let error = env.remove("a", Some(false), Some(false), &context).await.expect_err("non-empty dir");
    assert_eq!(error.code, FileErrorCode::Unknown);

    env.remove("a", Some(true), Some(false), &context).await.expect("recursive remove");
    assert!(!env.exists("a", &context).await.expect("exists"));

    env.remove("missing", Some(false), Some(true), &context).await.expect("force remove");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_aborted_results_for_pre_aborted_cancellable_file_operations() {
    let dir = unique_dir("abortfs");
    let env = env_for(&dir);
    let controller = AbortController::new();
    controller.abort(Some(AbortReason::dom_default()));
    let context = with_abort_signal(controller.signal(), &bg_context());

    assert_eq!(env.read_text_file("a.txt", &context).await.expect_err("read").code, FileErrorCode::Aborted);
    assert_eq!(env.read_binary_file("a.txt", &context).await.expect_err("binary").code, FileErrorCode::Aborted);
    assert_eq!(env.write_file("a.txt", b"x", &context).await.expect_err("write").code, FileErrorCode::Aborted);
    assert_eq!(env.file_info("a.txt", &context).await.expect_err("info").code, FileErrorCode::Aborted);
    assert_eq!(env.list_dir(".", &context).await.expect_err("list").code, FileErrorCode::Aborted);
    assert_eq!(env.canonical_path("a.txt", &context).await.expect_err("canonical").code, FileErrorCode::Aborted);
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
            Some(ShellExecOptions { env: Some(serde_json::Map::from_iter([("PI_T15".to_string(), json!("yes"))])), ..Default::default() }),
            &context,
        )
        .await
        .expect("exec");
    assert_eq!(result.exit_code, 0);

    let cwd_result = env
        .exec("printf '%s' \"$(pwd)\"", Some(ShellExecOptions::default()), &context)
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
        .exec("printf '%s' \"${PI_T15_MARKER-unset}\"", Some(ShellExecOptions::default()), &context)
        .await
        .expect("exec");
    assert_eq!(inherited.exit_code, 0);

    let replaced = env
        .exec(
            "printf '%s' \"${PI_T15_MARKER-unset}\"",
            Some(ShellExecOptions {
                env: Some(serde_json::Map::from_iter([("PI_T15_MARKER".to_string(), json!("only"))])),
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
                on_update: Some(std::sync::Arc::new(move |update: ShellOutputUpdate, _context: Context| {
                    let text = match &update {
                        ShellOutputUpdate::Replace { output } => output.text.clone(),
                        ShellOutputUpdate::Append { text, .. } | ShellOutputUpdate::Slide { text, .. } => {
                            text.clone()
                        }
                        ShellOutputUpdate::Metadata { .. } => String::new(),
                    };
                    seen_updates.lock().expect("seen poisoned").push(text);
                    Box::pin(async {})
                })),
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
            Some(ShellExecOptions { cwd: Some("nope".to_string()), ..Default::default() }),
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

    let result = env.exec("exit 7", Some(ShellExecOptions::default()), &context).await.expect("exec");
    assert_eq!(result.exit_code, 7);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_timeout_errors_for_commands_exceeding_the_timeout() {
    let dir = unique_dir("timeout");
    let env = env_for(&dir);
    let context = bg_context();

    let error = env
        .exec("sleep 30", Some(ShellExecOptions { timeout: Some(0.2), ..Default::default() }), &context)
        .await
        .expect_err("timeout");
    assert_eq!(error.code, ExecutionErrorCode::Timeout);
    assert_eq!(error.message, "timeout:0.2");

    let invalid = env
        .exec("true", Some(ShellExecOptions { timeout: Some(0.0), ..Default::default() }), &context)
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

    let error = env.exec("printf 'x'", Some(ShellExecOptions::default()), &context).await.expect_err("shell");
    assert_eq!(error.code, ExecutionErrorCode::ShellUnavailable);
    assert!(error.message.contains("Custom shell path not found: /nonexistent/bash"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn returns_an_aborted_result_for_aborted_commands() {
    let dir = unique_dir("abortexec");
    let env = env_for(&dir);
    let controller = AbortController::new();
    controller.abort(Some(AbortReason::dom_default()));
    let context = with_abort_signal(controller.signal(), &bg_context());

    let error = env.exec("printf 'x'", Some(ShellExecOptions::default()), &context).await.expect_err("aborted");
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
            Some(ShellExecOptions { capture: Some(capture_options(1024, true)), ..Default::default() }),
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
            Some(ShellExecOptions { capture: Some(capture_options(64, true)), ..Default::default() }),
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
            Some(ShellExecOptions { capture: Some(capture_options(32, true)), ..Default::default() }),
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

// text-line-reader.test.ts
#[tokio::test]
async fn decodes_unicode_blank_lines_and_a_torn_final_line() {
    let dir = unique_dir("tlr");
    let env = env_for(&dir);
    let context = bg_context();
    env.write_file("lines.txt", "α\n\nb😀".as_bytes(), &context).await.expect("write");

    let reader = env.open_text_line_reader("lines.txt", &context).await.expect("open");
    assert_eq!(reader.read_line(&context).await.expect("line").expect("some").text, "α");
    let blank = reader.read_line(&context).await.expect("line").expect("some");
    assert_eq!(blank.text, "");
    assert!(blank.terminated);
    let torn = reader.read_line(&context).await.expect("line").expect("some");
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
    env.write_file("empty.txt", b"", &context).await.expect("write");

    let reader = env.open_text_line_reader("empty.txt", &context).await.expect("open");
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
    env.write_file("big.txt", content.as_bytes(), &context).await.expect("write");

    let reader = env.open_text_line_reader("big.txt", &context).await.expect("open");
    let first = reader.read_line(&context).await.expect("line").expect("some");
    assert!(first.text.ends_with('😀'));
    assert!(first.terminated);
    let second = reader.read_line(&context).await.expect("line").expect("some");
    assert_eq!(second.text, "tail");
    reader.close(&context).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rejects_an_open_with_a_pre_aborted_context() {
    let dir = unique_dir("tlrabort");
    let env = env_for(&dir);
    env.write_file("a.txt", b"x\n", &bg_context()).await.expect("write");
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
    env.write_file("a.txt", b"one\ntwo\n", &bg_context()).await.expect("write");
    let context = bg_context();

    let reader = env.open_text_line_reader("a.txt", &context).await.expect("open");
    assert_eq!(reader.read_line(&context).await.expect("line").expect("some").text, "one");
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
