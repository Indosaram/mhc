#![cfg(feature = "sdk")]

use std::sync::{Arc, Mutex};

use maho_ai::providers::faux::{FauxAssistantMessageOptions, faux_assistant_message, faux_tool_call};
use maho_ext_api::*;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
use serde_json::json;

struct TestExtension(Arc<Mutex<Vec<String>>>);

impl Extension for TestExtension {
    fn register(&self, api: &mut ExtensionApi) {
        self.0.lock().expect("events").push("register".into());
        api.register_tool(ToolDefinition::new("echo", "Echo input", json!({
            "type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]
        }), Arc::new(|call| Box::pin(async move {
            let context = call.context.expect("bound tool context");
            assert!(!context.session_manager().session_id().is_empty());
            assert!(context.cwd().is_dir());
            Ok(ToolResult::text(call.params["value"].as_str().expect("value")))
        }))));
        let events = self.0.clone();
        api.register_command("record", None, None, Arc::new(move |args, context| {
            let events = events.clone();
            Box::pin(async move {
                context.actions()?;
                events.lock().expect("events").push(format!("command:{args}"));
                Ok(())
            })
        }));
        for kind in [EventKind::SessionStart, EventKind::BeforeAgentStart, EventKind::AgentStart,
            EventKind::TurnStart, EventKind::TurnEnd, EventKind::AgentEnd] {
            let events = self.0.clone();
            api.on(kind, Arc::new(move |event, context| {
                let events = events.clone();
                Box::pin(async move {
                    context.actions()?;
                    if let ExtensionEvent::SessionStart(start) = event {
                        assert_eq!(start.reason, SessionReason::Startup);
                    }
                    events.lock().expect("events").push(event.kind().as_str().to_owned());
                    Ok(EventResult::None)
                })
            }));
        }
    }
}

fn session(prompt: &str, events: Arc<Mutex<Vec<String>>>) -> FauxSession {
    FauxSession::new(FauxScript { name: "native-extension".into(), prompt: prompt.into(), responses: Vec::new() })
        .with_native_extension(NativeExtensionFactory {
            path: "<test-extension>".into(), source_info: SourceInfo { source: "inline".into(), ..Default::default() },
            extension: Box::new(TestExtension(events)),
        })
}

#[tokio::test]
async fn native_tool_is_callable_and_lifecycle_matches_senpi_order() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let session = session("echo", events.clone()).with_extension("native").with_native_responses(vec![
        faux_assistant_message(faux_tool_call("echo", serde_json::from_value(json!({"value":"tool-output"})).expect("args"), Some("call-1")),
            FauxAssistantMessageOptions { stop_reason: Some(maho_ai::types::StopReason::ToolUse), timestamp: Some(0), ..Default::default() }),
        faux_assistant_message("done", FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }),
    ]);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("turn settles").expect("native session");
    let messages = result["messages"].as_array().expect("messages");
    let tool = messages.iter().find(|message| message["role"] == "toolResult").expect("tool result");
    assert_eq!(tool["content"][0]["text"], "tool-output");
    assert_eq!(tool["isError"], false);
    assert!(result["entries"].as_array().expect("entries").iter().any(|entry| entry["message"] == *tool));
    assert_eq!(*events.lock().expect("events"), ["register", "session_start", "before_agent_start", "agent_start",
        "turn_start", "turn_end", "turn_start", "turn_end", "agent_end"]);
}

#[tokio::test]
async fn native_command_runs_after_startup_without_a_provider_turn() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let session = session("/record argument", events.clone());
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("command settles").expect("native session");
    assert_eq!(*events.lock().expect("events"), ["register", "session_start", "command:argument"]);
    assert_eq!(result["messages"], json!([]));
}

#[tokio::test]
async fn native_factory_registers_fresh_for_each_run() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let session = session("/record again", events.clone());
    for _ in 0..2 {
        tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
            .await.expect("command settles").expect("native session");
    }
    assert_eq!(*events.lock().expect("events"), ["register", "session_start", "command:again",
        "register", "session_start", "command:again"]);
}

#[tokio::test]
async fn named_extension_without_native_factory_preserves_golden_only_error() {
    let session = FauxSession::new(FauxScript {
        name: "golden-only".into(), prompt: "hi".into(), responses: Vec::new(),
    }).with_extension("omo");
    assert!(session.run_native().await.is_err());
}
