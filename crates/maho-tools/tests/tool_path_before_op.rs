use maho_machine_ops::{BeforeOpHook, OpContext, OpName, PolicyDecision};
use maho_tools::definition::{AbortSignal, ToolCall};
use maho_tools::index::{ToolsOptions, create_all_tool_definitions};
use maho_tools::machine::ToolPathPolicy;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn recording_policy(allow: bool) -> (Arc<Mutex<Vec<OpName>>>, ToolPathPolicy) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let hook: BeforeOpHook = {
        let calls = Arc::clone(&calls);
        Arc::new(
            move |op: OpName, _args: &serde_json::Value, _ctx: &OpContext| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.lock().expect("lock").push(op);
                    if allow {
                        Ok(PolicyDecision::Allow)
                    } else {
                        Ok(PolicyDecision::Deny {
                            reason: "denied by test policy".to_owned(),
                        })
                    }
                })
            },
        )
    };
    (
        calls,
        ToolPathPolicy {
            before_op: Some(hook),
        },
    )
}

#[tokio::test]
async fn calls_before_op_once_per_tool_and_denial_blocks_effect() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();
    std::fs::write(dir.join("note.txt"), "alpha\n").expect("write note.txt");

    // 1. Allow flow
    let (calls, policy) = recording_policy(true);
    let tools = create_all_tool_definitions(
        dir,
        ToolsOptions {
            machine_policy: Some(policy),
            ..Default::default()
        },
    );

    (tools["read"].execute)(ToolCall {
        id: "r",
        params: json!({"path": "note.txt"}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("read");

    (tools["write"].execute)(ToolCall {
        id: "w",
        params: json!({"path": "new.txt", "content": "fresh\n"}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("write");

    (tools["edit"].execute)(ToolCall {
        id: "e",
        params: json!({"path": "note.txt", "edits": [{"oldText": "alpha", "newText": "beta"}]}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("edit");

    (tools["bash"].execute)(ToolCall {
        id: "b",
        params: json!({"command": "printf ran > ran.txt"}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("bash");

    assert_eq!(
        *calls.lock().expect("lock"),
        vec![
            OpName::FilesRead,
            OpName::FilesWrite,
            OpName::FilesEdit,
            OpName::ProcessExec,
        ]
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("new.txt")).expect("read new"),
        "fresh\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("note.txt")).expect("read note"),
        "beta\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("ran.txt")).expect("read ran"),
        "ran"
    );

    // 2. Deny flow
    std::fs::write(dir.join("note.txt"), "alpha\n").expect("reset note.txt");
    let _ = std::fs::remove_file(dir.join("new.txt"));
    let _ = std::fs::remove_file(dir.join("ran.txt"));

    let (deny_calls, deny_policy) = recording_policy(false);
    let denied_tools = create_all_tool_definitions(
        dir,
        ToolsOptions {
            machine_policy: Some(deny_policy),
            ..Default::default()
        },
    );

    for (name, params) in [
        ("read", json!({"path": "note.txt"})),
        ("write", json!({"path": "new.txt", "content": "fresh\n"})),
        (
            "edit",
            json!({"path": "note.txt", "edits": [{"oldText": "alpha", "newText": "beta"}]}),
        ),
        ("bash", json!({"command": "printf ran > ran.txt"})),
    ] {
        let err = (denied_tools[name].execute)(ToolCall {
            id: name,
            params,
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await
        .expect_err("expected denial");
        assert!(err.to_string().contains("denied by test policy"), "{err}");
    }

    assert_eq!(deny_calls.lock().expect("lock").len(), 4);
    assert!(!dir.join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("note.txt")).expect("note unchanged"),
        "alpha\n"
    );
    assert!(!dir.join("ran.txt").exists());
}
