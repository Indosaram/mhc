use std::sync::Arc;
use maho_core::{agent_session::{AgentSession,AgentSessionConfig},model_runtime::{ModelRuntime,CreateModelRuntimeOptions},session_manager::SessionManager,settings_manager::{SettingsManager,InMemorySettingsStorage}};
use maho_rpc::{connection_handler::handle_session_command,rpc_types::RpcCommand};
#[tokio::test]async fn real_session_queue_commands_preserve_recovery_order(){
    let temp=tempfile::tempdir().unwrap();let cwd=temp.path().to_string_lossy().into_owned();
    let runtime=ModelRuntime::create_sync(CreateModelRuntimeOptions{models_path:Some(temp.path().join("models.json")),auth_path:Some(temp.path().join("auth.json")),providers:Some(vec![]),..Default::default()});
    let session=AgentSession::new(AgentSessionConfig{
        agent:maho_agent::Agent::new(maho_agent::AgentOptions{stream_fn:Some(Arc::new(|_,_,_|maho_ai::types::AssistantMessageEventStream::assistant())),..Default::default()}),
        session_manager:SessionManager::in_memory(&cwd,None,None),settings_manager:SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()),false),
        cwd:cwd.clone(),agent_dir:Some(cwd),fallback_now:None,retry_random:None,scoped_models:vec![],favorite_models:vec![],flag_values:Default::default(),custom_tools:vec![],model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
    }).unwrap();
    for kind in ["set_auto_compaction","set_auto_retry","abort_retry","abort_compaction","abort_bash"]{let mut value=serde_json::json!({"type":kind});if kind.starts_with("set_"){value["enabled"]=false.into();}let command:RpcCommand=serde_json::from_value(value).unwrap();let response=serde_json::to_value(handle_session_command(&session,&command).await.unwrap()).unwrap();assert_eq!(response["command"],kind);assert_eq!(response["success"],true);}
    assert!(!session.auto_compaction_enabled());assert!(!session.auto_retry_enabled());
    for(kind,field)in [("get_messages","messages"),("get_last_assistant_text","text")]{let command:RpcCommand=serde_json::from_value(serde_json::json!({"id":kind,"type":kind})).unwrap();let response=serde_json::to_value(handle_session_command(&session,&command).await.unwrap()).unwrap();assert_eq!(response["id"],kind);if field=="messages"{assert_eq!(response["data"][field],serde_json::json!([]));}else{assert!(response["data"].get(field).is_none());}}
    for value in [serde_json::json!({"id":"1","type":"follow_up","message":"later","enqueueOrder":2}),serde_json::json!({"id":"2","type":"steer","message":"first","enqueueOrder":1})]{let command:RpcCommand=serde_json::from_value(value).unwrap();let response=handle_session_command(&session,&command).await.unwrap();assert_eq!(serde_json::to_value(response).unwrap()["success"],true);}
    let command:RpcCommand=serde_json::from_value(serde_json::json!({"id":"3","type":"clear_queue"})).unwrap();let response=serde_json::to_value(handle_session_command(&session,&command).await.unwrap()).unwrap();assert_eq!(response["data"]["ordered"][0]["text"],"first");assert_eq!(response["data"]["ordered"][1]["text"],"later");assert_eq!(session.pending_message_count(),0);
}
