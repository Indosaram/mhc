use std::io;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use comment_checker_core::ByteStream;
use comment_checker_core::CheckResult;
use comment_checker_core::ExitFuture;
use comment_checker_core::HookInput;
use comment_checker_core::HookToolInput;
use comment_checker_core::ResolveCommentCheckerBinaryInput;
use comment_checker_core::RunCommentCheckerInput;
use comment_checker_core::RunCommentCheckerOptions;
use comment_checker_core::SpawnProcess;
use comment_checker_core::SpawnSignal;
use comment_checker_core::resolve_comment_checker_binary;
use comment_checker_core::run_comment_checker;
use pretty_assertions::assert_eq;
use serde_json::json;

#[derive(Default)]
struct Log {
    args: Vec<Vec<String>>,
    stdin: Vec<String>,
    stdin_ended: bool,
    kills: Vec<SpawnSignal>,
}

#[derive(Clone)]
enum Exit {
    Code(i32),
    Error,
    Never,
}

struct FakeProcess {
    log: Arc<Mutex<Log>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: Exit,
}

impl SpawnProcess for FakeProcess {
    fn write_stdin(&mut self, input: &str) -> io::Result<()> {
        self.log
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .stdin
            .push(input.to_string());
        Ok(())
    }

    fn end_stdin(&mut self) -> io::Result<()> {
        self.log
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .stdin_ended = true;
        Ok(())
    }

    fn take_stdout(&mut self) -> ByteStream {
        Box::pin(Cursor::new(std::mem::take(&mut self.stdout)))
    }

    fn take_stderr(&mut self) -> ByteStream {
        Box::pin(Cursor::new(std::mem::take(&mut self.stderr)))
    }

    fn exited(&mut self) -> ExitFuture<'_> {
        let exit = self.exit.clone();
        Box::pin(async move {
            match exit {
                Exit::Code(code) => Ok(code),
                Exit::Error => Err(io::Error::other("exit failed")),
                Exit::Never => std::future::pending().await,
            }
        })
    }

    fn kill(&mut self, signal: SpawnSignal) -> io::Result<()> {
        self.log
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .kills
            .push(signal);
        Err(io::Error::other("kill errors are swallowed"))
    }
}

struct Harness {
    log: Arc<Mutex<Log>>,
    stderr: &'static str,
    exit: Exit,
}

impl Harness {
    fn new(stderr: &'static str, exit: Exit) -> Self {
        Self {
            log: Arc::new(Mutex::new(Log::default())),
            stderr,
            exit,
        }
    }

    async fn run(
        &self,
        input: &RunCommentCheckerInput,
        timeout_ms: Option<u64>,
    ) -> io::Result<CheckResult> {
        let log = Arc::clone(&self.log);
        let stderr = self.stderr.as_bytes().to_vec();
        let exit = self.exit.clone();
        let spawn = move |args: &[String]| -> io::Result<Box<dyn SpawnProcess>> {
            log.lock()
                .map_err(|_| io::Error::other("poisoned"))?
                .args
                .push(args.to_vec());
            Ok(Box::new(FakeProcess {
                log: Arc::clone(&log),
                stdout: b"ignored stdout".to_vec(),
                stderr: stderr.clone(),
                exit: exit.clone(),
            }))
        };
        let exists = |path: &str| path == "/bin/comment-checker";
        let options = RunCommentCheckerOptions {
            spawn: &spawn,
            exists_sync: &exists,
            timeout_ms,
            kill_grace_ms: None,
        };
        run_comment_checker(input, &options).await
    }

    fn with_log<T>(&self, read: impl FnOnce(&Log) -> T) -> Option<T> {
        self.log.lock().ok().map(|log| read(&log))
    }
}

fn hook_input() -> HookInput {
    HookInput {
        session_id: "ses_1".to_string(),
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
    }
}

fn run_input(binary_path: Option<&str>, custom_prompt: Option<&str>) -> RunCommentCheckerInput {
    RunCommentCheckerInput {
        hook_input: hook_input(),
        binary_path: binary_path.map(str::to_string),
        custom_prompt: custom_prompt.map(str::to_string),
    }
}

fn empty() -> CheckResult {
    CheckResult::default()
}

#[tokio::test]
async fn run_returns_empty_without_spawning_when_binary_missing() {
    let harness = Harness::new("", Exit::Code(2));

    let absent = harness.run(&run_input(None, None), None).await.ok();
    let nonexistent = harness
        .run(&run_input(Some("/nope"), None), None)
        .await
        .ok();

    assert_eq!((absent, nonexistent), (Some(empty()), Some(empty())));
    assert_eq!(harness.with_log(|log| log.args.len()), Some(0));
}

#[tokio::test]
async fn run_exit_two_reports_comments_with_crlf_normalised_stderr() {
    let harness = Harness::new("COMMENT\r\nline two\r\n", Exit::Code(2));

    let result = harness
        .run(&run_input(Some("/bin/comment-checker"), None), None)
        .await
        .ok();

    assert_eq!(
        result,
        Some(CheckResult {
            has_comments: true,
            message: "COMMENT\nline two\n".to_string(),
        })
    );
}

#[tokio::test]
async fn run_pipes_hook_json_and_passes_check_and_prompt_args() {
    let harness = Harness::new("", Exit::Code(0));

    let result = harness
        .run(
            &run_input(Some("/bin/comment-checker"), Some("be strict")),
            None,
        )
        .await
        .ok();

    assert_eq!(result, Some(empty()));
    let (args, stdin, ended) = harness
        .with_log(|log| (log.args.clone(), log.stdin.clone(), log.stdin_ended))
        .unwrap_or_default();
    assert_eq!(
        args,
        vec![vec![
            "/bin/comment-checker".to_string(),
            "check".to_string(),
            "--prompt".to_string(),
            "be strict".to_string(),
        ]]
    );
    let sent: Vec<serde_json::Value> = stdin
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    assert_eq!(
        sent,
        vec![json!({
            "session_id": "ses_1",
            "tool_name": "Write",
            "transcript_path": "/tmp/t.jsonl",
            "cwd": "/repo",
            "hook_event_name": "PostToolUse",
            "tool_input": { "file_path": "src/a.ts", "content": "// hi\n" },
        })]
    );
    assert!(ended);
}

