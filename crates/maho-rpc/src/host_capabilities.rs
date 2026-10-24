//! Tool-runtime capabilities of a multi-session host (senpi `host-capabilities.ts` +
//! `cli/tool-runtime.ts`, senpi `d911de46a`).
//!
//! The host alone says where tool work runs: it advertises `tool_runtime:<kind>`, the placements the tool
//! child already serves and `offload_api`, and a capability a client negotiates under one of those names is
//! dropped, so there are never two answers.

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
/// negotiated tool-runtime capability is filtered out (senpi `multiSessionHostCapabilities`).
pub fn host_tool_runtime_capabilities(kind: ToolRuntimeKind, negotiated: &[String]) -> Vec<String> {
    let mut capabilities = tool_runtime_capabilities(kind);
    capabilities.extend(
        negotiated
            .iter()
            .filter(|capability| !is_host_owned_tool_runtime_capability(capability))
            .cloned(),
    );
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
    #[test]
    fn a_negotiated_tool_runtime_capability_is_dropped_and_the_host_answer_stands() {
        let negotiated = [
            "tool_runtime:child",
            "placement:hostcall",
            "offload_api",
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
