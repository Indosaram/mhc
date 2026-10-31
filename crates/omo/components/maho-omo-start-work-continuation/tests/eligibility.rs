use boulder_state::BoulderWorkStatus;
use maho_omo_start_work_continuation::boulder_eligibility::find_continuable_boulder_work_in;

// A work with no recorded timestamps and no transcripts is stale (activity decides).
fn write_stale_state(
    root: &std::path::Path,
    session: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let boulder = root.join(".omo");
    std::fs::create_dir_all(&boulder)?;
    let state = serde_json::json!({
        "schema_version": 2,
        "active_work_id": "stale-work",
        "works": { "stale-work": {
            "work_id": "stale-work",
            "active_plan": "plan.md",
            "plan_name": "plan",
            "session_ids": [session],
            "status": "active",
        }},
        "active_plan": "plan.md",
        "plan_name": "plan",
        "status": "active",
        "session_ids": [session],
    });
    std::fs::write(boulder.join("boulder.json"), state.to_string())?;
    Ok(())
}

fn read_state(root: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(root.join(".omo/boulder.json")).expect("state"))
        .expect("json")
}

#[test]
fn missing_state_ineligible() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    assert!(find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.is_none());
    Ok(())
}

#[test]
fn matching_session_owns_plan() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    let plan = root.path().join("plan.md");
    std::fs::write(&plan, "## TODOs\n- [ ] 1. first\n- [x] 2. second\n")?;
    let state = boulder_state::create_boulder_state(
        &plan.to_string_lossy(),
        "senpi:session",
        &boulder_state::WorkOwner::default(),
    );
    boulder_state::write_boulder_state(root.path(), &state)?;
    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?
        .expect("owned work");
    assert_eq!(work.checklist.total, 2);
    assert_eq!(work.checklist.remaining, 1);
    assert!(find_continuable_boulder_work_in(root.path(), "other", sessions.path())?.is_none());
    Ok(())
}

#[test]
fn empty_checklist_ineligible() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    let plan = root.path().join("plan.md");
    std::fs::write(&plan, "## TODOs\n")?;
    let state = boulder_state::create_boulder_state(
        &plan.to_string_lossy(),
        "senpi:session",
        &boulder_state::WorkOwner::default(),
    );
    boulder_state::write_boulder_state(root.path(), &state)?;
    assert!(find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.is_none());
    Ok(())
}

#[test]
fn work_status_and_harness_ownership_gate_continuation() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    let plan = root.path().join("plan.md");
    std::fs::write(&plan, "## TODOs\n- [ ] 1. task\n")?;
    for (status, session, expected) in [
        ("active", "senpi:session", true),
        ("paused", "senpi:session", true),
        ("completed", "senpi:session", false),
        ("abandoned", "senpi:session", false),
        ("active", "codex:session", false),
    ] {
        let mut doc = boulder_state::create_boulder_state(
            &plan.to_string_lossy(),
            session,
            &boulder_state::WorkOwner::default(),
        )
        .to_json_value();
        doc["status"] = serde_json::json!(status);
        for work in doc["works"].as_object_mut().expect("works").values_mut() {
            work["status"] = serde_json::json!(status);
        }
        let state = boulder_state::BoulderState::from_json_value(doc).expect("state object");
        boulder_state::write_boulder_state(root.path(), &state)?;
        assert_eq!(
            find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.is_some(),
            expected
        );
    }
    Ok(())
}

#[test]
fn consumer_reconciles_a_stale_work_before_reading_it() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("plan.md"), "## TODOs\n- [ ] 1. task\n")?;
    write_stale_state(root.path(), "senpi:session")?;
    let sessions = tempfile::tempdir()?;

    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?
        .expect("continuable");

    assert_eq!(work.work.status(), Some(BoulderWorkStatus::Paused));
    let state = read_state(root.path());
    assert_eq!(
        state["works"]["stale-work"]["status"],
        serde_json::json!("paused")
    );
    assert!(state["works"]["stale-work"]["stale_since"].is_string());
    Ok(())
}

#[test]
fn consumer_leaves_a_fresh_work_active() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    let plan = root.path().join("plan.md");
    std::fs::write(&plan, "## TODOs\n- [ ] 1. task\n")?;
    let state = boulder_state::create_boulder_state(
        &plan.to_string_lossy(),
        "senpi:session",
        &boulder_state::WorkOwner::default(),
    );
    boulder_state::write_boulder_state(root.path(), &state)?;

    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?
        .expect("continuable");

    assert_eq!(work.work.status(), Some(BoulderWorkStatus::Active));
    assert!(
        read_state(root.path())["works"]
            .as_object()
            .expect("works")
            .values()
            .all(|work| work["status"] == serde_json::json!("active"))
    );
    Ok(())
}