#[tokio::test]
async fn run_omits_prompt_args_without_custom_prompt() {
    let harness = Harness::new("", Exit::Code(0));

    let _ = harness
        .run(&run_input(Some("/bin/comment-checker"), None), None)
        .await;

    assert_eq!(
        harness.with_log(|log| log.args.clone()),
        Some(vec![vec![
            "/bin/comment-checker".to_string(),
            "check".to_string()
        ]])
    );
}

#[tokio::test]
async fn run_other_exit_codes_and_exit_errors_return_empty() {
    for exit in [Exit::Code(0), Exit::Code(1), Exit::Code(3), Exit::Error] {
        let harness = Harness::new("message", exit);

        let result = harness
            .run(&run_input(Some("/bin/comment-checker"), None), None)
            .await
            .ok();

        assert_eq!(result, Some(empty()));
    }
}

#[tokio::test(start_paused = true)]
async fn run_timeout_sends_sigterm_and_returns_empty() {
    let harness = Harness::new("COMMENT", Exit::Never);

    let result = harness
        .run(&run_input(Some("/bin/comment-checker"), None), Some(50))
        .await
        .ok();

    assert_eq!(result, Some(empty()));
    assert_eq!(
        harness.with_log(|log| log.kills.clone()),
        Some(vec![SpawnSignal::Sigterm])
    );
}

#[tokio::test]
async fn run_propagates_spawn_failure() {
    let spawn = |_: &[String]| -> io::Result<Box<dyn SpawnProcess>> {
        Err(io::Error::other("spawn failed"))
    };
    let exists = |_: &str| true;
    let options = RunCommentCheckerOptions {
        spawn: &spawn,
        exists_sync: &exists,
        timeout_ms: None,
        kill_grace_ms: None,
    };

    let result = run_comment_checker(&run_input(Some("/bin/x"), None), &options).await;

    assert_eq!(
        result.map_err(|error| error.to_string()),
        Err("spawn failed".to_string())
    );
}

fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn install_package(root: &Path, package: &str, binary: Option<&str>) -> Option<String> {
    let package_dir = root.join("node_modules").join(package);
    std::fs::create_dir_all(package_dir.join("bin")).ok()?;
    std::fs::write(package_dir.join("package.json"), "{}").ok()?;
    let binary = binary?;
    let path = std::fs::canonicalize(&package_dir)
        .ok()?
        .join("bin")
        .join(binary);
    std::fs::write(&path, "").ok()?;
    path.to_str().map(str::to_string)
}

fn resolve(
    binary: &str,
    cached: Option<&str>,
    url: Option<&str>,
    package: Option<&str>,
) -> Option<String> {
    resolve_comment_checker_binary(&ResolveCommentCheckerBinaryInput {
        binary_name: binary,
        cached_binary_path: cached,
        exists_sync: &exists,
        import_meta_url: url,
        package_name: package,
    })
}

#[test]
fn resolve_prefers_existing_cached_path() {
    let dir = tempfile::tempdir().ok();
    let cached = dir.as_ref().map(|d| d.path().join("cached"));
    if let Some(path) = &cached {
        let _ = std::fs::write(path, "");
    }
    let cached = cached.and_then(|p| p.to_str().map(str::to_string));

    assert_eq!(
        resolve("comment-checker", cached.as_deref(), None, None),
        cached
    );
}

#[test]
fn resolve_returns_none_without_import_meta_url_when_cache_is_stale() {
    assert_eq!(
        resolve("comment-checker", Some("/definitely/not/here"), None, None),
        None
    );
}

#[test]
fn resolve_finds_binary_through_node_modules_ancestors_of_file_url() {
    let dir = tempfile::tempdir().ok();
    let root = dir
        .as_ref()
        .map(|d| d.path().to_path_buf())
        .unwrap_or_default();
    let expected = install_package(
        &root,
        "@code-yeongyu/comment-checker",
        Some("comment-checker"),
    );
    let nested = root.join("packages/app/src");
    let _ = std::fs::create_dir_all(&nested);
    let url = url::Url::from_file_path(nested.join("index.ts"))
        .ok()
        .map(String::from);

    assert!(expected.is_some());
    assert_eq!(
        resolve("comment-checker", None, url.as_deref(), None),
        expected
    );
}

#[test]
fn resolve_honours_custom_package_name_and_missing_binary() {
    let dir = tempfile::tempdir().ok();
    let root = dir
        .as_ref()
        .map(|d| d.path().to_path_buf())
        .unwrap_or_default();
    let expected = install_package(&root, "custom-checker", Some("cc"));
    let url = url::Url::from_file_path(root.join("index.js"))
        .ok()
        .map(String::from);

    assert_eq!(
        resolve("cc", None, url.as_deref(), Some("custom-checker")),
        expected
    );
    assert_eq!(
        resolve("missing-bin", None, url.as_deref(), Some("custom-checker")),
        None
    );
    assert_eq!(resolve("cc", None, url.as_deref(), None), None);
}

#[test]
fn resolve_returns_none_for_unresolvable_import_meta_url() {
    assert_eq!(
        resolve("comment-checker", None, Some("relative/path.ts"), None),
        None
    );
    assert_eq!(
        resolve("comment-checker", None, Some("file://not a url"), None),
        None
    );
}
