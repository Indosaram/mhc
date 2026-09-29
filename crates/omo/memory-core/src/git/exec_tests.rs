use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::errors::GitError;
use super::exec::{GitExec, GitExecOptions, GitExecResult, system_git_exec};

struct FailingMockExec {
    call_count: Arc<AtomicUsize>,
}

impl GitExec for FailingMockExec {
    fn run(&self, argv: &[String], _options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("git command not found: {}", argv.join(" ")),
        ))
    }
}

struct TimeoutMockExec;

impl GitExec for TimeoutMockExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "git {} timed out after {}ms",
                argv.join(" "),
                options.timeout_ms
            ),
        ))
    }
}

#[test]
fn given_missing_working_directory_when_run_executes_then_reports_exit_code_128() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let missing_path = temp_dir.path().join("missing-dir");
    let exec = system_git_exec();

    let options = GitExecOptions {
        cwd: missing_path,
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };

    let result = exec
        .run(
            &[
                "rev-parse".to_string(),
                "--verify".to_string(),
                "HEAD".to_string(),
            ],
            &options,
        )
        .expect("run");

    assert_eq!(result.code, 128);
    assert!(result.stderr.contains("No such file or directory"));
}

#[test]
fn given_binary_not_found_when_run_executes_then_not_found_error_is_produced() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let call_count = Arc::new(AtomicUsize::new(0));
    let mock = FailingMockExec {
        call_count: Arc::clone(&call_count),
    };

    let options = GitExecOptions {
        cwd: temp_dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };

    let err = mock.run(&["--version".to_string()], &options).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[test]
fn given_process_exceeding_timeout_when_run_executes_then_timeout_error_is_produced() {
    let mock = TimeoutMockExec;
    let options = GitExecOptions {
        cwd: PathBuf::from("."),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };

    let err = mock.run(&["--version".to_string()], &options).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    let git_err = GitError::Timeout {
        argv: vec!["--version".to_string()],
        timeout_ms: 5000,
    };
    assert!(git_err.to_string().contains("timed out after 5000ms"));
}

#[test]
fn given_input_via_stdin_when_hash_object_reads_it_then_git_produces_sha() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let exec = system_git_exec();

    let init_opts = GitExecOptions {
        cwd: temp_dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };
    let init_res = exec.run(&["init".to_string()], &init_opts).expect("init");
    assert_eq!(init_res.code, 0);

    let stdin_opts = GitExecOptions {
        cwd: temp_dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: Some(b"hello world\n".to_vec()),
    };

    let hash_res = exec
        .run(
            &["hash-object".to_string(), "--stdin".to_string()],
            &stdin_opts,
        )
        .expect("hash-object");

    assert_eq!(hash_res.code, 0);
    assert_eq!(
        hash_res.stdout.trim(),
        "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
    );
}

// --- Hermetic ports of the createNodeGitExec runtime tests.

use std::io::{Error as IoError, ErrorKind};
use std::sync::Mutex;

use super::exec::{GitExecRuntime, GitPlatform, create_git_exec};

type Responder = dyn Fn(&str, &GitExecOptions) -> std::io::Result<GitExecResult> + Send + Sync;

struct Recorder {
    attempts: Arc<Mutex<Vec<String>>>,
    stdins: Arc<Mutex<Vec<Option<Vec<u8>>>>>,
    exec: Arc<dyn GitExec>,
}

fn hermetic(platform: GitPlatform, respond: Arc<Responder>) -> Recorder {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let stdins = Arc::new(Mutex::new(Vec::new()));
    let (a, s) = (Arc::clone(&attempts), Arc::clone(&stdins));
    let exec = create_git_exec(GitExecRuntime {
        platform: Some(platform),
        run_command: Some(Arc::new(
            move |exe: &str, _argv: &[String], options: &GitExecOptions| {
                a.lock().expect("attempts").push(exe.to_string());
                s.lock().expect("stdins").push(options.stdin.clone());
                respond(exe, options)
            },
        )),
    });
    Recorder {
        attempts,
        stdins,
        exec,
    }
}

impl Recorder {
    fn attempts(&self) -> Vec<String> {
        self.attempts.lock().expect("attempts").clone()
    }
}

fn success() -> GitExecResult {
    GitExecResult {
        code: 0,
        stdout: "git version test\n".to_string(),
        stderr: String::new(),
    }
}

fn missing_git() -> IoError {
    IoError::new(ErrorKind::NotFound, "spawn failed: ENOENT")
}

