use super::{projection_message_items::{MessageItemNotifier, MessageItemProjector}, projection_turn_diff::TurnDiffTracker, projection_types::{ProjectionResult, ProjectionTurnCompletion, message_id_from_message}, projection_wire_items::*};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::{Arc, Mutex}};

pub struct EventProjector {
    thread_id: String,
    turn_id: String,
    cwd: String,
    active_message_id: Arc<Mutex<Option<String>>>,
    message_counter: usize,
    message_items: MessageItemProjector,
    tool_items: Vec<ActiveToolItem>,
    completed_ids: BTreeSet<String>,
    turn_diff: TurnDiffTracker,
    compaction_id: Option<String>,
    finalized: bool,
}
fn notification(thread: &str, turn: &str, method: &str, mut params: Value) -> Value {
    params["threadId"] = json!(thread); params["turnId"] = json!(turn);
    json!({"method":method,"params":params})
}
impl EventProjector {
    pub fn new(thread_id: String, turn_id: String, cwd: String) -> Self {
        let active_message_id = Arc::new(Mutex::new(None::<String>));
        let active = active_message_id.clone();
        let thread = thread_id.clone(); let turn = turn_id.clone();
        let started_thread = thread_id.clone(); let started_turn = turn_id.clone();
        let completed_thread = thread_id.clone(); let completed_turn = turn_id.clone();
        let message_items = MessageItemProjector::new(MessageItemNotifier {
            item_id: Arc::new(move |index| { let active = active.lock().unwrap_or_else(std::sync::PoisonError::into_inner); format!("{}:{index}", active.as_deref().unwrap_or("message-1")) }),
            started: Arc::new(move |item| notification(&started_thread, &started_turn, "item/started", json!({"item":item}))),
            completed: Arc::new(move |item| notification(&completed_thread, &completed_turn, "item/completed", json!({"item":item}))),
            notification: Arc::new(move |method, params| notification(&thread, &turn, method, params)),
        });
        Self { thread_id, turn_id, cwd, active_message_id, message_counter: 0, message_items, tool_items: Vec::new(), completed_ids: BTreeSet::new(), turn_diff: TurnDiffTracker::default(), compaction_id: None, finalized: false }
    }
    fn note_message(&mut self, message: &Value, replace: bool) {
        let mut active = self.active_message_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(id) = message_id_from_message(message) { *active = Some(id.into()); }
        else if replace || active.is_none() { self.message_counter += 1; *active = Some(format!("message-{}", self.message_counter)); }
    }
    fn notify(&self, method: &str, params: Value) -> Value { notification(&self.thread_id, &self.turn_id, method, params) }
    fn complete_tool(&mut self, id: &str, is_error: bool, result: &Value) -> Vec<Value> {
        let Some(tool) = self.tool_items.iter_mut().find(|tool| tool.id == id && !tool.completed) else { return Vec::new(); };
        tool.completed = true;
        let text = extract_tool_text(result);
        if classify_tool(&tool.name) == ToolItemType::CommandExecution && !text.is_empty() { tool.output = cap_command_output(&text, MAX_TOOL_OUTPUT_BYTES).into(); }
        let status = if is_error { ToolExecutionStatus::Failed } else { ToolExecutionStatus::Completed };
        let (item, diff) = tool_wire_projection(tool, status, &self.cwd, result, is_error);
        let id = tool.id.clone();
        let cumulative = self.turn_diff.update(&id, &diff, self.tool_items.iter().map(|tool| tool.id.as_str()));
        let mut notifications = Vec::new();
        if self.completed_ids.insert(id) { notifications.push(self.notify("item/completed", json!({"item":build_wire_item(item)}))); }
        if let Some(diff) = cumulative { notifications.push(self.notify("turn/diff/updated", json!({"diff":diff}))); }
        notifications
    }
    pub fn project(&mut self, event: &Value) -> ProjectionResult {
        if self.finalized { return ProjectionResult::default(); }
        let mut notifications = Vec::new();
        let mut completion = None;
        match event["type"].as_str() {
            Some("message_start") if event["message"]["role"] == "assistant" => self.note_message(&event["message"], true),
            Some("message_update") if event["message"]["role"] == "assistant" => {
                self.note_message(&event["message"], false);
                let update = &event["assistantMessageEvent"];
                let index = update["contentIndex"].as_u64().and_then(|index| usize::try_from(index).ok()).unwrap_or_default();
                let delta = update["delta"].as_str().unwrap_or_default(); let content = update["content"].as_str().unwrap_or_default();
                match update["type"].as_str() {
                    Some("text_start") => notifications = self.message_items.start_text(index),
                    Some("text_delta") => notifications = self.message_items.delta_text(index, delta),
                    Some("text_end") => notifications = self.message_items.complete_text(index, content),
                    Some("thinking_start") => notifications = self.message_items.start_reasoning(index),
                    Some("thinking_delta") => notifications = self.message_items.delta_reasoning(index, delta),
                    Some("thinking_end") => notifications = self.message_items.complete_reasoning(index, content),
                    Some("toolcall_end") => {
                        let call = &update["toolCall"];
                        let tool = ActiveToolItem { id:call["id"].as_str().unwrap_or_default().into(), name:call["name"].as_str().unwrap_or_default().into(), args:call["arguments"].clone(), output:String::new(), completed:false };
                        let item = tool_wire_projection(&tool, ToolExecutionStatus::InProgress, &self.cwd, &Value::Null, false).0;
                        if let Some(position) = self.tool_items.iter().position(|previous| previous.id == tool.id) { self.tool_items[position] = tool; } else { self.tool_items.push(tool); }
                        notifications.push(self.notify("item/started", json!({"item":build_wire_item(item)})));
                    },
                    Some("done") => { notifications = self.message_items.close_dangling_items(); completion = Some(ProjectionTurnCompletion { status:"completed".into(), error_message:None }); },
                    Some("error") => completion = Some(ProjectionTurnCompletion { status:if update["reason"] == "aborted" { "interrupted" } else { "failed" }.into(), error_message:update["error"]["errorMessage"].as_str().map(str::to_owned) }),
                    _ => {},
                }
            },
            Some("message_end") if event["message"]["role"] == "assistant" => {
                self.note_message(&event["message"], false);
                notifications = self.message_items.complete_dangling_text(&event["message"]);
                for (index, content) in event["message"]["content"].as_array().into_iter().flatten().enumerate().filter(|(_, content)| content["type"] == "providerNative") {
                    let message_id = self.active_message_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().unwrap_or_default();
                    let item = provider_native_item(&format!("{message_id}:providerNative:{index}"), &event["message"], content);
                    notifications.push(self.notify("item/started", json!({"item":item})));
                    if self.completed_ids.insert(item["id"].as_str().unwrap_or_default().into()) { notifications.push(self.notify("item/completed", json!({"item":item}))); }
                }
            },
            Some("tool_execution_start") => {
                let id = event["toolCallId"].as_str().unwrap_or_default();
                if !self.tool_items.iter().any(|tool| tool.id == id) { self.tool_items.push(ActiveToolItem { id:id.into(), name:event["toolName"].as_str().unwrap_or_default().into(), args:event["args"].clone(), output:String::new(), completed:false }); }
            },
            Some("tool_execution_update") => {
                if let Some(tool) = self.tool_items.iter_mut().find(|tool| tool.id == event["toolCallId"].as_str().unwrap_or_default() && classify_tool(&tool.name) == ToolItemType::CommandExecution) {
                    let text = extract_tool_text(&event["partialResult"]); let delta = cap_command_output(&text, MAX_TOOL_OUTPUT_BYTES.saturating_sub(tool.output.len()));
                    if !delta.is_empty() { tool.output.push_str(delta); let id = tool.id.clone(); notifications.push(self.notify("item/commandExecution/outputDelta", json!({"itemId":id,"delta":delta}))); }
                }
            },
            Some("tool_execution_end") => notifications = self.complete_tool(event["toolCallId"].as_str().unwrap_or_default(), event["isError"] == true, &event["result"]),
            Some("compaction_start") => {
                let id = format!("{}:compaction", self.turn_id); self.compaction_id = Some(id.clone());
                notifications.push(self.notify("item/started", json!({"item":{"type":"contextCompaction","id":id}})));
            },
            Some("compaction_end") => {
                let id = self.compaction_id.take().unwrap_or_else(|| format!("{}:compaction", self.turn_id));
                if self.completed_ids.insert(id.clone()) { notifications.push(self.notify("item/completed", json!({"item":{"type":"contextCompaction","id":id}}))); }
            },
            _ => {},
        }
        ProjectionResult { notifications, turn_completion:completion }
    }
    pub fn finalize(&mut self) -> Vec<Value> {
        if self.finalized { return Vec::new(); }
        self.finalized = true;
        let mut notifications = self.message_items.close_dangling_items();
        let pending = self.tool_items.iter().filter(|tool| !tool.completed).map(|tool| tool.id.clone()).collect::<Vec<_>>();
        for id in pending { notifications.extend(self.complete_tool(&id, true, &json!({"content":[{"type":"text","text":"Tool execution interrupted"}]}))); }
        notifications
    }
}
