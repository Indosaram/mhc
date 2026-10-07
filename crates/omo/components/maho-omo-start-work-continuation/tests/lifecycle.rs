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
/// Records the injections offered to the idle-edge queue and the flush requests the consumer makes.
/// `accepted=false` models a coordinator that retired with the session (upstream `enqueue` -> false).
#[derive(Default)]
struct Coordinator { accepted: bool, injections: Mutex<Vec<IdleInjection>>, flushes: Mutex<usize> }
impl IdleInjectionCoordinator for Coordinator {
 fn enqueue(&self, injection: IdleInjection) -> bool { self.injections.lock().expect("injections").push(injection); self.accepted }
 fn schedule_flush(&self) { *self.flushes.lock().expect("flushes") += 1; }
 fn flush_soon(&self) {}
 fn flush_on_idle(&self) -> usize { 0 }
 fn pending_count(&self) -> usize { self.injections.lock().expect("injections").len() }
 fn remove(&self, _key: &str) -> bool { false }
}
/// Records the structured warn lines the consumer emits on a refused injection.
#[derive(Default)]
struct Logger { warns: Mutex<Vec<String>> }
impl ComponentLogger for Logger {
 fn info(&self, _message: &str, _details: Option<&JsonValue>) {}
 fn warn(&self, message: &str, _details: Option<&JsonValue>) { self.warns.lock().expect("warns").push(message.to_owned()); }
 fn error(&self, _message: &str, _details: Option<&JsonValue>) {}
}
#[tokio::test] async fn identical_signature_is_suppressed_and_user_input_resets()->Result<(),Box<dyn std::error::Error>> {
 let root=tempfile::tempdir()?;let plan=root.path().join("plan.md");std::fs::write(&plan,"## TODOs\n- [ ] 1. task\n")?;let state=boulder_state::create_boulder_state(&plan.to_string_lossy(),"senpi:session",&boulder_state::WorkOwner::default());boulder_state::write_boulder_state(root.path(),&state)?;
 let actions=Arc::new(Actions::default());let runtime=ExtensionRuntime::default();runtime.bind(actions.clone());let mut api=ExtensionApi::new(LoadedExtension::new("continuation",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);maho_omo_start_work_continuation::index::StartWorkContinuationComponent::default().register(&mut api);let mut ctx=support::context();ctx.cwd=root.path().into();
 // Turn 1 delivers; a second settle with the same signature is skipped outright (latest lastSignature).
 end(&api,&ctx).await?;assert_eq!(actions.0.lock().expect("messages").len(),1);
 end(&api,&ctx).await?;assert_eq!(actions.0.lock().expect("messages").len(),1);
 // A queued user steer resets the state and appends the steering reminder.
 let mut event=ExtensionEvent::Input(InputEvent{input_id:"id".into(),text:"steer".into(),images:None,source:InputSource::Interactive,streaming_behavior:Some(StreamingBehavior::Steer)});assert!(matches!(api.registered.handlers[&EventKind::Input][0](&mut event,&ctx).await?,EventResult::Input(InputEventResult::Transform{..})));
 end(&api,&ctx).await?;assert_eq!(actions.0.lock().expect("messages").len(),2);Ok(())
}

