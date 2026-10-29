use std::sync::{Arc, Mutex};
use maho_ext_api::*;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};
use maho_ai::providers::faux::{faux_assistant_message, faux_tool_call, FauxAssistantMessageOptions};

struct TodoTool;
impl Extension for TodoTool {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_tool(ToolDefinition::new("todo", "Record tasks", serde_json::json!({
            "type":"object", "properties":{"op":{"type":"string"},"items":{"type":"array","items":{"type":"string"}}}
        }), Arc::new(|_| Box::pin(async { Ok(ToolResult::text("recorded")) }))));
    }
}

#[tokio::test]
async fn real_tool_result_appends_shipped_reminder() {
    let expected = serde_json::to_value(ToolContent::text(maho_omo_todo_fanout_reminder::reminder::TODO_FANOUT_REMINDER)).expect("shipped reminder");
    let arming = Arc::new(Mutex::new(maho_omo_ultrawork::SessionArming::default()));
    let mut script = load_script("hello").expect("script");
    script.prompt = "ultrawork: record a task.".into();
    let mut session = FauxSession::new(script);
    for (path, extension) in [
        ("<ultrawork>", Box::new(maho_omo_ultrawork::UltraworkComponent { arming: arming.clone() }) as Box<dyn Extension>),
        ("<reminder>", Box::new(maho_omo_todo_fanout_reminder::TodoFanoutReminderComponent { arming })),
        ("<todo>", Box::new(TodoTool)),
    ] {
        session = session.with_native_extension(NativeExtensionFactory { path: path.into(), source_info: Default::default(), extension });
    }
    session = session.with_native_responses(vec![
        faux_assistant_message(faux_tool_call("todo", serde_json::from_value(serde_json::json!({"op":"init","items":["Exercise original component trigger behavior"]})).expect("args"), Some("todo-init")),
            FauxAssistantMessageOptions { stop_reason: Some(maho_ai::types::StopReason::ToolUse), timestamp: Some(0), ..Default::default() }),
        faux_assistant_message("Recorded.", FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }),
    ]);
    let actual = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("bounded session").expect("native run");
    let tool = actual["messages"].as_array().expect("messages").iter().find(|message| message["role"] == "toolResult").expect("actual tool result");
    assert_eq!(tool["isError"], false);
    assert_eq!(tool["content"][0]["text"], "recorded");
    assert_eq!(tool["content"].as_array().expect("content").last().expect("native reminder"), &expected);
}
