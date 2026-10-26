//! Computer call classification and allowlist policies mirrored from `src/call.ts`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One allowlisted method invocation in a direct computer call chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputerCallStep {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<Value>>,
}

/// Approval tier a direct computer helper needs: inspection reads, input and mutation execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ComputerCallPolicy {
    Read,
    Exec,
}

/// Method policies map.
pub type MethodPolicies = &'static [(&'static str, ComputerCallPolicy)];

/// Helpers callable on the `desktop` root; `window` and `ref` also anchor one-hop handle chains.
pub const DESKTOP_METHODS: &[(&str, ComputerCallPolicy)] = &[
    ("capabilities", ComputerCallPolicy::Read),
    ("displays", ComputerCallPolicy::Read),
    ("windows", ComputerCallPolicy::Read),
    ("window", ComputerCallPolicy::Read),
    ("focusedWindow", ComputerCallPolicy::Read),
    ("screenshot", ComputerCallPolicy::Read),
    ("click", ComputerCallPolicy::Exec),
    ("doubleClick", ComputerCallPolicy::Exec),
    ("move", ComputerCallPolicy::Exec),
    ("drag", ComputerCallPolicy::Exec),
    ("scroll", ComputerCallPolicy::Exec),
    ("type", ComputerCallPolicy::Exec),
    ("press", ComputerCallPolicy::Exec),
    ("elementAt", ComputerCallPolicy::Read),
    ("focusedElement", ComputerCallPolicy::Read),
    ("ref", ComputerCallPolicy::Read),
    ("clipboard.read", ComputerCallPolicy::Read),
    ("clipboard.write", ComputerCallPolicy::Exec),
];

/// Helpers callable on a window handle resolved through `desktop.window(id)`.
pub const WINDOW_METHODS: &[(&str, ComputerCallPolicy)] = &[
    ("screenshot", ComputerCallPolicy::Read),
    ("click", ComputerCallPolicy::Exec),
    ("doubleClick", ComputerCallPolicy::Exec),
    ("move", ComputerCallPolicy::Exec),
    ("drag", ComputerCallPolicy::Exec),
    ("scroll", ComputerCallPolicy::Exec),
    ("type", ComputerCallPolicy::Exec),
    ("press", ComputerCallPolicy::Exec),
    ("raise", ComputerCallPolicy::Exec),
    ("ax", ComputerCallPolicy::Read),
    ("find", ComputerCallPolicy::Read),
    ("ref", ComputerCallPolicy::Read),
];

/// Helpers callable on an AX element handle resolved through `desktop.ref(ref)`.
pub const ELEMENT_METHODS: &[(&str, ComputerCallPolicy)] = &[
    ("value", ComputerCallPolicy::Read),
    ("setValue", ComputerCallPolicy::Exec),
    ("bounds", ComputerCallPolicy::Read),
    ("attributes", ComputerCallPolicy::Read),
    ("actions", ComputerCallPolicy::Read),
    ("perform", ComputerCallPolicy::Exec),
    ("press", ComputerCallPolicy::Exec),
    ("click", ComputerCallPolicy::Exec),
    ("focus", ComputerCallPolicy::Exec),
    ("parent", ComputerCallPolicy::Read),
    ("children", ComputerCallPolicy::Read),
];

struct HandleRoot {
    label: &'static str,
    methods: &'static [(&'static str, ComputerCallPolicy)],
}

const HANDLE_ROOTS: &[(&str, HandleRoot)] = &[
    ("window", HandleRoot { label: "window", methods: WINDOW_METHODS }),
    ("ref", HandleRoot { label: "element", methods: ELEMENT_METHODS }),
];

/// Why a call chain was refused before classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ComputerCallErrorReason {
    Empty,
    TooLong,
    UnknownMethod,
    NotChainable,
}

impl ComputerCallErrorReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooLong => "tooLong",
            Self::UnknownMethod => "unknownMethod",
            Self::NotChainable => "notChainable",
        }
    }
}

/// A call chain that is not an allowlisted one-hop desktop call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ComputerCallError {
    pub reason: ComputerCallErrorReason,
    pub message: String,
}

impl ComputerCallError {
    pub fn new(reason: ComputerCallErrorReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

fn policy_of(methods: &[(&str, ComputerCallPolicy)], method: &str) -> Option<ComputerCallPolicy> {
    methods.iter().find(|(name, _)| *name == method).map(|(_, policy)| *policy)
}

fn describe(methods: &[(&str, ComputerCallPolicy)]) -> String {
    methods.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
}

pub fn classify_computer_call(chain: &[ComputerCallStep]) -> Result<ComputerCallPolicy, ComputerCallError> {
    if chain.is_empty() {
        return Err(ComputerCallError::new(
            ComputerCallErrorReason::Empty,
            "Action 'call' requires a non-empty 'chain'.",
        ));
    }
    let root = &chain[0];
    let step = chain.get(1);
    let rest = if chain.len() > 2 { &chain[2..] } else { &[] };

    if !rest.is_empty() {
        return Err(ComputerCallError::new(
            ComputerCallErrorReason::TooLong,
            "Call chains support one handle hop at most; use computer.run(fn) for longer sequences.",
        ));
    }

    let root_policy = policy_of(DESKTOP_METHODS, &root.method).ok_or_else(|| {
        ComputerCallError::new(
            ComputerCallErrorReason::UnknownMethod,
            format!(
                "Unknown desktop method \"{}\". Desktop helpers support: {}.",
                root.method,
                describe(DESKTOP_METHODS)
            ),
        )
    })?;

    let step = match step {
        Some(s) => s,
        None => return Ok(root_policy),
    };

    let handle = HANDLE_ROOTS.iter().find(|(name, _)| *name == root.method.as_str()).map(|(_, h)| h);
    let handle = match handle {
        Some(h) => h,
        None => {
            return Err(ComputerCallError::new(
                ComputerCallErrorReason::NotChainable,
                format!(
                    "Only desktop.window(id)/desktop.ref(ref) results accept a chained call; got desktop.{}().",
                    root.method
                ),
            ));
        }
    };

    let step_policy = policy_of(handle.methods, &step.method).ok_or_else(|| {
        ComputerCallError::new(
            ComputerCallErrorReason::UnknownMethod,
            format!(
                "Unknown {} method \"{}\". Supported {} methods: {}.",
                handle.label,
                step.method,
                handle.label,
                describe(handle.methods)
            ),
        )
    })?;

    Ok(step_policy)
}

/// Whether every step of a direct computer call is inspection-only, so the call needs read approval.
/// Returns Err(ComputerCallError) for an empty, over-long, unknown, or unchainable chain.
pub fn is_read_only_computer_call(chain: &[ComputerCallStep]) -> Result<bool, ComputerCallError> {
    classify_computer_call(chain).map(|policy| policy == ComputerCallPolicy::Read)
}
