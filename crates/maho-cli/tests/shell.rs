use maho_cli::utils::shell::*;
#[test]
fn classifies_shells_and_legacy_wsl_transport() {
    assert_eq!(resolve_shell_kind("C:\\Windows\\cmd.exe"), ShellKind::Cmd);
    assert_eq!(resolve_shell_kind("/usr/bin/pwsh"), ShellKind::Powershell);
    assert_eq!(shell_config_for_path("C:/Windows/System32/bash.exe").command_transport, Some("stdin"));
    assert_eq!(shell_config_for_path("/bin/bash").args, ["-c"]);
}
#[test]
fn sanitization_keeps_allowed_controls_and_unicode() {
    assert_eq!(sanitize_binary_output("a\0\u{fff9}\t한글\n\u{7f}"), "a\t한글\n\u{7f}");
}
#[test]
fn tracked_snapshots_are_independent() {
    track_detached_child_pid(2147483000);
    let snapshot = list_tracked_detached_children();
    untrack_detached_child_pid(2147483000);
    assert!(snapshot.iter().any(|entry| entry.pid == 2147483000 && !entry.leader_exited));
    assert!(!list_tracked_detached_children().iter().any(|entry| entry.pid == 2147483000));
}
#[tokio::test]
async fn explicit_shell_resolution_checks_file_and_invocation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cmd.exe");
    std::fs::write(&path, b"").unwrap();
    let config = get_shell_config(Some(path.to_str().unwrap())).await.unwrap();
    assert_eq!(config.args, ["/c"]);
    assert!(get_shell_config(Some(directory.path().join("missing").to_str().unwrap())).await.is_err());
}
