use std::{cell::RefCell, rc::Rc};
use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use maho_interactive::{components::tool_execution::{ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation}, replay_assistant_tools::*, theme::{Theme, ColorMode}};
use maho_tui::tui::Component;

#[derive(Default)]
struct Host { messages: Vec<AssistantMessage>, tools: Vec<Rc<RefCell<ToolExecutionComponent>>>, pending: Vec<String> }
impl ReplayToolHost for Host {
    fn expanded(&self) -> bool { true }
    fn add_message(&mut self, message: AssistantMessage) { self.messages.push(message); }
    fn add_child(&mut self, component: Rc<RefCell<ToolExecutionComponent>>) { self.tools.push(component); }
    fn create_tool(&mut self, name: &str, id: &str, args: &serde_json::Map<String, serde_json::Value>) -> ToolExecutionComponent {
        ToolExecutionComponent::new(name, id, serde_json::Value::Object(args.clone()), ToolExecutionOptions::default(), None, "/tmp", ToolExecutionPresentation::Classic, None, Theme::builtin("dark", ColorMode::Truecolor).expect("theme"))
    }
    fn add_pending(&mut self, id: &str, _: Rc<RefCell<ToolExecutionComponent>>) { self.pending.push(id.into()); }
}
fn message(reason: StopReason) -> AssistantMessage {
    serde_json::from_value(serde_json::json!({
        "content": [{ "type": "text", "text": "before" }, { "type": "toolCall", "id": "first", "name": "custom", "arguments": {} }, { "type": "text", "text": "between" }, { "type": "toolCall", "id": "second", "name": "custom", "arguments": {} }, { "type": "text", "text": "after" }],
        "api": "anthropic-messages", "provider": "test", "model": "test", "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 } }, "stopReason": reason, "timestamp": 0
    })).expect("message")
}

#[test]
fn replay_keeps_text_segments_in_original_positions_and_tracks_pending_tools() {
    let mut host = Host::default();
    replay_assistant_tools(&message(StopReason::ToolUse), &mut host);
    assert_eq!(host.messages.iter().map(|message| message.content.len()).collect::<Vec<_>>(), [2, 1, 1]);
    assert_eq!(host.pending, ["first", "second"]);
    assert!(matches!(&host.messages[1].content[0], ContentBlock::Text(text) if text.text == "between"));
}

#[test]
fn aborted_replay_finishes_tools_as_error_instead_of_leaving_them_pending() {
    let mut host = Host::default();
    replay_assistant_tools(&message(StopReason::Aborted), &mut host);
    assert!(host.pending.is_empty());
    let lines = host.tools[0].borrow_mut().render(80).join("\n");
    assert!(lines.contains("Provider request failed"));
}

#[test]
fn message_without_tools_is_replayed_once_without_segmentation() {
    let mut host = Host::default();
    let mut message = message(StopReason::Stop);
    message.content = vec![ContentBlock::text("only text")];
    replay_assistant_tools(&message, &mut host);
    assert_eq!(host.messages, [message]);
    assert!(host.tools.is_empty());
}
