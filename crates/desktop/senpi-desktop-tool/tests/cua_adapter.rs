//! Tests matching upstream `test/cua-adapter.test.ts`.

use std::collections::HashMap;
use maho_tools::definition::AbortSignal;
use maho_tools::definition::ToolCall;
use senpi_desktop_tool::cua_adapter::create_computer_actions_tool;
use senpi_desktop_tool::cua_definition::computer_actions_permission_parser;
use senpi_desktop_tool::tool::ComputerToolDeps;
use serde_json::json;

use crate::fixtures;
use fixtures::{desktop_fixture, methods_of, TestToolContext};

#[tokio::test]
async fn kill_switch_bypass_stop_path_refusal_halts_batch_and_never_dispatches_followup() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_INPUT_ERROR".into(), "StopPathUnavailable".into());
    let fixture = desktop_fixture(None, env);

    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "batch",
        "actions": [
            { "action": "screenshot" },
            { "action": "click", "x": 10, "y": 10 },
            { "action": "keypress", "keys": ["enter"] }
        ]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));

    let val = details.get("value").expect("value");
    let fail_code = val.get("failure").and_then(|f| f.get("code")).or_else(|| val.get("code")).and_then(|v| v.as_str());
    assert_eq!(fail_code, Some("COMPUTER_SUPERVISOR_NOT_LIVE"));

    let text = result.content.iter().filter_map(|c| match c {
        maho_tools::definition::ToolContent::Text { text, .. } => Some(text.as_str()),
        _ => None,
    }).collect::<Vec<_>>().join("
");
    assert!(text.contains("Stop and wait for the user"));

    let methods = methods_of(&fixture.log).await;
    let clicks = methods.iter().filter(|m| m.as_str() == "click").count();
    let keys = methods.iter().filter(|m| m.as_str() == "keyChord").count();
    assert_eq!(clicks, 1);
    assert_eq!(keys, 0);
}

#[tokio::test]
async fn suspended_enforcement_suspended_session_refuses_input_and_stops_batch() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_INPUT_ERROR".into(), "Suspended".into());
    let fixture = desktop_fixture(None, env);

    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "batch",
        "actions": [
            { "action": "type", "text": "hi" },
            { "action": "screenshot" }
        ]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    let val = details.get("value").expect("value");
    let fail_code = val.get("failure").and_then(|f| f.get("code")).or_else(|| val.get("code")).and_then(|v| v.as_str());
    assert_eq!(fail_code, Some("COMPUTER_SUSPENDED"));

    let methods = methods_of(&fixture.log).await;
    let captures = methods.iter().filter(|m| m.as_str() == "capture").count();
    assert_eq!(captures, 0);
}

#[tokio::test]
async fn permission_revoked_os_refusal_maps_to_computer_permission_required() {
    let mut env = HashMap::new();
    env.insert("FAKE_ENGINE_INPUT_ERROR".into(), "PermissionDenied".into());
    let fixture = desktop_fixture(None, env);

    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "keypress",
        "keys": ["cmd", "a"]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));

    let val = details.get("value").expect("value");
    let fail_code = val.get("failure").and_then(|f| f.get("code")).or_else(|| val.get("code")).and_then(|v| v.as_str());
    assert_eq!(fail_code, Some("COMPUTER_PERMISSION_REQUIRED"));

    let failure_obj = val.get("failure").unwrap_or(val);
    assert_eq!(failure_obj.get("permission").and_then(|v| v.as_str()), Some("accessibility"));
    assert_eq!(failure_obj.get("app").and_then(|v| v.as_str()), Some("QA App"));
    assert_eq!(failure_obj.get("relaunchRequired").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn display_stale_pointer_input_without_current_frame_is_computer_display_stale() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "click",
        "x": 5,
        "y": 5
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));

    let val = details.get("value").expect("value");
    let fail_code = val.get("failure").and_then(|f| f.get("code")).or_else(|| val.get("code")).and_then(|v| v.as_str());
    assert_eq!(fail_code, Some("COMPUTER_DISPLAY_STALE"));

    let text = result.content.iter().filter_map(|c| match c {
        maho_tools::definition::ToolContent::Text { text, .. } => Some(text.as_str()),
        _ => None,
    }).collect::<Vec<_>>().join("
");
    assert!(text.contains("Capture a fresh screenshot"));
}

