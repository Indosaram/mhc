use maho_core::{model_runtime::{CreateModelRuntimeOptions,ModelRuntime},sdk::{create_agent_session,CreateAgentSessionOptions,NoToolsMode},session_manager::SessionManager,settings_manager::{SettingsManager,InMemorySettingsStorage}};
use maho_ai::providers::faux::{faux_provider,faux_assistant_message,FauxAssistantMessageOptions};
#[tokio::test]
async fn print_runner_drives_real_session_and_drains_native_json_events(){
    let temp=tempfile::tempdir().unwrap();let cwd=temp.path().to_string_lossy().into_owned();
    let provider=faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions{api:Some("faux".into()),token_size:Some(maho_ai::providers::faux::FauxTokenSize{min:Some(3),max:Some(3)}),tokens_per_second:Some(0.),..Default::default()});let model=provider.get_model(Some("faux-1")).unwrap();
    provider.set_responses(vec![faux_assistant_message("native print",FauxAssistantMessageOptions{timestamp:Some(0),..Default::default()}).into(),faux_assistant_message("native json",FauxAssistantMessageOptions{timestamp:Some(0),..Default::default()}).into(),faux_assistant_message("native rpc",FauxAssistantMessageOptions{timestamp:Some(0),..Default::default()}).into()]);
    let credentials=maho_core::auth_storage::AuthStorage::in_memory(Default::default());credentials.set(&model.provider,Some(serde_json::json!({"type":"api_key","key":"faux-test"}))).unwrap();
    let runtime=ModelRuntime::create_sync(CreateModelRuntimeOptions{models_path:Some(temp.path().join("models.json")),auth_path:Some(temp.path().join("auth.json")),credentials:Some(std::sync::Arc::new(credentials)),providers:Some(vec![provider.provider])});
    let session=create_agent_session(CreateAgentSessionOptions{cwd:Some(cwd.clone()),agent_dir:Some(cwd.clone()),model_runtime:Some(runtime),model:Some(model),session_manager:Some(SessionManager::in_memory(&cwd,None,None)),settings_manager:Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()),false)),no_tools:Some(NoToolsMode::All),auto_title_sessions:Some(false),..Default::default()}).await.unwrap().session;
    let mut output=Vec::new();let mut errors=Vec::new();
    let code=maho_rpc::print_mode::run_print_session(&session,false,Some(("first".into(),vec![])),&[],&mut output,&mut errors).await.unwrap();
    assert_eq!(code,0,"{}",String::from_utf8_lossy(&errors));assert_eq!(output,b"native print\n");assert!(errors.is_empty());
    output.clear();
    let code=maho_rpc::print_mode::run_print_session(&session,true,None,&["second".into()],&mut output,&mut errors).await.unwrap();
    assert_eq!(code,0);assert!(errors.is_empty());
    let records=String::from_utf8(output).unwrap().lines().map(|line|serde_json::from_str::<serde_json::Value>(line).unwrap()).collect::<Vec<_>>();
    assert_eq!(records[0]["type"],"session");
    assert!(records.iter().any(|record|record["type"]=="message_end"&&record["message"]["content"][0]["text"]=="native json"));
    assert!(records.iter().any(|record|record["type"]=="agent_idle"));
    {
        use tokio::io::{AsyncBufReadExt,AsyncWriteExt};
        let(client,host)=tokio::net::UnixStream::pair().unwrap();let(input,output)=host.into_split();let(read,mut write)=client.into_split();
        let run=Box::pin(maho_rpc::rpc_mode::run_command_stream(&session,input,output));
        let drive=Box::pin(async{
            let mut lines=tokio::io::BufReader::new(read).lines();
            write.write_all(b"{\"type\":\"get_messages\",\"id\":\"ready\"}\n").await.unwrap();
            let ready:serde_json::Value=serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();assert_eq!(ready["id"],"ready");assert_eq!(ready["success"],true);
            let prompt=Box::pin(session.prompt("third",Default::default()));
            let receive=async{
                let mut records=vec![];
                loop{let record:serde_json::Value=serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();let idle=record["type"]=="agent_idle";records.push(record);if idle{break;}}
                records
            };
            let(result,records)=tokio::join!(prompt,receive);result.unwrap();
            let settled=records.iter().position(|record|record["type"]=="agent_settled").unwrap();assert_eq!(records[settled+1]["type"],"agent_idle");
            let last=records.iter().find(|record|record["type"]=="message_end"&&record["message"]["role"]=="assistant").unwrap();assert_eq!(last["message"]["content"][0]["text"],"native rpc");assert_eq!(last["message"]["api"],"faux");
            for update in records.iter().filter(|record|record["type"]=="message_update"){assert!(update.get("message").is_none());assert!(update["assistantMessageEvent"].get("partial").is_none());assert_eq!(update["usage"],last["message"]["usage"]);}
            write.shutdown().await.unwrap();assert!(lines.next_line().await.unwrap().is_none());
        });
        let(result,())=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(run,drive)}).await.unwrap();result.unwrap();
    }
    let models=session.model_registry().clone();let services=maho_core::agent_session_services::AgentSessionServices{cwd:cwd.clone(),agent_dir:cwd,auth_storage:std::sync::Arc::clone(&models.auth_storage),model_registry:models,settings_manager:SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()),false),diagnostics:vec![]};
    let runtime=maho_core::agent_session_runtime::AgentSessionRuntime::new(session,services,vec![],None,None);let scope=maho_ai::node::provider_scope::ProviderScope::new();
    let mut output=Vec::new();let mut errors=Vec::new();assert_eq!(maho_rpc::print_mode::run_print_runtime(&runtime,&scope,false,None,&[],&mut output,&mut errors).await.unwrap(),0);assert_eq!(output,b"native rpc\n");assert!(errors.is_empty());assert_eq!(scope.state(),maho_ai::node::provider_scope::ScopeState::Closed);
}
