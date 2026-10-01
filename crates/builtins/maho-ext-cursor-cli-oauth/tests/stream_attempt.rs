use maho_ext_cursor_cli_oauth::{stream::{spawn_attempt,SpawnAttemptInput},accounts::{CursorCliAccountSlot,AccountSource},guardrails::ExecutionDecision,session_router::SessionAttempt,settings::ExecutionMode};
use std::{collections::BTreeMap,os::unix::fs::PermissionsExt,time::Duration};
fn input(root:&std::path::Path,body:&str)->SpawnAttemptInput {
    let executable=root.join("cursor-agent");std::fs::write(&executable,format!("#!/bin/sh\n{body}\n")).expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    SpawnAttemptInput {executable,cwd:root.into(),agent_dir:root.join("agent"),slot:CursorCliAccountSlot {name:"a".into(),display_name:None,access:"fake-access".into(),refresh:"fake-refresh".into(),expires:100000.0,source:AccountSource::Login,blocked_until:None,block_reason:None},attempt:SessionAttempt {prompt:"hello".into(),resume_chat_id:None},model:"test".into(),policy:ExecutionDecision {force:false,execution_mode:ExecutionMode::Plan,sandbox_mode:None,deny_commands:vec!["rm test".into()],warnings:Vec::new()},environment:BTreeMap::new(),signal:None}
}
#[tokio::test]
async fn delivers_output_before_process_exit_and_prepares_deny_config() {
    let root=tempfile::tempdir().expect("root");let controller=maho_ai::utils::abort::AbortController::new();
    let mut invocation=input(root.path(),r#"printf '%s\n' '{"type":"thinking","subtype":"delta","text":"ready"}'
exec /usr/bin/sleep 300"#);invocation.signal=Some(controller.signal());
    let mut events=spawn_attempt(invocation);
    let event=tokio::time::timeout(Duration::from_secs(10),events.recv()).await.expect("live output").expect("event").expect("ready");assert_eq!(event["text"],"ready");
    let config=std::fs::read_to_string(root.path().join("agent/cursor-cli-oauth/accounts/a/home/.cursor/cli-config.json")).expect("deny config");let config:serde_json::Value=serde_json::from_str(&config).expect("json");assert_eq!(config["permissions"]["deny"][0],"Shell(rm test)");
    controller.abort(None);
    let failure=tokio::time::timeout(Duration::from_secs(10),events.recv()).await.expect("abort").expect("failure").expect_err("aborted");assert_eq!(failure["kind"],"aborted");
    assert!(events.recv().await.is_none());
}
#[tokio::test]
async fn stderr_exit_failure_precedes_parser_incomplete_event() {
    let root=tempfile::tempdir().expect("root");let mut events=spawn_attempt(input(root.path(),"printf 'HTTP 429 rate limit' >&2; exit 7"));
    let error=events.recv().await.expect("failure").expect_err("exit error");assert_eq!(error["exitCode"],7);assert_eq!(error["stderr"],"HTTP 429 rate limit");assert!(events.recv().await.is_none());
}
