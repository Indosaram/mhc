use serde_json::{Map, Value};

pub const OPENAI_REMOTE_COMPACTION_SCHEMA: &str = "senpi.compaction.openai-remote.v1";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiRemoteTransport { Websocket, ResponsesV2, CompactEndpoint }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAiRemoteCompactionOrigin { pub endpoint: String, pub trust_domain: String, pub auth_tenant_fingerprint: String }
#[derive(Clone, Debug, PartialEq)]
pub struct OpenAiRemoteCompactionDetails {
    pub provider: String,
    pub api: String,
    pub transport: OpenAiRemoteTransport,
    pub model_id: String,
    pub response_id: String,
    pub created_at: f64,
    pub request_input_item_count: f64,
    pub retained_input_item_count: f64,
    pub replacement_input: Vec<Map<String, Value>>,
    pub origin: Option<OpenAiRemoteCompactionOrigin>,
    pub usage: Option<Map<String, Value>>,
}
fn parse_origin(value: &Value) -> Option<OpenAiRemoteCompactionOrigin> {
    let fingerprint = value.get("authTenantFingerprint")?.as_str()?;
    if !fingerprint.starts_with("sha256:") { return None; }
    Some(OpenAiRemoteCompactionOrigin { endpoint: value.get("endpoint")?.as_str()?.into(), trust_domain: value.get("trustDomain")?.as_str()?.into(), auth_tenant_fingerprint: fingerprint.into() })
}
pub fn get_openai_remote_compaction_details(value: &Value) -> Option<OpenAiRemoteCompactionDetails> {
    let record = value.as_object()?;
    if record.get("schema")?.as_str()? != OPENAI_REMOTE_COMPACTION_SCHEMA || record.get("mode")?.as_str()? != "openai-remote" { return None; }
    let provider = record.get("provider")?.as_str()?;
    let api = record.get("api")?.as_str()?;
    if !((!provider.is_empty() && api == "openai-responses") || (provider == "chatgpt-subscription" && api == "openai-codex-responses")) { return None; }
    Some(OpenAiRemoteCompactionDetails {
        provider: provider.into(), api: api.into(),
        transport: match record.get("transport").and_then(Value::as_str) { Some("websocket") => OpenAiRemoteTransport::Websocket, Some("responses-v2") => OpenAiRemoteTransport::ResponsesV2, _ => OpenAiRemoteTransport::CompactEndpoint },
        model_id: record.get("modelId")?.as_str()?.into(), response_id: record.get("responseId")?.as_str()?.into(),
        created_at: record.get("createdAt")?.as_f64()?, request_input_item_count: record.get("requestInputItemCount")?.as_f64()?, retained_input_item_count: record.get("retainedInputItemCount")?.as_f64()?,
        replacement_input: record.get("replacementInput")?.as_array()?.iter().filter_map(Value::as_object).cloned().collect(),
        origin: record.get("origin").and_then(parse_origin), usage: record.get("usage").and_then(Value::as_object).cloned(),
    })
}
pub fn is_openai_compaction_item(item: &Value) -> bool { item.get("type").and_then(Value::as_str) == Some("compaction") && item.get("encrypted_content").is_some_and(Value::is_string) }
pub fn is_openai_context_compaction_item(item: &Value) -> bool { item.get("type").and_then(Value::as_str) == Some("context_compaction") && item.get("encrypted_content").is_some_and(Value::is_string) }
pub fn is_openai_remote_compaction_output_item(item: &Value) -> bool { is_openai_compaction_item(item) || is_openai_context_compaction_item(item) }
pub fn is_retained_remote_output_item(item: &Value) -> bool {
    is_openai_remote_compaction_output_item(item) || (item.get("type").and_then(Value::as_str) == Some("message") && matches!(item.get("role").and_then(Value::as_str), Some("user" | "system" | "developer")))
}
pub fn is_retained_responses_stream_input_item(item: &Value) -> bool { item.get("role").and_then(Value::as_str) == Some("user") }
