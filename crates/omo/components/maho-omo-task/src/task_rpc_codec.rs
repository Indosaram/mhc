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
