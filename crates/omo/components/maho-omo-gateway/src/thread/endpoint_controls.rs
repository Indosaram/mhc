//! Endpoint accepted session controls (`thread/endpoint-controls.ts`).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::endpoint_registry::EndpointKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadControl {
    Send,
    Read,
    Rename,
    SetModel,
    SetReasoning,
    Interrupt,
}

impl ThreadControl {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Read => "read",
            Self::Rename => "rename",
            Self::SetModel => "set_model",
            Self::SetReasoning => "set_reasoning",
            Self::Interrupt => "interrupt",
        }
    }
}

pub const ALL_CONTROLS: &[ThreadControl] = &[
    ThreadControl::Send,
    ThreadControl::Read,
    ThreadControl::Rename,
    ThreadControl::SetModel,
    ThreadControl::SetReasoning,
    ThreadControl::Interrupt,
];

fn required_commands(control: ThreadControl) -> &'static [&'static str] {
    match control {
        ThreadControl::Send => &["wake"],
        ThreadControl::Read => &["get_messages"],
        ThreadControl::Rename => &["set_session_name"],
        ThreadControl::SetModel => &["get_available_models", "set_model"],
        ThreadControl::SetReasoning => &["get_available_thinking_levels", "set_thinking_level"],
        ThreadControl::Interrupt => &["interrupt"],
    }
}

/// Legacy TUI commands when a terminal control endpoint lists no `commands`.
pub fn legacy_tui_commands() -> HashSet<String> {
    [
        "get_protocol_info",
        "list_sessions",
        "get_state",
        "get_messages",
        "set_session_name",
        "wake",
        "subscribe",
        "extension_ui_response",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub fn tui_commands_from(protocol_info: &Value) -> HashSet<String> {
    let Some(commands) = protocol_info.get("commands").and_then(Value::as_array) else {
        return legacy_tui_commands();
    };
    commands
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

pub fn accepted_controls(
    kind: EndpointKind,
    tui_commands: Option<&HashSet<String>>,
) -> Vec<ThreadControl> {
    if kind != EndpointKind::Tui {
        return ALL_CONTROLS.to_vec();
    }
    let owned;
    let accepted = match tui_commands {
        Some(set) => set,
        None => {
            owned = legacy_tui_commands();
            &owned
        }
    };
    ALL_CONTROLS
        .iter()
        .copied()
        .filter(|ctrl| {
            required_commands(*ctrl)
                .iter()
                .all(|cmd| accepted.contains(*cmd))
        })
        .collect()
}
