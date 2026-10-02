mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
#[tokio::test] async fn spawn_failure_does_not_transform_or_inject() {let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/nonexistent-maho-test-toolkit".into()),js_runtime:"bun".into()}.register(&mut api);let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::Input(InputEventResult::Continue)));let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::None));}
#[derive(Default)] struct Actions(Mutex<Vec<(CustomMessage,SendMessageOptions)>>);
impl ExtensionActions for Actions {
 fn send_message(&self,m:CustomMessage,o:SendMessageOptions)->Result<(),ExtensionFailure>{self.0.lock().expect("messages").push((m,o));Ok(())}
 fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("unexpected user message")}
 fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure>{Ok(())}
 fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![])}
}
#[tokio::test] async fn active_run_continues_once_per_status()->Result<(),Box<dyn std::error::Error>> {
 use std::os::unix::fs::PermissionsExt;
 let root=tempfile::tempdir()?;let binary=root.path().join("toolkit");std::fs::write(&binary,"#!/bin/sh\nprintf '%s' '{\"ok\":true,\"plan\":{\"goals\":[{\"status\":\"pending\"}]}}'\n")?;std::fs::set_permissions(&binary,std::fs::Permissions::from_mode(0o700))?;
 let actions=Arc::new(Actions::default());let runtime=ExtensionRuntime::default();runtime.bind(actions.clone());let mut api=ExtensionApi::new(LoadedExtension::new("loop",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
 maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some(binary.to_string_lossy().into_owned()),js_runtime:"bun".into()}.register(&mut api);let mut ctx=support::context();ctx.cwd=root.path().into();
 for _ in 0..2 {let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&ctx).await?;}
 let messages=actions.0.lock().expect("messages");assert_eq!(messages.len(),1);assert!(!messages[0].0.display);assert!(messages[0].1.trigger_turn);assert_eq!(messages[0].1.deliver_as,Some(DeliverAs::FollowUp));Ok(())
}
#[tokio::test] async fn missing_binary_registers_inert_handlers() {let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_ulw_loop::index::UlwLoopComponent{bin:None,js_runtime:"bun".into()}.register(&mut api);let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::None));}
