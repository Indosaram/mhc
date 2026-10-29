use isolation_core::backends::git_fixture::repo;

#[test]
fn fixture_commits_leave_no_detached_maintenance_process_behind() {
    let f = repo();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(&f.repo_root)
        .args(["commit", "--allow-empty", "-q", "-m", "probe"])
        .env("GIT_TRACE", "1")
        .output()
        .expect("commit");
    assert!(output.status.success());
    let trace = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(trace.contains("built-in: git commit"));
    assert!(!trace.contains("run_command: git maintenance run --auto"));
    assert!(!trace.contains("run_command: git gc --auto"));
}
