use maho_ext_cursor_cli_oauth::{transport::*,spawn_args::CursorCliArgsInput};
use std::{collections::BTreeMap,path::Path,os::unix::fs::PermissionsExt,time::Duration};

fn executable(root:&Path,body:&str) -> std::path::PathBuf {
    let path=root.join("cursor-agent");
    std::fs::write(&path,format!("#!/bin/sh\n{body}\n")).expect("write test executable");
    std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o700)).expect("set test executable permissions"); path
}
fn start(root:&Path,binary:&Path,prompt:&str) -> TransportHandle {
    spawn_cursor_cli(binary,CursorCliArgsInput { prompt,..Default::default() },root.to_str().expect("UTF-8 test directory"),root,
        &BTreeMap::from([("PATH".into(),"/usr/bin:/bin".into()),("SENPI_TRANSPORT_SECRET".into(),"must-not-leak".into())]),None).expect("spawn test process")
}
const RESULT:&str=r#"printf '%s\n' '{"type":"result","subtype":"success","result":"ok","usage":{"inputTokens":0,"outputTokens":1,"cacheReadTokens":0,"cacheWriteTokens":0},"request_id":"req","duration_ms":1,"is_error":false}'"#;
#[tokio::test]
async fn happy_invocation_reaps_child() {
    let root=tempfile::tempdir().unwrap(); let binary=executable(root.path(),RESULT); let mut handle=start(root.path(),&binary,"hello");
    let event=tokio::time::timeout(Duration::from_secs(10),handle.events.recv()).await.unwrap().unwrap().unwrap();
    assert_eq!(event["type"],"result");
    let outcome=handle.completed.await.unwrap().unwrap();
    assert!(matches!(outcome,TransportOutcome::Completed { exit_code:Some(0),.. }));
    assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(handle.pid).unwrap()),None).is_err());
}
#[tokio::test]
async fn explicit_environment_only() {
    let root=tempfile::tempdir().unwrap();
    let binary=executable(root.path(),&format!("env > invocation-env\n{RESULT}")); let handle=start(root.path(),&binary,"hello");
    handle.completed.await.unwrap().unwrap();
    let output=std::fs::read_to_string(root.path().join("invocation-env")).unwrap();
    assert!(output.contains("AGENT_CLI_CREDENTIAL_STORE=file")); assert!(!output.contains("SENPI_TRANSPORT_SECRET"));
}
#[tokio::test]
async fn silent_zero_exit_is_incomplete() {
    let root=tempfile::tempdir().unwrap(); let binary=executable(root.path(),"exit 0"); let mut handle=start(root.path(),&binary,"hello");
    let event=handle.events.recv().await.unwrap().unwrap(); assert_eq!(event["reason"],"incomplete_stream");
    assert!(matches!(handle.completed.await.unwrap().unwrap(),TransportOutcome::Completed { exit_code:Some(0),.. }));
}
#[tokio::test]
async fn oversize_prompts_rejected_before_spawn() {
    let root=tempfile::tempdir().unwrap();
    for size in [130001,449999] {
        let prompt="x".repeat(size);
        let result=spawn_cursor_cli(Path::new("/nonexistent"),CursorCliArgsInput { prompt:&prompt,..Default::default() },"/tmp",root.path(),&BTreeMap::new(),None);
        assert!(matches!(result,Err(TransportError::PromptTooLarge { actual_bytes,limit_bytes:130000 }) if actual_bytes==size));
    }
}
#[tokio::test]
async fn under_ceiling_prompt_spawns() {
    let root=tempfile::tempdir().unwrap(); let binary=executable(root.path(),RESULT); let handle=start(root.path(),&binary,&"x".repeat(129999));
    assert!(matches!(handle.completed.await.unwrap().unwrap(),TransportOutcome::Completed { exit_code:Some(0),.. }));
}
#[tokio::test]
async fn abort_after_exact_ready_event_reaps_group() {
    let root=tempfile::tempdir().unwrap();
    let binary=executable(root.path(),r#"printf '%s\n' '{"type":"thinking","subtype":"delta","text":"ready"}'
exec /usr/bin/sleep 300"#);
    let mut handle=start(root.path(),&binary,"slow");
    let ready=tokio::time::timeout(Duration::from_secs(10),handle.events.recv()).await.unwrap().unwrap().unwrap();
    assert_eq!(ready["text"],"ready"); handle.abort();
    let outcome=tokio::time::timeout(Duration::from_secs(10),handle.completed).await.unwrap().unwrap().unwrap();
    assert!(matches!(outcome,TransportOutcome::Aborted));
    assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(handle.pid).unwrap()),None).is_err());
}
#[tokio::test]
async fn ignored_sigterm_escalates_to_sigkill() {
    let root=tempfile::tempdir().unwrap();
    let binary=executable(root.path(),r#"trap '' TERM
printf '%s\n' '{"type":"thinking","subtype":"delta","text":"ready"}'
exec /usr/bin/sleep 300"#);
    let mut handle=start(root.path(),&binary,"slow");
    tokio::time::timeout(Duration::from_secs(10),handle.events.recv()).await.unwrap().unwrap().unwrap(); handle.abort();
    assert!(matches!(tokio::time::timeout(Duration::from_secs(10),handle.completed).await.unwrap().unwrap().unwrap(),TransportOutcome::Aborted));
}
