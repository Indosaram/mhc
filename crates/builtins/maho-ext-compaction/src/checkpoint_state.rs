use serde_json::{Value,json};
use crate::restoration_tracker::POST_COMPACT_RESTORATION_CUSTOM_TYPE;
pub const CHECKPOINT_CUSTOM_TYPE:&str="compaction.agent-checkpoint";
pub const CHECKPOINT_SCHEMA:&str="senpi.compaction.agent-checkpoint.v1";
pub const RESTORATION_DIRECTIVE:&str="[restore checkpointed session agent configuration after compaction]";
pub fn parse_checkpoint(value:&Value)->Option<Value> {
    if !value.is_object() && !value.is_array() {return None;}
    if value.get("schema").and_then(Value::as_str)==Some(CHECKPOINT_SCHEMA) && let Some(data)=value.get("data").filter(|v|v.is_object() || v.is_array()) {return parse_checkpoint(data);}
    let active:Vec<_>=value.get("activeTools").and_then(Value::as_array).into_iter().flatten().filter(|v|v.is_string()).cloned().collect();
    let model=value.get("model").filter(|v|v.is_object() || v.is_array()).map(|m|json!({"provider":m.get("provider").and_then(Value::as_str).unwrap_or_default(),"modelId":m.get("modelId").and_then(Value::as_str).unwrap_or_default()}));
    let mut result=json!({"activeTools":active,"thinkingLevel":value.get("thinkingLevel").and_then(Value::as_str),"agentName":value.get("agentName").and_then(Value::as_str)});
    if let Some(id)=value.get("modelId").and_then(Value::as_str).or_else(||model.as_ref().and_then(|m|m.get("modelId")).and_then(Value::as_str)) {result["modelId"]=json!(id);}
    if let Some(timestamp)=value.get("timestamp").filter(|v|v.is_number()) {result["timestamp"]=timestamp.clone();}
    if let Some(model)=model {result["model"]=model;}
    Some(result)
}
pub fn capture_agent_checkpoint(agent_name:Option<&str>,model:Option<&Value>,active_tools:&[String],thinking_level:Option<&str>,now:f64)->Value {
    let mut checkpoint=json!({"activeTools":active_tools,"thinkingLevel":thinking_level,"agentName":agent_name,"timestamp":now});
    if let Some(model)=model {checkpoint["model"]=model.clone();if let Some(id)=model.get("modelId") {checkpoint["modelId"]=id.clone();}}
    checkpoint
}
pub fn serialize_checkpoint(checkpoint:&Value)->Value {
    let mut output=checkpoint.clone();output["schema"]=json!(CHECKPOINT_SCHEMA);output["data"]=checkpoint.clone();output
}
pub fn capture_live_agent_checkpoint(api: &maho_ext_api::ExtensionApi, context: &maho_ext_api::ExtensionContext) -> Result<Value, maho_ext_api::ExtensionFailure> {
    let entries = context.session_manager.get_entries();
    let agent_name = entries.iter().rev().filter(|entry| entry.kind == "custom")
        .find_map(|entry| entry.data.get("data").unwrap_or(&entry.data).get("agentName").and_then(Value::as_str)
            .or_else(|| entry.data.get("data").unwrap_or(&entry.data).get("agent").and_then(Value::as_str)));
    let model = context.model.as_ref().map(|model| json!({"provider":model.provider,"modelId":model.id}));
    let thinking = serde_json::to_value(api.get_thinking_level()?).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
    Ok(capture_agent_checkpoint(agent_name, model.as_ref(), &api.get_active_tools()?, thinking.as_str(), chrono::Utc::now().timestamp_millis() as f64))
}
pub fn persist_checkpoint(api: &maho_ext_api::ExtensionApi, checkpoint: &Value) -> Result<(), maho_ext_api::ExtensionFailure> {
    api.append_entry(CHECKPOINT_CUSTOM_TYPE, Some(serialize_checkpoint(checkpoint)))
}
pub fn get_latest_checkpoint(entries:&[Value])->Option<Value> {
    entries.iter().rev().filter(|e|e.get("type").and_then(Value::as_str)==Some("custom") && e.get("customType").and_then(Value::as_str)==Some(CHECKPOINT_CUSTOM_TYPE)).find_map(|e|e.get("data").and_then(parse_checkpoint))
}
pub fn latest_legacy_checkpoint(checkpoints:&[Value],append_calls:&[Value])->Option<Value> {
    let candidates:Vec<_>=if checkpoints.is_empty() {append_calls.iter().filter(|e|e.get("customType").and_then(Value::as_str)==Some(CHECKPOINT_CUSTOM_TYPE)).filter_map(|e|e.get("data").and_then(parse_checkpoint)).collect()} else {checkpoints.to_vec()};
    candidates.into_iter().reduce(|a,b|if b.get("timestamp").and_then(Value::as_f64).unwrap_or(0.)>a.get("timestamp").and_then(Value::as_f64).unwrap_or(0.) {b} else {a})
}
pub fn build_restoration_hints(checkpoint:&Value)->String {
    let model=checkpoint.get("modelId").and_then(Value::as_str).or_else(||checkpoint.get("model").and_then(|m|m.get("modelId")).and_then(Value::as_str)).unwrap_or("unknown");
    let tools=checkpoint.get("activeTools").and_then(Value::as_array).map(|a|a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).filter(|s|!s.is_empty()).unwrap_or_else(||"none".into());
    format!("{RESTORATION_DIRECTIVE}\n\nRestore checkpointed session configuration:\n- Agent: {}\n- Tools: {tools}\n- Model: {model}",checkpoint.get("agentName").and_then(Value::as_str).unwrap_or("unknown"))
}
#[derive(Clone, Debug, Default)]
pub struct RestorationDirectiveState {pub delivered_checkpoint_timestamp:Option<f64>}
pub fn attach_restoration_directive(state:&mut RestorationDirectiveState,checkpoint:Option<&Value>,message:Option<Value>)->Option<Value> {
    let Some(checkpoint)=checkpoint else {return message;};
    let timestamp=checkpoint.get("timestamp").and_then(Value::as_f64);
    if timestamp.is_some() && state.delivered_checkpoint_timestamp==timestamp {return message;}
    state.delivered_checkpoint_timestamp=timestamp;
    let hints=build_restoration_hints(checkpoint);
    if let Some(mut message)=message {
        if let Some(content)=message.get("content").and_then(Value::as_str) {message["content"]=json!(format!("{hints}\n\n{content}"));}
        Some(message)
    } else {Some(json!({"customType":POST_COMPACT_RESTORATION_CUSTOM_TYPE,"content":hints,"display":false}))}
}
pub fn inject_restoration_directive(prompt:Option<&str>,checkpoint:Option<&Value>,fallback_model:Option<&Value>)->String {
    if let Some(prompt)=prompt {return format!("{prompt}\n\n{}",build_restoration_hints(checkpoint.expect("prompt form requires checkpoint")));}
    if let Some(id)=fallback_model.and_then(|m|m.get("modelId")).and_then(Value::as_str) {format!("{RESTORATION_DIRECTIVE}\nModel: {id}")} else {RESTORATION_DIRECTIVE.into()}
}
