use ulw_loop_core::sdk::*;
use ulw_loop_core::surface::UlwLoopToolkitSurface;

#[test]
fn test_ulw_loop_manifest_operations() {
    assert_eq!(
        ULW_LOOP_OPERATIONS,
        [
            "help",
            "create-goals",
            "status",
            "complete-goals",
            "checkpoint",
            "steer",
            "add-goal",
            "criteria",
            "record-evidence",
            "record-review-blockers",
        ]
    );
    let manifest = ulw_loop_manifest();
    let names: Vec<&str> = manifest.operations.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ULW_LOOP_OPERATIONS);
}

#[test]
fn test_sdk_blank_session_id_fails() {
    let ctx = ToolkitContext {
        cwd: ".".into(),
        session_id: "  ".into(),
        surface: UlwLoopToolkitSurface::LazyCodex,
    };
    assert!(AgentToolkit::new(ctx, None).is_err());
}

#[test]
fn test_sdk_help() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let ctx = ToolkitContext {
        cwd: tmp.path().to_string_lossy().into(),
        session_id: "sess-help".into(),
        surface: UlwLoopToolkitSurface::LazyCodex,
    };
    let toolkit = AgentToolkit::new(ctx, None)?;
    let resp = toolkit.help();
    assert!(matches!(resp, ToolkitResponse::Success { .. }));
    Ok(())
}

#[test]
fn test_sdk_create_goals_and_status() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let ctx = ToolkitContext {
        cwd: tmp.path().to_string_lossy().into(),
        session_id: "sess-create".into(),
        surface: UlwLoopToolkitSurface::LazyCodex,
    };
    let toolkit = AgentToolkit::new(ctx, None)?;
    let resp = toolkit.create_goals(&ulw_loop_core::command_types::CreateGoalsOptions {
        brief: "- alpha goal\n- beta goal".into(),
        goals: None,
        codex_goal_mode: None,
        force: None,
        validation_batches_json: None,
    });
    assert!(matches!(resp, ToolkitResponse::Success { .. }));

    let st = toolkit.status();
    assert!(matches!(st, ToolkitResponse::Success { .. }));
    if let ToolkitResponse::Success { result, .. } = st {
        assert_eq!(result.plan.goals.len(), 2);
    }
    Ok(())
}