fn env_options(pairs: &[(&str, &str)]) -> GitExecOptions {
    let mut env = BTreeMap::from([("PATH".to_string(), "/nonexistent".to_string())]);
    for (k, v) in pairs {
        env.insert((*k).to_string(), (*v).to_string());
    }
    GitExecOptions {
        cwd: PathBuf::from("."),
        timeout_ms: 5000,
        env,
        stdin: None,
    }
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

const NON_ENOENT: [(ErrorKind, &str); 3] = [
    (ErrorKind::PermissionDenied, "EACCES"),
    (ErrorKind::PermissionDenied, "EPERM"),
    (ErrorKind::Other, "EIO"),
];

#[test]
fn given_bare_runner_reports_missing_cwd_result_when_run_then_windows_fallbacks_are_not_attempted()
{
    let missing_cwd = GitExecResult {
        code: 128,
        stdout: String::new(),
        stderr: "fatal: cannot change directory".to_string(),
    };
    let expected = missing_cwd.clone();
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(move |_, _| Ok(missing_cwd.clone())),
    );

    let result = rec
        .exec
        .run(
            &argv(&["status"]),
            &env_options(&[("ProgramFiles", "C:\\Program Files")]),
        )
        .expect("run");

    assert_eq!(result, expected);
    assert_eq!(rec.attempts(), vec!["git"]);
}

#[test]
fn given_cwd_exists_but_no_git_on_path_when_git_runs_then_not_found_error_fires() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = GitExecOptions {
        cwd: dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::from([("PATH".to_string(), "/nonexistent".to_string())]),
        stdin: None,
    };

    let err = create_git_exec(GitExecRuntime {
        platform: Some(GitPlatform::Other),
        run_command: None,
    })
    .run(&argv(&["--version"]), &options)
    .expect_err("git must be missing");

    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn given_git_resolves_on_path_when_windows_roots_exist_then_bare_git_keeps_precedence() {
    let rec = hermetic(GitPlatform::Windows, Arc::new(|_, _| Ok(success())));

    let result = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[("ProgramFiles", "C:\\Program Files")]),
        )
        .expect("run");

    assert_eq!(result, success());
    assert_eq!(rec.attempts(), vec!["git"]);
}

#[test]
fn given_non_windows_platform_and_missing_git_when_git_runs_then_windows_roots_are_not_attempted() {
    let rec = hermetic(GitPlatform::Other, Arc::new(|_, _| Err(missing_git())));

    let err = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[("ProgramFiles", "C:\\Program Files")]),
        )
        .expect_err("missing");

    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(rec.attempts(), vec!["git"]);
}

#[test]
fn given_bare_git_fails_with_non_enoent_cause_when_windows_roots_exist_then_error_propagates_without_fallback()
 {
    for (kind, code) in NON_ENOENT {
        let rec = hermetic(
            GitPlatform::Windows,
            Arc::new(move |_, _| Err(IoError::new(kind, code))),
        );

        let err = rec
            .exec
            .run(
                &argv(&["--version"]),
                &env_options(&[("ProgramFiles", "C:\\Program Files")]),
            )
            .expect_err(code);

        assert_eq!((err.kind(), err.to_string()), (kind, code.to_string()));
        assert_eq!(rec.attempts(), vec!["git"], "{code}");
    }
}

#[test]
fn given_unc_device_and_root_relative_roots_when_fallback_runs_then_only_drive_qualified_root_is_attempted()
 {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            if exe == "git" {
                Err(missing_git())
            } else {
                Ok(success())
            }
        }),
    );

    let result = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[
                ("ProgramFiles", "\\\\server\\share"),
                ("ProgramW6432", "\\\\?\\C:\\Program Files"),
                ("ProgramFiles(x86)", "\\Program Files (x86)"),
                ("LOCALAPPDATA", "C:\\Users\\Example User\\AppData\\Local"),
            ]),
        )
        .expect("run");

    assert_eq!(result, success());
    assert_eq!(
        rec.attempts(),
        vec![
            "git".to_string(),
            "C:\\Users\\Example User\\AppData\\Local\\Programs\\Git\\cmd\\git.exe".to_string(),
        ]
    );
}

#[test]
fn given_distinct_standard_roots_when_every_executable_is_absent_then_candidates_attempted_in_deterministic_order()
 {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            Err(IoError::new(
                ErrorKind::NotFound,
                if exe == "git" { "original" } else { "fallback" },
            ))
        }),
    );

    let err = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[
                ("ProgramFiles", "C:\\Program Files"),
                ("ProgramW6432", "D:\\Program Files"),
                ("ProgramFiles(x86)", "E:\\Program Files (x86)"),
                ("LOCALAPPDATA", "F:\\User Data"),
            ]),
        )
        .expect_err("all absent");

    assert_eq!(err.to_string(), "original");
    assert_eq!(
        rec.attempts(),
        vec![
            "git",
            "C:\\Program Files\\Git\\cmd\\git.exe",
            "D:\\Program Files\\Git\\cmd\\git.exe",
            "E:\\Program Files (x86)\\Git\\cmd\\git.exe",
            "F:\\User Data\\Programs\\Git\\cmd\\git.exe",
        ]
    );
}

