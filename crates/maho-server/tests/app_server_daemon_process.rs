use maho_server::app_server::daemon_process::*;
#[test]
fn pidfile_requires_positive_integer_and_explicit_identity_guard() {
    for text in ["invalid", "null", "[]", "{}", r#"{"pid":0,"processStartTime":"time"}"#, r#"{"pid":1.5,"processStartTime":"time"}"#, r#"{"pid":1}"#, r#"{"pid":1,"processStartTime":" "}"#] {
        assert!(parse_daemon_pid_file(text).is_none());
    }
    assert_eq!(parse_daemon_pid_file(r#"{"pid":1,"processStartTime":null}"#), Some(DaemonPidFile { pid: 1, process_start_time: None }));
    assert_eq!(parse_daemon_pid_file(r#"{"pid":1,"processStartTime":" time "}"#), Some(DaemonPidFile { pid: 1, process_start_time: Some(" time ".into()) }));
}

#[tokio::test]
async fn identity_guard_matches_current_process_and_never_signals_mismatched_child() {
    let pid = u64::from(std::process::id());
    let identity = read_process_start_time(pid).await.unwrap().unwrap();
    assert!(process_matches_pid_file(&DaemonPidFile { pid, process_start_time:Some(identity) }).await.unwrap());
    assert!(process_matches_pid_file(&DaemonPidFile { pid, process_start_time:None }).await.is_err());
    let mut child = tokio::process::Command::new("sleep").arg("600").kill_on_drop(true).spawn().unwrap();
    let pid = u64::from(child.id().unwrap());
    let mismatched = DaemonPidFile { pid, process_start_time:Some("wrong identity".into()) };
    stop_validated_pid(&mismatched,"-TERM").await.unwrap();
    assert!(child.try_wait().unwrap().is_none());
    let identity = read_process_start_time(pid).await.unwrap().unwrap();
    let file = DaemonPidFile { pid, process_start_time:Some(identity) };
    assert!(process_matches_pid_file(&file).await.unwrap());
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    assert!(!process_matches_pid_file(&file).await.unwrap());
}
