use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::config_lock::{
    is_git_lock_error_str, with_git_lock_retry, with_serialized_git_config_mutation,
};
use super::errors::GitError;
use super::exec::{GitExec, GitExecOptions, GitExecResult, system_git_exec};
use super::repo::GitMemoryRepo;
use super::repo_types::GitMemoryRepoOptions;

struct ContentionProbeExec {
    active_writes: AtomicUsize,
    max_writes: AtomicUsize,
    inner: Arc<dyn GitExec>,
}

impl GitExec for ContentionProbeExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        let is_config_write = argv.first().map(String::as_str) == Some("config")
            && argv.iter().any(|a| a == "--local")
            && !argv.iter().any(|a| a == "--get");

        if !is_config_write {
            return self.inner.run(argv, options);
        }

        let current = self.active_writes.fetch_add(1, Ordering::SeqCst) + 1;
        let mut prev_max = self.max_writes.load(Ordering::SeqCst);
        while current > prev_max {
            match self.max_writes.compare_exchange(
                prev_max,
                current,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => prev_max = actual,
            }
        }

        let res = self.inner.run(argv, options);
        self.active_writes.fetch_sub(1, Ordering::SeqCst);
        res
    }
}

#[test]
fn given_transient_config_lock_failures_when_mutation_runs_then_it_retries() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = Arc::clone(&attempts);

    let result = with_serialized_git_config_mutation(temp_dir.path(), move || {
        let count = attempts_clone.fetch_add(1, Ordering::SeqCst) + 1;
        if count < 3 {
            return Err(GitError::Command {
                argv: vec!["config".to_string()],
                code: 1,
                stdout: String::new(),
                stderr: "could not lock config file .git/config: File exists".to_string(),
            });
        }
        Ok("success")
    });

    assert_eq!(result.expect("success"), "success");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn given_concurrent_config_set_calls_when_mutating_then_they_do_not_overlap() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let probe = Arc::new(ContentionProbeExec {
        active_writes: AtomicUsize::new(0),
        max_writes: AtomicUsize::new(0),
        inner: system_git_exec(),
    });

    let options = GitMemoryRepoOptions {
        dir: temp_dir.path().to_path_buf(),
        agent_id: "lock-agent".to_string(),
        exec: Some(probe.clone()),
        install_hooks: None,
    };
    let repo = Arc::new(GitMemoryRepo::new(options).expect("repo"));
    repo.init(None).expect("init");

    let repo1 = Arc::clone(&repo);
    let repo2 = Arc::clone(&repo);

    let t1 = std::thread::spawn(move || repo1.config_set("omo.testOne", "one"));
    let t2 = std::thread::spawn(move || repo2.config_set("omo.testTwo", "two"));

    t1.join().unwrap().expect("set one");
    t2.join().unwrap().expect("set two");

    assert_eq!(probe.max_writes.load(Ordering::SeqCst), 1);
    assert_eq!(
        repo.config_get("omo.testOne").expect("get"),
        Some("one".to_string())
    );
    assert_eq!(
        repo.config_get("omo.testTwo").expect("get"),
        Some("two".to_string())
    );
}

#[test]
fn given_index_lock_contention_when_classified_then_is_retryable() {
    let msg = "fatal: Unable to create '/tmp/probe/.git/index.lock': File exists.";
    assert!(is_git_lock_error_str(msg));
}

#[test]
fn given_ref_lock_failure_when_classified_then_is_retryable() {
    let msg =
        "fatal: cannot lock ref 'HEAD': Unable to create '/tmp/probe/.git/HEAD.lock': File exists.";
    assert!(is_git_lock_error_str(msg));
}

#[test]
fn given_config_lock_failure_when_classified_then_is_retryable() {
    let msg = "error: could not lock config file .git/config: File exists";
    assert!(is_git_lock_error_str(msg));
}

#[test]
fn given_unrelated_git_failure_when_classified_then_is_not_retryable() {
    let msg = "error: pathspec 'nope.txt' did not match any file(s) known to git";
    assert!(!is_git_lock_error_str(msg));
}

#[test]
fn given_transient_index_lock_failures_when_operation_runs_then_it_retries() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = Arc::clone(&attempts);

    let result = with_git_lock_retry(move || {
        let count = attempts_clone.fetch_add(1, Ordering::SeqCst) + 1;
        if count < 3 {
            return Err(GitError::Command {
                argv: vec!["commit".to_string()],
                code: 1,
                stdout: String::new(),
                stderr: "fatal: Unable to create '/tmp/r/.git/index.lock': File exists."
                    .to_string(),
            });
        }
        Ok("committed")
    });

    assert_eq!(result.expect("committed"), "committed");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn given_lock_that_never_clears_when_exhausted_then_error_surfaces() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = Arc::clone(&attempts);

    let result: Result<&str, GitError> = with_git_lock_retry(move || {
        attempts_clone.fetch_add(1, Ordering::SeqCst);
        Err(GitError::Command {
            argv: vec!["commit".to_string()],
            code: 1,
            stdout: String::new(),
            stderr: "fatal: Unable to create '/x/.git/index.lock': File exists.".to_string(),
        })
    });

    assert!(result.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 5);
}

#[test]
fn given_non_lock_git_failure_when_operation_runs_then_it_surfaces_immediately() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = Arc::clone(&attempts);

    let result: Result<&str, GitError> = with_git_lock_retry(move || {
        attempts_clone.fetch_add(1, Ordering::SeqCst);
        Err(GitError::Command {
            argv: vec!["checkout".to_string()],
            code: 1,
            stdout: String::new(),
            stderr: "error: pathspec 'nope.txt' did not match any file(s) known to git".to_string(),
        })
    });

    assert!(result.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
