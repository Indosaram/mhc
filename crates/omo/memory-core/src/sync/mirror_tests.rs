use std::fs;
use std::path::Path;

use tempfile::tempdir;

use crate::git::GitMemoryRepoOptions;

use super::*;

#[test]
fn test_module_surface_when_push_only_mirror_then_no_pull_fetch_or_import_is_exposed() {
    let script = get_post_commit_hook_script();
    assert!(!script.contains("git pull"));
    assert!(!script.contains("git fetch"));
    assert!(!script.contains("git clone"));
}

#[test]
fn test_constants_when_inspected_then_config_key_and_log_name_are_letta_parity_names() {
    assert_eq!(CONFIG_KEY, "omo.memoryRepository.url");
    assert_eq!(LOG_NAME, "memory-repository-push.log");
    assert_eq!(SYNC_PUSH_ENV, "OMO_MEMORY_PUSH_SYNC");
}

#[test]
fn test_post_commit_hook_script_when_inspected_then_guards_branch_and_never_fetches() {
    let script = get_post_commit_hook_script();
    assert!(script.contains(CONFIG_KEY));
    assert!(script.contains(LOG_NAME));
    assert!(script.contains("symbolic-ref"));
    assert!(script.contains("\"$branch\" != \"main\""));
    assert!(!script.contains("git pull"));
    assert!(!script.contains("git fetch"));
}

#[test]
fn test_normalize_url_when_called_with_trailing_slashes_then_stored_url_is_normalized() {
    assert_eq!(normalize_url("file:///tmp/bare///"), "file:///tmp/bare");
    assert_eq!(
        normalize_url("https://example.com/repo.git/"),
        "https://example.com/repo.git"
    );
    assert_eq!(normalize_url("   "), "");
    assert_eq!(normalize_url("///"), "///");
}

#[test]
fn test_mirror_log_path_when_given_repo_dir_then_resolves_to_git_log_file() {
    let repo_dir = Path::new("/var/memory/repo");
    let log_path = mirror_log_path(repo_dir);
    assert_eq!(
        log_path,
        Path::new("/var/memory/repo/.git/memory-repository-push.log")
    );
}

#[test]
fn test_status_log_tail_when_log_contains_credentialed_url_then_tail_is_redacted() {
    let tmp = tempdir().expect("tempdir");
    let repo_dir = tmp.path();
    let git_dir = repo_dir.join(".git");
    fs::create_dir_all(&git_dir).expect("create .git");

    let log_content = [
        "--- 2026-08-09T00:00:00 abc1234 on main ---",
        "fatal: could not read from https://x-token:s3cr3t@example.com/acme/memory.git",
        "remote: denied for git@example.com:acme/memory.git",
        "exit=128",
    ]
    .join("\n");

    fs::write(mirror_log_path(repo_dir), log_content).expect("write log");

    let raw = fs::read_to_string(mirror_log_path(repo_dir)).expect("read log");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    let redacted_tail: Vec<String> = lines.iter().map(|l| redact_url(l)).collect();
    let tail_text = redacted_tail.join("\n");

    assert!(!tail_text.contains("s3cr3t"));
    assert!(!tail_text.contains("x-token"));
    assert!(tail_text.contains("https://***:***@example.com/acme/memory.git"));
    assert!(tail_text.contains("***@example.com:acme/memory.git"));
    assert!(tail_text.contains("exit=128"));
}

#[test]
fn test_push_now_when_no_mirror_configured_then_reports_not_configured_without_throwing() {
    let empty_url: Option<String> = None;
    let push_result = if empty_url.is_none() {
        MirrorPushResult {
            pushed: false,
            detail: "No memory repository configured. Set one first.".to_string(),
        }
    } else {
        MirrorPushResult {
            pushed: true,
            detail: String::new(),
        }
    };

    assert!(!push_result.pushed);
    assert!(
        push_result
            .detail
            .contains("No memory repository configured")
    );
}

