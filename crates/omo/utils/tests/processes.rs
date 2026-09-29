//! Translated from command-executor/execute-hook-command, runtime/runtime-shims, runtime/git-bash,
//! process-tree, port-utils, logging/logger, logging/product-identity and git-worktree tests.
//! Real subprocesses are used wherever the TS suite spawns them (`/bin/sh` stands in for `node -e`).
#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use utils::*;

fn tempdir() -> TempDir {
    TempDir::new().unwrap_or_else(|error| panic!("{error}"))
}

fn cwd(dir: &TempDir) -> String {
    dir.path().display().to_string()
}

#[test]
fn hook_allowed_env_vars_scrub_the_child_environment() {
    // SAFETY: nextest runs every test in its own process, so no other thread reads the env concurrently.
    unsafe {
        std::env::set_var("__OMO_TEST_ALLOWED_VAR", "visible");
        std::env::set_var("__OMO_TEST_SECRET_VAR", "hidden");
    }
    let dir = tempdir();
    let options = ExecuteHookOptions {
        allowed_env_vars: Some(vec!["__OMO_TEST_ALLOWED_VAR".to_string()]),
        ..Default::default()
    };
    let result = execute_hook_command(
        "echo \"$__OMO_TEST_ALLOWED_VAR\" \"$__OMO_TEST_SECRET_VAR\"",
        "",
        &cwd(&dir),
        &options,
    );
    assert_eq!(result.exit_code, 0);
    let stdout = result.stdout.unwrap_or_default();
    assert!(stdout.contains("visible"));
    assert!(!stdout.contains("hidden"));
}

#[test]
fn hook_without_allowlist_inherits_full_env() {
    // SAFETY: nextest runs every test in its own process, so no other thread reads the env concurrently.
    unsafe { std::env::set_var("__OMO_TEST_FULL_ENV_VAR", "present") };
    let dir = tempdir();
    let result = execute_hook_command(
        "echo \"$__OMO_TEST_FULL_ENV_VAR\"",
        "",
        &cwd(&dir),
        &ExecuteHookOptions::default(),
    );
    assert_eq!(
        (result.exit_code, result.stdout.as_deref()),
        (0, Some("present"))
    );
}

#[test]
fn hook_timeout_returns_124_instead_of_hanging() {
    let dir = tempdir();
    let options = ExecuteHookOptions {
        timeout_ms: Some(20),
        kill_grace_ms: Some(20),
        ..Default::default()
    };
    let started = Instant::now();
    let result = execute_hook_command("sleep 5", "", &cwd(&dir), &options);
    assert_eq!(result.exit_code, 124);
    assert!(
        result
            .stderr
            .unwrap_or_default()
            .contains("Hook command timed out after 20ms")
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn hook_plugin_root_is_exported_and_substituted() {
    let dir = tempdir();
    let cases = [
        (
            "echo plugin-root=$CLAUDE_PLUGIN_ROOT",
            "/tmp/plugin-x",
            None,
            "plugin-root=/tmp/plugin-x",
        ),
        (
            "echo plugin-root=$CLAUDE_PLUGIN_ROOT",
            "/tmp/plugin-y",
            Some(Vec::new()),
            "plugin-root=/tmp/plugin-y",
        ),
        (
            "echo \"rooted=${CLAUDE_PLUGIN_ROOT}/scripts/foo.sh\"",
            "/tmp/plugin-z",
            None,
            "rooted=/tmp/plugin-z/scripts/foo.sh",
        ),
    ];
    for (command, root, allowed, expected) in cases {
        let options = ExecuteHookOptions {
            plugin_root: Some(root.to_string()),
            allowed_env_vars: allowed,
            ..Default::default()
        };
        let result = execute_hook_command(command, "", &cwd(&dir), &options);
        assert_eq!(result.exit_code, 0);
        assert!(
            result.stdout.unwrap_or_default().contains(expected),
            "{command}"
        );
    }
}

#[test]
fn hook_env_export_survives_even_when_command_disables_substitution() {
    let dir = tempdir();
    let options = ExecuteHookOptions {
        plugin_root: Some("/tmp/plugin-env".to_string()),
        ..Default::default()
    };
    let result = execute_hook_command("printenv CLAUDE_PLUGIN_ROOT", "", &cwd(&dir), &options);
    assert_eq!(result.stdout.as_deref(), Some("/tmp/plugin-env"));
}

fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()]
}

