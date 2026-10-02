use serde_json::Value;
use senpi_task::tools::{control::{cancel::TaskCancelInput, send_schema::{TaskSendInput, TaskSendMessage}}, output::output::{TaskOutputInput, TaskOutputMode}};

fn valid_id(id: &str) -> bool {
    id.encode_utf16().count() <= 256 && id.strip_prefix("st_").is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'))
}
fn required_id<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    if !value.is_object() { return Err("Request must be an object.".into()) }
    let id = value.get(field).and_then(Value::as_str).filter(|id| !id.trim().is_empty()).ok_or_else(|| format!("{field} is required."))?;
    if !valid_id(id) { return Err(format!("{field} must be a task id of at most 256 characters.")) }
    Ok(id)
}
pub fn parse_task_send(value: &Value) -> Result<TaskSendInput, String> {
    let to = required_id(value, "to")?;
    let message = value.get("message").and_then(Value::as_str).filter(|message| !message.trim().is_empty()).ok_or("message is required.")?;
    if message.encode_utf16().count() > 32000 { return Err("message must be at most 32000 characters.".into()) }
    Ok(TaskSendInput { to: to.into(), message: Some(TaskSendMessage::Plain(message.into())), team_run_id: None, summary: None, all_scope: None })
}
pub fn parse_task_cancel(value: &Value) -> Result<TaskCancelInput, String> {
    let task_id = required_id(value, "task_id")?;
    let reason = match value.get("reason") { None => None, Some(Value::String(reason)) => Some(reason), Some(_) => return Err("reason must be a string.".into()) };
    if reason.is_some_and(|reason| reason.encode_utf16().count() > 2000) { return Err("reason must be at most 2000 characters.".into()) }
    Ok(TaskCancelInput { task_id: Some(task_id.into()), name: None, reason: reason.cloned() })
}
pub fn parse_task_output(value: &Value) -> Result<TaskOutputInput, String> {
    let task_id = required_id(value, "task_id")?;
    let mode = match value.get("mode") { None => None, Some(Value::String(mode)) if mode == "status" => Some(TaskOutputMode::Status), Some(Value::String(mode)) if mode == "tail" => Some(TaskOutputMode::Tail), Some(Value::String(mode)) if mode == "full" => Some(TaskOutputMode::Full), Some(_) => return Err("mode must be status, tail, or full.".into()) };
    let tail_lines = match value.get("tail_lines") { None => None, Some(value) => { let number = value.as_u64().filter(|number| *number > 0).ok_or("tail_lines must be a positive integer.")?; if number > 1000 { return Err("tail_lines must be at most 1000.".into()) } Some(usize::try_from(number).map_err(|error| error.to_string())?) } };
    Ok(TaskOutputInput { task_id: Some(task_id.into()), mode, tail_lines, ..TaskOutputInput::default() })
}

fn bounded_text(text: &str, maximum: usize) -> String {
    let mut units = 0;
    text.chars().take_while(|character| { units += character.len_utf16(); units <= maximum }).collect()
}
fn bound_field(value: &mut Value, field: &str, maximum: usize, truncated_flag: bool) {
    if let Some(text) = value.get(field).and_then(Value::as_str) {
        let truncated = text.encode_utf16().count() > maximum;
        let bounded = bounded_text(text, maximum);
        value[field] = Value::String(bounded);
        if truncated && truncated_flag { value[format!("{field}_truncated")] = Value::Bool(true); }
    }
}
pub fn live_progress_snapshot(details: &senpi_task::progress::ToolProgressDetails) -> Value {
    let mut value = serde_json::json!({"activity":details.progress.activity,"started_at":details.progress.started_at,"turns":details.turns});
    for (key, field) in [("current_tool", details.current_tool.as_ref()), ("last_assistant_line", details.last_assistant_line.as_ref())] { if let Some(field) = field { value[key] = serde_json::json!(field); } }
    for (key, field) in [("tool_calls",details.tool_calls),("total_tokens",details.tokens),("output_tokens",details.output_tokens),("tokens_per_second",details.tokens_per_second)] { if let Some(field) = field { value[key] = serde_json::json!(field); } }
    value
}
pub fn task_snapshot(record: &senpi_task::state::TaskRecord, live_stats: Option<&senpi_task::state::TaskRunStats>, live_progress: Option<&Value>) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(senpi_task::tools::task::result_details::record_summary(record, true))?;
    for (key, field) in [("child_session_id", record.child_session_id.as_ref()), ("final_response", record.final_response.as_ref()), ("error_message",record.error_message.as_ref())] { if let Some(field) = field { value[key] = serde_json::json!(field); } }
    if let Some(stats) = live_stats { value["run_stats"] = serde_json::to_value(stats)?; }
    if let Some(progress) = live_progress { value["live_progress"] = progress.clone(); }
    for key in ["task_id", "child_session_id"] { bound_field(&mut value, key, 256, false); }
    for key in ["name", "task_summary", "agent_type", "category", "model"] { bound_field(&mut value, key, 32000, false); }
    for key in ["description", "final_response", "error_message"] { bound_field(&mut value, key, 32000, true); }
    Ok(value)
}
pub fn invalid_arguments(reason: &str) -> Value { serde_json::json!({"kind":"invalid_arguments","reason":reason}) }
pub fn bounded_task_output(details: &senpi_task::tools::output::types::TaskOutputDetails) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(details)?;
    match value.get("kind").and_then(Value::as_str) {
        Some("not_found") => return Ok(serde_json::json!({"kind":"not_found","reason":"Task not found."})),
        Some("invalid_arguments") => return Ok(value),
        _ => {}
    }
    if let Some(snapshot) = value.get_mut("snapshot") {
        for key in ["task_id", "parent_session_id", "root_session_id", "child_session_id"] { bound_field(snapshot, key, 256, false); }
        for key in ["name", "task_summary", "execution_mode", "model", "agent_type", "category"] { bound_field(snapshot, key, 32000, false); }
        for key in ["description", "final_response", "error_message"] { bound_field(snapshot, key, 32000, true); }
        if let Some(model) = snapshot.get_mut("resolved_model") { for key in ["provider", "model_id", "display", "variant", "reasoning_effort", "reasoning"] { bound_field(model, key, 32000, false); } }
        if let Some(suspended) = snapshot.get_mut("suspended") { bound_field(suspended, "explanation", 32000, false); }
        if let Some(lost) = snapshot.get_mut("lost") { for key in ["explanation", "session_dir"] { bound_field(lost, key, 32000, false); } }
    }
    if let Some(text) = value.get("transcript").and_then(Value::as_str) && text.encode_utf16().count() > 32000 { value["transcript"] = Value::String(bounded_text(text, 32000)); value["truncated"] = Value::Bool(true); }
    Ok(value)
}
