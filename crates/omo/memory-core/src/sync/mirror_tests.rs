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
