use maho_ai::{auth::types::*,types::{Context,Message,UserMessage,UserContent},model::Model};
use maho_ext_cursor_cli_oauth::{stream::{stream_cursor_cli,StreamDeps},accounts::{add_account,empty_credential,CursorCliAccountSlot,AccountSource},settings::CursorCliOauthProviderSettings};
use std::{sync::Arc,collections::BTreeMap,os::unix::fs::PermissionsExt};
struct Flow;
#[tokio::test]
async fn held_process_allows_status_and_other_session_but_orders_same_session() {
    use maho_ext_cursor_cli_oauth::{session_router::{SessionRouter,TurnInput,SessionPolicy,SessionAttempt},stream::{spawn_attempt,SpawnAttemptInput}};
    use tokio::io::AsyncWriteExt;
    let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("cursor-agent");
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listener");
    std::fs::write(directory.path().join("address"),listener.local_addr().expect("address").to_string()).expect("address file");
    std::fs::write(&executable,r#"#!/usr/bin/python3
import json,socket,sys
prompt=sys.argv[sys.argv.index('-p')+1]
if prompt=='A2':
 assert sys.argv[sys.argv.index('--resume')+1]=='chat-A'
chat='chat-'+prompt
print(json.dumps({'type':'system','subtype':'init','session_id':chat,'model':'test','apiKeySource':'file','permissionMode':'plan','cwd':'test'}),flush=True)
if prompt=='A':
 host,port=open('address').read().split(':')
 with socket.create_connection((host,int(port))) as gate:
  assert gate.recv(1)==b'R'
print(json.dumps({'type':'assistant','message':{'content':[{'type':'text','text':prompt}]}}),flush=True)
print(json.dumps({'type':'result','subtype':'success','result':prompt,'usage':{'inputTokens':0,'outputTokens':1,'cacheReadTokens':0,'cacheWriteTokens':0},'request_id':'r','duration_ms':1,'is_error':False}),flush=True)
"#).expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let router=Arc::new(tokio::sync::Mutex::new(SessionRouter::default()));
    let spawn=|attempt:SessionAttempt| {
        let policy=maho_ext_cursor_cli_oauth::guardrails::resolve_execution_policy(&CursorCliOauthProviderSettings {execution_mode:maho_ext_cursor_cli_oauth::settings::ExecutionMode::Plan,..Default::default()},&mut Default::default(),&[]).expect("policy");
        spawn_attempt(SpawnAttemptInput {executable:executable.clone(),cwd:directory.path().into(),agent_dir:directory.path().join("agent"),slot:CursorCliAccountSlot {name:"a".into(),display_name:None,access:"fixture".into(),refresh:"fixture".into(),expires:10000.0,source:AccountSource::Login,blocked_until:None,block_reason:None},attempt,model:"test".into(),policy,environment:BTreeMap::new(),signal:None})
    };
    let policy=SessionPolicy::default();
    let (ready,observed)=tokio::sync::oneshot::channel();let mut ready=Some(ready);
    let accept=listener.accept();
    let mut a=Box::pin(SessionRouter::run_shared_turn(&router,TurnInput {session:"s-a",account:"a",prompt:"A",model:Some("test"),recent:&[],policy:&policy},|attempt|std::future::ready(Ok(spawn(attempt))),||1,|input|maho_ext_cursor_cli_oauth::errors::classify_cursor_cli_error(Some(input)).kind,|event| {if event["type"]=="system" {let _=ready.take().expect("one init").send(());}}));
    let (mut gate,_)=tokio::time::timeout(std::time::Duration::from_secs(10),async {
        tokio::select! {result=&mut a=>panic!("A settled before release: {result:?}"),connection=accept=>connection.expect("connection")}
    }).await.expect("bounded child barrier");
    tokio::time::timeout(std::time::Duration::from_secs(10),async {
        tokio::select! {result=&mut a=>panic!("A settled before init: {result:?}"),result=observed=>result.expect("routed init")}
    }).await.expect("bounded init");
    let status=tokio::time::timeout(std::time::Duration::from_secs(5),router.lock()).await.expect("status read proceeds before releasing A");
    assert_eq!(status.get_record("s-a").expect("A binding").chat_id,"chat-A");drop(status);
    let started=std::cell::Cell::new(false);let mut resume=None;
    let mut a2=Box::pin(SessionRouter::run_shared_turn(&router,TurnInput {session:"s-a",account:"a",prompt:"A2",model:Some("test"),recent:&[],policy:&policy},|attempt| {started.set(true);resume=attempt.resume_chat_id.clone();std::future::ready(Ok(spawn(attempt)))},||3,|input|maho_ext_cursor_cli_oauth::errors::classify_cursor_cli_error(Some(input)).kind,|_|{}));
    std::future::poll_fn(|cx| {assert!(a2.as_mut().poll(cx).is_pending());std::task::Poll::Ready(())}).await;
    assert!(!started.get(),"same-session subprocess must wait for A settlement");
    tokio::time::timeout(std::time::Duration::from_secs(10),SessionRouter::run_shared_turn(&router,TurnInput {session:"s-b",account:"a",prompt:"B",model:Some("test"),recent:&[],policy:&policy},|attempt|std::future::ready(Ok(spawn(attempt))),||2,|input|maho_ext_cursor_cli_oauth::errors::classify_cursor_cli_error(Some(input)).kind,|_|{})).await.expect("B progresses while A held").expect("B turn");
    assert!(!started.get());assert_eq!(router.lock().await.get_record("s-b").expect("B binding").chat_id,"chat-B");
    gate.write_all(b"R").await.expect("release A");
    tokio::time::timeout(std::time::Duration::from_secs(10),&mut a).await.expect("A settlement").expect("A turn");drop(a);
    tokio::time::timeout(std::time::Duration::from_secs(10),&mut a2).await.expect("A2 settlement").expect("A2 turn");drop(a2);
    assert_eq!(resume.as_deref(),Some("chat-A"));
    let status=router.lock().await;let record=status.get_record("s-a").expect("A2 binding");assert_eq!(record.chat_id,"chat-A2");assert_eq!(record.account_name,"a");assert_eq!(record.last_model,"test");assert_eq!(record.last_used_at,3);assert_eq!(status.get_record("s-b").expect("B binding").chat_id,"chat-B");
}
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

#[tokio::test]
async fn retired_native_generation_cancels_live_child_and_rejects_new_spawn() {
    use maho_ext_cursor_cli_oauth::{extension::CursorCliExtension,oauth_login::CursorCliOAuth};
    use maho_ai::types::AssistantMessageEvent;
    let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("cursor-agent");
    std::fs::write(&executable,r#"#!/usr/bin/python3
import json,sys,signal
signal.signal(signal.SIGTERM,lambda *_: sys.exit(0))
print(json.dumps({'type':'assistant','message':{'content':[{'type':'text','text':'ready'}]}}),flush=True)
signal.pause()
"#).expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
    let credential=add_account(&empty_credential(),CursorCliAccountSlot {name:"a".into(),display_name:None,access:"fixture".into(),refresh:"fixture".into(),expires:10000.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("slot");
    store.modify("cursor-cli-oauth",Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("seed");
    let settings=Arc::new(||CursorCliOauthProviderSettings {execution_mode:maho_ext_cursor_cli_oauth::settings::ExecutionMode::Plan,..Default::default()});
    let oauth=Arc::new(CursorCliOAuth {store,flow:Arc::new(Flow),settings,resolve:Arc::new(|_|Ok(())),persist_acknowledgement:Arc::new(|_|Ok(())),persist_enabled:Arc::new(|_|Ok(())),now:Arc::new(||1)});
    let extension=CursorCliExtension::native(oauth,executable,directory.path().into(),directory.path().join("agent"),Default::default());
    let model=maho_ext_cursor_cli_oauth::models::static_models()[0].clone();
    let model:Model=serde_json::from_value(serde_json::json!({"id":model.id,"name":model.name,"api":"cursor-agent","provider":"cursor-cli-oauth","baseUrl":"cursor-cli-oauth","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":200000,"maxTokens":64000})).expect("model");
    let context=Context {messages:vec![Message::User(UserMessage {content:UserContent::Text("hello".into()),timestamp:1})],..Default::default()};
    let stream=(extension.stream)(&model,&context,None);
    tokio::time::timeout(std::time::Duration::from_secs(10),async {
        while let Some(event)=stream.next().await.expect("stream event") {if matches!(event,AssistantMessageEvent::TextDelta {..}) {return;}}
        panic!("child ended before its readiness text");
    }).await.expect("bounded child readiness");
    extension.shutdown.as_ref().expect("generation").abort(None);
    let output=tokio::time::timeout(std::time::Duration::from_secs(10),stream.result()).await.expect("bounded retirement").expect("output");
    assert_eq!(output.stop_reason,maho_ai::types::StopReason::Aborted);
    let next=(extension.stream)(&model,&context,None);
    assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(10),next.result()).await.expect("bounded retired attempt").expect("output").stop_reason,maho_ai::types::StopReason::Aborted);
}
