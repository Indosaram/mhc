use std::sync::Arc;
use std::time::Duration;

use maho_ai::providers::faux::{faux_assistant_message, faux_provider, FauxAssistantMessageOptions, FauxTokenSize, RegisterFauxProviderOptions};
use maho_core::{auth_storage::AuthStorage, agent_session::AgentSession, model_runtime::{CreateModelRuntimeOptions, ModelRuntime}, sdk::{create_agent_session, CreateAgentSessionOptions, NoToolsMode}, session_manager::SessionManager, settings_manager::{InMemorySettingsStorage, SettingsManager}};
use maho_ext_api::AgentSessionEvent;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::time::timeout;

async fn next_record<R: tokio::io::AsyncBufRead + Unpin>(lines: &mut Lines<R>) -> Value {
    let line = lines.next_line().await.unwrap().expect("the stream stays open");
    serde_json::from_str(&line).unwrap()
}

async fn drain_to_eof<R: tokio::io::AsyncBufRead + Unpin>(lines: &mut Lines<R>) {
    while lines.next_line().await.unwrap().is_some() {}
}

struct Gated {
    session: AgentSession,
    gate: tokio::sync::watch::Sender<bool>,
    model_calls: tokio::sync::mpsc::UnboundedReceiver<()>,
}

async fn gated_session(cwd: &std::path::Path, responses: Vec<String>) -> Gated {
    let (gate, gate_rx) = tokio::sync::watch::channel(true);
    let (calls_tx, model_calls) = tokio::sync::mpsc::unbounded_channel();
    let hook_gate = gate_rx.clone();
    let provider = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".to_owned()),
        token_size: Some(FauxTokenSize { min: Some(30_000), max: Some(30_000) }),
        scheduler_hook: Some(Arc::new(move || {
            let _ = calls_tx.send(());
            let mut rx = hook_gate.clone();
            Box::pin(async move { while !*rx.borrow_and_update() { if rx.changed().await.is_err() { break; } } })
        })),
        ..Default::default()
    });
    let model = provider.get_model(Some("faux-1")).unwrap();
    provider.set_responses(responses.iter().map(|content| faux_assistant_message(content.as_str(), FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()).collect());
    let credentials = AuthStorage::in_memory(Default::default());
    credentials.set(&model.provider, Some(serde_json::json!({"type":"api_key","key":"faux-test"}))).unwrap();
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
        models_path: Some(cwd.join("models.json")),
        auth_path: Some(cwd.join("auth.json")),
        credentials: Some(Arc::new(credentials)),
        providers: Some(vec![provider.provider.clone()]),
        ..Default::default()
    });
    let cwd = cwd.to_string_lossy().into_owned();
    let session = create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(cwd.clone()), model_runtime: Some(runtime), model: Some(model),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)),
        settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)),
        no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default()
    }).await.unwrap().session;
    Gated { session, gate, model_calls }
}

async fn compactable_session(cwd: &std::path::Path) -> Gated {
    let mut gated = gated_session(cwd, vec!["x".repeat(120_000), "small".to_owned(), "summary".to_owned()]).await;
    gated.session.prompt("first", Default::default()).await.unwrap();
    gated.session.prompt("second", Default::default()).await.unwrap();
    while gated.model_calls.try_recv().is_ok() {}
    gated
}

#[tokio::test]
async fn session_events_stream_while_a_command_is_pending() {
    let temp = tempfile::tempdir().unwrap();
    let mut gated = compactable_session(temp.path()).await;
    gated.gate.send(false).unwrap();
    let (mut client, host) = tokio::net::UnixStream::pair().unwrap();
    let (input, output) = host.into_split();
    let run = maho_rpc::rpc_mode::run_command_stream(&gated.session, input, output);
    let drive = async {
        client.write_all(b"{\"id\":\"held\",\"type\":\"compact\"}\n").await.unwrap();
        let (read, mut write) = client.into_split();
        let mut lines = BufReader::new(read).lines();
        loop { if next_record(&mut lines).await["type"] == "compaction_start" { break; } }
        timeout(Duration::from_secs(5), gated.model_calls.recv()).await.expect("the compaction model call starts").expect("the provider stays alive");
        gated.session.emit(AgentSessionEvent::AgentSettled);
        let event = timeout(Duration::from_secs(5), next_record(&mut lines)).await.expect("the event streams while the command is still held");
        assert_eq!(event["type"], "agent_settled");
        gated.gate.send(true).unwrap();
        let response = loop { let record = next_record(&mut lines).await; if record["id"] == "held" { break record; } };
        assert_eq!(response["success"], true);
        write.shutdown().await.unwrap();
        drain_to_eof(&mut lines).await;
    };
    let (result, ()) = timeout(Duration::from_secs(30), async { tokio::join!(run, drive) }).await.expect("the stream completes");
    result.unwrap();
}

