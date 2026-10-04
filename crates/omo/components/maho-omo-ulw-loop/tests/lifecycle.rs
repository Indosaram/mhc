mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
#[tokio::test] async fn queued_user_transform_preserves_images_and_extension_is_silent() {
 let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
 maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/toolkit".into()),js_runtime:"bun".into(),logger:None,run_command:Some(Arc::new(|_,_,_|Box::pin(async {Ok(maho_omo_ulw_loop::omo_command::CommandResult{code:0,stdout:r#"{"ok":true,"plan":{"goals":[{"status":"pending"}]}}"#.into()})})))}.register(&mut api);
 for(source,expected)in[(InputSource::Interactive,true),(InputSource::Extension,false)] {let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:Some(vec![]),source,streaming_behavior:Some(StreamingBehavior::Steer)});let result=api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch");if expected {let EventResult::Input(InputEventResult::Transform{text,images})=result else{panic!("transform");};assert!(text.starts_with("continue\n\n"));assert_eq!(images,Some(vec![]));}else{assert!(matches!(result,EventResult::Input(InputEventResult::Continue)));}}
}
#[tokio::test] async fn rejected_command_and_malformed_status_pass_through() {for output in [None,Some("not json"),Some(r#"{"ok":false}"#)] {let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/toolkit".into()),js_runtime:"bun".into(),logger:None,run_command:Some(Arc::new(move |_,_,_|Box::pin(async move {output.map_or_else(||Err(std::io::Error::other("spawn EINVAL")),|stdout|Ok(maho_omo_ulw_loop::omo_command::CommandResult{code:0,stdout:stdout.into()}))})))}.register(&mut api);let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::Input(InputEventResult::Continue)));}}
#[tokio::test] async fn changing_status_is_capped_and_user_input_resets()->Result<(),Box<dyn std::error::Error>> {
 use std::sync::atomic::{AtomicUsize,Ordering};
 let count=Arc::new(AtomicUsize::new(0));let observed=count.clone();let actions=Arc::new(Actions::default());let runtime=ExtensionRuntime::default();runtime.bind(actions.clone());let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
 maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/toolkit".into()),js_runtime:"bun".into(),logger:None,run_command:Some(Arc::new(move |_,_,_|{let n=observed.fetch_add(1,Ordering::SeqCst);Box::pin(async move {Ok(maho_omo_ulw_loop::omo_command::CommandResult{code:0,stdout:serde_json::json!({"ok":true,"revision":n,"plan":{"goals":[{"status":"pending"}]}}).to_string()})})}))}.register(&mut api);
 for _ in 0..10 {let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await?;}
 assert_eq!(actions.0.lock().expect("messages").len(),8);assert_eq!(count.load(Ordering::SeqCst),8);
 let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:None,source:InputSource::Interactive,streaming_behavior:None});api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await?;
 let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await?;assert_eq!(actions.0.lock().expect("messages").len(),9);Ok(())
}
#[tokio::test] async fn spawn_failure_does_not_transform_or_inject() {let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some("/nonexistent-maho-test-toolkit".into()),js_runtime:"bun".into(),logger:None,run_command:None}.register(&mut api);let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"continue".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::Input(InputEventResult::Continue)));let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::None));}
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
 maho_omo_ulw_loop::index::UlwLoopComponent{bin:Some(binary.to_string_lossy().into_owned()),js_runtime:"bun".into(),logger:None,run_command:None}.register(&mut api);let mut ctx=support::context();ctx.cwd=root.path().into();
 for _ in 0..2 {let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&ctx).await?;}
 let messages=actions.0.lock().expect("messages");assert_eq!(messages.len(),1);assert!(!messages[0].0.display);assert!(messages[0].1.trigger_turn);assert_eq!(messages[0].1.deliver_as,Some(DeliverAs::FollowUp));Ok(())
}
#[tokio::test] async fn missing_binary_registers_inert_handlers() {let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_omo_ulw_loop::index::UlwLoopComponent{bin:None,js_runtime:"bun".into(),logger:None,run_command:None}.register(&mut api);let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&support::context()).await.expect("dispatch"),EventResult::None));}

