use maho_ai::{auth::types::*,types::{Context,Message,UserMessage,UserContent},model::Model};
use maho_ext_cursor_cli_oauth::{stream::{stream_cursor_cli,StreamDeps},accounts::{add_account,empty_credential,CursorCliAccountSlot,AccountSource},settings::CursorCliOauthProviderSettings};
use std::{sync::Arc,collections::BTreeMap,os::unix::fs::PermissionsExt};
struct Flow;
#[tokio::test]
async fn failure_result_is_delivered_after_child_exit() {
    use maho_ext_cursor_cli_oauth::{stream::{spawn_attempt,SpawnAttemptInput},session_router::SessionAttempt};
    let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("cursor-agent");
    std::fs::write(&executable,r#"#!/bin/sh
printf '%s\n' '{"type":"result","subtype":"error","is_error":true,"error":"session missing","result":"","usage":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0},"request_id":"r","duration_ms":1}'
touch "$PWD/exited"
"#).expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let settings=CursorCliOauthProviderSettings {execution_mode:maho_ext_cursor_cli_oauth::settings::ExecutionMode::Plan,..Default::default()};
    let policy=maho_ext_cursor_cli_oauth::guardrails::resolve_execution_policy(&settings,&mut Default::default(),&[]).expect("policy");
    let mut events=spawn_attempt(SpawnAttemptInput {executable,cwd:directory.path().into(),agent_dir:directory.path().join("agent"),slot:CursorCliAccountSlot {name:"a".into(),display_name:None,access:"fake".into(),refresh:"fake".into(),expires:10000.0,source:AccountSource::Login,blocked_until:None,block_reason:None},attempt:SessionAttempt {prompt:"hello".into(),resume_chat_id:None},model:"test".into(),policy,environment:BTreeMap::new(),signal:None});
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),events.recv()).await.expect("bounded event").expect("event").expect("wire result");
    assert_eq!(result["type"],"result");assert_eq!(result["is_error"],true);assert!(directory.path().join("exited").exists());
}
#[async_trait::async_trait]
impl OAuthAuth for Flow {
    fn name(&self)->&str {"test"}
    async fn login(&self,_:&ProviderAuthInteraction)->anyhow::Result<OAuthCredential> {panic!("no login")}
    async fn refresh(&self,_:&OAuthCredential,_:&maho_ai::utils::abort::AbortSignal)->anyhow::Result<OAuthCredential> {panic!("unexpired must not refresh")}
    async fn to_auth(&self,_:&OAuthCredential)->anyhow::Result<ModelAuth> {Ok(ModelAuth::default())}
}
#[tokio::test]
async fn native_turn_runs_process_router_and_mapper() {
    let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("cursor-agent");
    std::fs::write(&executable,r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"chat","model":"test","apiKeySource":"file","permissionMode":"plan","cwd":"test"}' '{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}' '{"type":"result","subtype":"success","result":"hello","usage":{"inputTokens":900000,"outputTokens":1,"cacheReadTokens":0,"cacheWriteTokens":0},"request_id":"r","duration_ms":1,"is_error":false}'
"#).expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
    let credential=add_account(&empty_credential(),CursorCliAccountSlot {name:"a".into(),display_name:None,access:"fake-access".into(),refresh:"fake-refresh".into(),expires:10000.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("slot");
    store.modify("cursor-cli-oauth",Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("store");
    let model:Model=serde_json::from_value(serde_json::json!({"id":"test","name":"Test","api":"cursor-agent","provider":"cursor-cli-oauth","baseUrl":"cursor-cli-oauth","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":200000,"maxTokens":64000})).expect("model");
    let router=Arc::new(tokio::sync::Mutex::new(Default::default()));
    let stream=stream_cursor_cli(model,Context {messages:vec![Message::User(UserMessage {content:UserContent::Text("hello".into()),timestamp:1})],..Default::default()},None,StreamDeps {cwd:directory.path().into(),agent_dir:directory.path().join("agent"),executable,store,oauth:Arc::new(Flow),settings:CursorCliOauthProviderSettings {execution_mode:maho_ext_cursor_cli_oauth::settings::ExecutionMode::Plan,..Default::default()},router:router.clone(),environment:BTreeMap::new(),now:Arc::new(||1)});
    let output=tokio::time::timeout(std::time::Duration::from_secs(10),stream.result()).await.expect("bounded turn").expect("output");assert_eq!(output.stop_reason,maho_ai::types::StopReason::Stop,"{:?}",output.error_message);assert!(output.usage.input<100);assert_eq!(output.usage.output,1);
    assert!(matches!(&output.content[0],maho_ai::types::ContentBlock::Text(t) if t.text=="hello"));
    assert_eq!(router.lock().await.get_record("cursor-cli-oauth-default").expect("binding").chat_id,"chat");
}