#[test]
fn spawn_pipes_stdout_and_stderr_and_reports_exit_code() {
    let options = SpawnOptions {
        stdout: Some(StdioMode::Pipe),
        stderr: Some(StdioMode::Pipe),
        ..Default::default()
    };
    let process = spawn(&sh("echo out-ok; echo err-ok >&2"), &options)
        .unwrap_or_else(|error| panic!("{error}"));
    let (code, stdout, stderr) = process
        .wait_with_output()
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        (code, stdout.trim(), stderr.trim()),
        (0, "out-ok", "err-ok")
    );
}

#[test]
fn spawn_with_ignored_stdio_yields_empty_streams() {
    let options = SpawnOptions {
        stdio: Some([StdioMode::Ignore; 3]),
        ..Default::default()
    };
    let mut process =
        spawn(&sh("echo ignored"), &options).unwrap_or_else(|error| panic!("{error}"));
    assert!(process.stdout.is_none() && process.stderr.is_none());
    assert_eq!(process.exited().ok(), Some(0));
    assert_eq!(process.exit_code(), Some(0));
}

#[test]
fn spawn_sync_preserves_nonzero_exit_and_stderr() {
    let options = SpawnOptions {
        stdout: Some(StdioMode::Pipe),
        stderr: Some(StdioMode::Pipe),
        ..Default::default()
    };
    let result = spawn_sync(&sh("echo sync-err >&2; exit 7"), &options)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(result.exit_code, 7);
    assert!(!result.success);
    assert_eq!(
        String::from_utf8_lossy(&result.stderr.unwrap_or_default()).trim(),
        "sync-err"
    );
    assert!(result.pid > 0);
}

#[test]
fn spawn_rejects_empty_command() {
    assert!(spawn(&[], &SpawnOptions::default()).is_err());
}

#[test]
fn bun_which_constrains_lookup() {
    assert!(bun_which("sh").is_some_and(|path| path.ends_with("sh")));
    assert_eq!(bun_which("../sh"), None);
    assert_eq!(bun_which("sh\0evil"), None);
}

#[test]
fn runtime_file_round_trips() {
    let dir = tempdir();
    let path = dir.path().join("content.txt");
    assert_eq!(
        bun_write(&path, "hello runtime").ok(),
        Some("hello runtime".len())
    );
    let file = bun_file(&path);
    assert!(file.exists());
    assert_eq!(file.text().ok().as_deref(), Some("hello runtime"));
    file.delete().unwrap_or_else(|error| panic!("{error}"));
    assert!(!file.exists());
}

const PROGRAM_FILES_GIT_BASH: &str = "C:\\Program Files\\Git\\bin\\bash.exe";
const PROGRAM_FILES_X86_GIT_BASH: &str = "C:\\Program Files (x86)\\Git\\bin\\bash.exe";

fn resolve(
    platform: &str,
    env: &[(&str, &str)],
    exists: &dyn Fn(&str) -> bool,
    where_list: Vec<&str>,
) -> GitBashResolution {
    let env: HashMap<String, String> = env
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    let listed: Vec<String> = where_list.into_iter().map(str::to_string).collect();
    resolve_git_bash(&GitBashResolverInput {
        platform,
        env: &env,
        exists,
        where_bash: &|| listed.clone(),
    })
}

#[test]
fn git_bash_not_required_off_windows() {
    assert_eq!(
        resolve("darwin", &[], &|_| false, vec![]),
        GitBashResolution::Found {
            path: None,
            source: GitBashSource::NotRequired,
            checked_paths: vec![]
        }
    );
}

#[test]
fn git_bash_env_override_wins() {
    let override_path = "D:\\Tools\\Git\\bin\\bash.exe";
    let result = resolve(
        "win32",
        &[(GIT_BASH_ENV_KEY, override_path)],
        &|path| path == override_path || path == PROGRAM_FILES_GIT_BASH,
        vec![PROGRAM_FILES_GIT_BASH],
    );
    assert_eq!(
        result,
        GitBashResolution::Found {
            path: Some(override_path.to_string()),
            source: GitBashSource::Env,
            checked_paths: vec![override_path.to_string()]
        }
    );
}