#[tokio::test]
async fn output_failure_during_a_pending_command_ends_the_stream() {
    let temp = tempfile::tempdir().unwrap();
    let mut gated = compactable_session(temp.path()).await;
    gated.gate.send(false).unwrap();
    let (mut client, host) = tokio::net::UnixStream::pair().unwrap();
    let (out_read, out_write) = tokio::io::duplex(4096);
    let run = maho_rpc::rpc_mode::run_command_stream(&gated.session, host, out_write);
    let drive = async {
        client.write_all(b"{\"id\":\"held\",\"type\":\"compact\"}\n").await.unwrap();
        let mut lines = BufReader::new(out_read).lines();
        loop { if next_record(&mut lines).await["type"] == "compaction_start" { break; } }
        timeout(Duration::from_secs(5), gated.model_calls.recv()).await.expect("the compaction model call starts").expect("the provider stays alive");
        drop(lines);
        gated.session.emit(AgentSessionEvent::AgentSettled);
    };
    let (result, ()) = timeout(Duration::from_secs(10), async { tokio::join!(run, drive) }).await.expect("a failed output write ends the stream while the command is held");
    assert!(result.is_err(), "a failed output write ends the stream with an error");
}

#[tokio::test]
async fn idle_eof_releases_the_stream_and_its_output() {
    let temp = tempfile::tempdir().unwrap();
    let gated = gated_session(temp.path(), vec!["unused".to_owned()]).await;
    let (mut client, host) = tokio::net::UnixStream::pair().unwrap();
    let (mut out_read, out_write) = tokio::io::duplex(4096);
    let run = maho_rpc::rpc_mode::run_command_stream(&gated.session, host, out_write);
    let drive = async {
        client.write_all(b"{\"id\":\"ready\",\"type\":\"get_fast_mode\"}\n").await.unwrap();
        client.shutdown().await.unwrap();
        let mut lines = BufReader::new(&mut out_read).lines();
        let record = next_record(&mut lines).await;
        assert_eq!(record["id"], "ready");
        assert_eq!(record["success"], true);
    };
    let (result, ()) = timeout(Duration::from_secs(10), async { tokio::join!(run, drive) }).await.expect("EOF ends the stream");
    result.unwrap();
    let mut tail = Vec::new();
    let read = timeout(Duration::from_secs(5), out_read.read_to_end(&mut tail)).await.expect("the stream released its output; a writer clone surviving the return would keep the duplex open");
    assert_eq!(read.unwrap(), 0);
    assert!(tail.is_empty());
}

#[test]
fn wire_fields_match_the_pinned_serializer_for_optional_fields() {
    let record = maho_rpc::session_binding::session_event_record(&AgentSessionEvent::CompactionEnd {
        reason: maho_ext_api::CompactionReason::Manual,
        result: Some(maho_ext_api::CompactionResult { summary: "s".into(), first_kept_entry_id: "e".into(), tokens_before: 3, details: None }),
        aborted: false,
        will_retry: true,
        request_id: Some("r".into()),
        accepted: Some(true),
        rejection_cause: None,
        error_message: None,
    })
    .unwrap();
    assert_eq!(record["type"], "compaction_end");
    assert_eq!(record["willRetry"], true);
    assert_eq!(record["requestId"], "r");
    assert_eq!(record["accepted"], true);
    assert_eq!(record["result"]["firstKeptEntryId"], "e");
    assert!(record.get("errorMessage").is_none(), "an absent optional field is omitted");
    assert_eq!(
        maho_rpc::session_binding::session_event_record(&AgentSessionEvent::SessionInfoChanged { name: None }).unwrap(),
        serde_json::json!({ "type": "session_info_changed" })
    );
    assert_eq!(
        maho_rpc::session_binding::session_event_record(&AgentSessionEvent::BashExecutionUpdate { id: Some("b".into()), delta: "out".into() }).unwrap(),
        serde_json::json!({ "type": "bash_execution_update", "delta": "out", "id": "b" })
    );
}

#[tokio::test]
async fn pending_command_eof_releases_the_stream_and_its_output() {
    let temp=tempfile::tempdir().unwrap();
    let mut gated=compactable_session(temp.path()).await;
    gated.gate.send(false).unwrap();
    let(mut client,host)=tokio::net::UnixStream::pair().unwrap();
    let(out_read,out_write)=tokio::io::duplex(4096);
    let run=maho_rpc::rpc_mode::run_command_stream(&gated.session,host,out_write);
    let drive=async {
        client.write_all(b"{\"id\":\"pending-eof\",\"type\":\"compact\"}\n").await.unwrap();
        let mut lines=BufReader::new(out_read).lines();
        loop {if next_record(&mut lines).await["type"]=="compaction_start"{break;}}
        gated.model_calls.recv().await.expect("provider command is pending");
        client.shutdown().await.unwrap();
        while let Some(line)=lines.next_line().await.unwrap(){
            let record:Value=serde_json::from_str(&line).unwrap();
            assert_ne!(record["id"],"pending-eof");
        }
        assert!(!*gated.gate.borrow(),"completion did not require releasing the provider");
    };
    let(result,())=timeout(Duration::from_secs(10),async{tokio::join!(run,drive)}).await.expect("pending EOF drops the command and writer");
    result.unwrap();
}

