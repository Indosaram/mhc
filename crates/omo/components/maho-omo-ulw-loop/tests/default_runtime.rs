use crate::support;

use maho_ext_api::*;
use maho_omo_ulw_loop::index::UlwLoopComponent;

#[tokio::test]
async fn default_registration_executes_status_in_process() -> Result<(), Box<dyn std::error::Error>>
{
    let root = tempfile::tempdir()?;
    let plan_dir = root.path().join(".omo/ulw-loop/session");
    std::fs::create_dir_all(&plan_dir)?;
    std::fs::write(
        plan_dir.join("goals.json"),
        r#"{"version":1,"revision":1,"goals":[{"id":"G001","title":"G1","objective":"O1","status":"pending","attempt":0,"createdAt":"","updatedAt":"","successCriteria":[]}],"goalsPath":".omo/ulw-loop/goals.json","ledgerPath":".omo/ulw-loop/ledger.jsonl","briefPath":".omo/ulw-loop/brief.md","createdAt":"","updatedAt":""}"#,
    )?;
    let component = UlwLoopComponent::default();
    let mut api = ExtensionApi::new(
        LoadedExtension::new("loop", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    component.register(&mut api);
    let mut ctx = support::context();
    ctx.cwd = root.path().into();
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent {
        reason: SessionReason::Startup,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    let res = api.registered.handlers[&EventKind::SessionStart][0](&mut event, &ctx).await?;
    assert!(matches!(res, EventResult::None));
    let mut input = ExtensionEvent::Input(InputEvent {
        input_id: "default-native".into(),
        text: "continue".into(),
        images: None,
        source: InputSource::Interactive,
        streaming_behavior: Some(StreamingBehavior::Steer),
    });
    let result = api.registered.handlers[&EventKind::Input][0](&mut input, &ctx).await?;
    assert!(matches!(
        result,
        EventResult::Input(InputEventResult::Transform { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn missing_plan_registration_handles_status_cleanly() -> Result<(), Box<dyn std::error::Error>>
{
    let root = tempfile::tempdir()?;
    let component = UlwLoopComponent::default();
    let mut api = ExtensionApi::new(
        LoadedExtension::new("loop", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    component.register(&mut api);
    let mut ctx = support::context();
    ctx.cwd = root.path().into();
    let mut event = ExtensionEvent::AgentEnd {
        messages: vec![],
        aborted: Some(false),
        abort_source: None,
        will_retry: Some(false),
    };
    let res = api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &ctx).await?;
    assert!(matches!(res, EventResult::None));
    Ok(())
}
