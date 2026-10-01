//! Port of senpi `packages/agent/src/harness/runtime/reducer.ts`.

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneSnapshotReduction { Rebase }

fn matching_operation<'a>(snapshot: &'a mut Value, id: &Value) -> Option<&'a mut Value> {
    snapshot.get_mut("operation").filter(|op| op.get("id") == Some(id))
}

pub fn reduce_lane_snapshot(snapshot: &mut Value, event: &Value) -> Option<LaneSnapshotReduction> {
    let kind = event["type"].as_str()?;
    if event.get("lane").is_some_and(|lane| !lane.is_null() && lane != &snapshot["lane"]) && kind != "usage" { return None; }
    match kind {
        "run_start" | "compaction_start" | "navigation_start" => {
            if kind == "compaction_start" && !snapshot["operation"].is_null() { return None; }
            snapshot["operation"] = json!({"id":event["runId"],"kind":match kind { "run_start" => "run", "compaction_start" => "compaction", _ => "navigation" },"startedAt":event["startedAt"],"fromTipId":snapshot["tipId"],"status":"open","runningTools":[]});
        }
        "operation_abort" => { if let Some(op) = matching_operation(snapshot, &event["operationId"]) { op["status"] = json!("aborting"); } }
        "run_resume" => { if let Some(op) = matching_operation(snapshot, &event["runId"]) { op.as_object_mut()?.remove("deferred"); } }
        "run_suspend" => { if let Some(op) = matching_operation(snapshot, &event["runId"]) { op.as_object_mut()?.remove("streamingMessage"); op["deferred"] = json!({"handle":event["deferred"],"poll":event["poll"]}); } }
        "retry_scheduled" => { if let Some(op) = matching_operation(snapshot, &event["runId"]) { op["retry"] = json!({"attempt":event["attempt"],"maxAttempts":event["maxAttempts"],"nextAttemptAt":event["notBefore"]}); } }
        "retry_start" | "retry_end" => { if let Some(op) = matching_operation(snapshot, &event["runId"]) { op.as_object_mut()?.remove("retry"); } }
        "message_start" | "message_update" => {
            if event["message"]["role"] != "assistant" || event.get("runId").is_none() || (kind == "message_start" && event["message"]["stopReason"] != "pending") { return None; }
            if let Some(op) = matching_operation(snapshot, &event["runId"]) { op["streamingMessage"] = event["message"].clone(); }
        }
        "message_end" => { if let Some(id) = event.get("runId") && let Some(op) = matching_operation(snapshot, id) { op.as_object_mut()?.remove("streamingMessage"); } }
        "tool_start" => {
            if let Some(op) = matching_operation(snapshot, &event["runId"]) {
                let tools = op["runningTools"].as_array_mut()?;
                let tool = json!({"status":"running","toolCallId":event["toolCallId"],"toolName":event["toolName"],"args":event["args"]});
                match tools.iter().position(|t| t["toolCallId"] == event["toolCallId"]) { Some(index) => tools[index] = tool, None => tools.push(tool) }
            }
        }
        "tool_update" | "tool_end" => {
            if let Some(op) = matching_operation(snapshot, &event["runId"])
                && let Some(tool) = op["runningTools"].as_array_mut()?.iter_mut().find(|t| t["toolCallId"] == event["toolCallId"]) {
                    if kind == "tool_end" { *tool = json!({"status":"settled","toolCallId":event["toolCallId"],"toolName":event["toolName"],"args":tool["args"],"result":event["result"],"isError":event["isError"]}); }
                    else if tool["status"] == "running" { tool["result"] = event["partialResult"].clone(); }
            }
        }
        "entry_added" => {
            let entry = &event["entry"];
            if entry["type"] == "message" && entry["message"]["role"] == "toolResult"
                && let Some(tools) = snapshot["operation"]["runningTools"].as_array_mut()
                && let Some(index) = tools.iter().position(|t| t["toolCallId"] == entry["message"]["toolCallId"]) { tools.remove(index); }
            let transcript = snapshot["transcript"].as_array_mut()?;
            if entry["type"] == "compaction" { transcript.clear(); }
            transcript.push(entry.clone());
            snapshot["tipId"] = entry["id"].clone();
            if entry["type"] == "message" { snapshot["stats"]["messageCount"] = json!(snapshot["stats"]["messageCount"].as_u64()?.checked_add(1)?); }
        }
        "queue_update" => snapshot["queues"] = event["queues"].clone(),
        "usage" => snapshot["stats"]["usage"] = event["totals"].clone(),
        "config_update" => {
            if event.get("lane") != snapshot.get("lane") { return None; }
            let field = match event["property"].as_str()? { "model" => "model", "thinkingLevel" => "thinkingLevel", "activeTools" => "activeToolNames", _ => return None };
            snapshot["configuration"][field] = event["value"].clone();
        }
        "run_end" | "compaction_end" => {
            let operation = matching_operation(snapshot, &event["runId"])?;
            let expected = if kind == "run_end" { "run" } else { "compaction" };
            if operation["kind"] != expected { return None; }
            let started_at = operation["startedAt"].clone();
            let from = if kind == "run_end" { event["fromTipId"].clone() } else { operation["fromTipId"].clone() };
            let tip = if kind == "run_end" { event["tipId"].clone() } else { snapshot["tipId"].clone() };
            let mut record = json!({"operationId":event["runId"],"kind":expected,"status":event["status"],"fromTipId":from,"tipId":tip,"startedAt":started_at,"endedAt":event["endedAt"]});
            if event["status"] == "failed" { record["error"] = event["error"].clone(); }
            snapshot["lastResult"] = record;
            snapshot["operation"] = Value::Null;
            if kind == "run_end" { snapshot["tipId"] = event["tipId"].clone(); }
        }
        "navigation_end" => return Some(LaneSnapshotReduction::Rebase),
        "fault" => snapshot["faulted"] = json!(true),
        "handler_error" | "turn_start" | "turn_end" | "value_update" | "lane_created" => {}
        _ => {}
    }
    None
}
