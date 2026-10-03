use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct CompletionRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub system: Option<String>,
    pub schema: Option<Value>,
    pub opts: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CompletionError(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionTier { Smol, Default, Slow }

pub fn normalize_request(mut request: CompletionRequest) -> CompletionRequest {
    if let Some(opts) = &request.opts {
        if let Some(model) = opts["model"].as_str() { request.model = Some(model.into()); }
        if let Some(system) = opts["system"].as_str() { request.system = Some(system.into()); }
        if let Some(schema) = opts.get("schema") { request.schema = Some(schema.clone()); }
    }
    request
}

pub fn resolve_completion_tier(requested: Option<&str>) -> Result<CompletionTier, CompletionError> {
    match requested {
        None | Some("default") => Ok(CompletionTier::Default),
        Some("smol") => Ok(CompletionTier::Smol),
        Some("slow") => Ok(CompletionTier::Slow),
        Some(tier) => Err(CompletionError(format!("completion() could not resolve the \"{tier}\" model tier; expected \"smol\", \"default\", or \"slow\"."))),
    }
}

pub fn format_completion(message: &AssistantMessage, provider: &str, model_id: &str, structured: bool) -> Result<Value, CompletionError> {
    if message.stop_reason == StopReason::Error { return Err(CompletionError(message.error_message.clone().unwrap_or_else(|| "completion() request failed.".into()))); }
    if message.stop_reason == StopReason::Aborted { return Err(CompletionError("completion() request aborted.".into())); }
    let text = message.content.iter().filter_map(|part| match part { ContentBlock::Text(text) => Some(text.text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
    if text.is_empty() { return Err(CompletionError("completion() returned no text output.".into())); }
    let details = json!({"model":format!("{provider}/{model_id}"),"structured":structured});
    if structured {
        let value = serde_json::from_str::<Value>(&text).unwrap_or_else(|error| json!({"parseError":error.to_string()}));
        Ok(json!({"value":value,"details":details}))
    } else { Ok(json!({"text":text,"details":details})) }
}