fn register_status() -> (ExtensionApi, Arc<Actions>, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = calls.clone();
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None,
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

#[tokio::test]
async fn rejected_status_keeps_previous_snapshot_guard() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None,
        run_command: Some(Arc::new(move |_, _, _| {
            let call = captured.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if call == 1 { return Err(std::io::Error::other("rejected status")); }
                Ok(maho_omo_ulw_loop::omo_command::CommandResult { code: 0,
                    stdout: r#"{"ok":true,"plan":{"goals":[{"status":"pending"}]}}"#.into() })
            })
        })) }.register(&mut api);
    for _ in 0..3 { end(&api).await; }
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
}

#[tokio::test]
async fn completed_status_never_delivers_continuation() {
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None,
        run_command: Some(Arc::new(|_, _, _| Box::pin(async {
            Ok(maho_omo_ulw_loop::omo_command::CommandResult { code: 0,
                stdout: r#"{"ok":true,"plan":{"aggregateCompletion":{"status":"complete"},"goals":[{"status":"pending"}]}}"#.into() })
        }))) }.register(&mut api);
    end(&api).await;
    assert!(actions.0.lock().expect("messages").is_empty());
}

#[tokio::test]
async fn relevant_tool_results_refresh_status_at_session_cwd() {
    let (api, _, calls) = register_status();
    for name in ["create_goal", "update_goal", "bash", "interactive_bash"] {
        let mut event = ExtensionEvent::ToolResult(ToolResultEvent { tool_name: name.into(),
            tool_call_id: "id".into(), input: serde_json::json!({}), content: Vec::new(), details: None,
            is_error: false, usage: None });
        api.registered.handlers[&EventKind::ToolResult][0](&mut event, &support::context()).await.expect("dispatch");
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4);
}

#[tokio::test]
async fn active_boulder_owner_defers_loop_without_querying_status() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir_all(root.path().join(".omo/plans"))?;
    std::fs::write(root.path().join(".omo/plans/plan.md"), "## TODOs\n- [ ] 1. task\n")?;
    std::fs::write(root.path().join(".omo/boulder.json"), serde_json::json!({
        "schema_version":2,"active_work_id":"work","works":{"work":{
            "work_id":"work","active_plan":".omo/plans/plan.md","plan_name":"plan",
            "session_ids":["senpi:session"],"status":"active","started_at":"2026-07-17T00:00:00Z"
        }}
    }).to_string())?;
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None,
        run_command: Some(Arc::new(|_, _, _| panic!("boulder precedence must bypass status"))) }.register(&mut api);
    let mut ctx = support::context();
    ctx.cwd = root.path().into();
    let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: Some(false), abort_source: None, will_retry: Some(false) };
    assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &ctx).await?, EventResult::None));
    Ok(())
}

#[tokio::test]
async fn default_discovery_executes_path_toolkit_for_queued_input() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir()?;
    let binary = root.path().join("omo-agent-toolkit");
    std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' \"$@\" > argv.txt\nprintf '%s' '{\"ok\":true,\"plan\":{\"goals\":[{\"status\":\"pending\"}]}}'\n")?;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    let env = std::collections::BTreeMap::from([("PATH".into(), root.path().to_string_lossy().into_owned())]);
    let component = maho_omo_ulw_loop::index::UlwLoopComponent::from_env(&env);
    assert_eq!(component.bin.as_deref(), binary.to_str());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    component.register(&mut api);
    let mut ctx = support::context(); ctx.cwd = root.path().into();
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "continue".into(),
        images: None, source: InputSource::Interactive, streaming_behavior: Some(StreamingBehavior::Steer) });
    assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event, &ctx).await?,
        EventResult::Input(InputEventResult::Transform { .. })));
    assert_eq!(std::fs::read_to_string(root.path().join("argv.txt"))?, "ulw-loop\nstatus\n--json\n");
    Ok(())
}

