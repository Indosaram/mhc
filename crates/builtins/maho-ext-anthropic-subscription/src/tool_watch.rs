use std::collections::{BTreeMap, HashMap, VecDeque};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TOOL_WATCH_CUSTOM_TYPE: &str = "claude-sdk-oauth-tool-watch";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedToolExecution {
    pub tool_call_id: String, pub tool_name: String, pub content: String, pub is_error: bool, pub timestamp: i64,
}
#[derive(Default)]
pub struct ToolWatch { states: HashMap<String, VecDeque<TrackedToolExecution>> }

fn truncate(text: &str, limit: usize) -> String {
    let units: Vec<_> = text.encode_utf16().collect();
    if units.len() <= limit { text.into() } else { format!("{}\n...[truncated]", String::from_utf16_lossy(&units[..limit])) }
}
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|block| match block["type"].as_str() {
            Some("text") => block["text"].as_str().map(str::to_owned),
            Some("image") => Some(format!("[image:{}]", block["mimeType"].as_str().unwrap_or("unknown"))),
            Some(kind) => Some(format!("[{kind}]")), None => None,
        }).filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}
impl ToolWatch {
    pub fn track(&mut self, session: &str, mut execution: TrackedToolExecution) {
        execution.content = truncate(&execution.content, 4000);
        let completed = self.states.entry(session.into()).or_default();
        completed.retain(|existing| existing.tool_call_id != execution.tool_call_id);
        completed.push_back(execution);
        if completed.len() > 256 { completed.pop_front(); }
    }
    pub fn get(&self, session: &str, id: &str) -> Option<&TrackedToolExecution> { self.states.get(session)?.iter().find(|execution| execution.tool_call_id == id) }
    pub fn delete_session(&mut self, session: &str) { self.states.remove(session); }
    pub fn reconcile(&mut self, session: &str, messages: &[Value]) {
        for message in messages.iter().filter(|message| message["role"] == "toolResult") {
            self.track(session, TrackedToolExecution { tool_call_id: message["toolCallId"].as_str().expect("tool id").into(), tool_name: message["toolName"].as_str().expect("tool name").into(), content: content_text(&message["content"]), is_error: message["isError"] == true, timestamp: message["timestamp"].as_i64().expect("timestamp") });
        }
    }
    pub fn hydrate(&mut self, session: &str, entries: &[Value], now: i64) {
        self.states.insert(session.into(), VecDeque::new());
        for entry in entries {
            if entry["type"] == "message" { self.reconcile(session, std::slice::from_ref(&entry["message"])); }
            else if entry["type"] == "custom" && entry["customType"] == TOOL_WATCH_CUSTOM_TYPE {
                let data = &entry["data"];
                if data["type"] == "tool_execution_end" && let (Some(id), Some(name), Some(content)) = (data["toolCallId"].as_str(), data["toolName"].as_str(), data["content"].as_str()) {
                    self.track(session, TrackedToolExecution { tool_call_id: id.into(), tool_name: name.into(), content: content.into(), is_error: data["isError"] == true, timestamp: data["timestamp"].as_i64().unwrap_or(now) });
                }
            }
        }
    }
    pub fn prompt_note(&self, session: &str, messages: &[Value], custom: &BTreeMap<String, String>) -> Option<String> {
        let results: std::collections::HashSet<_> = messages.iter().filter(|message| message["role"] == "toolResult").filter_map(|message| message["toolCallId"].as_str()).collect();
        let mut pending = Vec::new();
        for message in messages.iter().filter(|message| message["role"] == "assistant") {
            for block in message["content"].as_array().into_iter().flatten().filter(|block| block["type"] == "toolCall") {
                let id = block["id"].as_str().expect("tool id");
                pending.retain(|(existing, _, _)| *existing != id);
                pending.push((id, block["name"].as_str().expect("tool name"), message["timestamp"].as_i64().expect("timestamp")));
            }
        }
        pending.retain(|(id, _, _)| !results.contains(id));
        pending.sort_by_key(|(_, _, timestamp)| std::cmp::Reverse(*timestamp));
        let notes: Vec<_> = pending.into_iter().take(4).map(|(id, name, _)| match self.get(session, id) {
            Some(execution) => format!("TOOL RESULT (recovered {}, id={id}, status={}):\n{}", crate::tools::map_pi_tool_name(&execution.tool_name, custom), if execution.is_error { "error" } else { "ok" }, truncate(if execution.content.is_empty() { "(empty tool result)" } else { &execution.content }, 1200)),
            None => format!("TOOL RESULT (missing execution {}, id={id}, status=error):\nTool execution did not complete or its result was not observed. Do not guess. Call the tool again.", crate::tools::map_pi_tool_name(name, custom)),
        }).collect();
        (!notes.is_empty()).then(|| notes.join("\n\n"))
    }
}

