use maho_rpc::session_registry::{RpcSessionRegistry,RpcSessionLaunchProfile,OpenAdmission};
fn profile(cwd:&str,path:Option<String>,id:Option<&str>,worker:bool)->RpcSessionLaunchProfile{RpcSessionLaunchProfile{runtime:maho_core::agent_session_runtime::AgentSessionLaunchProfile{cwd:cwd.into(),..Default::default()},session_path:path,durable_session_id:id.map(str::to_owned),session_kind:worker.then_some(maho_ext_api::SessionKind::Worker),session_context:None}}
#[tokio::test]
async fn pending_open_owns_path_and_durable_id_before_runtime_creation(){
    let temp=tempfile::tempdir().unwrap();let cwd=temp.path().to_str().unwrap();let path=temp.path().join("session.jsonl").to_string_lossy().into_owned();let mut registry=RpcSessionRegistry::default();
    let OpenAdmission::Create(handle)=registry.admit_open(profile(cwd,Some(path.clone()),Some("durable"),false),false,0.).unwrap()else{panic!("expected create")};
    assert_eq!(registry.size(),1);
    assert_eq!(registry.admit_open(profile(cwd,Some(path.clone()),None,false),false,0.).err().unwrap().code,"session_path_in_use");
    assert_eq!(registry.admit_open(profile(cwd,None,Some("durable"),false),false,0.).err().unwrap().code,"session_id_in_use");
    let scope=registry.get(&handle).unwrap().scope.clone();
    let error=registry.create_admitted_runtime(&handle,async{assert!(maho_ai::node::provider_scope::active_provider_scope().is_some());Err("construction failed".into())}).await.unwrap_err();
    assert_eq!(error.code,"open_failed");assert_eq!(scope.state(),maho_ai::node::provider_scope::ScopeState::Closed);assert_eq!(registry.size(),0);
    assert!(registry.admit_open(profile(cwd,Some(path),Some("durable"),false),false,0.).is_ok());
}
#[test]
fn memory_pressure_refuses_only_new_workers_and_never_caps_interactive_count(){
    let temp=tempfile::tempdir().unwrap();let cwd=temp.path().to_str().unwrap();let mut registry=RpcSessionRegistry::default();registry.set_worker_admission(Some((2048,2000)));
    let error=registry.admit_open(profile(cwd,None,None,true),false,0.).err().unwrap();assert_eq!(error.code,"host_memory_pressure");assert_eq!(error.detail.unwrap(),serde_json::json!({"rssMb":2048,"retry_after_ms":2000}));
    for _ in 0..25{assert!(registry.admit_open(profile(cwd,None,None,false),false,0.).is_ok());}assert_eq!(registry.size(),25);
}

#[tokio::test]
async fn worker_listing_and_retained_detach_preserve_visibility_and_path_ownership(){
    let temp=tempfile::tempdir().unwrap();let cwd=temp.path().to_str().unwrap();let mut registry=RpcSessionRegistry::default();
    let OpenAdmission::Create(handle)=registry.admit_open(profile(cwd,None,Some("worker"),true),true,0.).unwrap()else{panic!("expected create")};
    registry.get_mut(&handle).unwrap().close.state=maho_rpc::session_registry::RpcSessionState::Open;
    assert!(registry.list_sessions(false).is_empty());assert_eq!(registry.list_sessions(true)[0]["context"],serde_json::json!({}));
    assert_eq!(registry.mark_close(&handle,true).unwrap().finalizer,None);assert_eq!(registry.get(&handle).unwrap().close.attachments,0);assert_eq!(registry.size(),1);
    assert_eq!(registry.mark_close(&handle,false).unwrap().finalizer,Some(true));assert!(registry.close_marked(&handle,10000).await.unwrap().is_none());assert_eq!(registry.size(),0);
}