#[tokio::test]
async fn default_discovery_does_not_execute_stale_bare_omo() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir()?;
    let binary = root.path().join("omo");
    std::fs::write(&binary, "#!/bin/sh\ntouch invoked\n")?;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    let env = std::collections::BTreeMap::from([("PATH".into(), root.path().to_string_lossy().into_owned())]);
    let component = maho_omo_ulw_loop::index::UlwLoopComponent::from_env(&env);
    assert!(component.bin.is_none());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    component.register(&mut api);
    let mut ctx = support::context(); ctx.cwd = root.path().into();
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "continue".into(),
        images: None, source: InputSource::Interactive, streaming_behavior: Some(StreamingBehavior::Steer) });
    assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event, &ctx).await?,
        EventResult::Input(InputEventResult::Continue)));
    assert!(!root.path().join("invoked").exists());
    Ok(())
}

#[derive(Default)]
struct RecordingCoordinator {
    enqueued: Mutex<Vec<IdleInjection>>,
    scheduled: std::sync::atomic::AtomicUsize,
}
impl IdleInjectionCoordinator for RecordingCoordinator {
    fn enqueue(&self, injection: IdleInjection) { self.enqueued.lock().expect("enqueued").push(injection); }
    fn schedule_flush(&self) { self.scheduled.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }
    fn flush_soon(&self) {}
    fn flush_on_idle(&self) -> usize { 0 }
    fn pending_count(&self) -> usize { self.enqueued.lock().expect("enqueued").len() }
    fn remove(&self, _: &str) -> bool { false }
}

fn active_runner() -> maho_omo_ulw_loop::index::CommandRunner {
    Arc::new(|_, _, _| Box::pin(async { Ok(maho_omo_ulw_loop::omo_command::CommandResult { code: 0, stdout: r#"{"ok":true,"plan":{"goals":[{"status":"pending"}]}}"#.into() }) }))
}

fn coordinator_harness(runner: Option<maho_omo_ulw_loop::index::CommandRunner>) -> (ExtensionApi, Arc<Actions>, Arc<RecordingCoordinator>) {
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(Arc::clone(&actions));
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None, run_command: runner }.register(&mut api);
    (api, actions, Arc::new(RecordingCoordinator::default()))
}

#[tokio::test]
async fn coordinator_routes_the_continuation_instead_of_a_direct_message() {
    let (api, actions, coordinator) = coordinator_harness(Some(active_runner()));
    let mut ctx = support::context();
    ctx.idle_coordinator = Some(Arc::clone(&coordinator) as Arc<dyn IdleInjectionCoordinator>);
    let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: Some(false), abort_source: None, will_retry: Some(false) };
    assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &ctx).await.expect("dispatch"), EventResult::None));
    let enqueued = coordinator.enqueued.lock().expect("enqueued");
    assert_eq!(enqueued.len(), 1);
    assert_eq!(enqueued[0].key, "omo-senpi-ulw-loop-continuation");
    assert_eq!(enqueued[0].source, IdleInjectionSource::UlwContinuation);
    assert_eq!(enqueued[0].custom_type.as_deref(), Some("omo-senpi:ulw-continuation"));
    assert_eq!(enqueued[0].content, maho_omo_ulw_loop::index::CONTINUATION_PROMPT);
    assert_eq!(enqueued[0].display, Some(false));
    drop(enqueued);
    assert_eq!(coordinator.scheduled.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(actions.0.lock().expect("messages").is_empty());
}

#[tokio::test]
async fn queued_completion_and_continuation_share_one_coordinator_queue() {
    let (api, actions, coordinator) = coordinator_harness(Some(active_runner()));
    coordinator.enqueue(IdleInjection { key: "st_done".into(), source: IdleInjectionSource::TaskCompletion, custom_type: Some("senpi-task:completion".into()), content: "task st_done completed".into(), display: Some(false), details: None, on_flushed: None, on_delivery_failed: None });
    let mut ctx = support::context();
    ctx.idle_coordinator = Some(Arc::clone(&coordinator) as Arc<dyn IdleInjectionCoordinator>);
    let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: Some(false), abort_source: None, will_retry: Some(false) };
    assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &ctx).await.expect("dispatch"), EventResult::None));
    let enqueued = coordinator.enqueued.lock().expect("enqueued");
    assert_eq!(enqueued.len(), 2);
    let combined = enqueued.iter().map(|injection| injection.content.as_str()).collect::<Vec<_>>().join("\n\n");
    assert_eq!(combined, "task st_done completed\n\nContinue the active omo-agent-toolkit ulw-loop run.\nRun `omo-agent-toolkit ulw-loop status --json` in this session cwd, inspect the active incomplete goals, and keep working until the run is complete or safely checkpointed.");
    drop(enqueued);
    assert!(actions.0.lock().expect("messages").is_empty());
}

