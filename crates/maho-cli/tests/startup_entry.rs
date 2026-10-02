fn run(args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().expect("isolated CLI directory");
    std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path()).env("HOME", dir.path()).env_remove("__PI_INTERNAL_SPAWN")
        .args(args).output().expect("run real CLI")
}
#[test]
fn real_entry_rejects_rpc_attachments_before_mode_dispatch() {
    let result = run(&["--mode", "rpc", "@missing.txt"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8(result.stderr).unwrap().contains("@file arguments are not supported in RPC mode"));
}
#[test]
fn real_entry_rejects_session_conflicts_before_mode_dispatch() {
    let result = run(&["--fork", "source", "--continue"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8(result.stderr).unwrap().contains("--fork cannot be combined with --continue"));
    let result = run(&["--session-id", "bad/id"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8(result.stderr).unwrap().contains("Session id must be non-empty"));
}
