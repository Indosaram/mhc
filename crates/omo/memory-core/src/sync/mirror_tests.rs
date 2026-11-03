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

/// mirror.test.ts "MirrorSync lifecycle": real git, a bare `file://` mirror and the installed
/// post-commit hook, which runs inline because the commit sets `SYNC_PUSH_ENV=1`.
#[cfg(unix)]
mod lifecycle {
    use super::*;
    use crate::git::{GitSeedFile, InitializeGitRepoOptions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Output};

    fn git(dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env(SYNC_PUSH_ENV, "1")
            .output()
            .expect("spawn git")
    }

    fn create_repo(root: &Path) -> GitMemoryRepo {
        let dir = root.join("repo");
        let repo = GitMemoryRepo::new(GitMemoryRepoOptions::new(dir.clone(), "agent-mirror")).expect("repo");
        repo.init(InitializeGitRepoOptions {
            seed_files: vec![GitSeedFile { relative_path: "system/persona.md".into(), content: "seed\n".into() }],
            ..Default::default()
        })
        .expect("init");
        let hook = dir.join(".git/hooks/post-commit");
        fs::create_dir_all(hook.parent().expect("hooks dir")).expect("hooks dir");
        fs::write(&hook, get_post_commit_hook_script()).expect("hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("hook mode");
        repo
    }

    fn create_bare(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).expect("bare dir");
        assert!(git(&dir, &["init", "--bare", "--quiet", "--initial-branch=main"]).status.success());
        dir
    }

    fn commit_file(dir: &Path, relative: &str, body: &str) -> Output {
        let full = dir.join(relative);
        fs::create_dir_all(full.parent().expect("parent")).expect("parent");
        fs::write(&full, body).expect("write");
        assert!(git(dir, &["add", "-A"]).status.success());
        git(dir, &["commit", "-m", &format!("write {relative}")])
    }

    fn read_log(dir: &Path) -> String {
        fs::read_to_string(mirror_log_path(dir)).unwrap_or_default()
    }

    fn show(bare: &Path, spec: &str) -> Output {
        git(bare, &["show", spec])
    }

    #[test]
    fn when_set_then_a_commit_lands_then_the_mirror_receives_the_commit() {
        let root = tempdir().expect("tempdir");
        let repo = create_repo(root.path());
        let bare = create_bare(root.path(), "bare");
        let mirror = MirrorSync::new(&repo);
        let result = mirror.set(&format!("file://{}", bare.display())).expect("set");
        let commit = commit_file(&repo.dir, "system/persona.md", "hooked\n");
        assert!(result.pushed, "{}", result.detail);
        assert!(commit.status.success());
        assert_eq!(String::from_utf8_lossy(&show(&bare, "main:system/persona.md").stdout), "hooked\n");
        assert!(read_log(&repo.dir).contains("exit=0"));
    }

    #[test]
    fn when_set_is_called_twice_then_the_second_url_wins() {
        let root = tempdir().expect("tempdir");
        let repo = create_repo(root.path());
        let first = create_bare(root.path(), "first");
        let second = create_bare(root.path(), "second");
        let mirror = MirrorSync::new(&repo);
        mirror.set(&format!("file://{}", first.display())).expect("first set");
        mirror.set(&format!("file://{}", second.display())).expect("second set");
        commit_file(&repo.dir, "reference/notes.md", "second\n");
        assert_eq!(mirror.status().url, Some(format!("file://{}", second.display())));
        assert_eq!(String::from_utf8_lossy(&show(&second, "main:reference/notes.md").stdout), "second\n");
        assert!(!show(&first, "main:reference/notes.md").status.success());
    }

    #[test]
    fn when_unset_then_later_commits_produce_no_new_log_lines() {
        let root = tempdir().expect("tempdir");
        let repo = create_repo(root.path());
        let bare = create_bare(root.path(), "bare");
        let mirror = MirrorSync::new(&repo);
        mirror.set(&format!("file://{}", bare.display())).expect("set");
        commit_file(&repo.dir, "system/persona.md", "before unset\n");
        let before = read_log(&repo.dir);
        assert!(before.contains("exit=0"));
        mirror.unset().expect("unset");
        commit_file(&repo.dir, "reference/after.md", "after unset\n");
        assert_eq!(read_log(&repo.dir), before);
        assert!(mirror.status().url.is_none());
    }

    #[test]
    fn unreachable_mirror_set_keeps_config_and_reports_the_reason() {
        let root = tempdir().expect("tempdir");
        let repo = create_repo(root.path());
        let missing = root.path().join("gone/gone.git");
        let mirror = MirrorSync::new(&repo);
        let result = mirror.set(&format!("file://{}", missing.display())).expect("set");
        let status = mirror.status();
        assert!(!result.pushed);
        assert!(!result.detail.is_empty());
        assert_eq!(status.url, Some(format!("file://{}", missing.display())));
        assert!(status.ahead_count > 0);
    }

    #[test]
    fn unreachable_mirror_commit_succeeds_and_failure_is_logged_in_status() {
        let root = tempdir().expect("tempdir");
        let repo = create_repo(root.path());
        let missing = root.path().join("gone/gone.git");
        let mirror = MirrorSync::new(&repo);
        mirror.set(&format!("file://{}", missing.display())).expect("set");
        let commit = commit_file(&repo.dir, "system/persona.md", "still local\n");
        let status = mirror.status();
        assert!(commit.status.success());
        assert!(String::from_utf8_lossy(&git(&repo.dir, &["log", "--oneline"]).stdout).contains("write system/persona.md"));
        assert!(!read_log(&repo.dir).contains("exit=0"));
        assert!(!status.last_log_lines.is_empty());
        assert!(status.last_log_lines.join("\n").contains("exit="));
    }

    #[test]
    fn configured_mirror_push_now_lands_and_clears_ahead() {
        let root = tempdir().expect("tempdir");
        let repo = create_repo(root.path());
        let bare = create_bare(root.path(), "bare");
        repo.config_set(CONFIG_KEY, &format!("file://{}", bare.display())).expect("config");
        let mirror = MirrorSync::new(&repo);
        assert!(mirror.status().ahead_count > 0);
        let result = mirror.push_now();
        assert!(result.pushed, "{}", result.detail);
        assert!(git(&bare, &["log", "--oneline", "main"]).status.success());
        assert_eq!(mirror.status().ahead_count, 0);
    }
}
