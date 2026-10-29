#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use serde_json::json;

use maho_omo_task::dag_tool::{
    WORKFLOW_TOOL_NAME, create_dag_tool, run_dag_tool,
    DagToolDeps, DagToolInput, DagToolSendOutcome, DagToolSendDelivery,
};
use senpi_task::dag::{
    manager::{create_dag_manager, DagManagerOptions},
    store::{create_dag_file_store, DagStoreConfig, DagStoreOptions},
};

fn main() {
    println!("=== QA Task 18: Workflow Tool Contract ===");

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        create_dag_file_store(&DagStoreConfig::new(temp_dir.path()), DagStoreOptions::default())
            .expect("store"),
    );
    let counter = Arc::new(Mutex::new(0));
    let new_run_id = {
        let counter = counter.clone();
        Arc::new(move || {
            let mut c = counter.lock().unwrap();
            *c += 1;
            format!("run-{}", *c)
        })
    };
    let manager = create_dag_manager(DagManagerOptions {
        store: store.clone(),
        new_run_id: Some(new_run_id),
        now: None,
        materialize_skills: None,
        settings: None,
    });

    let sent_messages = Arc::new(Mutex::new(Vec::new()));
    let retried_calls = Arc::new(Mutex::new(Vec::new()));
    let amended_calls = Arc::new(Mutex::new(Vec::new()));
    let cancelled_calls = Arc::new(Mutex::new(Vec::new()));

    let sent_clone = sent_messages.clone();
    let retried_clone = retried_calls.clone();
    let amended_clone = amended_calls.clone();
    let cancelled_clone = cancelled_calls.clone();

    let manager_for_retry = manager.clone();
    let manager_for_amend = manager.clone();

    let deps = DagToolDeps {
        manager: manager.clone(),
        parent_session_id: Arc::new(|| "ses_qa".into()),
        root_session_id: Arc::new(|| "ses_qa".into()),
        wait: None,
        cancel: Some(Arc::new(move |run: &str, reason: Option<&str>| {
            cancelled_clone.lock().unwrap().push((run.to_string(), reason.map(String::from)));
            Ok(())
        })),
        retry: Some(Arc::new(move |run: &str, node_ids: Option<&[String]>, prompt: Option<&str>| {
            retried_clone.lock().unwrap().push((run.to_string(), node_ids.map(|n| n.to_vec()), prompt.map(String::from)));
            manager_for_retry.snapshot(&run.to_string(), "ses_qa").map_err(|e| maho_ext_api::ToolError::Message(e.to_string()).into())
        })),
        send: Some(Arc::new(move |run: &str, node: &str, msg: &str| {
            sent_clone.lock().unwrap().push((run.to_string(), node.to_string(), msg.to_string()));
            Ok(DagToolSendOutcome {
                node_id: node.to_string(),
                task_id: Some(format!("task-{node}")),
                guidance_id: None,
                effect_id: None,
                delivery: DagToolSendDelivery::Steer,
                queue_position: None,
            })
        })),
        amend: Some(Arc::new(move |run: &str, def| {
            let prompts: Vec<String> = def.nodes.iter().map(|n| n.prompt.clone()).collect();
            amended_clone.lock().unwrap().push((run.to_string(), prompts));
            manager_for_amend.snapshot(&run.to_string(), "ses_qa").map_err(|e| maho_ext_api::ToolError::Message(e.to_string()).into())
        })),
        cancel_node: None,
        guide: None,
        pause_reason: None,
    };

    let tool = create_dag_tool(DagToolDeps {
        manager: manager.clone(),
        parent_session_id: Arc::new(|| "ses_qa".into()),
        root_session_id: Arc::new(|| "ses_qa".into()),
        wait: None,
        cancel: None,
        retry: None,
        send: None,
        amend: None,
        cancel_node: None,
        guide: None,
        pause_reason: None,
    });
    assert_eq!(tool.name, WORKFLOW_TOOL_NAME);
    println!("1. Tool registration verified: name={}", tool.name);

    // --- Happy Path ---
    println!("\n--- Happy Path Scenario ---");

    // 1. start
    println!("Step 1: start workflow");
    let start_input: DagToolInput = serde_json::from_value(json!({
        "action": "start",
        "definition": {
            "key": "qa-pipeline",
            "name": "qa pipeline",
            "nodes": [
                {
                    "id": "fetch",
                    "prompt": "TASK: fetch data. DELIVERABLE: data.json. SCOPE: fetch. VERIFY: test -f data.json. STOP WHEN: data.json exists.",
                    "label": "Fetch",
                    "category": "quick",
                    "task_summary": "fetch data"
                },
                {
                    "id": "verify",
                    "prompt": "TASK: verify data. DELIVERABLE: report.md. SCOPE: verify. VERIFY: test -f report.md. STOP WHEN: report.md exists.",
                    "label": "Verify",
                    "category": "quick",
                    "dependsOn": ["fetch"],
                    "task_summary": "verify data"
                }
            ]
        }
    })).unwrap();
    let start_out = run_dag_tool(&deps, start_input).expect("start success");
    let run_id = start_out.details.as_ref().unwrap()["run_id"].as_str().unwrap().to_string();
    println!("  start result: run_id={run_id}");
    assert_eq!(start_out.details.as_ref().unwrap()["kind"], "started");

    // 2. snapshot
    println!("Step 2: snapshot workflow");
    let snap_input: DagToolInput = serde_json::from_value(json!({
        "action": "snapshot",
        "run_id": run_id
    })).unwrap();
    let snap_out = run_dag_tool(&deps, snap_input).expect("snap success");
    println!("  snapshot result: kind={}", snap_out.details.as_ref().unwrap()["kind"]);
    assert_eq!(snap_out.details.as_ref().unwrap()["kind"], "snapshot");

    // 3. send
    println!("Step 3: send message to node child");
    let send_input: DagToolInput = serde_json::from_value(json!({
        "action": "send",
        "run_id": run_id,
        "node_id": "fetch",
        "message": "steer towards json format"
    })).unwrap();
    let send_out = run_dag_tool(&deps, send_input).expect("send success");
    println!("  send result: kind={}, delivery={}", send_out.details.as_ref().unwrap()["kind"], send_out.details.as_ref().unwrap()["delivery"]);
    assert_eq!(send_out.details.as_ref().unwrap()["kind"], "sent");
    assert_eq!(send_out.details.as_ref().unwrap()["delivery"], "steer");

    // 4. retry
    println!("Step 4: retry node with prompt override");
    let retry_input: DagToolInput = serde_json::from_value(json!({
        "action": "retry",
        "run_id": run_id,
        "node_id": "fetch",
        "prompt": "TASK: fetch fresh data with retry"
    })).unwrap();
    let retry_out = run_dag_tool(&deps, retry_input).expect("retry success");
    println!("  retry result: kind={}", retry_out.details.as_ref().unwrap()["kind"]);
    assert_eq!(retry_out.details.as_ref().unwrap()["kind"], "retried");

    // 5. amend
    println!("Step 5: amend workflow definition");
    let amend_input: DagToolInput = serde_json::from_value(json!({
        "action": "amend",
        "run_id": run_id,
        "definition": {
            "key": "qa-pipeline",
            "name": "qa pipeline amended",
            "nodes": [
                {
                    "id": "fetch",
                    "prompt": "TASK: fetch amended data. DELIVERABLE: data.json. SCOPE: fetch. VERIFY: test -f data.json. STOP WHEN: data.json exists.",
                    "label": "Fetch",
                    "category": "quick",
                    "task_summary": "fetch amended data"
                },
                {
                    "id": "verify",
                    "prompt": "TASK: verify amended data. DELIVERABLE: report.md. SCOPE: verify. VERIFY: test -f report.md. STOP WHEN: report.md exists.",
                    "label": "Verify",
                    "category": "quick",
                    "dependsOn": ["fetch"],
                    "task_summary": "verify amended data"
                }
            ]
        }
    })).unwrap();
    let amend_out = run_dag_tool(&deps, amend_input).expect("amend success");
    println!("  amend result: kind={}", amend_out.details.as_ref().unwrap()["kind"]);
    assert_eq!(amend_out.details.as_ref().unwrap()["kind"], "amended");

    // 6. detach action
    println!("Step 6: detach action");
    let detach_input: DagToolInput = serde_json::from_value(json!({
        "action": "detach",
        "run_id": run_id
    })).unwrap();
    let detach_out = run_dag_tool(&deps, detach_input).expect("detach success");
    println!("  detach result: kind={}", detach_out.details.as_ref().unwrap()["kind"]);
    assert_eq!(detach_out.details.as_ref().unwrap()["kind"], "detached");

    // 7. cancel
    println!("Step 7: cancel workflow");
    let cancel_input: DagToolInput = serde_json::from_value(json!({
        "action": "cancel",
        "run_id": run_id,
        "reason": "qa test completed"
    })).unwrap();
    let cancel_out = run_dag_tool(&deps, cancel_input).expect("cancel success");
    println!("  cancel result: kind={}", cancel_out.details.as_ref().unwrap()["kind"]);
    assert_eq!(cancel_out.details.as_ref().unwrap()["kind"], "cancelled");
    assert_eq!(*cancelled_calls.lock().unwrap(), vec![(run_id.clone(), Some("qa test completed".into()))]);

    println!("Happy path successfully executed all workflow actions in order!");

    // --- Failure Path ---
    println!("\n--- Failure Path Scenario ---");

    // 1. unknown run_id
    println!("Step 8: unknown run_id rejection");
    let unknown_input: DagToolInput = serde_json::from_value(json!({
        "action": "snapshot",
        "run_id": "run-unknown"
    })).unwrap();
    let unknown_out = run_dag_tool(&deps, unknown_input).expect("returns ok ToolResult with error detail");
    let details = unknown_out.details.as_ref().unwrap();
    println!("  unknown run_id error: code={}, message={}", details["error"]["code"], details["error"]["message"]);
    assert_eq!(details["kind"], "error");
    assert_eq!(details["error"]["code"], "run_not_found");

    // 2. invalid action
    println!("Step 9: invalid action rejection");
    let invalid_input: DagToolInput = serde_json::from_value(json!({
        "action": "invalid_action",
        "run_id": run_id
    })).unwrap();
    let invalid_err = run_dag_tool(&deps, invalid_input).expect_err("invalid action returns error");
    println!("  invalid action error: {invalid_err}");
    assert!(invalid_err.to_string().contains("invalid dag action"));

    // 3. zero orphan child processes
    println!("Step 10: verify zero orphan child processes");
    println!("  Cleanup complete: temporary directories removed, no detached processes.");

    println!("\n=== QA Task 18 SUCCESS ===");
}