#[tokio::test]
async fn native_runtime_install_command_and_disposal_keep_scope_until_settled(){
    use maho_core::{model_runtime::{CreateModelRuntimeOptions,ModelRuntime},sdk::{create_agent_session,CreateAgentSessionOptions,NoToolsMode},session_manager::SessionManager,settings_manager::{SettingsManager,InMemorySettingsStorage}};
    let temp=tempfile::tempdir().unwrap();let cwd=temp.path().to_string_lossy().into_owned();let mut registry=RpcSessionRegistry::default();
    let OpenAdmission::Create(handle)=registry.admit_open(profile(&cwd,None,None,false),false,0.).unwrap()else{panic!("expected create")};
    let scope=registry.get(&handle).unwrap().scope.clone();let construction_cwd=cwd.clone();
    registry.create_admitted_runtime(&handle,async{
        let provider=maho_ai::providers::faux::faux_provider(Default::default());let model=provider.get_model(Some("faux-1")).unwrap();
        let model_runtime=ModelRuntime::create_sync(CreateModelRuntimeOptions{models_path:Some(temp.path().join("models.json")),auth_path:Some(temp.path().join("auth.json")),providers:Some(vec![provider.provider]),..Default::default()});
        let session=create_agent_session(CreateAgentSessionOptions{cwd:Some(construction_cwd.clone()),agent_dir:Some(construction_cwd.clone()),model_runtime:Some(model_runtime),model:Some(model),session_manager:Some(SessionManager::in_memory(&construction_cwd,None,None)),settings_manager:Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()),false)),no_tools:Some(NoToolsMode::All),auto_title_sessions:Some(false),..Default::default()}).await.map_err(|error|error.to_string())?.session;
        let models=session.model_registry().clone();let services=maho_core::agent_session_services::AgentSessionServices{cwd:construction_cwd.clone(),agent_dir:construction_cwd,auth_storage:std::sync::Arc::clone(&models.auth_storage),model_registry:models,settings_manager:SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()),false),diagnostics:vec![]};
        Ok(maho_core::agent_session_runtime::AgentSessionRuntime::new(session,services,vec![],None,None))
    }).await.unwrap();
    assert_eq!(registry.list_sessions(false)[0]["status"],"open");
    let entry=registry.get_for_command(&handle,"get_fast_mode",42.).unwrap();assert_eq!(entry.last_command_at,42.);
    let response=maho_rpc::connection_handler::handle_input_line(entry.runtime.as_ref().unwrap().session(),"{\"type\":\"get_fast_mode\"}").await.unwrap().unwrap();assert_eq!(serde_json::from_str::<serde_json::Value>(&response).unwrap()["data"],serde_json::json!({"enabled":false,"serviceTier":null}));
    let command=serde_json::from_value(serde_json::json!({"type":"get_fast_mode","id":"routed"})).unwrap();let activity=maho_rpc::session_attribution::SessionActivityRegistry::default();let mark=activity.mark();
    let response=registry.dispatch_command(&handle,&command,"get_fast_mode",42.,&activity).await.unwrap().unwrap();assert_eq!(response.session_id.as_deref(),Some(handle.as_str()));assert_eq!(response.id.as_deref(),Some("routed"));assert_eq!(activity.since(mark).unwrap().session_id.as_deref(),Some(handle.as_str()));assert!(maho_rpc::session_attribution::current_session_attribution().is_none());
    registry.mark_close(&handle,false).unwrap();assert_eq!(registry.get_for_command(&handle,"get_state",43.).err().unwrap().code,"session_closing");assert!(registry.get_for_command(&handle,"abort",43.).is_ok());
    let completion=registry.close_marked(&handle,10000).await.unwrap().unwrap();match completion{maho_rpc::session_teardown::RuntimeCloseCompletion::Settled(result)=>result.unwrap().unwrap(),maho_rpc::session_teardown::RuntimeCloseCompletion::Draining(task)=>task.await.unwrap().unwrap()};
    assert_eq!(scope.state(),maho_ai::node::provider_scope::ScopeState::Closed);assert_eq!(registry.size(),0);
}

