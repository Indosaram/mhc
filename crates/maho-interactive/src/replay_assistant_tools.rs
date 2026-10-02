use std::{cell::RefCell, rc::Rc};
use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use maho_tools::definition::ToolContent;
use crate::{aborted_error_label::aborted_error_label, components::{tool_execution::ToolExecutionComponent, tool_execution_types::ToolExecutionResult}};

pub trait ReplayToolHost {
    fn expanded(&self) -> bool;
    fn add_message(&mut self, message: AssistantMessage);
    fn add_child(&mut self, component: Rc<RefCell<ToolExecutionComponent>>);
    fn create_tool(&mut self, name: &str, id: &str, args: &serde_json::Map<String, serde_json::Value>) -> ToolExecutionComponent;
    fn add_pending(&mut self, id: &str, component: Rc<RefCell<ToolExecutionComponent>>);
}

pub fn replay_assistant_tools(message: &AssistantMessage, host: &mut impl ReplayToolHost) {
    let first = message.content.iter().position(|block| matches!(block, ContentBlock::ToolCall(_)));
    let mut head = message.clone();
    if let Some(index) = first { head.content.truncate(index + 1); }
    host.add_message(head);
    let Some(mut index) = first else { return; };
    while index < message.content.len() {
        if let ContentBlock::ToolCall(call) = &message.content[index] {
            index += 1;
            let component = Rc::new(RefCell::new(host.create_tool(&call.name, &call.id, &call.arguments)));
            component.borrow_mut().set_expanded(host.expanded());
            host.add_child(component.clone());
            if matches!(message.stop_reason, StopReason::Aborted | StopReason::Error) {
                let error = if message.stop_reason == StopReason::Aborted {
                    aborted_error_label(message.error_message.as_deref(), 0, None)
                } else {
                    message.error_message.as_deref().filter(|text| !text.is_empty()).unwrap_or("Error").to_owned()
                };
                component.borrow_mut().update_result(ToolExecutionResult { content: vec![ToolContent::text(error)], details: None, is_error: true }, false);
            } else {
                host.add_pending(&call.id, component);
            }
        } else {
            let start = index;
            while index < message.content.len() && !matches!(message.content[index], ContentBlock::ToolCall(_)) { index += 1; }
            let mut segment = message.clone();
            segment.content = message.content[start..index].to_vec();
            host.add_message(segment);
        }
    }
}
