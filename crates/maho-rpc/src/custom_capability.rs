//! Additive client feature gates. Absent flags preserve classic output.
use serde_json::{Value,json};
pub const CUSTOM_UNSUPPORTED_CAPABILITY: &str = "custom_unsupported";
pub const EXTENSION_EVENTS_CAPABILITY: &str = "extension_events";
pub const RENDERED_COMPONENTS_CAPABILITY: &str = "rendered_components";
pub const AUTO_TITLE_SESSIONS_CAPABILITY: &str = "auto_title_sessions";
pub const MEDIA_PLACEHOLDERS_CAPABILITY: &str = "media_placeholders";
pub const QUESTION_CAPABILITY: &str = "question";
pub const RETAIN_ON_DISCONNECT_CAPABILITY: &str = "retain_on_disconnect";
pub const SESSION_CONTEXT_CAPABILITY: &str = "session_context";
pub const SESSION_KIND_CAPABILITY: &str = "session_kind";
pub const AUTO_TITLE_PER_SESSION_CAPABILITY: &str = "auto_title_per_session";
pub const DURABLE_SESSION_ID_CAPABILITY: &str = "durable_session_id";
pub const RPC_CLIENT_CAPABILITIES_ENV: &str = "SENPI_RPC_CLIENT_CAPABILITIES";
pub const DEFAULT_CUSTOM_EXTENSION_LABEL: &str = "custom UI component";

pub fn parse_client_capabilities(value: Option<&str>) -> Vec<String> {
    value.unwrap_or_default().split(',').map(str::trim).filter(|part| !part.is_empty()).map(str::to_owned).collect()
}
/// The caller supplies the UUID from its request-id source.
pub fn build_custom_unsupported_request(capabilities: &[String], extension_name: &str, id: &str) -> Option<Value> {
    if !capabilities.iter().any(|c| c == CUSTOM_UNSUPPORTED_CAPABILITY) { return None; }
    let name = extension_name.trim();
    Some(json!({"type":"extension_ui_request","id":id,"method":"custom_unsupported","extensionName":if name.is_empty() { DEFAULT_CUSTOM_EXTENSION_LABEL } else { name }}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn default_client_receives_no_custom_notice() { assert_eq!(build_custom_unsupported_request(&[],"test","id"),None); }
    #[test] fn capable_client_receives_trimmed_extension_notice() { assert_eq!(build_custom_unsupported_request(&[CUSTOM_UNSUPPORTED_CAPABILITY.into()]," test ","id").unwrap()["extensionName"],"test"); }
    #[test] fn empty_name_uses_fallback_label() { assert_eq!(build_custom_unsupported_request(&[CUSTOM_UNSUPPORTED_CAPABILITY.into()]," ","id").unwrap()["extensionName"],DEFAULT_CUSTOM_EXTENSION_LABEL); }
    #[test] fn parser_trims_and_preserves_duplicates() { assert_eq!(parse_client_capabilities(Some(" question, ,question,media_placeholders ")),vec!["question","question","media_placeholders"]); assert!(parse_client_capabilities(None).is_empty()); }
}
