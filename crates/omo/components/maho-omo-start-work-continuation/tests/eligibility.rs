use boulder_state::BoulderWorkStatus;
use maho_omo_start_work_continuation::boulder_eligibility::find_continuable_boulder_work_in;

// A work with no recorded timestamps and no transcripts is stale (activity decides).
fn write_stale_state(root: &std::path::Path, session: &str) -> Result<(), Box<dyn std::error::Error>> {
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
    let state = boulder_state::create_boulder_state(&plan.to_string_lossy(), "senpi:session", &boulder_state::WorkOwner::default());
    boulder_state::write_boulder_state(root.path(), &state)?;
    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.expect("owned work");
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
    let state = boulder_state::create_boulder_state(&plan.to_string_lossy(), "senpi:session", &boulder_state::WorkOwner::default());
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
    for (status, session, expected) in [("active", "senpi:session", true), ("paused", "senpi:session", true),
        ("completed", "senpi:session", false), ("abandoned", "senpi:session", false), ("active", "codex:session", false)] {
        let mut doc = boulder_state::create_boulder_state(&plan.to_string_lossy(), session, &boulder_state::WorkOwner::default()).to_json_value();
        doc["status"] = serde_json::json!(status);
        for work in doc["works"].as_object_mut().expect("works").values_mut() {
            work["status"] = serde_json::json!(status);
        }
        let state = boulder_state::BoulderState::from_json_value(doc).expect("state object");
        boulder_state::write_boulder_state(root.path(), &state)?;
        assert_eq!(find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.is_some(), expected);
    }
    Ok(())
}

#[test]
fn consumer_reconciles_a_stale_work_before_reading_it() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("plan.md"), "## TODOs\n- [ ] 1. task\n")?;
    write_stale_state(root.path(), "senpi:session")?;
    let sessions = tempfile::tempdir()?;

    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.expect("continuable");

    assert_eq!(work.work.status(), Some(BoulderWorkStatus::Paused));
    let state = read_state(root.path());
    assert_eq!(state["works"]["stale-work"]["status"], serde_json::json!("paused"));
    assert!(state["works"]["stale-work"]["stale_since"].is_string());
    Ok(())
}

#[test]
fn consumer_leaves_a_fresh_work_active() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    let plan = root.path().join("plan.md");
    std::fs::write(&plan, "## TODOs\n- [ ] 1. task\n")?;
    let state = boulder_state::create_boulder_state(&plan.to_string_lossy(), "senpi:session", &boulder_state::WorkOwner::default());
    boulder_state::write_boulder_state(root.path(), &state)?;

    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?.expect("continuable");

    assert_eq!(work.work.status(), Some(BoulderWorkStatus::Active));
    assert!(read_state(root.path())["works"].as_object().expect("works").values().all(|work| work["status"] == serde_json::json!("active")));
    Ok(())
}

#[test]
fn consumer_demotes_but_does_not_continue_a_stale_work_without_a_plan() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    write_stale_state(root.path(), "senpi:session")?;
    let sessions = tempfile::tempdir()?;

    let work = find_continuable_boulder_work_in(root.path(), "session", sessions.path())?;

    assert!(work.is_none());
    assert_eq!(read_state(root.path())["works"]["stale-work"]["status"], serde_json::json!("paused"));
    Ok(())
}
