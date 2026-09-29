use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::exec::{GitExec, GitExecOptions, GitExecResult};
use super::repo::GitMemoryRepo;
use super::repo_types::GitMemoryRepoOptions;

struct OverlapTrackingGitExec {
    active_mutations: AtomicUsize,
    max_active_mutations: AtomicUsize,
}

impl OverlapTrackingGitExec {
    fn new() -> Self {
        Self {
            active_mutations: AtomicUsize::new(0),
            max_active_mutations: AtomicUsize::new(0),
        }
    }
}

impl GitExec for OverlapTrackingGitExec {
    fn run(&self, argv: &[String], _options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        let is_worktree_mutation = argv.first().map(String::as_str) == Some("worktree")
            && argv
                .get(1)
                .map(String::as_str)
                .is_some_and(|sub| sub == "add" || sub == "remove");

        if is_worktree_mutation {
            let active = self.active_mutations.fetch_add(1, Ordering::SeqCst) + 1;
            let mut prev_max = self.max_active_mutations.load(Ordering::SeqCst);
            while active > prev_max {
                match self.max_active_mutations.compare_exchange(
                    prev_max,
                    active,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(actual) => prev_max = actual,
                }
            }
            std::thread::yield_now();
            self.active_mutations.fetch_sub(1, Ordering::SeqCst);
        }

        Ok(GitExecResult {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[test]
fn given_concurrent_worktree_add_calls_then_they_do_not_overlap() {
    let tracking = Arc::new(OverlapTrackingGitExec::new());
    let repo = Arc::new(
        GitMemoryRepo::new(GitMemoryRepoOptions {
            dir: PathBuf::from("/repo"),
            agent_id: "test".to_string(),
            exec: Some(tracking.clone()),
            install_hooks: None,
        })
        .expect("repo"),
    );

    let r1 = Arc::clone(&repo);
    let r2 = Arc::clone(&repo);

    let t1 = std::thread::spawn(move || r1.worktree_add(PathBuf::from("/w1"), "branch-1", None));
    let t2 = std::thread::spawn(move || r2.worktree_add(PathBuf::from("/w2"), "branch-2", None));

    t1.join().unwrap().expect("add 1");
    t2.join().unwrap().expect("add 2");

    assert_eq!(tracking.max_active_mutations.load(Ordering::SeqCst), 1);
}

#[test]
fn given_concurrent_worktree_remove_calls_then_they_do_not_overlap() {
    let tracking = Arc::new(OverlapTrackingGitExec::new());
    let repo = Arc::new(
        GitMemoryRepo::new(GitMemoryRepoOptions {
            dir: PathBuf::from("/repo"),
            agent_id: "test".to_string(),
            exec: Some(tracking.clone()),
            install_hooks: None,
        })
        .expect("repo"),
    );

    let r1 = Arc::clone(&repo);
    let r2 = Arc::clone(&repo);

    let t1 = std::thread::spawn(move || r1.worktree_remove(PathBuf::from("/w1"), true));
    let t2 = std::thread::spawn(move || r2.worktree_remove(PathBuf::from("/w2"), true));

    t1.join().unwrap().expect("remove 1");
    t2.join().unwrap().expect("remove 2");

    assert_eq!(tracking.max_active_mutations.load(Ordering::SeqCst), 1);
}