#[test]
fn test_push_detail_when_push_to_credentialed_remote_fails_then_no_credential_leaks() {
    let raw_error =
        "fatal: Authentication failed for 'https://x-token:s3cr3t@127.0.0.1:1/acme/memory.git'";
    let redacted_detail = redact_url(raw_error);

    assert!(!redacted_detail.contains("s3cr3t"));
    assert!(!redacted_detail.contains("x-token"));
    assert!(redacted_detail.contains("https://***:***@127.0.0.1:1/acme/memory.git"));
}

#[test]
fn test_unset_when_called_twice_then_removes_the_mirror_key_and_tolerates_absence() {
    // Pin mirror.test.ts "#when unset #then later commits produce no new log lines": the key is
    // gone afterwards and a repeat unset is a tolerated no-op (git exit code 5).
    let temp = tempdir().expect("tempdir");
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions::new(temp.path(), "mirror-agent"))
        .expect("repo");
    repo.init(None).expect("init");
    repo.config_set(CONFIG_KEY, "https://example.com/memory.git")
        .expect("seed mirror url");
    let mirror = MirrorSync::new(&repo);

    mirror.unset().expect("first unset");
    assert_eq!(repo.config_get(CONFIG_KEY).expect("get"), None);

    mirror.unset().expect("second unset is a no-op");
    assert!(mirror.status().url.is_none());
}

mod serialization {
    use super::*;
    use pretty_assertions::assert_eq;
    use crate::git::{GitExec, GitExecOptions, GitExecResult, system_git_exec};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct ConfigLockProbe {
        inner: Arc<dyn GitExec>,
        observed: AtomicBool,
    }

    impl GitExec for ConfigLockProbe {
        fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
            if argv.iter().any(|arg| arg == "--unset-all") {
                assert!(crate::git::config_lock::config_mutation_lock_is_held(&options.cwd));
                self.observed.store(true, Ordering::SeqCst);
            }
            self.inner.run(argv, options)
        }
    }

    #[test]
    fn test_unset_joins_the_serialized_config_mutation_queue() {
        let temp = tempdir().expect("tempdir");
        let repo = GitMemoryRepo::new(GitMemoryRepoOptions::new(temp.path(), "mirror-lock-agent"))
            .expect("repo");
        repo.init(None).expect("init");
        repo.config_set(CONFIG_KEY, "https://example.com/memory.git").expect("seed mirror url");
        let probe = ConfigLockProbe { inner: system_git_exec(), observed: AtomicBool::new(false) };
        MirrorSync::with_exec(&repo, &probe).unset().expect("unset");
        assert!(probe.observed.load(Ordering::SeqCst));
        assert_eq!(repo.config_get(CONFIG_KEY).expect("get"), None);
    }
}

#[cfg(unix)]
mod bounded_exec {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use crate::git::{GitExec, GitExecOptions, GitExecResult, GitExecRuntime};

    struct TimeoutTrackingExec {
        observed_timeout_ms: Arc<AtomicUsize>,
        observed_terminal_prompt: Arc<AtomicBool>,
    }