#[test]
fn consumer_demotes_but_does_not_continue_a_stale_work_without_a_plan()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_stale_state(root.path(), "senpi:session")?;
    let sessions = tempfile::tempdir()?;

    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?;

    assert!(work.is_none());
    assert_eq!(
        read_state(root.path())["works"]["stale-work"]["status"],
        serde_json::json!("paused")
    );
    Ok(())
}

fn write_work_with(
    root: &std::path::Path,
    status: &str,
    session: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let plan = root.join("plan.md");
    std::fs::write(&plan, "## TODOs\n- [ ] 1. task\n")?;
    let mut doc = boulder_state::create_boulder_state(
        &plan.to_string_lossy(),
        session,
        &boulder_state::WorkOwner::default(),
    )
    .to_json_value();
    doc["status"] = serde_json::json!(status);
    for work in doc["works"].as_object_mut().expect("works").values_mut() {
        work["status"] = serde_json::json!(status);
    }
    let state = boulder_state::BoulderState::from_json_value(doc).expect("state object");
    boulder_state::write_boulder_state(root, &state)?;
    Ok(())
}

// Pin index.test.ts: "#given completed work #when agent_end fires #then no injection".
#[test]
fn completed_work_is_not_continuable() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    write_work_with(root.path(), "completed", "senpi:session")?;
    assert!(find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.is_none());
    Ok(())
}

// Pin index.test.ts: "#given work owned by another harness #when agent_end fires #then no injection".
#[test]
fn work_owned_by_another_harness_is_not_continuable() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    write_work_with(root.path(), "active", "codex:session")?;
    assert!(find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.is_none());
    Ok(())
}

// Pin index.test.ts: "#given paused work #when agent_end fires #then injects continuation directive".
#[test]
fn paused_work_is_continuable() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    write_work_with(root.path(), "paused", "senpi:session")?;
    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?
        .expect("paused work continues");
    assert_eq!(work.checklist.remaining, 1);
    Ok(())
}

struct NoSession;
impl maho_ext_api::ToolSessionManager for NoSession {
    fn session_id(&self) -> &str {
        ""
    }
    fn session_file(&self) -> Option<&std::path::Path> {
        None
    }
}
impl maho_ext_api::SessionManager for NoSession {
    fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

#[derive(Default)]
struct RecordingActions(std::sync::Mutex<Vec<maho_ext_api::CustomMessage>>);
impl maho_ext_api::ExtensionActions for RecordingActions {
    fn send_message(
        &self,
        message: maho_ext_api::CustomMessage,
        _: maho_ext_api::SendMessageOptions,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        self.0.lock().expect("messages").push(message);
        Ok(())
    }
    fn send_user_message(
        &self,
        _: maho_ext_api::UserMessageContent,
        _: maho_ext_api::SendUserMessageOptions,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        panic!("unexpected user message")
    }
    fn append_entry(
        &self,
        _: &str,
        _: Option<maho_ext_api::JsonValue>,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, maho_ext_api::ExtensionFailure> {
        Ok(vec![])
    }
}

// Pin index.test.ts: "#given missing session manager #when agent_end fires #then skips silently".
// The native context always carries a session manager; an empty session id is its absent identity.
#[tokio::test]
async fn missing_session_identity_skips_silently() -> Result<(), Box<dyn std::error::Error>> {
    use maho_ext_api::*;
    let root = tempfile::tempdir()?;
    write_work_with(root.path(), "active", "senpi:session")?;
    let actions = std::sync::Arc::new(RecordingActions::default());
    let runtime = ExtensionRuntime::default();
    runtime.bind(actions.clone());
    let mut api = ExtensionApi::new(
        LoadedExtension::new("continuation", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        runtime,
    );
    maho_omo_start_work_continuation::index::StartWorkContinuationComponent::default()
        .register(&mut api);
    let mut ctx = crate::support::context();
    ctx.cwd = root.path().into();
    ctx.session_manager = std::sync::Arc::new(NoSession);
    let messages: Vec<AgentMessage> = serde_json::from_value(serde_json::json!([{
        "role": "assistant", "content": [], "api": "test", "provider": "test", "model": "test",
        "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 } },
        "stopReason": "stop", "timestamp": 0
    }]))?;
    let mut end = ExtensionEvent::AgentEnd {
        messages,
        aborted: Some(false),
        abort_source: None,
        will_retry: Some(false),
    };
    api.registered.handlers[&EventKind::AgentEnd][0](&mut end, &ctx).await?;
    let mut settled = ExtensionEvent::AgentSettled;
    api.registered.handlers[&EventKind::AgentSettled][0](&mut settled, &ctx).await?;
    assert!(actions.0.lock().expect("messages").is_empty());
    Ok(())
}