#[test]
fn given_duplicate_standard_roots_when_fallback_runs_then_equivalent_candidates_attempted_once() {
    let rec = hermetic(GitPlatform::Windows, Arc::new(|_, _| Err(missing_git())));

    let _ = rec.exec.run(
        &argv(&["--version"]),
        &env_options(&[
            ("ProgramFiles", "C:\\Program Files"),
            ("ProgramW6432", "c:\\PROGRAM FILES"),
            ("ProgramFiles(x86)", "D:\\Program Files (x86)"),
        ]),
    );

    assert_eq!(
        rec.attempts(),
        vec![
            "git",
            "C:\\Program Files\\Git\\cmd\\git.exe",
            "D:\\Program Files (x86)\\Git\\cmd\\git.exe",
        ]
    );
}

#[test]
fn given_first_fallback_yields_enoent_when_fallback_runs_then_next_candidate_is_attempted() {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            if exe == "git" || exe.starts_with("C:") {
                Err(missing_git())
            } else {
                Ok(success())
            }
        }),
    );

    let result = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[
                ("ProgramFiles", "C:\\Program Files"),
                ("ProgramW6432", "D:\\Program Files"),
            ]),
        )
        .expect("run");

    assert_eq!(result, success());
    assert_eq!(
        rec.attempts(),
        vec![
            "git",
            "C:\\Program Files\\Git\\cmd\\git.exe",
            "D:\\Program Files\\Git\\cmd\\git.exe",
        ]
    );
}

fn two_roots() -> GitExecOptions {
    env_options(&[
        ("ProgramFiles", "C:\\Program Files"),
        ("ProgramW6432", "D:\\Program Files"),
    ])
}

#[test]
fn given_fallback_fails_with_non_enoent_error_when_fallback_runs_then_error_propagates_without_later_candidates()
 {
    // Covers both TS loops: GitNotFoundError caused by EACCES/EPERM/EIO and a raw spawn EACCES/EPERM/EIO.
    for (kind, code) in NON_ENOENT {
        let rec = hermetic(
            GitPlatform::Windows,
            Arc::new(move |exe, _| {
                if exe == "git" {
                    Err(missing_git())
                } else {
                    Err(IoError::new(kind, code))
                }
            }),
        );

        let err = rec
            .exec
            .run(&argv(&["--version"]), &two_roots())
            .expect_err(code);

        assert_eq!(err.to_string(), code);
        assert_eq!(
            rec.attempts(),
            vec!["git", "C:\\Program Files\\Git\\cmd\\git.exe"],
            "{code}"
        );
    }
}

#[test]
fn given_fallback_timeout_when_fallback_runs_then_timeout_propagates_without_later_candidates() {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            if exe == "git" {
                Err(missing_git())
            } else {
                Err(IoError::new(ErrorKind::TimedOut, "timeout"))
            }
        }),
    );

    let err = rec
        .exec
        .run(&argv(&["--version"]), &two_roots())
        .expect_err("timeout");

    assert_eq!(err.kind(), ErrorKind::TimedOut);
    assert_eq!(
        rec.attempts(),
        vec!["git", "C:\\Program Files\\Git\\cmd\\git.exe"]
    );
}

#[test]
fn given_fallback_nonzero_result_when_fallback_runs_then_result_returns_without_later_candidates() {
    let nonzero = GitExecResult {
        code: 128,
        stdout: String::new(),
        stderr: "fatal: command failed".to_string(),
    };
    let expected = nonzero.clone();
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(move |exe, _| {
            if exe == "git" {
                Err(missing_git())
            } else {
                Ok(nonzero.clone())
            }
        }),
    );

    let result = rec.exec.run(&argv(&["status"]), &two_roots()).expect("run");

    assert_eq!(result, expected);
    assert_eq!(
        rec.attempts(),
        vec!["git", "C:\\Program Files\\Git\\cmd\\git.exe"]
    );
}

#[test]
fn given_stdin_when_real_git_runs_then_bytes_are_piped_like_a_file_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exec = system_git_exec();
    let content = b"stdin-content\n".to_vec();
    let path = dir.path().join("content.txt");
    std::fs::write(&path, &content).expect("write");
    let base = GitExecOptions {
        cwd: dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };
    let expected = exec
        .run(
            &argv(&["hash-object", "--", &path.to_string_lossy()]),
            &base,
        )
        .expect("hash file");

    let result = exec
        .run(
            &argv(&["hash-object", "--stdin"]),
            &GitExecOptions {
                stdin: Some(content),
                ..base
            },
        )
        .expect("hash stdin");

    assert_eq!(result.code, 0);
    assert_eq!(result.stdout, expected.stdout);
}

#[test]
fn given_stdin_and_windows_fallback_when_bare_git_absent_then_stdin_reaches_fallback_runner() {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            if exe == "git" {
                Err(missing_git())
            } else {
                Ok(success())
            }
        }),
    );
    let options = GitExecOptions {
        stdin: Some(b"fallback-input".to_vec()),
        ..env_options(&[("ProgramFiles", "C:\\Program Files")])
    };

    rec.exec
        .run(&argv(&["hash-object", "--stdin"]), &options)
        .expect("run");

    let input = Some(b"fallback-input".to_vec());
    assert_eq!(
        *rec.stdins.lock().expect("stdins"),
        vec![input.clone(), input]
    );
}
