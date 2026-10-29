#[test]
fn real_entry_dispatches_native_ulw_loop_help() {
    let dir = tempfile::tempdir().expect("isolated CLI directory");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["ulw-loop", "help"])
        .output()
        .expect("run real CLI");
    assert_eq!(result.status.code(), Some(0));
    assert!(!result.stdout.is_empty());
}

#[test]
fn real_entry_propagates_native_ulw_loop_failure_status() {
    let dir = tempfile::tempdir().expect("isolated CLI directory");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["ulw-loop", "not-a-command"])
        .output()
        .expect("run real CLI");
    assert_eq!(result.status.code(), Some(1));
}