#[tokio::test]
async fn active_boulder_defers_without_enqueuing_a_continuation() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir_all(root.path().join(".omo/plans"))?;
    std::fs::write(root.path().join(".omo/plans/plan.md"), "## TODOs\n- [ ] 1. task\n")?;
    std::fs::write(root.path().join(".omo/boulder.json"), serde_json::json!({
        "schema_version":2,"active_work_id":"work","works":{"work":{
            "work_id":"work","active_plan":".omo/plans/plan.md","plan_name":"plan",
            "session_ids":["senpi:session"],"status":"active","started_at":"2026-07-17T00:00:00Z"
        }}
    }).to_string())?;
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(Arc::clone(&actions));
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", root.path().into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None, run_command: Some(Arc::new(|_, _, _| panic!("boulder precedence must bypass status"))) }.register(&mut api);
    let coordinator = Arc::new(RecordingCoordinator::default());
    let mut ctx = support::context();
    ctx.cwd = root.path().into();
    ctx.idle_coordinator = Some(Arc::clone(&coordinator) as Arc<dyn IdleInjectionCoordinator>);
    let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: Some(false), abort_source: None, will_retry: Some(false) };
    assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &ctx).await?, EventResult::None));
    assert_eq!(coordinator.pending_count(), 0);
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn inactive_registration_logs_through_the_component_logger() {
    let recorder = Arc::new(support::RecordingLogger::default());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: None, js_runtime: "bun".into(), run_command: None,
        logger: Some(Arc::clone(&recorder) as Arc<dyn ComponentLogger>) }.register(&mut api);
    assert_eq!(support::logger_entries(&recorder), vec![("info".to_owned(),
        "omo-senpi ulw-loop inactive; omo binary not found".to_owned(), None)]);
}

#[tokio::test]
async fn malformed_status_warns_through_the_context_logger() {
    let recorder = Arc::new(support::RecordingLogger::default());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None,
        run_command: Some(Arc::new(|_, _, _| Box::pin(async { Ok(maho_omo_ulw_loop::omo_command::CommandResult { code: 0, stdout: "{bad json".into() }) }))) }.register(&mut api);
    let mut ctx = support::context_with_logger(Arc::clone(&recorder));
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "hi".into(), images: None, source: InputSource::Interactive, streaming_behavior: Some(StreamingBehavior::Steer) });
    api.registered.handlers[&EventKind::Input][0](&mut event, &ctx).await.expect("dispatch");
    assert!(support::logger_entries(&recorder).contains(&("warn".to_owned(),
        "omo-senpi ulw-loop status ignored".to_owned(), Some(serde_json::json!({"reason":"malformed-json"})))));
}