#[test]
fn git_bash_missing_env_override_stops_probing() {
    let override_path = "D:\\Missing\\Git\\bin\\bash.exe";
    let result = resolve(
        "win32",
        &[(GIT_BASH_ENV_KEY, override_path)],
        &|path| path == PROGRAM_FILES_GIT_BASH,
        vec![PROGRAM_FILES_GIT_BASH],
    );
    let GitBashResolution::Missing {
        checked_paths,
        install_hint,
    } = result
    else {
        panic!("expected missing")
    };
    assert_eq!(checked_paths, vec![override_path.to_string()]);
    assert!(install_hint.contains("winget install --id Git.Git -e --source winget"));
    assert!(install_hint.contains("OMO_CODEX_GIT_BASH_PATH=C:\\path\\to\\bash.exe"));
}

#[test]
fn git_bash_program_files_has_priority() {
    let result = resolve(
        "win32",
        &[],
        &|path| path == PROGRAM_FILES_GIT_BASH || path == PROGRAM_FILES_X86_GIT_BASH,
        vec![],
    );
    assert_eq!(
        result,
        GitBashResolution::Found {
            path: Some(PROGRAM_FILES_GIT_BASH.to_string()),
            source: GitBashSource::ProgramFiles,
            checked_paths: vec![PROGRAM_FILES_GIT_BASH.to_string()],
        }
    );
}

#[test]
fn git_bash_skips_windows_launchers_on_path() {
    let system32 = "C:\\Windows\\System32\\bash.exe";
    let windows_apps = "C:/Users/dev/AppData/Local/Microsoft/WindowsApps/bash.exe";
    let git_bash = "E:\\Git\\bin\\bash.exe";
    let result = resolve(
        "win32",
        &[],
        &|path| [system32, windows_apps, git_bash].contains(&path),
        vec![system32, windows_apps, git_bash],
    );
    assert_eq!(
        result,
        GitBashResolution::Found {
            path: Some(git_bash.to_string()),
            source: GitBashSource::Path,
            checked_paths: [
                PROGRAM_FILES_GIT_BASH,
                PROGRAM_FILES_X86_GIT_BASH,
                system32,
                windows_apps,
                git_bash
            ]
            .map(str::to_string)
            .to_vec(),
        }
    );
}

fn tree_options(
    script: &str,
    max_buffer: usize,
    timeout_ms: u64,
) -> ProcessTreeRunOptions<'static> {
    ProcessTreeRunOptions {
        command: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: std::env::temp_dir(),
        env: HashMap::from([(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        )]),
        max_buffer,
        timeout_ms,
        termination_grace_ms: None,
        termination_wait_ms: None,
        on_termination_report: None,
    }
}

#[test]
fn process_tree_preserves_utf8_split_across_writes() {
    let result = run_process_with_tree_timeout(&tree_options(
        "printf '\\342'; sleep 0.05; printf '\\202\\254'",
        1024,
        5_000,
    ));
    assert_eq!((result.exit_code, result.stdout.as_str()), (0, "\u{20ac}"));
}

#[test]
fn process_tree_overflow_fails_without_partial_text() {
    let result = run_process_with_tree_timeout(&tree_options("printf '\\342\\202\\254'", 2, 5_000));
    assert_eq!((result.exit_code, result.stdout.as_str()), (1, ""));
}

fn alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|output| {
            let stat = String::from_utf8_lossy(&output.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        })
}

#[test]
fn process_tree_timeout_kills_sigterm_resistant_grandchild() {
    let reports = std::cell::RefCell::new(Vec::new());
    let observe = |report: &ProcessTreeTerminationReport| reports.borrow_mut().push(report.clone());
    let script = "trap '' TERM; sh -c \"trap '' TERM; while :; do sleep 1; done\" & echo $$ $!; while :; do sleep 1; done";
    let mut options = tree_options(script, 1024, 1_000);
    options.termination_grace_ms = Some(50);
    options.termination_wait_ms = Some(1_000);
    options.on_termination_report = Some(&observe);

    let result = run_process_with_tree_timeout(&options);
    let pids: Vec<u32> = result
        .stdout
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect();

    assert!(result.timed_out);
    assert_eq!(result.exit_code, 124);
    assert_eq!(pids.len(), 2);
    assert!(!pids.iter().any(|pid| alive(*pid)));
    let termination = result.termination.clone().unwrap_or_default();
    assert_eq!(termination.survivor_pids, Vec::<u32>::new());
    assert!(
        termination
            .attempts
            .iter()
            .any(|attempt| attempt.signal == TreeSignal::Kill)
    );
    assert_eq!(reports.into_inner(), vec![termination]);
}