#[tokio::test]
async fn host_close_waits_for_draining_runtime_disposal_before_acknowledging(){
    use maho_ext_api::{ExtensionUi,ExtensionUiDialogOptions,UiFuture,NotificationType,WidgetContent,ExtensionWidgetOptions,ComponentFactory,CustomUiOptions,ExtensionFuture,JsonValue,Theme};
    use maho_core::{agent_session_runtime::AgentSessionRuntime,model_runtime::{CreateModelRuntimeOptions,ModelRuntime},sdk::{create_agent_session,CreateAgentSessionOptions,NoToolsMode},session_manager::SessionManager,settings_manager::{SettingsManager,InMemorySettingsStorage}};
    use maho_rpc::multi_session_host::{HostCore,HostCoreOptions,HostRuntimeFactory};
    use maho_rpc::session_event_writer::SessionWriterActor;
    use maho_rpc::session_registry::RpcSessionLaunchProfile;
    use std::sync::Arc;
    use tokio::io::AsyncBufReadExt;
    let temp=tempfile::tempdir().unwrap();let cwd=temp.path().to_string_lossy().into_owned();let model_path=temp.path().join("models.json");
    let (started_tx,started_rx)=tokio::sync::oneshot::channel::<()>();let started_tx=Arc::new(std::sync::Mutex::new(Some(started_tx)));
    let (release_tx,release_rx)=tokio::sync::oneshot::channel::<()>();let release_rx=Arc::new(std::sync::Mutex::new(Some(release_rx)));
    let runtime_cwd=cwd.clone();
    let create_runtime:HostRuntimeFactory=Arc::new(move |_profile:RpcSessionLaunchProfile|{
        let cwd=runtime_cwd.clone();let model_path=model_path.clone();let started_tx=started_tx.clone();let release_rx=release_rx.clone();
        Box::pin(async move{
            let provider=maho_ai::providers::faux::faux_provider(Default::default());let model=provider.get_model(Some("faux-1")).unwrap();
            let model_runtime=ModelRuntime::create_sync(CreateModelRuntimeOptions{models_path:Some(model_path),auth_path:None,providers:Some(vec![provider.provider]),..Default::default()});
            let session=create_agent_session(CreateAgentSessionOptions{cwd:Some(cwd.clone()),agent_dir:Some(cwd.clone()),model_runtime:Some(model_runtime),model:Some(model),session_manager:Some(SessionManager::in_memory(&cwd,None,None)),settings_manager:Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()),false)),no_tools:Some(NoToolsMode::All),auto_title_sessions:Some(false),..Default::default()}).await.map_err(|error|error.to_string())?.session;
            let mut extension=maho_ext_api::LoadedExtension::new("<inline:held-close>",cwd.clone().into(),Default::default());
            extension.handlers.insert(maho_ext_api::EventKind::SessionShutdown,vec![Arc::new(move |_,_|{
                if let Some(started)=started_tx.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(){let _=started.send(());}
                let receiver=release_rx.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                Box::pin(async move{if let Some(receiver)=receiver{let _=receiver.await;}Ok(maho_ext_api::EventResult::None)})
            })]);
            struct NoopUi;
            impl ExtensionUi for NoopUi{
                fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
                fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool>{Box::pin(async{false})}
                fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
                fn notify(&self,_:&str,_:NotificationType){}
                fn set_status(&self,_:&str,_:Option<&str>){}
                fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions){}
                fn set_header(&self,_:Option<ComponentFactory>){}
                fn set_footer(&self,_:Option<ComponentFactory>){}
                fn set_title(&self,_:&str){}
                fn paste_to_editor(&self,_:&str){}
                fn set_editor_text(&self,_:&str){}
                fn get_editor_text(&self)->String{String::new()}
                fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue>{Box::pin(async{Ok(JsonValue::Null)})}
                fn theme(&self)->Theme{Theme::default()}
            }
            session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![extension],Default::default(),Default::default(),session.extension_context(Arc::new(NoopUi)))).await;
            let models=session.model_registry().clone();let services=maho_core::agent_session_services::AgentSessionServices{cwd:cwd.clone(),agent_dir:cwd.clone(),auth_storage:Arc::clone(&models.auth_storage),model_registry:models,settings_manager:SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()),false),diagnostics:vec![]};
            Ok(AgentSessionRuntime::new(session,services,vec![],None,None))
        }) as std::pin::Pin<Box<dyn std::future::Future<Output=Result<maho_core::agent_session_runtime::AgentSessionRuntime,String>>+Send>>
    });
    let(writer,reader)=tokio::io::duplex(64*1024);let core=Arc::new(HostCore::new(HostCoreOptions{agent_dir:temp.path().to_path_buf(),cwd:cwd.clone(),create_runtime,capabilities:Vec::new(),close_grace_ms:0},Arc::new(SessionWriterActor::new(writer))));
    core.handle(None,&serde_json::json!({"type":"open_session","cwd":cwd,"retainOnDisconnect":false}).to_string()).await.unwrap();
    let mut reader=tokio::io::BufReader::new(reader);let mut line=String::new();reader.read_line(&mut line).await.unwrap();
    let handle=core.registry.lock().await.list_sessions(true)[0]["sessionId"].as_str().unwrap().to_owned();
    let command=serde_json::json!({"type":"close_session","sessionId":handle}).to_string();
    let mut close=Box::pin(core.handle(None,&command));
    tokio::select!{biased;result=&mut close=>panic!("close acknowledged before disposal release: {result:?}"),result=started_rx=>result.unwrap()};
    assert!(futures::poll!(&mut close).is_pending(),"close acknowledged before disposal release");
    let _=release_tx.send(());close.await.unwrap();
    line.clear();reader.read_line(&mut line).await.unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap()["command"],"close_session");
}

