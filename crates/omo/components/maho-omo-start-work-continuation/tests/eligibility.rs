use maho_omo_start_work_continuation::boulder_eligibility::find_continuable_boulder_work;
#[test] fn missing_state_ineligible()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;assert!(find_continuable_boulder_work(root.path(),"session")?.is_none());Ok(())}
#[test] fn matching_session_owns_plan()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let plan=root.path().join("plan.md");std::fs::write(&plan,"## TODOs\n- [ ] 1. first\n- [x] 2. second\n")?;let state=boulder_state::create_boulder_state(&plan.to_string_lossy(),"senpi:session",&boulder_state::WorkOwner::default());boulder_state::write_boulder_state(root.path(),&state)?;let work=find_continuable_boulder_work(root.path(),"session")?.expect("owned work");assert_eq!(work.checklist.total,2);assert_eq!(work.checklist.remaining,1);assert!(find_continuable_boulder_work(root.path(),"other")?.is_none());Ok(())}
#[test] fn empty_checklist_ineligible()->Result<(),Box<dyn std::error::Error>> {let root=tempfile::tempdir()?;let plan=root.path().join("plan.md");std::fs::write(&plan,"## TODOs\n")?;let state=boulder_state::create_boulder_state(&plan.to_string_lossy(),"senpi:session",&boulder_state::WorkOwner::default());boulder_state::write_boulder_state(root.path(),&state)?;assert!(find_continuable_boulder_work(root.path(),"session")?.is_none());Ok(())}

#[test]
fn work_status_and_harness_ownership_gate_continuation() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
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
        assert_eq!(find_continuable_boulder_work(root.path(), "session")?.is_some(), expected);
    }
    Ok(())
}
