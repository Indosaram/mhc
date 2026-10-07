//! Port of `terminal-outcome.test.ts` (the `boulder` half): the registered component must not
//! answer a turn the host aborted, is retrying, or refused, and must decide only on `agent_settled`.

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

const CODEX_ERROR: &str = "Codex error: This request was blocked by our safety systems. Reason: Potentially unintended activity.";

fn messages(value: JsonValue) -> Vec<AgentMessage> { serde_json::from_value(value).expect("agent messages") }

fn clean_messages() -> Vec<AgentMessage> {
    messages(serde_json::json!([{ "role": "assistant", "stopReason": "stop", "content": [] }]))
}

fn demotion_diagnostic() -> JsonValue { serde_json::json!({ "type": "empty_tool_use_terminal_state", "timestamp": 0, "details": {} }) }

fn aborted_tool_result() -> JsonValue {
    serde_json::json!({ "role": "toolResult", "toolCallId": "call-1", "toolName": "bash", "isError": true,
        "content": [{ "type": "text", "text": "Command aborted by user" }] })
}

fn register(root: &std::path::Path) -> (ExtensionApi, Arc<Actions>, ExtensionContext) {
    let actions = Arc::new(Actions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(LoadedExtension::new("continuation", root.into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), runtime);
    maho_omo_start_work_continuation::index::StartWorkContinuationComponent::default().register(&mut api);
    let mut ctx = support::context();
    ctx.cwd = root.into();
    (api, actions, ctx)
}

fn write_work(root: &std::path::Path, checklist: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let plan = root.join("plan.md");
    std::fs::write(&plan, checklist)?;
    let state = boulder_state::create_boulder_state(&plan.to_string_lossy(), "senpi:session", &boulder_state::WorkOwner::default());
    boulder_state::write_boulder_state(root, &state)?;
    Ok(plan)
}

fn end_event(messages: Vec<AgentMessage>, aborted: bool, will_retry: bool) -> ExtensionEvent {
    ExtensionEvent::AgentEnd { messages, aborted: Some(aborted), abort_source: None, will_retry: Some(will_retry) }
}

async fn dispatch_agent_end(api: &ExtensionApi, ctx: &ExtensionContext, event: ExtensionEvent) -> Result<(), ExtensionFailure> {
    let mut event = event;
    api.registered.handlers[&EventKind::AgentEnd][0](&mut event, ctx).await?;
    Ok(())
}

async fn settle(api: &ExtensionApi, ctx: &ExtensionContext) -> Result<(), ExtensionFailure> {
    let mut event = ExtensionEvent::AgentSettled;
    api.registered.handlers[&EventKind::AgentSettled][0](&mut event, ctx).await?;
    Ok(())
}

async fn blocked_outcomes() -> Vec<(&'static str, ExtensionEvent)> {
    vec![
        ("aborted run", end_event(clean_messages(), true, false)),
        ("host retry", end_event(clean_messages(), false, true)),
        ("missing outcome", end_event(Vec::new(), false, false)),
        ("refusal", end_event(messages(serde_json::json!([{ "role": "assistant", "stopReason": "toolUse", "content": [], "stopDetails": { "type": "refusal" } }])), false, false)),
        ("unfinished turn", end_event(messages(serde_json::json!([{ "role": "assistant", "stopReason": "error", "errorMessage": CODEX_ERROR, "content": [] }])), false, false)),
        ("aborted tool result", end_event(messages(serde_json::json!([{ "role": "assistant", "stopReason": "stop" }, aborted_tool_result()])), false, false)),
        ("normalized empty-tool-use refusal", end_event(messages(serde_json::json!([{ "role": "assistant", "stopReason": "stop", "content": [], "diagnostics": [demotion_diagnostic()], "stopDetails": { "type": "refusal" } }])), false, false)),
    ]
}

#[tokio::test]
async fn blocked_outcomes_never_inject_and_do_not_consume_the_cap() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan = write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path());
    for (name, event) in blocked_outcomes().await {
        for _ in 0..2 {
            dispatch_agent_end(&api, &ctx, event.clone()).await?;
            settle(&api, &ctx).await?;
        }
        assert!(actions.0.lock().expect("messages").is_empty(), "case: {name}");
    }
    for total in 1..=9 {
        std::fs::write(&plan, format!("## TODOs\n{}", "- [ ] 1. task\n".repeat(total)))?;
        dispatch_agent_end(&api, &ctx, end_event(clean_messages(), false, false)).await?;
        settle(&api, &ctx).await?;
    }
    assert_eq!(actions.0.lock().expect("messages").len(), 8, "the cap is intact after blocked edges");
    Ok(())
}

