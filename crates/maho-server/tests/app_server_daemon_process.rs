use maho_server::app_server::daemon_process::*;
#[test]
fn pidfile_requires_positive_integer_and_explicit_identity_guard() {
    for text in ["invalid", "null", "[]", "{}", r#"{"pid":0,"processStartTime":"time"}"#, r#"{"pid":1.5,"processStartTime":"time"}"#, r#"{"pid":1}"#, r#"{"pid":1,"processStartTime":" "}"#] {
        assert!(parse_daemon_pid_file(text).is_none());
    }
    assert_eq!(parse_daemon_pid_file(r#"{"pid":1,"processStartTime":null}"#), Some(DaemonPidFile { pid: 1, process_start_time: None }));
    assert_eq!(parse_daemon_pid_file(r#"{"pid":1,"processStartTime":" time "}"#), Some(DaemonPidFile { pid: 1, process_start_time: Some(" time ".into()) }));
}
