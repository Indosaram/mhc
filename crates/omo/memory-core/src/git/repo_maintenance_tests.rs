use super::repo_maintenance::*;
use std::path::Path;
use std::sync::atomic::AtomicBool;

#[test]
fn cancellation_is_checked_before_git_spawn() {
    let cancelled = AtomicBool::new(true);
    assert!(matches!(run_memory_repo_maintenance(Path::new("."), &GitMaintenanceOptions { min_loose_objects: 0, timeout_ms: 100, executable: None }, &cancelled), Err(GitMaintenanceError::Aborted)));
}

#[test]
fn interval_stamp_suppresses_a_second_pass() {
    let root = tempfile::tempdir().unwrap();
    let init = std::process::Command::new("git").args(["init", "-q"]).current_dir(root.path()).status().unwrap();
    assert!(init.success());
    let config = std::process::Command::new("git").args(["config", "--local", "omo.maintenanceAt", "100"]).current_dir(root.path()).status().unwrap();
    assert!(config.success());
    let result = maintain_repository(root.path(), "omo.maintenanceAt", 101, 1000, &GitMaintenanceOptions { min_loose_objects: 1, timeout_ms: 1000, executable: None }, &AtomicBool::new(false)).unwrap();
    assert_eq!(result, GitMaintenanceResult::IntervalSuppressed);
}

#[test]
fn timeout_kills_and_reaps_stalled_git_child() {
    #[cfg(windows)]
    let mut child = std::process::Command::new("node")
        .args(["-e", "process.stdout.write('READY\\n'); process.stdin.resume();"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    #[cfg(not(windows))]
    let (_root, mut child) = {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("git");
        std::fs::write(&executable, "#!/bin/sh\nprintf 'READY\\n'\nIFS= read -r line\n").unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let child = std::process::Command::new(&executable)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        (root, child)
    };
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        sender.send((line, reader.into_inner())).unwrap();
    });
    let (line, stdout) = receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(line.trim(), "READY");
    reader.join().unwrap();
    child.stdout = Some(stdout);
    let cancelled = std::sync::Arc::new(AtomicBool::new(false));
    let signal = cancelled.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        let mut child = child;
        let result = run_child_with_start(&mut child, &["timeout"], 0, &signal, || started_tx.send(()).unwrap());
        (result, child)
    });
    started_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    let (result, mut child) = runner.join().unwrap();
    assert!(matches!(result, Err(GitMaintenanceError::Aborted)));
    assert!(child.try_wait().unwrap().is_some());
}

#[test]
fn injected_git_emits_ready_before_cancellation_and_is_reaped() {
    #[cfg(not(windows))]
    let (_root, exe_for_thread) = {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("git-worker");
        std::fs::write(&executable, "#!/bin/sh\nprintf 'READY\\n'\nIFS= read -r line\n").unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        (root, executable)
    };
    let cancelled = std::sync::Arc::new(AtomicBool::new(false));
    let child_pid = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let cancel_for_thread = cancelled.clone();
    let pid_for_thread = child_pid.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let runner = std::thread::spawn(move || {
        #[cfg(windows)]
        let child = std::process::Command::new("node")
            .args(["-e", "process.stdout.write('READY\\n'); process.stdin.resume();"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        #[cfg(not(windows))]
        let child = std::process::Command::new(exe_for_thread).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
        pid_for_thread.store(u64::from(child.id()), std::sync::atomic::Ordering::Release);
        let mut child = child;
        let stdout = child.stdout.take().unwrap();
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        assert_eq!(line.trim(), "READY");
        child.stdout = Some(reader.into_inner());
        ready_tx.send(()).unwrap();
        run_child(&mut child, &["held"], 0, &cancel_for_thread)
    });
    ready_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(runner.join().unwrap(), Err(GitMaintenanceError::Aborted)));
    let pid = child_pid.load(std::sync::atomic::Ordering::Acquire) as u32;
    #[cfg(not(unix))]
    let _ = pid;
    #[cfg(target_os = "linux")]
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    #[cfg(all(unix, not(target_os = "linux")))]
    assert!(!std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().unwrap().success());
}

#[test]
fn large_stdout_and_stderr_are_drained_concurrently() {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("node");
        c.args(["-e", "const b = Buffer.alloc(131072); process.stdout.write(b); process.stderr.write(b);"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", "head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2"]);
        c
    };
    let mut child = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let result=run_child(&mut child,&["large"],10_000,&AtomicBool::new(false)).unwrap();
    assert_eq!(result.len(),131072);
}