#[tokio::test]
async fn continuable_outcomes_still_continue() -> Result<(), Box<dyn std::error::Error>> {
    let cases = vec![
        ("explanatory assistant text", serde_json::json!([{ "role": "assistant", "stopReason": "stop", "content": [{ "type": "text", "text": CODEX_ERROR }] }])),
        ("a demoted empty tool-use turn without refusal details", serde_json::json!([{ "role": "assistant", "stopReason": "stop", "content": [], "diagnostics": [demotion_diagnostic()] }])),
        ("a plain stop carrying stale refusal details", serde_json::json!([{ "role": "assistant", "stopReason": "stop", "content": [], "stopDetails": { "type": "refusal" } }])),
        ("a length stop", serde_json::json!([{ "role": "assistant", "stopReason": "length" }])),
    ];
    for (name, value) in cases {
        let root = tempfile::tempdir()?;
        write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
        let (api, actions, ctx) = register(root.path());
        dispatch_agent_end(&api, &ctx, end_event(messages(value), false, false)).await?;
        settle(&api, &ctx).await?;
        assert_eq!(actions.0.lock().expect("messages").len(), 1, "case: {name}");
    }
    Ok(())
}

#[tokio::test]
async fn a_run_the_host_has_not_settled_delivers_nothing_until_it_settles() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path());
    dispatch_agent_end(&api, &ctx, end_event(clean_messages(), false, false)).await?;
    assert!(actions.0.lock().expect("messages").is_empty(), "agent_end alone must not deliver");
    settle(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_settle_with_no_recorded_run_delivers_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path());
    settle(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn user_input_before_the_settle_owns_the_edge() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path());
    dispatch_agent_end(&api, &ctx, end_event(clean_messages(), false, false)).await?;
    let mut input = ExtensionEvent::Input(InputEvent { input_id: "id".into(), text: "stop and do this instead".into(),
        images: None, source: InputSource::Interactive, streaming_behavior: None });
    api.registered.handlers[&EventKind::Input][0](&mut input, &ctx).await?;
    settle(&api, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}

#[tokio::test]
async fn an_already_continued_signature_survives_an_intervening_failure() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plan = write_work(root.path(), "## TODOs\n- [ ] 1. task\n")?;
    let (api, actions, ctx) = register(root.path());
    dispatch_agent_end(&api, &ctx, end_event(clean_messages(), false, false)).await?;
    settle(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
    // a NEW plan revision: an ungated handler would deliver on this edge
    std::fs::write(&plan, "## TODOs\n- [ ] 1. task\n- [ ] 2. task\n")?;
    dispatch_agent_end(&api, &ctx, end_event(messages(serde_json::json!([{ "role": "assistant", "stopReason": "error", "errorMessage": CODEX_ERROR, "content": [] }])), false, false)).await?;
    settle(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 1);
    // the same revision then ends cleanly: its unspent signature still continues
    dispatch_agent_end(&api, &ctx, end_event(clean_messages(), false, false)).await?;
    settle(&api, &ctx).await?;
    assert_eq!(actions.0.lock().expect("messages").len(), 2);
    Ok(())
}
