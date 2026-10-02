mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
#[tokio::test] async fn queued_user_transform_preserves_images_and_extension_is_silent() {
 let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
 maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/toolkit".into()),js_runtime:"bun".into(),run_command:Some(Arc::new(|_,_,_|Box::pin(async {Ok(maho_omo_ulw_loop::omo_command::CommandResult{code:0,stdout:r#"{"ok":true,"plan":{"goals":[{"status":"pending"}]}}"#.into()})})))}.register(&mut api);
 for(source,expected)in[(InputSource::Interactive,true),(InputSource::Extension,false)] {let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:Some(vec![]),source,streaming_behavior:Some(StreamingBehavior::Steer)});let result=api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch");if expected {let EventResult::Input(InputEventResult::Transform{text,images})=result else{panic!("transform");};assert!(text.starts_with("continue\n\n"));assert_eq!(images,Some(vec![]));}else{assert!(matches!(result,EventResult::Input(InputEventResult::Continue)));}}
}
#[tokio::test] async fn rejected_command_and_malformed_status_pass_through() {for output in [None,Some("not json"),Some(r#"{"ok":false}"#)] {let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/toolkit".into()),js_runtime:"bun".into(),run_command:Some(Arc::new(move |_,_,_|Box::pin(async move {output.map_or_else(||Err(std::io::Error::other("spawn EINVAL")),|stdout|Ok(maho_omo_ulw_loop::omo_command::CommandResult{code:0,stdout:stdout.into()}))})))}.register(&mut api);let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::Input(InputEventResult::Continue)));}}
#[tokio::test] async fn changing_status_is_capped_and_user_input_resets()->Result<(),Box<dyn std::error::Error>> {
 use std::sync::atomic::{AtomicUsize,Ordering};
 let count=Arc::new(AtomicUsize::new(0));let observed=count.clone();let actions=Arc::new(Actions::default());let runtime=ExtensionRuntime::default();runtime.bind(actions.clone());let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
 maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/toolkit".into()),js_runtime:"bun".into(),run_command:Some(Arc::new(move |_,_,_|{let n=observed.fetch_add(1,Ordering::SeqCst);Box::pin(async move {Ok(maho_omo_ulw_loop::omo_command::CommandResult{code:0,stdout:serde_json::json!({"ok":true,"revision":n,"plan":{"goals":[{"status":"pending"}]}}).to_string()})})}))}.register(&mut api);
 for _ in 0..10 {let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await?;}
 assert_eq!(actions.0.lock().expect("messages").len(),8);assert_eq!(count.load(Ordering::SeqCst),8);
 let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:None,source:InputSource::Interactive,streaming_behavior:None});api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await?;
 let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await?;assert_eq!(actions.0.lock().expect("messages").len(),9);Ok(())
}
#[tokio::test] async fn spawn_failure_does_not_transform_or_inject() {let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/nonexistent-maho-test-toolkit".into()),js_runtime:"bun".into(),run_command:None}.register(&mut api);let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::Input(InputEventResult::Continue)));let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::None));}
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
 maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some(binary.to_string_lossy().into_owned()),js_runtime:"bun".into(),run_command:None}.register(&mut api);let mut ctx=support::context();ctx.cwd=root.path().into();
 for _ in 0..2 {let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&ctx).await?;}
 let messages=actions.0.lock().expect("messages");assert_eq!(messages.len(),1);assert!(!messages[0].0.display);assert!(messages[0].1.trigger_turn);assert_eq!(messages[0].1.deliver_as,Some(DeliverAs::FollowUp));Ok(())
}
#[tokio::test] async fn missing_binary_registers_inert_handlers() {let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_ulw_loop::index::UlwLoopComponent{bin:None,js_runtime:"bun".into(),run_command:None}.register(&mut api);let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::None));}

fn register_status() -> (ExtensionApi, Arc<Actions>, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = calls.clone();
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(),
        run_command: Some(Arc::new(move |bin, args, cwd| {
            assert_eq!(bin, "/toolkit");
            assert_eq!(args, ["ulw-loop", "status", "--json"]);
            assert_eq!(cwd, std::path::PathBuf::from("/tmp"));
            captured.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(maho_omo_ulw_loop::omo_command::CommandResult {
                code: 0, stdout: r#"{"ok":true,"plan":{"goals":[{"status":"pending"}]}}"#.into()
            }) })
        })) }.register(&mut api);
    (api, actions, calls)
}

async fn end(api: &ExtensionApi) {
    let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: Some(false),
        abort_source: None, will_retry: Some(false) };
    api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &support::context()).await.expect("dispatch");
}

#[tokio::test]
async fn idle_user_input_resets_stale_status_without_querying() {
    let (api, actions, calls) = register_status();
    end(&api).await;
    end(&api).await;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "literal prompt".into(),
        images: None, source: InputSource::Interactive, streaming_behavior: None });
    assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event, &support::context()).await.expect("dispatch"),
        EventResult::Input(InputEventResult::Continue)));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    end(&api).await;
    assert_eq!(actions.0.lock().expect("messages").len(), 2);
}

#[tokio::test]
async fn extension_input_preserves_stale_status_guard() {
    let (api, actions, _) = register_status();
    end(&api).await;
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "generated prompt".into(),
        images: None, source: InputSource::Extension, streaming_behavior: Some(StreamingBehavior::FollowUp) });
    assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event, &support::context()).await.expect("dispatch"),
        EventResult::Input(InputEventResult::Continue)));
    end(&api).await;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
}

#[tokio::test]
async fn unrelated_tool_results_do_not_query_loop_status() {
    let (api, _, calls) = register_status();
    let mut event = ExtensionEvent::ToolResult(ToolResultEvent { tool_name: "read".into(),
        tool_call_id: "id".into(), input: serde_json::json!({}), content: Vec::new(), details: None,
        is_error: false, usage: None });
    api.registered.handlers[&EventKind::ToolResult][0](&mut event, &support::context()).await.expect("dispatch");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}