#[tokio::test]
async fn out_of_bounds_drift_max_minus_one_lands_max_and_neg_refused() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");

    // 1. Take a screenshot
    let call_shot = ToolCall {
        id: "call-1",
        params: json!({ "action": "screenshot" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };
    let _ = (tool.execute)(call_shot).await.expect("execute");

    // 2. Click within bounds (1279, 799)
    let call_edge = ToolCall {
        id: "call-2",
        params: json!({ "action": "click", "x": 1279, "y": 799 }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };
    let res_edge = (tool.execute)(call_edge).await.expect("execute");
    assert_ne!(res_edge.details.as_ref().and_then(|d| d.get("isError")).and_then(|v| v.as_bool()), Some(true));

    // 3. Click out of bounds (1280, 10)
    let call_over = ToolCall {
        id: "call-3",
        params: json!({ "action": "click", "x": 1280, "y": 10 }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };
    let res_over = (tool.execute)(call_over).await.expect("execute");
    let val_over = res_over.details.as_ref().unwrap().get("value").unwrap();
    let code_over = val_over.get("failure").and_then(|f| f.get("code")).or_else(|| val_over.get("code")).and_then(|v| v.as_str());
    assert_eq!(code_over, Some("COMPUTER_COORD_INVALID"));

    // 4. Drag with negative coordinate (-1, 10)
    let call_neg = ToolCall {
        id: "call-4",
        params: json!({ "action": "drag", "x": 10, "y": 10, "to_x": -1, "to_y": 10 }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };
    let res_neg = (tool.execute)(call_neg).await.expect("execute");
    let val_neg = res_neg.details.as_ref().unwrap().get("value").unwrap();
    let code_neg = val_neg.get("failure").and_then(|f| f.get("code")).or_else(|| val_neg.get("code")).and_then(|v| v.as_str());
    assert_eq!(code_neg, Some("COMPUTER_COORD_INVALID"));

    let methods = methods_of(&fixture.log).await;
    let clicks = methods.iter().filter(|m| m.as_str() == "click").count();
    let drags = methods.iter().filter(|m| m.as_str() == "drag").count();
    assert_eq!(clicks, 1);
    assert_eq!(drags, 0);
}

#[tokio::test]
async fn runaway_loop_halt_batch_deadline_cancels_step_that_never_finishes() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "batch",
        "timeout": 1,
        "actions": [{ "action": "wait", "ms": 600_000 }]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));

    let val = details.get("value").expect("value");
    let fail_code = val.get("failure").and_then(|f| f.get("code")).or_else(|| val.get("code")).and_then(|v| v.as_str());
    assert_eq!(fail_code, Some("COMPUTER_CANCELLED"));
}

#[tokio::test]
async fn blast_radius_in_batch_screenshot_reanchors_and_first_failure_stops_everything() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });

    let input = json!({
        "action": "batch",
        "actions": [
            { "action": "screenshot" },
            { "action": "click", "x": 100, "y": 100 },
            { "action": "click", "x": 5000, "y": 100 },
            { "action": "type", "text": "never" }
        ]
    });

    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: input,
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };

    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    let val = details.get("value").expect("value");
    let fail_code = val.get("failure").and_then(|f| f.get("code")).or_else(|| val.get("code")).and_then(|v| v.as_str());
    assert_eq!(fail_code, Some("COMPUTER_COORD_INVALID"));

    let text = result.content.iter().filter_map(|c| match c {
        maho_tools::definition::ToolContent::Text { text, .. } => Some(text.as_str()),
        _ => None,
    }).collect::<Vec<_>>().join("
");
    assert!(text.contains("Action 3 (click) failed"));

    let methods = methods_of(&fixture.log).await;
    let clicks = methods.iter().filter(|m| m.as_str() == "click").count();
    let types = methods.iter().filter(|m| m.as_str() == "typeText").count();
    assert_eq!(clicks, 1);
    assert_eq!(types, 0);
}

#[test]
fn publishes_one_root_object_schema_whose_action_enum_includes_batch() {
    let schema = senpi_desktop_tool::cua_definition::computer_actions_params_schema();
    assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
    assert!(schema.get("anyOf").is_none());
    let act_enum = schema["properties"]["action"]["enum"].as_array().expect("enum");
    let names: Vec<_> = act_enum.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(names, vec![
        "screenshot", "click", "double_click", "move", "drag", "scroll", "type", "keypress", "wait", "batch"
    ]);
}