static PORT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn contiguous_block_in_range(base_port: u16, count: u16, held: u16) -> (u16, Vec<TcpListener>) {
    static COUNTERS: [std::sync::atomic::AtomicU16; 8] = [
        std::sync::atomic::AtomicU16::new(0),
        std::sync::atomic::AtomicU16::new(0),
        std::sync::atomic::AtomicU16::new(0),
        std::sync::atomic::AtomicU16::new(0),
        std::sync::atomic::AtomicU16::new(0),
        std::sync::atomic::AtomicU16::new(0),
        std::sync::atomic::AtomicU16::new(0),
        std::sync::atomic::AtomicU16::new(0),
    ];
    let idx = usize::from((base_port / 1000) % 8);
    for _ in 0..100 {
        let step = COUNTERS[idx].fetch_add(count + 5, std::sync::atomic::Ordering::Relaxed);
        let start = base_port + (step % 800);
        if held == 0 {
            return (start, Vec::new());
        }
        let listeners: Vec<TcpListener> = (0..held)
            .map_while(|offset| TcpListener::bind((DEFAULT_PORT_HOSTNAME, start + offset)).ok())
            .collect();
        if listeners.len() == usize::from(held) {
            return (start, listeners);
        }
    }
    panic!("could not find {count} contiguous ports in range {base_port}");
}

#[test]
fn port_released_is_available_and_bound_is_not() {
    let _lock = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (port, _) = contiguous_block_in_range(31_000, 1, 0);
    assert!(is_port_available(port, DEFAULT_PORT_HOSTNAME));
    let blocker =
        TcpListener::bind((DEFAULT_PORT_HOSTNAME, 0)).unwrap_or_else(|error| panic!("{error}"));
    let bound = blocker
        .local_addr()
        .map(|address| address.port())
        .unwrap_or_default();
    assert!(!is_port_available(bound, DEFAULT_PORT_HOSTNAME));
}

#[test]
fn default_hostname_probe_targets_loopback() {
    let blocker = TcpListener::bind(("127.0.0.1", 0)).unwrap_or_else(|error| panic!("{error}"));
    let port = blocker
        .local_addr()
        .map(|address| address.port())
        .unwrap_or_default();

    assert!(!is_port_available(port, DEFAULT_PORT_HOSTNAME));
}

#[test]
fn default_probe_ignores_port_held_on_another_interface() {
    // Linux routes all of 127/8 to loopback; macOS only has 127.0.0.1 unless aliased.
    let Ok(blocker) = TcpListener::bind(("127.0.0.2", 0)) else {
        return;
    };
    let port = blocker
        .local_addr()
        .map(|address| address.port())
        .unwrap_or_default();

    assert!(is_port_available(port, DEFAULT_PORT_HOSTNAME));
    assert!(!is_port_available(port, "127.0.0.2"));
}

#[test]
fn port_probe_releases_the_socket_immediately() {
    let _lock = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (port, _) = contiguous_block_in_range(32_000, 1, 0);
    assert!(is_port_available(port, DEFAULT_PORT_HOSTNAME));
    let rebound =
        TcpListener::bind((DEFAULT_PORT_HOSTNAME, port)).and_then(|listener| listener.local_addr());
    assert_eq!(rebound.map(|address| address.port()).ok(), Some(port));
}

#[test]
fn find_available_port_returns_start_when_free() {
    let _lock = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (start, _held) = contiguous_block_in_range(33_000, 1, 0);
    assert_eq!(find_available_port(start, DEFAULT_PORT_HOSTNAME), Ok(start));
}

#[test]
fn find_available_port_skips_blocked_ports() {
    let _lock = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (start, _held) = contiguous_block_in_range(34_000, 4, 3);
    assert_eq!(
        find_available_port(start, DEFAULT_PORT_HOSTNAME),
        Ok(start + 3)
    );
}