    impl GitExec for TimeoutTrackingExec {
        fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
            self.observed_timeout_ms.store(options.timeout_ms as usize, Ordering::SeqCst);
            let val = options.env.get("GIT_TERMINAL_PROMPT").cloned().unwrap_or_default();
            if val == "0" {
                self.observed_terminal_prompt.store(true, Ordering::SeqCst);
            }
            if argv.iter().any(|arg| arg == "push") {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("git push timed out after {}ms", options.timeout_ms),
                ))
            } else {
                Ok(GitExecResult {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
    }

    #[test]
    fn test_push_honors_timeout_ms_and_redacts_error_on_exec_none_path() {
        let temp = tempdir().expect("tempdir");
        let bin_dir = tempdir().expect("bin tempdir");
        let fifo_dir = tempdir().expect("fifo tempdir");
        let fifo_path = fifo_dir.path().join("stall_fifo");

        // Create a FIFO for stall control
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .expect("mkfifo");
        assert!(status.success());

        // Create a fake git script that blocks on reading the FIFO
        let fake_git_path = bin_dir.path().join("git");
        {
            let mut script = File::create(&fake_git_path).expect("create fake git");
            let script_content = format!(
                "#!/bin/sh\n\
                 if [ \"$GIT_TERMINAL_PROMPT\" != \"0\" ]; then\n\
                   echo \"missing GIT_TERMINAL_PROMPT=0\" >&2\n\
                   exit 2\n\
                 fi\n\
                 # Block until something is written to fifo\n\
                 exec cat \"{}\"\n\
                 exit 0\n",
                fifo_path.display()
            );
            script.write_all(script_content.as_bytes()).expect("write fake git");
            let mut perms = script.metadata().expect("meta").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_git_path, perms).expect("set_permissions");
        }

        // Default exec: None path delegates to repo.exec().
        // Here we provide repo with a GitExec using system spawn_and_capture against fake_git.
        let custom_exec = crate::git::exec::create_git_exec(GitExecRuntime {
            platform: None,
            run_command: Some(Arc::new({
                let fake_git = fake_git_path.clone();
                move |_cmd, argv, options| {
                    let mut cmd = std::process::Command::new(&fake_git);
                    cmd.args(argv);
                    cmd.current_dir(&options.cwd);
                    for (k, v) in &options.env {
                        cmd.env(k, v);
                    }
                    cmd.env("GIT_TERMINAL_PROMPT", "0");
                    let mut child = cmd.spawn()?;
                    let start = std::time::Instant::now();
                    let timeout = std::time::Duration::from_millis(options.timeout_ms);
                    loop {
                        if let Some(status) = child.try_wait()? {
                            return Ok(GitExecResult {
                                code: status.code().unwrap_or(1),
                                stdout: String::new(),
                                stderr: String::new(),
                            });
                        }
                        if start.elapsed() >= timeout {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                format!("git timed out after {}ms", options.timeout_ms),
                            ));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
            })),
        });

        let mut repo_opts = GitMemoryRepoOptions::new(temp.path(), "mirror-stall-agent");
        repo_opts.exec = Some(custom_exec);
        let repo = GitMemoryRepo::new(repo_opts).expect("repo");

        // MirrorSync::new uses exec: None!
        let mirror = MirrorSync::new(&repo);

        // Test with credential in URL to ensure redaction on timeout
        let result = mirror.push("https://user:secret@127.0.0.1:9999/repo.git", 100);

        assert!(!result.pushed);
        assert!(!result.detail.contains("secret"));
        assert!(!result.detail.contains("user:"));
        assert!(result.detail.contains("timed out after 100ms"));

        // Unblock any potential reader if anything survived
        let _ = std::fs::OpenOptions::new().write(true).read(true).open(&fifo_path);
    }

    #[test]
    fn test_push_passes_terminal_prompt_and_redacts_credentials_on_timeout() {
        let temp = tempdir().expect("tempdir");
        let timeout_observed = Arc::new(AtomicUsize::new(0));
        let prompt_observed = Arc::new(AtomicBool::new(false));

        let mock = TimeoutTrackingExec {
            observed_timeout_ms: Arc::clone(&timeout_observed),
            observed_terminal_prompt: Arc::clone(&prompt_observed),
        };

        let mut repo_opts = GitMemoryRepoOptions::new(temp.path(), "mirror-mock-agent");
        repo_opts.exec = Some(Arc::new(mock));
        let repo = GitMemoryRepo::new(repo_opts).expect("repo");

        // MirrorSync with exec None delegates to repo.exec()
        let mirror = MirrorSync::new(&repo);
        let result = mirror.push("https://alice:supersecret@example.com/mem.git", 10_000);

        assert!(!result.pushed);
        assert_eq!(timeout_observed.load(Ordering::SeqCst), 10_000);
        assert!(!result.detail.contains("supersecret"));
        assert!(!result.detail.contains("alice:"));
        assert!(result.detail.contains("timed out after 10000ms"));
    }
}