#[tokio::test]
async fn prompt_admission_and_abort_are_correlated_before_provider_completion() {
    let temp=tempfile::tempdir().unwrap();
    let mut gated=gated_session(temp.path(),vec!["held".into()]).await;
    gated.gate.send(false).unwrap();
    let(mut client,host)=tokio::net::UnixStream::pair().unwrap();
    let(input,output)=host.into_split();
    let run=maho_rpc::rpc_mode::run_command_stream(&gated.session,input,output);
    let drive=async {
        client.write_all(b"{\"id\":\"req_1\",\"type\":\"prompt\",\"message\":\"hello\",\"sessionTitlePrompt\":false}\n").await.unwrap();
        let(read,mut write)=client.into_split();
        let mut lines=BufReader::new(read).lines();
        let receipt=next_record(&mut lines).await;
        assert_eq!(receipt["type"],"response");
        assert_eq!(receipt["id"],"req_1");
        assert_eq!(receipt["data"]["disposition"],"started");
        gated.model_calls.recv().await.unwrap();
        write.write_all(b"{\"id\":\"req_2\",\"type\":\"abort\"}\n").await.unwrap();
        loop {let record=next_record(&mut lines).await;if record["id"]=="req_2"{assert_eq!(record["success"],true);break;}assert_ne!(record["id"],"req_1","prompt admission is emitted once");}
        assert!(!*gated.gate.borrow());
        write.shutdown().await.unwrap();
        drain_to_eof(&mut lines).await;
    };
    let(result,())=timeout(Duration::from_secs(10),async{tokio::join!(run,drive)}).await.unwrap();
    result.unwrap();
}

#[tokio::test]
async fn invalid_prompt_and_switch_preserve_deterministic_response_ids() {
    let temp=tempfile::tempdir().unwrap();
    let gated=gated_session(temp.path(),vec!["unused".into()]).await;
    let(mut client,host)=tokio::net::UnixStream::pair().unwrap();
    let(input,output)=host.into_split();
    let run=maho_rpc::rpc_mode::run_command_stream(&gated.session,input,output);
    let drive=async {
        let mut frames=maho_rpc::rpc_client::RpcClientFrames::default();
        let prompt=frames.command(serde_json::json!({"type":"prompt","message":"invalid","sessionTitlePrompt":true}),false,true);
        let switch=frames.command(serde_json::json!({"type":"switch_session","sessionPath":temp.path().join("absent.jsonl"),"cwdOverride":temp.path()}),false,true);
        assert_eq!(prompt["id"],"req_1");assert_eq!(switch["id"],"req_2");
        client.write_all(format!("{prompt}\n{switch}\n").as_bytes()).await.unwrap();
        let(read,mut write)=client.into_split();let mut lines=BufReader::new(read).lines();
        for id in ["req_1","req_2"]{let response=next_record(&mut lines).await;assert_eq!(response["id"],id);assert_eq!(response["success"],false);}
        write.shutdown().await.unwrap();drain_to_eof(&mut lines).await;
    };
    let(result,())=timeout(Duration::from_secs(10),async{tokio::join!(run,drive)}).await.unwrap();result.unwrap();
}

#[tokio::test]
async fn switch_session_applies_cwd_override_and_returns_correlated_success() {
    let temp=tempfile::tempdir().unwrap();
    let gated=gated_session(temp.path(),vec!["unused".into()]).await;
    let directory=temp.path().to_string_lossy().into_owned();
    let mut manager=SessionManager::create(&directory,Some(&directory),None);
    manager.append_message(serde_json::to_value(faux_assistant_message("replacement",FauxAssistantMessageOptions{timestamp:Some(0),..Default::default()})).unwrap());
    let path=manager.session_file().unwrap().to_owned();
    let target_id=manager.session_id().to_owned();
    let override_dir=temp.path().join("replacement");std::fs::create_dir(&override_dir).unwrap();
    let(mut client,host)=tokio::net::UnixStream::pair().unwrap();let(input,output)=host.into_split();
    let run=maho_rpc::rpc_mode::run_command_stream(&gated.session,input,output);
    let drive=async {
        let command=serde_json::json!({"id":"req_1","type":"switch_session","sessionPath":path,"cwdOverride":override_dir});
        client.write_all(format!("{command}\n").as_bytes()).await.unwrap();
        let(read,mut write)=client.into_split();let mut lines=BufReader::new(read).lines();
        loop{let record=next_record(&mut lines).await;if record["id"]=="req_1"{assert_eq!(record["success"],true);assert_eq!(record["data"]["cancelled"],false);break;}}
        assert_eq!(gated.session.session_id(),target_id);
        assert_eq!(gated.session.cwd(),override_dir.to_string_lossy());
        write.shutdown().await.unwrap();drain_to_eof(&mut lines).await;
    };
    let(result,())=timeout(Duration::from_secs(10),async{tokio::join!(run,drive)}).await.unwrap();result.unwrap();
}