#[test]
fn find_available_port_errors_when_all_attempts_blocked() {
    let _lock = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (start, _held) = contiguous_block_in_range(35_000, 20, 20);
    let error = find_available_port(start, DEFAULT_PORT_HOSTNAME)
        .err()
        .map(|error| error.to_string());
    assert_eq!(
        error,
        Some(format!(
            "No available port found in range {start}-{}",
            start + 19
        ))
    );
}

#[test]
fn get_available_server_port_prefers_then_autoselects() {
    let _lock = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (free, _none) = contiguous_block_in_range(36_000, 1, 0);
    assert_eq!(
        get_available_server_port(free, DEFAULT_PORT_HOSTNAME),
        Ok(AutoPortResult {
            port: free,
            was_auto_selected: false
        })
    );
    let (blocked, _held) = contiguous_block_in_range(37_000, 2, 1);
    assert_eq!(
        get_available_server_port(blocked, DEFAULT_PORT_HOSTNAME),
        Ok(AutoPortResult {
            port: blocked + 1,
            was_auto_selected: true
        })
    );
}

#[test]
fn default_server_port_is_4096() {
    assert_eq!(DEFAULT_SERVER_PORT, 4096);
}

fn logger_at(path: &Path) -> BoundLogger {
    let target = path.to_path_buf();
    let mut options = LoggerOptions::new("unused.log");
    options.resolve_log_file_path = Some(Box::new(move |_| target.clone()));
    create_logger(options)
}

