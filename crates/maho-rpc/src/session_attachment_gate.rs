//! Socket ownership gate (senpi `session-attachment-gate.ts`): checked before lifecycle routing AND before worker
//! dispatch. Effect-free reads stay exempt; every other session command requires an attachment by default.
use std::collections::HashMap;

/// Effect-based reads only. New session commands require attachment by default.
pub const ATTACH_GATE_EXEMPT: &[&str] = &[
    "get_protocol_info",
    "list_sessions",
    "subscribe_sessions",
    "unsubscribe_sessions",
    "get_subscription_limits",
    "warm",
    "detach_connection",
    "open_session",
    "attach_session",
    "reclaim_session",
    "get_effect_status",
    "get_state",
    "get_snapshot",
    "get_messages",
    "get_entries",
    "get_tree",
    "get_fork_messages",
    "get_last_assistant_text",
    "get_media",
    "get_commands",
    "get_loaded_surfaces",
    "get_available_models",
    "get_available_thinking_levels",
    "get_fast_mode",
    "get_session_stats",
    "get_pending_inputs",
    "get_steering_messages",
    "get_follow_up_messages",
    "get_auth_providers",
    "get_provider_accounts",
    "resolve_path",
    "memory_report",
    "eval_cells.list",
    "eval_cells.peek",
    "check_reload_veto",
];

/// `<BRAND>_RPC_REQUIRE_ATTACH=1` turns the warning into a `session_not_attached` refusal.
pub const REQUIRE_ATTACH_ENV_SUFFIX: &str = "RPC_REQUIRE_ATTACH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentGate {
    Pass,
    /// Not attached, not yet enforced: the host warns on stderr (refused from the next release).
    Warn(String),
    Refuse,
}

/// A client-chosen id as one log token: control characters and quotes escaped, so it never adds a line.
fn log_safe(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    quoted[1..quoted.len().saturating_sub(1).max(1)].to_owned()
}

/// senpi `attachmentGate`: `owner` is the requesting connection (absent on stdio), `attached` whether it holds the
/// session.
pub fn attachment_gate(
    owner: Option<&str>,
    command: &str,
    session_id: Option<&str>,
    attached: bool,
    env: impl FnOnce() -> HashMap<String, String>,
) -> AttachmentGate {
    if owner.is_none() || ATTACH_GATE_EXEMPT.contains(&command) {
        return AttachmentGate::Pass;
    }
    let Some(session_id) = session_id else {
        return AttachmentGate::Pass;
    };
    if matches!(
        command,
        "close_session" | "set_session_context" | "release_attachment"
    ) || attached
    {
        return AttachmentGate::Pass;
    }
    // Read per refusal, like upstream's `process.env` lookup, and only on the unattached path.
    if maho_core::brand::env_value(REQUIRE_ATTACH_ENV_SUFFIX, &env()).as_deref() == Some("1") {
        return AttachmentGate::Refuse;
    }
    AttachmentGate::Warn(format!(
        "senpi rpc host: {command} on {} from a connection not attached to it (refused from the next release)",
        log_safe(session_id)
    ))
}

/// The `errorData` a refused command carries.
pub fn not_attached_detail() -> serde_json::Value {
    serde_json::json!({
        "reason": "this connection is not attached to that session",
        "action": "attach first with attach_session { sessionId, durableSessionId } or open_session { sessionPath }",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_stdio_and_attached_pass_and_others_warn_or_refuse() {
        let env = HashMap::new();
        assert_eq!(
            attachment_gate(None, "prompt", Some("s"), false, || env.clone()),
            AttachmentGate::Pass
        );
        assert_eq!(
            attachment_gate(Some("c"), "get_state", Some("s"), false, || env.clone()),
            AttachmentGate::Pass
        );
        assert_eq!(
            attachment_gate(Some("c"), "prompt", None, false, || env.clone()),
            AttachmentGate::Pass
        );
        assert_eq!(
            attachment_gate(Some("c"), "prompt", Some("s"), true, || env.clone()),
            AttachmentGate::Pass
        );
        assert_eq!(
            attachment_gate(Some("c"), "close_session", Some("s"), false, || env.clone()),
            AttachmentGate::Pass
        );
        let AttachmentGate::Warn(line) =
            attachment_gate(Some("c"), "prompt", Some("a\"\nb"), false, || env.clone())
        else {
            panic!("expected a warning");
        };
        assert!(!line.contains('\n') && line.contains("a\\\"\\nb"));
        let env = HashMap::from([("SENPI_RPC_REQUIRE_ATTACH".to_owned(), "1".to_owned())]);
        assert_eq!(
            attachment_gate(Some("c"), "prompt", Some("s"), false, || env.clone()),
            AttachmentGate::Refuse
        );
    }
}
