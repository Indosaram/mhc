//! Additive client feature gates. Absent flags preserve classic output.
use serde_json::{Value, json};
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
pub const DETACH_CONNECTION_CAPABILITY: &str = "detach_connection";
pub const SESSION_STATUS_CAPABILITY: &str = "session_status";
pub const WARM_CAPABILITY: &str = "warm";
pub const MOVED_PATH_GUARD_CAPABILITY: &str = "moved_path_guard";
pub const PROMPT_SURFACE_CAPABILITY: &str = "prompt_surface";
pub const BROWSER_ENGINE_CAPABILITY: &str = "browser_engine";
pub const RETRY_FALLBACK_PROFILE_CAPABILITY: &str = "retry_fallback_profile";
pub const RETRY_FALLBACK_COMMAND_CAPABILITY: &str = "retry_fallback_command";
pub const PROMPT_SURFACE_CHAT_CAPABILITY: &str = "prompt_surface_chat";
pub const SESSION_HELD_CAPABILITY: &str = "session_held";
/// Host supports explicit held-wake release; its command reply acknowledges admission.
pub const RESUME_SESSION_CAPABILITY: &str = "resume_session";
/// Host honors `open_session.attended:false` instead of silently starting unattended work.
pub const ATTENDED_CAPABILITY: &str = "attended";
/// Connection role (senpi#3811): may exercise a person's authority (Stop with intent, release a pause).
pub const TASK_PARENT_CAPABILITY: &str = "task_parent";
pub const INTERACTIVE_USER_CAPABILITY: &str = "interactive_user";
/// Host accepts `record_user_activity`.
pub const USER_ACTIVITY_CAPABILITY: &str = "user_activity";
/// Both directions, like `media_placeholders`: a host advertises it in `get_protocol_info` (its sessions send
/// `host_request` / `relocation_due`), and a client sends it in `set_client_info` to offer moving a session elsewhere.
pub const SESSION_RELOCATION_CAPABILITY: &str = "session_relocation";
/// The capability a client's handshake carries when it can present an MCP grant decision (senpi `grant_select`);
/// only such a connection is ever asked, and only its answer counts.
pub const GRANT_SELECT_CAPABILITY: &str = "grant_select";
pub const RPC_CLIENT_CAPABILITIES_ENV: &str = "SENPI_RPC_CLIENT_CAPABILITIES";
pub const DEFAULT_CUSTOM_EXTENSION_LABEL: &str = "custom UI component";

pub fn parse_client_capabilities(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}
/// The caller supplies the UUID from its request-id source.
pub fn build_custom_unsupported_request(
    capabilities: &[String],
    extension_name: &str,
    id: &str,
) -> Option<Value> {
    if !capabilities
        .iter()
        .any(|c| c == CUSTOM_UNSUPPORTED_CAPABILITY)
    {
        return None;
    }
    let name = extension_name.trim();
    Some(
        json!({"type":"extension_ui_request","id":id,"method":"custom_unsupported","extensionName":if name.is_empty() { DEFAULT_CUSTOM_EXTENSION_LABEL } else { name }}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_client_receives_no_custom_notice() {
        assert_eq!(build_custom_unsupported_request(&[], "test", "id"), None);
    }
    #[test]
    fn capable_client_receives_trimmed_extension_notice() {
        assert_eq!(
            build_custom_unsupported_request(
                &[CUSTOM_UNSUPPORTED_CAPABILITY.into()],
                " test ",
                "id"
            )
            .unwrap()["extensionName"],
            "test"
        );
    }
    #[test]
    fn empty_name_uses_fallback_label() {
        assert_eq!(
            build_custom_unsupported_request(&[CUSTOM_UNSUPPORTED_CAPABILITY.into()], " ", "id")
                .unwrap()["extensionName"],
            DEFAULT_CUSTOM_EXTENSION_LABEL
        );
    }
    #[test]
    fn parser_trims_and_preserves_duplicates() {
        assert_eq!(
            parse_client_capabilities(Some(" question, ,question,media_placeholders ")),
            vec!["question", "question", "media_placeholders"]
        );
        assert!(parse_client_capabilities(None).is_empty());
    }
    #[test]
    fn newer_host_capabilities_match_rpc_protocol_names() {
        assert_eq!(
            [
                DETACH_CONNECTION_CAPABILITY,
                SESSION_STATUS_CAPABILITY,
                WARM_CAPABILITY,
                MOVED_PATH_GUARD_CAPABILITY,
                PROMPT_SURFACE_CAPABILITY,
                BROWSER_ENGINE_CAPABILITY,
                RETRY_FALLBACK_PROFILE_CAPABILITY,
                RETRY_FALLBACK_COMMAND_CAPABILITY,
                PROMPT_SURFACE_CHAT_CAPABILITY
            ],
            [
                "detach_connection",
                "session_status",
                "warm",
                "moved_path_guard",
                "prompt_surface",
                "browser_engine",
                "retry_fallback_profile",
                "retry_fallback_command",
                "prompt_surface_chat"
            ]
        );
    }
}