#[test]
fn logger_flush_writes_timestamped_json_line() {
    let dir = tempdir();
    let path = dir.path().join("product.log");
    let logger = logger_at(&path);
    logger.log("LOGGER-OK", Some(&json!({"qa": true})));
    logger.flush_for_testing();
    let content = fs::read_to_string(&path).unwrap_or_default();
    let pattern = regex::Regex::new(r#"^\[\d{4}-\d{2}-\d{2}T.*Z\] LOGGER-OK \{"qa":true\}\n$"#)
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(pattern.is_match(&content), "{content:?}");
}

#[test]
fn logger_swallows_unserializable_data() {
    let dir = tempdir();
    let path = dir.path().join("product.log");
    let logger = logger_at(&path);
    let unserializable: HashMap<(i32, i32), i32> = HashMap::from([((1, 2), 3)]);
    logger.log("CYCLIC", Some(&unserializable));
    logger.flush_for_testing();
    assert!(!path.exists());
}

#[test]
fn logger_reset_restores_default_path() {
    let dir = tempdir();
    let default_path = dir.path().join("default.log");
    let logger = logger_at(&default_path);
    logger.set_logger_for_testing(&LoggerTestOverrides {
        file_path: Some(dir.path().join("override.log")),
        max_size_bytes: Some(1),
        max_backups: Some(1),
    });
    logger.reset_logger_for_testing();
    assert_eq!(logger.get_log_file_path(), default_path);
}

fn identity_input(accepted: Option<Vec<String>>) -> ProductIdentityInput {
    ProductIdentityInput {
        plugin_name: "canonical-plugin".to_string(),
        legacy_plugin_name: "legacy-plugin".to_string(),
        published_package_name: "published-package".to_string(),
        config_basename: "canonical-config".to_string(),
        legacy_config_basename: "legacy-config".to_string(),
        log_file_name: "product.log".to_string(),
        cache_dir_name: "product-cache".to_string(),
        accepted_package_names: accepted,
    }
}

#[test]
fn product_identity_derives_or_preserves_accepted_names() {
    assert_eq!(
        create_product_identity(identity_input(None)).accepted_package_names,
        vec!["published-package", "canonical-plugin"]
    );
    let supplied = vec![
        "legacy-plugin".to_string(),
        "published-package".to_string(),
        "canonical-plugin".to_string(),
    ];
    assert_eq!(
        create_product_identity(identity_input(Some(supplied.clone()))).accepted_package_names,
        supplied
    );
}

#[test]
fn porcelain_line_cases() {
    let parsed = |file: &str, status| {
        Some(ParsedGitStatusPorcelainLine {
            file_path: file.to_string(),
            status,
        })
    };
    assert_eq!(
        parse_git_status_porcelain_line(" M src/a.ts"),
        parsed("src/a.ts", GitFileStatus::Modified)
    );
    assert_eq!(
        parse_git_status_porcelain_line("A  src/b.ts"),
        parsed("src/b.ts", GitFileStatus::Added)
    );
    assert_eq!(
        parse_git_status_porcelain_line("?? src/c.ts"),
        parsed("src/c.ts", GitFileStatus::Added)
    );
    assert_eq!(
        parse_git_status_porcelain_line("D  src/d.ts"),
        parsed("src/d.ts", GitFileStatus::Deleted)
    );
    assert_eq!(
        parse_git_status_porcelain_line("R  src/old.ts -> src/new.ts"),
        parsed("src/new.ts", GitFileStatus::Modified)
    );
    assert_eq!(parse_git_status_porcelain_line(""), None);
    assert_eq!(parse_git_status_porcelain_line(" M "), None);
}

#[test]
fn porcelain_output_maps_paths() {
    let map = parse_git_status_porcelain(" M src/a.ts\nA  src/b.ts\n?? src/c.ts\nD  src/d.ts");
    let expected = HashMap::from([
        ("src/a.ts".to_string(), GitFileStatus::Modified),
        ("src/b.ts".to_string(), GitFileStatus::Added),
        ("src/c.ts".to_string(), GitFileStatus::Added),
        ("src/d.ts".to_string(), GitFileStatus::Deleted),
    ]);
    assert_eq!(map, expected);
}

fn stat(path: &str, added: u64, removed: u64, status: GitFileStatus) -> GitFileStat {
    GitFileStat {
        path: path.to_string(),
        added,
        removed,
        status,
    }
}

#[test]
fn numstat_parses_with_status_map() {
    let map = parse_git_status_porcelain(" M src/a.ts\nA  src/b.ts");
    assert_eq!(
        parse_git_diff_numstat("1\t2\tsrc/a.ts\n3\t0\tsrc/b.ts\n-\t-\tbin.dat", &map),
        vec![
            stat("src/a.ts", 1, 2, GitFileStatus::Modified),
            stat("src/b.ts", 3, 0, GitFileStatus::Added),
            stat("bin.dat", 0, 0, GitFileStatus::Modified),
        ]
    );
}

#[test]
fn format_file_changes_groups_and_notepad() {
    let summary = format_file_changes(
        &[
            stat("src/a.ts", 1, 2, GitFileStatus::Modified),
            stat("src/b.ts", 3, 0, GitFileStatus::Added),
            stat("src/c.ts", 0, 4, GitFileStatus::Deleted),
        ],
        None,
    );
    for needle in [
        "[FILE CHANGES SUMMARY]",
        "Modified files:",
        "Created files:",
        "Deleted files:",
        "src/a.ts",
        "src/b.ts",
        "src/c.ts",
    ] {
        assert!(summary.contains(needle), "{needle}");
    }
    let notepad = ".omo/notepads/work/notes.md";
    let plan = format_file_changes(
        &[stat(".omo/plans/work.md", 1, 0, GitFileStatus::Modified)],
        Some(notepad),
    );
    assert!(!plan.contains("[NOTEPAD UPDATED]"));
    let updated = format_file_changes(
        &[stat(notepad, 1, 0, GitFileStatus::Modified)],
        Some(notepad),
    );
    assert!(updated.contains("[NOTEPAD UPDATED]") && updated.contains(notepad));
    let other = format_file_changes(
        &[stat(
            ".omo/notepads/other/notes.md",
            1,
            0,
            GitFileStatus::Modified,
        )],
        Some(notepad),
    );
    assert!(!other.contains("[NOTEPAD UPDATED]"));
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        status.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

#[test]
fn collect_git_diff_stats_uses_argv_and_counts_untracked_lines() {
    let root = tempdir();
    let repo = root.path().join("safe-repo;touch pwn");
    fs::create_dir_all(&repo).unwrap_or_default();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("file.ts"), "a\nb\n").unwrap_or_default();
    git(&repo, &["add", "file.ts"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    fs::write(repo.join("file.ts"), "c\n").unwrap_or_default();
    fs::write(
        repo.join("new-file.ts"),
        (1..=10).map(|n| format!("line{n}\n")).collect::<String>(),
    )
    .unwrap_or_default();

    let stats = collect_git_diff_stats(&repo);

    assert_eq!(
        stats,
        vec![
            stat("file.ts", 1, 2, GitFileStatus::Modified),
            stat("new-file.ts", 10, 0, GitFileStatus::Added)
        ]
    );
    assert!(!repo.join("pwn").exists() && !root.path().join("pwn").exists());
}
