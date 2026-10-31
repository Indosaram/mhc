//! Tool-runtime capabilities of a multi-session host (senpi `host-capabilities.ts` +
//! `cli/tool-runtime.ts`, senpi `d911de46a`).
//!
//! The host alone says where tool work runs: it advertises `tool_runtime:<kind>`, the placements the tool
//! child already serves and `offload_api`, and a capability a client negotiates under one of those names is
//! dropped, so there are never two answers.

/// Advertised only by a host whose deployment enforces private MCP presenters (senpi `mcpPresenters`); a client
/// can never negotiate it in.
pub const MCP_FORM_PRESENTER_CAPABILITY: &str = "mcp_form_presenter_v1";
/// Advertised in both runtimes: `ctx.offload` / `registerChildModule` exist (where they run is the runtime's choice).
pub const OFFLOAD_API_CAPABILITY: &str = "offload_api";
/// What the tool child already serves, advertised as `placement:<name>` beside `tool_runtime:child`.
pub const TOOL_CHILD_PLACEMENTS: [&str; 5] = [
    "launch-barrier",
    "job-policy",
    "hostcall",
    "offload",
    "terminal",
];
/// `unload_session` and `session_parked.reason` (senpi#3225).
pub const UNLOAD_SESSION_CAPABILITY: &str = "unload_session";
/// `get_subscription_limits` and the `subscription_limits` host record (senpi#3370).
pub const SUBSCRIPTION_LIMITS_CAPABILITY: &str = "subscription_limits";
/// `resolve_path`: which file a message's path means, the engine's one rule (senpi#3415).
pub const RESOLVE_PATH_CAPABILITY: &str = "resolve_path";
const TOOL_RUNTIME_PREFIX: &str = "tool_runtime:";
const PLACEMENT_PREFIX: &str = "placement:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRuntimeKind {
    InProcess,
    Child,
}
impl ToolRuntimeKind {
    /// senpi `isToolRuntimeKind` as a parse: `in-process` or `child`, nothing else.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "in-process" => Some(Self::InProcess),
            "child" => Some(Self::Child),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in-process",
            Self::Child => "child",
        }
    }
}

/// `tool_runtime:<kind>` (senpi `toolRuntimeCapability`).
pub fn tool_runtime_capability(kind: ToolRuntimeKind) -> String {
    format!("{TOOL_RUNTIME_PREFIX}{}", kind.as_str())
}

/// Every capability the tool runtime advertises: the kind, the child's placements and the offload API.
pub fn tool_runtime_capabilities(kind: ToolRuntimeKind) -> Vec<String> {
    let mut capabilities = vec![tool_runtime_capability(kind)];
    if kind == ToolRuntimeKind::Child {
        capabilities.extend(
            TOOL_CHILD_PLACEMENTS
                .iter()
                .map(|name| format!("{PLACEMENT_PREFIX}{name}")),
        );
    }
    capabilities.push(OFFLOAD_API_CAPABILITY.to_owned());
    capabilities
}

/// Whether the host alone owns this capability name.
pub fn is_host_owned_tool_runtime_capability(capability: &str) -> bool {
    capability.starts_with(TOOL_RUNTIME_PREFIX)
        || capability.starts_with(PLACEMENT_PREFIX)
        || capability == OFFLOAD_API_CAPABILITY
}

/// The host's own tool-runtime capabilities followed by the negotiated ones a client may legitimately add: a
/// negotiated tool-runtime capability and a negotiated `mcp_form_presenter_v1` are filtered out (senpi
/// `multiSessionHostCapabilities`).
pub fn host_tool_runtime_capabilities(kind: ToolRuntimeKind, negotiated: &[String]) -> Vec<String> {
    let mut capabilities = tool_runtime_capabilities(kind);
    capabilities.extend(
        negotiated
            .iter()
            .filter(|capability| {
                *capability != MCP_FORM_PRESENTER_CAPABILITY
                    && !is_host_owned_tool_runtime_capability(capability)
            })
            .cloned(),
    );
    capabilities
}