#[tokio::test]
async fn stale_status_logs_skipped_through_the_context_logger() {
    let recorder = Arc::new(support::RecordingLogger::default());
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(Arc::clone(&actions));
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None,
        run_command: Some(Arc::new(|_, _, _| Box::pin(async { Ok(maho_omo_ulw_loop::omo_command::CommandResult { code: 0, stdout: r#"{"ok":true,"plan":{"goals":[{"status":"pending"}]}}"#.into() }) }))) }.register(&mut api);
    let mut ctx = support::context_with_logger(Arc::clone(&recorder));
    for _ in 0..2 {
        let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: Some(false), abort_source: None, will_retry: Some(false) };
        api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &ctx).await.expect("dispatch");
    }
    assert!(support::logger_entries(&recorder).contains(&("info".to_owned(),
        "omo-senpi ulw-loop continuation skipped".to_owned(), Some(serde_json::json!({"reason":"stale-status"})))));
}

#[tokio::test]
async fn cap_reached_logs_skipped_through_the_context_logger() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let recorder = Arc::new(support::RecordingLogger::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(Arc::clone(&actions));
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None,
        run_command: Some(Arc::new(move |_, _, _| { let n = observed.fetch_add(1, Ordering::SeqCst); Box::pin(async move {
            Ok(maho_omo_ulw_loop::omo_command::CommandResult { code: 0, stdout: serde_json::json!({"ok":true,"revision":n,"plan":{"goals":[{"status":"pending"}]}}).to_string() }) }) })) }.register(&mut api);
    let mut ctx = support::context_with_logger(Arc::clone(&recorder));
    for _ in 0..9 {
        let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: Some(false), abort_source: None, will_retry: Some(false) };
        api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &ctx).await.expect("dispatch");
    }
    assert!(support::logger_entries(&recorder).contains(&("info".to_owned(),
        "omo-senpi ulw-loop continuation skipped".to_owned(), Some(serde_json::json!({"reason":"continuation-cap-reached","count":8})))));
}

#[tokio::test]
async fn shell_tool_result_activates_the_footer_immediately() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempfile::tempdir()?;
    let goal = root.path().join("goal.json");
    std::fs::write(&goal, r#"{"version":1,"goal":{"status":"active"}}"#)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let ui = Arc::new(support::TestUi::default());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_omo_ulw_loop::index::UlwLoopComponent { bin: Some("/toolkit".into()), js_runtime: "bun".into(), logger: None,
        run_command: Some(Arc::new(move |_, _, _| { let n = observed.fetch_add(1, Ordering::SeqCst); Box::pin(async move {
            Ok(maho_omo_ulw_loop::omo_command::CommandResult { code: 0, stdout: if n == 0 {
                r#"{"ok":true,"plan":{"aggregateCompletion":{"status":"complete"},"goals":[{"status":"pending"}]}}"#.into()
            } else { r#"{"ok":true,"plan":{"goals":[{"status":"pending"}]}}"#.into() } }) }) })) }.register(&mut api);
    let mut ctx = support::context();
    ctx.cwd = root.path().into();
    ctx.has_ui = true;
    ctx.ui = ui.clone();
    ctx.goal_store_file = Some(goal);
    let mut start = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None });
    api.registered.handlers[&EventKind::SessionStart][0](&mut start, &ctx).await?;
    let tool = |name: &str| ExtensionEvent::ToolResult(ToolResultEvent { tool_name: name.into(), tool_call_id: "id".into(), input: serde_json::json!({}), content: Vec::new(), details: None, is_error: false, usage: None });
    let mut read = tool("read");
    api.registered.handlers[&EventKind::ToolResult][0](&mut read, &ctx).await?;
    let mut shell = tool("bash");
    api.registered.handlers[&EventKind::ToolResult][0](&mut shell, &ctx).await?;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let statuses = ui.0.lock().expect("status");
    assert_eq!(statuses.first(), Some(&Some(maho_omo_ulw_loop::footer_status::ULW_LOOP_FOOTER_FRAMES[0].to_string())));
    Ok(())
}