// 4 it.each parameter expansions for invalid arguments before engine starts
#[tokio::test]
async fn refuses_unknown_action_frobnicate() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });
    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: json!({ "action": "frobnicate" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };
    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));
    let val = details.get("value").expect("value");
    assert_eq!(val.get("code").and_then(|v| v.as_str()), Some("COMPUTER_INVALID_ARGUMENTS"));
    assert_eq!(fixture.log.children_count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refuses_click_missing_y() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });
    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: json!({ "action": "click", "x": 1 }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };
    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));
    let val = details.get("value").expect("value");
    assert_eq!(val.get("code").and_then(|v| v.as_str()), Some("COMPUTER_INVALID_ARGUMENTS"));
    assert_eq!(fixture.log.children_count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refuses_type_with_extra_keys() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });
    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: json!({ "action": "type", "text": "hi", "keys": ["a"] }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };
    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));
    let val = details.get("value").expect("value");
    assert_eq!(val.get("code").and_then(|v| v.as_str()), Some("COMPUTER_INVALID_ARGUMENTS"));
    assert_eq!(fixture.log.children_count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refuses_batch_with_unknown_action_teleport() {
    let fixture = desktop_fixture(None, HashMap::new());
    let tool = create_computer_actions_tool(ComputerToolDeps {
        handle: fixture.handle.clone(),
        execute_tool: None,
    });
    let ctx = TestToolContext::new(env!("CARGO_MANIFEST_DIR"), None, "session-1", "/sessions/project");
    let call = ToolCall {
        id: "call",
        params: json!({
            "action": "batch",
            "actions": [{ "action": "screenshot" }, { "action": "teleport" }]
        }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&ctx),
    };
    let result = (tool.execute)(call).await.expect("execute");
    let details = result.details.expect("details");
    assert_eq!(details.get("isError").and_then(|v| v.as_bool()), Some(true));
    let val = details.get("value").expect("value");
    assert_eq!(val.get("code").and_then(|v| v.as_str()), Some("COMPUTER_INVALID_ARGUMENTS"));
    assert_eq!(fixture.log.children_count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn computer_actions_permission_tier_classification() {
    let p = |v: serde_json::Value| {
        computer_actions_permission_parser("computer_actions", &v, std::path::Path::new("/"))
            .into_iter()
            .flat_map(|r| r.patterns)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        p(json!({ "action": "batch", "actions": [{ "action": "screenshot" }, { "action": "wait", "ms": 5 }] })),
        vec!["read"]
    );
    assert_eq!(
        p(json!({ "action": "batch", "actions": [{ "action": "screenshot" }, { "action": "click", "x": 1, "y": 1 }] })),
        vec!["exec"]
    );
    assert_eq!(p(json!({ "action": "batch", "actions": [] })), vec!["exec"]);
    assert_eq!(p(json!({ "action": "nope" })), vec!["exec"]);
}

#[test]
fn launch_context_failure_maps_to_structured_computer_failure() {
    let error = senpi_desktop_service::rpc_client::DesktopLaunchContextError {
        message: "engine exited before handshake: self-responsible".into(),
        reason: "self-responsible".into(),
    };
    let failure = senpi_desktop_tool::cua_errors::launch_context_failure(&error);

    assert_eq!(failure.code, "COMPUTER_LAUNCH_CONTEXT_REQUIRED");
    assert_eq!(failure.message, error.message);
    assert_eq!(failure.reason.as_deref(), Some("self-responsible"));
    assert!(failure.permission.is_none());
    assert!(failure.settings_url.is_none());
    assert!(failure.app.is_none());
    assert!(failure.relaunch_required.is_none());

    let value = serde_json::to_value(&failure).expect("serialize");
    assert_eq!(
        value.get("code").and_then(|v| v.as_str()),
        Some("COMPUTER_LAUNCH_CONTEXT_REQUIRED")
    );
    assert_eq!(value.get("reason").and_then(|v| v.as_str()), Some("self-responsible"));
    assert!(value.get("permission").is_none());
}