fn register(root: &std::path::Path, limit: usize) -> (ExtensionApi, Arc<Actions>, ExtensionContext) {
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("continuation", root.into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_start_work_continuation::index::StartWorkContinuationComponent { continuation_limit: limit }
        .register(&mut api);
    let mut ctx = support::context();
    ctx.cwd = root.into();
    (api, actions, ctx)
}

async fn end(api: &ExtensionApi, ctx: &ExtensionContext) -> Result<(), ExtensionFailure> {
    let mut event = clean_agent_end();
    api.registered.handlers[&EventKind::AgentEnd][0](&mut event, ctx).await?;
    let mut settled = ExtensionEvent::AgentSettled;
    api.registered.handlers[&EventKind::AgentSettled][0](&mut settled, ctx).await?;
    Ok(())
}

fn clean_agent_end() -> ExtensionEvent {
    ExtensionEvent::AgentEnd { messages: clean_messages(), aborted: Some(false), abort_source: None, will_retry: Some(false) }
}

fn clean_messages() -> Vec<AgentMessage> {
    serde_json::from_value(serde_json::json!([{
        "role": "assistant", "content": [], "api": "test", "provider": "test", "model": "test",
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 } },
        "stopReason": "stop", "timestamp": 0
    }]))
    .expect("clean assistant message")
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
    let (api, actions, ctx) = register(root.path(), 8);
    end(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn zero_continuation_limit_sends_no_message() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path(), 0);
    end(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn signature_progress_delivers_again() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan = write_work(root.path(), "## TODOs\n- [ ] 1. first\n- [ ] 2. second\n")?;
    let (api, actions, ctx) = register(root.path(), 8);
    end(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
    // Same signature: suppressed.
    end(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
    // A plan revision changes the signature, so the next edge delivers again.
    std::fs::write(plan, "## TODOs\n- [x] 1. first\n- [ ] 2. second\n")?;
    end(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 2);
    Ok(())
}

#[tokio::test]
async fn extension_input_does_not_reset_the_signature() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path(), 8);
    end(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
    // An extension-sourced input is not user input: it neither transforms nor resets the signature.
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
    let (api, actions, ctx) = register(root.path(), 1);
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
    let (api, actions, ctx) = register(root.path(), 8);
    end(&api, &ctx).await?;
    let messages = actions.0.lock().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].custom_type, "omo-senpi:ulw-execute-continuation");
    assert!(!messages[0].display);
    Ok(())
}

#[tokio::test]
async fn corrupt_boulder_json_does_not_fail_registered_handler() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join(".omo"))?;
    std::fs::write(root.path().join(".omo/boulder.json"), "{broken")?;
    let (api, actions, ctx) = register(root.path(), 8);
    end(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn changing_signatures_obey_global_cap_and_user_reset() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan = write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path(), 8);
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

// D3: the producer's `enqueue` now returns whether the coordinator accepted the injection. A refusal
// means the coordinator retired with the session, so the consumer must log and stop rather than
// schedule a flush for an injection that will never be delivered.
#[tokio::test]
async fn retired_coordinator_refuses_the_continuation_without_flushing() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, mut ctx) = register(root.path(), 8);
    let coordinator = Arc::new(Coordinator { accepted: false, ..Coordinator::default() });
    let logger = Arc::new(Logger::default());
    ctx.idle_coordinator = Some(coordinator.clone());
    ctx.logger = Some(logger.clone());
    end(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty(), "a refused injection must not fall back to a direct send");
    assert_eq!(coordinator.injections.lock().expect("injections").len(), 1, "the continuation is offered to the coordinator");
    assert_eq!(*coordinator.flushes.lock().expect("flushes"), 0, "schedule_flush must not run for a refused injection");
    let warns = logger.warns.lock().expect("warns");
    assert_eq!(warns.len(), 1, "the refusal is logged, never dropped in silence");
    assert_eq!(warns[0], "omo-senpi ulw execute continuation skipped: idle-injection coordinator retired");
    Ok(())
}

#[tokio::test]
async fn accepted_coordinator_queues_and_schedules_a_flush() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, mut ctx) = register(root.path(), 8);
    let coordinator = Arc::new(Coordinator { accepted: true, ..Coordinator::default() });
    ctx.idle_coordinator = Some(coordinator.clone());
    end(&api, &ctx).await?;
    assert_eq!(*coordinator.flushes.lock().expect("flushes"), 1, "an accepted injection schedules exactly one flush");
    let injections = coordinator.injections.lock().expect("injections");
    assert_eq!(injections.len(), 1);
    assert_eq!(injections[0].key, "omo-senpi-ulw-execute-continuation");
    assert_eq!(injections[0].source, IdleInjectionSource::BoulderContinuation);
    assert!(actions.0.lock().expect("messages").is_empty(), "the coordinator path never sends directly");
    Ok(())
}
