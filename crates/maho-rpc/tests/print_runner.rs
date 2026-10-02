use maho_core::{model_runtime::{CreateModelRuntimeOptions,ModelRuntime},sdk::{create_agent_session,CreateAgentSessionOptions,NoToolsMode},session_manager::SessionManager,settings_manager::{SettingsManager,InMemorySettingsStorage}};
use maho_ai::providers::faux::{faux_provider,faux_assistant_message,FauxAssistantMessageOptions};
#[tokio::test]
async fn print_runner_drives_real_session_and_drains_native_json_events(){
    let temp=tempfile::tempdir().unwrap();let cwd=temp.path().to_string_lossy().into_owned();
    let provider=faux_provider(Default::default());let model=provider.get_model(Some("faux-1")).unwrap();
    provider.set_responses(vec![faux_assistant_message("native print",FauxAssistantMessageOptions{timestamp:Some(0),..Default::default()}).into(),faux_assistant_message("native json",FauxAssistantMessageOptions{timestamp:Some(0),..Default::default()}).into()]);
    let mut credentials=maho_core::auth_storage::AuthStorage::in_memory(Default::default());credentials.set(&model.provider,Some(serde_json::json!({"type":"api_key","key":"faux-test"}))).unwrap();
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
    session.dispose().await;
}