/// The capabilities a multi-session host advertises in `get_protocol_info`, in their wire order (senpi
/// `multiSessionHostCapabilities`). `warm` is added only when the runtime is in-process, `tool_runtime:<kind>` names
/// where heavy tool work runs, and `negotiated` carries the launch capabilities this connection was given; a
/// negotiated `mcp_form_presenter_v1` or tool-runtime capability is dropped, so the host's answer is the only one.
///
/// The names whose constants live in modules this crate does not carry (`custom-capability.ts`'s `eval_cells`,
/// `continue_from_leaf`, `tool_barrier`, `auth_key_verify`; the permission system's two preset capabilities) are
/// spelled here.
pub fn multi_session_host_capabilities(
    warm: bool,
    tool_runtime: ToolRuntimeKind,
    negotiated: &[String],
    mcp_presenters: bool,
) -> Vec<String> {
    use crate::custom_capability::*;
    let mut capabilities = Vec::<String>::new();
    let mut push = |capability: &str| {
        if !capabilities.iter().any(|existing| existing == capability) {
            capabilities.push(capability.to_owned());
        }
    };
    for capability in [
        "multi_session",
        SNAPSHOT_CURSOR_CAPABILITY,
        UI_ANSWER_LEDGER_CAPABILITY,
        DIALOG_OBSERVER_CAPABILITY,
    ] {
        push(capability);
    }
    if mcp_presenters {
        push(MCP_FORM_PRESENTER_CAPABILITY);
    }
    for capability in [
        "session_lifecycle",
        "expected_host_instance",
        ATTACH_SESSION_CAPABILITY,
        RELEASE_ATTACHMENT_CAPABILITY,
        RELEASE_CALLER_AWARE_CAPABILITY,
        RECLAIM_SESSION_CAPABILITY,
        AUTO_TITLE_SESSIONS_CAPABILITY,
        MEDIA_PLACEHOLDERS_CAPABILITY,
        crate::client_admission_record::DURABLE_CLIENT_MESSAGE_ID_CAPABILITY,
        crate::effect_record::EFFECT_LEDGER_CAPABILITY,
        SUBSCRIPTION_LIMITS_CAPABILITY,
        RESOLVE_PATH_CAPABILITY,
        crate::rpc_types::STEER_WHEN_IDLE_CAPABILITY,
        "continue_from_leaf",
        "eval_cells",
        "auth_key_verify",
        crate::rpc_types::QUEUED_MESSAGE_REVISION_CAPABILITY,
        "tool_barrier",
        crate::rpc_types::USER_ACTIVITY_CAPABILITY,
        // Both shared runtimes dispatch through the active session-writing holder guard.
        SESSION_HELD_CAPABILITY,
        RESUME_SESSION_CAPABILITY,
        ATTENDED_CAPABILITY,
        // `get_state`/`get_entries` marked `observe` never attach, and history reads page backward too (senpi#4267).
        OBSERVE_READS_CAPABILITY,
        ENTRIES_TAIL_CAPABILITY,
        // Only a multi-session host owns the attachment refcount, the per-session launch profile and the listing.
        RETAIN_ON_DISCONNECT_CAPABILITY,
        DETACH_CONNECTION_CAPABILITY,
        UNLOAD_SESSION_CAPABILITY,
        crate::session_move_wire::MOVE_SESSION_CAPABILITY,
        crate::session_move_wire::MOVE_SESSION_REVOKE_CAPABILITY,
        SESSION_RELOCATION_CAPABILITY,
        SESSION_CONTEXT_CAPABILITY,
        SESSION_KIND_CAPABILITY,
        SESSION_STATUS_CAPABILITY,
        AUTO_TITLE_PER_SESSION_CAPABILITY,
        DURABLE_SESSION_ID_CAPABILITY,
        MOVED_PATH_GUARD_CAPABILITY,
        PROMPT_SURFACE_CAPABILITY,
        PROMPT_SURFACE_CHAT_CAPABILITY,
        BROWSER_ENGINE_CAPABILITY,
        RETRY_FALLBACK_PROFILE_CAPABILITY,
        "permission_preset_accept_edits",
        "permission_preset_auto",
    ] {
        push(capability);
    }
    // Only an in-process runtime shares the loop a warm loads into (senpi#2314).
    if warm {
        push(WARM_CAPABILITY);
    }
    for capability in host_tool_runtime_capabilities(tool_runtime, negotiated) {
        push(capability.as_str());
    }
    capabilities
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn in_process_advertises_its_kind_and_the_offload_api_and_child_adds_placements() {
        assert_eq!(
            tool_runtime_capabilities(ToolRuntimeKind::InProcess),
            vec!["tool_runtime:in-process", "offload_api"]
        );
        let child = tool_runtime_capabilities(ToolRuntimeKind::Child);
        assert_eq!(
            child.first().map(String::as_str),
            Some("tool_runtime:child")
        );
        assert!(
            child
                .iter()
                .any(|capability| capability == "placement:terminal")
        );
        assert_eq!(
            child.last().map(String::as_str),
            Some(OFFLOAD_API_CAPABILITY)
        );
    }
    #[test]
    fn parse_reads_exactly_the_two_kinds() {
        assert_eq!(
            ToolRuntimeKind::parse("in-process"),
            Some(ToolRuntimeKind::InProcess)
        );
        assert_eq!(
            ToolRuntimeKind::parse("child"),
            Some(ToolRuntimeKind::Child)
        );
        assert_eq!(ToolRuntimeKind::parse("bogus"), None);
        assert_eq!(ToolRuntimeKind::parse(""), None);
    }
    #[test]
    fn child_advertises_every_placement_in_wire_order() {
        assert_eq!(
            tool_runtime_capabilities(ToolRuntimeKind::Child),
            vec![
                "tool_runtime:child",
                "placement:launch-barrier",
                "placement:job-policy",
                "placement:hostcall",
                "placement:offload",
                "placement:terminal",
                "offload_api",
            ]
        );
    }
    /// tool-runtime-capabilities.test.ts "drops negotiated tool_runtime and placement entries so the host's own
    /// answer is the only one".
    #[test]
    fn multi_session_host_capabilities_keep_only_the_hosts_tool_runtime_answer() {
        let negotiated = [
            "tool_runtime:child",
            "placement:offload",
            "offload_api",
            "custom:keep",
        ]
        .map(str::to_owned);
        let owned = |capabilities: Vec<String>| {
            capabilities
                .into_iter()
                .filter(|capability| is_host_owned_tool_runtime_capability(capability))
                .collect::<Vec<_>>()
        };
        let in_process =
            multi_session_host_capabilities(false, ToolRuntimeKind::InProcess, &negotiated, false);
        assert!(
            in_process
                .iter()
                .any(|capability| capability == "custom:keep")
        );
        assert_eq!(
            owned(in_process),
            vec!["tool_runtime:in-process", "offload_api"]
        );
        assert_eq!(
            owned(multi_session_host_capabilities(
                false,
                ToolRuntimeKind::Child,
                &negotiated,
                false
            )),
            tool_runtime_capabilities(ToolRuntimeKind::Child)
        );
    }
    /// host-04: the attach/snapshot/dialog set leads in wire order; `mcp_form_presenter_v1` and `warm` appear only
    /// when the deployment says so, a negotiated presenter capability never does, and nothing repeats.
    #[test]
    fn multi_session_host_capabilities_lead_with_the_session_set_and_gate_presenters_and_warm() {
        let negotiated = [MCP_FORM_PRESENTER_CAPABILITY, "multi_session"].map(str::to_owned);
        let plain =
            multi_session_host_capabilities(false, ToolRuntimeKind::InProcess, &negotiated, false);
        assert_eq!(
            plain[..4],
            [
                "multi_session",
                "snapshot_cursor",
                "ui_answer_ledger",
                "dialog_observer"
            ]
        );
        for wanted in [
            "attach_session",
            "release_attachment",
            "release_caller_aware",
            "reclaim_session",
            "observe_reads",
            "entries_tail",
            UNLOAD_SESSION_CAPABILITY,
            SUBSCRIPTION_LIMITS_CAPABILITY,
            RESOLVE_PATH_CAPABILITY,
        ] {
            assert!(
                plain.iter().any(|capability| capability == wanted),
                "{wanted}"
            );
        }
        assert!(
            !plain
                .iter()
                .any(|capability| capability == MCP_FORM_PRESENTER_CAPABILITY)
        );
        assert!(!plain.iter().any(|capability| capability == "warm"));
        let mut unique = plain.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), plain.len());
        let enforced = multi_session_host_capabilities(true, ToolRuntimeKind::InProcess, &[], true);
        assert_eq!(enforced[4], MCP_FORM_PRESENTER_CAPABILITY);
        assert!(enforced.iter().any(|capability| capability == "warm"));
    }
    #[test]
    fn a_negotiated_tool_runtime_capability_is_dropped_and_the_host_answer_stands() {
        let negotiated = [
            "tool_runtime:child",
            "placement:hostcall",
            "offload_api",
            MCP_FORM_PRESENTER_CAPABILITY,
            "warm",
        ]
        .map(str::to_owned);
        let capabilities = host_tool_runtime_capabilities(ToolRuntimeKind::InProcess, &negotiated);
        assert_eq!(
            capabilities,
            vec!["tool_runtime:in-process", "offload_api", "warm"]
        );
        assert!(!is_host_owned_tool_runtime_capability("warm"));
    }
}
