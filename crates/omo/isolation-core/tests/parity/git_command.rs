use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use isolation_core::test_support::fixture;
use isolation_core::{
    run_git, str_args, GitOptions, IsolationError, SpawnObserver,
};

fn kill_on_spawn(signal: i32) -> SpawnObserver {
    Arc::new(move |pid| {
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    })
}

fn process_group_is_gone(pgid: u32) -> bool {
    for _ in 0..50 {
        let result = unsafe { libc::kill(-(pgid as libc::pid_t), 0) };
        if result != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_missing_git_binary_is_typed_unavailable_not_a_generic_spawn_failure() {
    let f = fixture();
    let mut options = GitOptions::new(f.repo_root.clone());
    options.env.push((
        "PATH".to_string(),
        f.root.join("no-git-here").to_string_lossy().into_owned(),
    ));
    let error = run_git(&str_args(&["status"]), &options).expect_err("must fail");
    assert!(error.is_unavailable());
}

#[test]
fn a_git_terminated_by_a_signal_is_a_failure_never_a_zero_exit() {
    let f = fixture();
    let mut options = GitOptions::new(f.repo_root.clone());
    options.allowed_exit_codes = Some((0..256).collect());
    options.on_spawn = Some(kill_on_spawn(libc::SIGKILL));
    let result = run_git(&str_args(&["-c", "alias.wait=!sleep 5", "wait"]), &options);
    if cfg!(windows) {
        let resolved = result.expect("win32 reports a non-zero exit code");
        assert_ne!(resolved.code, 0);
    } else {
        match result.expect_err("must fail") {
            IsolationError::Git { message, .. } => assert!(message.contains("signal")),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

#[test]
fn input_written_to_a_child_that_dies_before_reading_rejects_instead_of_crashing() {
    let f = fixture();
    let mut options = GitOptions::new(f.repo_root.clone());
    options.input = Some(b"payload\n".to_vec());
    options.on_spawn = Some(kill_on_spawn(libc::SIGKILL));
    let error = run_git(&str_args(&["-c", "alias.wait=!sleep 5", "wait"]), &options)
        .expect_err("must fail");
    assert!(matches!(error, IsolationError::Git { .. }));
}

#[test]
fn input_written_to_a_child_whose_survivors_hold_its_pipes_settles_before_the_survivor_exits() {
    let f = fixture();
    let release = f.root.join("survivor-release");
    let exited = f.root.join("survivor-exited");
    let survivor = "cd / && while [ ! -e \"$0\" ]; do sleep 0.05; done; : > \"$1\"";
    let alias = format!(
        "alias.orphan=!sh -c '{}' '{}' '{}' & exit 1",
        survivor,
        release.to_string_lossy(),
        exited.to_string_lossy()
    );
    let mut options = GitOptions::new(f.repo_root.clone());
    options.input = Some(b"payload\n".to_vec());
    let error = run_git(&str_args(&["-c", &alias, "orphan"]), &options).expect_err("must fail");
    assert!(matches!(error, IsolationError::Git { .. }));
    assert!(
        !exited.exists(),
        "the survivor still holds the stdio pipes; settling with it alive proves the run did not wait on close"
    );
    std::fs::write(&release, "").expect("release survivor");
}

#[test]
fn a_git_process_that_never_exits_is_terminated_at_its_command_deadline() {
    let f = fixture();
    let pgid = Arc::new(AtomicU32::new(0));
    let sink = Arc::clone(&pgid);
    let mut options = GitOptions::new(f.repo_root.clone());
    options.timeout_ms = Some(50);
    options.on_spawn = Some(Arc::new(move |pid| {
        sink.store(pid, Ordering::SeqCst);
    }));
    let error = run_git(&str_args(&["-c", "alias.wait=!sleep 60", "wait"]), &options)
        .expect_err("must fail");
    assert!(matches!(error, IsolationError::GitTimeout { .. }));
    let pgid = pgid.load(Ordering::SeqCst);
    if !cfg!(windows) && pgid != 0 {
        assert!(process_group_is_gone(pgid));
    }
}

#[test]
fn a_budget_breach_on_a_still_streaming_child_preserves_the_typed_limit_error() {
    let f = fixture();
    let pgid = Arc::new(AtomicU32::new(0));
    let sink = Arc::clone(&pgid);
    let mut options = GitOptions::new(f.repo_root.clone());
    options.max_output_bytes = Some(4096);
    options.output_limit_error = Some(Arc::new(|| IsolationError::other("budget breach")));
    options.on_spawn = Some(Arc::new(move |pid| {
        sink.store(pid, Ordering::SeqCst);
    }));
    let error = run_git(&str_args(&["-c", "alias.spam=!yes x", "spam"]), &options)
        .expect_err("must fail");
    assert!(error.to_string().contains("budget breach"));
    let pgid = pgid.load(Ordering::SeqCst);
    if !cfg!(windows) && pgid != 0 {
        assert!(process_group_is_gone(pgid));
    }
}

#[test]
fn a_budget_breach_tears_down_the_writer_even_when_the_alias_shell_survives_its_child() {
    let f = fixture();
    let pgid = Arc::new(AtomicU32::new(0));
    let sink = Arc::clone(&pgid);
    let mut options = GitOptions::new(f.repo_root.clone());
    options.max_output_bytes = Some(4096);
    options.output_limit_error = Some(Arc::new(|| IsolationError::other("budget breach")));
    options.on_spawn = Some(Arc::new(move |pid| {
        sink.store(pid, Ordering::SeqCst);
    }));
    let error = run_git(&str_args(&["-c", "alias.spam=!sh -c 'yes x'", "spam"]), &options)
        .expect_err("must fail");
    assert!(error.to_string().contains("budget breach"));
    let pgid = pgid.load(Ordering::SeqCst);
    if !cfg!(windows) && pgid != 0 {
        assert!(process_group_is_gone(pgid));
    }
    let _ = PathBuf::new();
}
