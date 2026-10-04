mod support;
use std::sync::{Arc,Mutex};
use maho_ext_api::*;
#[derive(Default)] struct Actions(Mutex<Vec<CustomMessage>>);
impl ExtensionActions for Actions {
 fn send_message(&self,m:CustomMessage,o:SendMessageOptions)->Result<(),ExtensionFailure>{assert!(o.trigger_turn);assert_eq!(o.deliver_as,Some(DeliverAs::FollowUp));self.0.lock().expect("messages").push(m);Ok(())}
 fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("unexpected user message")}
 fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure>{Ok(())}
 fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(vec![])}
}
#[tokio::test] async fn signature_retry_budget_and_user_reset()->Result<(),Box<dyn std::error::Error>> {
 let root=tempfile::tempdir()?;let plan=root.path().join("plan.md");std::fs::write(&plan,"## TODOs\n- [ ] 1. task\n")?;let state=boulder_state::create_boulder_state(&plan.to_string_lossy(),"senpi:session",&boulder_state::WorkOwner::default());boulder_state::write_boulder_state(root.path(),&state)?;
 let actions=Arc::new(Actions::default());let runtime=ExtensionRuntime::default();runtime.bind(actions.clone());let mut api=ExtensionApi::new(LoadedExtension::new("continuation",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);maho_omo_start_work_continuation::index::StartWorkContinuationComponent::default().register(&mut api);let mut ctx=support::context();ctx.cwd=root.path().into();
 for _ in 0..3 {let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&ctx).await?;}
 assert_eq!(actions.0.lock().expect("messages").len(),2);
 let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"steer".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&ctx).await?,EventResult::Input(InputEventResult::Transform{..})));
 let mut event=ExtensionEvent::AgentEnd{messages:vec![],aborted:Some(false),abort_source:None,will_retry:Some(false)};api.registered.handlers[&EventKind::AgentEnd][0](&mut event,&ctx).await?;assert_eq!(actions.0.lock().expect("messages").len(),3);Ok(())
}

fn register(root: &std::path::Path, limit: usize, retries: usize) -> (ExtensionApi, Arc<Actions>, ExtensionContext) {
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("continuation", root.into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_start_work_continuation::index::StartWorkContinuationComponent {
        continuation_limit: limit, max_same_signature_retries: retries,
    }.register(&mut api);
    let mut ctx = support::context();
    ctx.cwd = root.into();
    (api, actions, ctx)
}

async fn end(api: &ExtensionApi, ctx: &ExtensionContext) -> Result<(), ExtensionFailure> {
    let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: Some(false),
        abort_source: None, will_retry: Some(false) };
    api.registered.handlers[&EventKind::AgentEnd][0](&mut event, ctx).await?;
    Ok(())
}

fn write_work(root: &std::path::Path, checklist: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let plan = root.join("plan.md");
    std::fs::write(&plan, checklist)?;
    let state = boulder_state::create_boulder_state(&plan.to_string_lossy(), "senpi:session", &boulder_state::WorkOwner::default());
    boulder_state::write_boulder_state(root, &state)?;
    Ok(plan)
}

#[tokio::test]
async fn no_boulder_state_sends_no_message() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let (api, actions, ctx) = register(root.path(), 8, 1);
    end(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn zero_continuation_limit_sends_no_message() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path(), 0, 1);
    end(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn signature_progress_rearms_retry_budget() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan = write_work(root.path(), "## TODOs\n- [ ] 1. first\n- [ ] 2. second\n")?;
    let (api, actions, ctx) = register(root.path(), 8, 1);
    for _ in 0..3 { end(&api, &ctx).await?; }
    assert_eq!(actions.0.lock().expect("messages").len(), 2);
    std::fs::write(plan, "## TODOs\n- [x] 1. first\n- [ ] 2. second\n")?;
    for _ in 0..3 { end(&api, &ctx).await?; }
    assert_eq!(actions.0.lock().expect("messages").len(), 4);
    Ok(())
}

#[tokio::test]
async fn extension_input_does_not_reset_exhausted_budget() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path(), 8, 0);
    end(&api, &ctx).await?;
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "generated".into(),
        images: None, source: InputSource::Extension, streaming_behavior: Some(StreamingBehavior::Steer) });
    assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event, &ctx).await?,
        EventResult::Input(InputEventResult::Continue)));
    end(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
    Ok(())
}

#[tokio::test]
async fn idle_user_input_resets_without_transformation() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path(), 1, 0);
    end(&api, &ctx).await?;
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "user".into(),
        images: None, source: InputSource::Interactive, streaming_behavior: None });
    assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event, &ctx).await?,
        EventResult::Input(InputEventResult::Continue)));
    end(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 2);
    Ok(())
}

#[tokio::test]
async fn continuation_delivery_is_hidden_custom_message() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [x] 1. task\n")?;
    let (api, actions, ctx) = register(root.path(), 8, 1);
    end(&api, &ctx).await?;
    let messages = actions.0.lock().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].custom_type, "omo-senpi:start-work-continuation");
    assert!(!messages[0].display);
    Ok(())
}

#[tokio::test]
async fn corrupt_boulder_json_does_not_fail_registered_handler() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join(".omo"))?;
    std::fs::write(root.path().join(".omo/boulder.json"), "{broken")?;
    let (api, actions, ctx) = register(root.path(), 8, 1);
    end(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn changing_signatures_obey_global_cap_and_user_reset() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan = write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path(), 8, 1);
    for total in 1..=9 {
        std::fs::write(&plan, format!("## TODOs\n{}", "- [ ] 1. task\n".repeat(total)))?;
        end(&api, &ctx).await?;
    }
    assert_eq!(actions.0.lock().expect("messages").len(), 8);
    let mut event = ExtensionEvent::Input(InputEvent { input_id: "cap-reset".into(), text: "continue".into(),
        images: None, source: InputSource::Interactive, streaming_behavior: None });
    api.registered.handlers[&EventKind::Input][0](&mut event, &ctx).await?;
    end(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 9);
    Ok(())
}