pub fn register(api: &mut maho_ext_api::ExtensionApi, watch: std::sync::Arc<std::sync::Mutex<ToolWatch>>) {
    use maho_ext_api::{EventKind, ExtensionEvent, EventResult};
    for kind in [EventKind::SessionStart, EventKind::SessionTree, EventKind::SessionShutdown] {
        let watch = watch.clone();
        let runtime=api.runtime.clone();
        api.on(kind, std::sync::Arc::new(move |event, ctx| {
            let watch = watch.clone();let runtime=runtime.clone(); Box::pin(async move {
                runtime.assert_active()?;
                let session = ctx.session_manager.session_id().to_owned();
                let mut watch = watch.lock().expect("tool watch");
                if event.kind() == EventKind::SessionShutdown { watch.delete_session(&session); }
                else {
                    let entries: Vec<_> = ctx.session_manager.get_branch().into_iter().map(|entry| {
                        let mut data = entry.data; data["type"] = Value::String(entry.kind); data
                    }).collect();
                    watch.hydrate(&session, &entries, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_millis() as i64);
                }
                Ok(EventResult::None)
            })
        }));
    }
    let runtime = api.runtime.clone(); let cwd = api.cwd.clone(); let profile = api.profile.clone();
    api.on(EventKind::ToolExecutionEnd, std::sync::Arc::new(move |event, ctx| {
        let watch = watch.clone(); let runtime = runtime.clone(); let cwd = cwd.clone(); let profile = profile.clone();
        Box::pin(async move {
            runtime.assert_active()?;
            if maho_ai::legacy_provider_ids::normalize_provider_id(ctx.model.as_ref().map_or("", |model| &model.provider)) != "anthropic-subscription" { return Ok(EventResult::None); }
            if let ExtensionEvent::ToolExecutionEnd { tool_call_id, tool_name, result, is_error } = event {
                let content = content_text(&result["content"]);
                let content = if content.is_empty() { result.to_string() } else { content };
                let execution = TrackedToolExecution { tool_call_id: tool_call_id.clone(), tool_name: tool_name.clone(), content: truncate(&content, 4000), is_error: *is_error, timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_millis() as i64 };
                watch.lock().expect("tool watch").track(ctx.session_manager.session_id(), execution.clone());
                let mut data = serde_json::to_value(execution).expect("execution"); data["type"] = Value::String("tool_execution_end".into());
                let api = maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("tool-watch", cwd, Default::default()), profile, Default::default(), runtime);
                api.append_entry(TOOL_WATCH_CUSTOM_TYPE, Some(data))?;
            }
            Ok(EventResult::None)
        })
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fifo_rerecord_and_session_delete_preserve_bounded_recovery_state() {
        let mut watch = ToolWatch::default();
        for i in 0..257 { watch.track("s", TrackedToolExecution { tool_call_id: i.to_string(), tool_name: "read".into(), content: String::new(), is_error: false, timestamp: i }); }
        assert!(watch.get("s", "0").is_none());
        watch.track("s", TrackedToolExecution { tool_call_id: "1".into(), tool_name: "read".into(), content: "fresh".into(), is_error: false, timestamp: 300 });
        assert_eq!(watch.states["s"].len(), 256); assert_eq!(watch.states["s"].back().expect("last").content, "fresh");
        watch.delete_session("s"); assert!(watch.get("s", "1").is_none());
    }
    #[test]
    fn prompt_recovers_only_missing_results_and_limits_recent_calls() {
        let mut watch = ToolWatch::default();
        let messages = [serde_json::json!({"role":"toolResult","toolCallId":"a","toolName":"read","content":[{"type":"text","text":"result"}],"isError":false,"timestamp":1}), serde_json::json!({"role":"assistant","timestamp":2,"content":[{"type":"toolCall","id":"a","name":"read"},{"type":"toolCall","id":"b","name":"read"}]})];
        watch.reconcile("s", &messages); assert_eq!(watch.get("s", "a").expect("tracked").content, "result");
        let note = watch.prompt_note("s", &messages, &BTreeMap::new()).expect("note"); assert!(!note.contains("id=a")); assert!(note.contains("id=b"));
        assert_eq!(truncate("a😀b", 3), "a😀\n...[truncated]");
    }
}
