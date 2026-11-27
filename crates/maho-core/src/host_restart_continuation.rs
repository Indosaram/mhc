//! Port of senpi packages/coding-agent/src/core/host-restart-continuation.ts.
//!
//! What a host restart released on a session that was live when the previous host ended (senpi#7020,
//! senpi#7079).

use serde_json::Value;

use crate::engine_turn_hold::{INTERRUPTED_RUN_HOLD_KEY, SessionHold};
use crate::engine_turn_limit::{
    ENGINE_TURN_START_ENTRY_TYPE, is_user_directed_turn, resets_engine_turn_history,
};
use crate::extensions::session_control_types::SESSION_CONTROL_DELIVERY_TYPE;
use crate::stop_resume_directive::STOP_RESUME_CUSTOM_TYPE;

/// The hold a superseded host's drain puts on a session nobody is attached to (senpi#3114).
pub const HOST_DRAIN_HOLD_KEY: &str = "host-drain";

/// Consecutive continuations of a cut run without a person's input or a peer delivery in between (senpi#7079).
pub const RESTART_CONTINUATION_LIMIT: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartHoldReason {
    RepeatedRestartNoProgress,
    RepeatedRestartLimit,
}

impl RestartHoldReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepeatedRestartNoProgress => "repeated_restart_no_progress",
            Self::RepeatedRestartLimit => "repeated_restart_limit",
        }
    }
}

enum RestartVerdict {
    Continue,
    Hold(Option<RestartHoldReason>),
}

impl RestartVerdict {
    fn is_continue(&self) -> bool {
        matches!(self, Self::Continue)
    }
}

fn is_restart_continuation(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("custom_message")
        && entry.get("customType").and_then(Value::as_str) == Some(STOP_RESUME_CUSTOM_TYPE)
        && entry
            .pointer("/details/resumeCause")
            .and_then(Value::as_str)
            == Some("interrupted")
}

/// A completed model response or a recorded tool result; a result the restore wrote for a cut tool is not progress.
fn is_progress(entry: &Value) -> bool {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return false;
    }
    let Some(message) = entry.get("message") else {
        return false;
    };
    match message.get("role").and_then(Value::as_str) {
        Some("assistant") => matches!(
            message.get("stopReason").and_then(Value::as_str),
            Some("stop" | "length" | "toolUse")
        ),
        Some("toolResult") => message.pointer("/details/interrupted").is_none(),
        _ => false,
    }
}

/// A person's input or a peer delivery: what a session that keeps continuing after restarts never sends itself.
fn is_new_input(entry: &Value) -> bool {
    match entry.get("type").and_then(Value::as_str) {
        Some("message") => entry.pointer("/message/role").and_then(Value::as_str) == Some("user"),
        Some("custom_message") => {
            let ct = entry
                .get("customType")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ct == SESSION_CONTROL_DELIVERY_TYPE
                || resets_engine_turn_history(ct, entry.get("details"))
        }
        _ => false,
    }
}

fn restart_verdict(branch: &[Value]) -> RestartVerdict {
    let mut continuations = 0usize;
    let mut progressed = false;
    let mut engine_started: Option<bool> = None;

    for entry in branch.iter().rev() {
        if engine_started.is_some() {
            break;
        }
        if continuations == 0 && is_progress(entry) {
            progressed = true;
        }
        if is_restart_continuation(entry) {
            continuations += 1;
        } else if (entry.get("type").and_then(Value::as_str) == Some("message")
            && entry.pointer("/message/role").and_then(Value::as_str) == Some("user"))
            || (entry.get("type").and_then(Value::as_str) == Some("custom_message")
                && entry
                    .get("customType")
                    .and_then(Value::as_str)
                    .is_some_and(is_user_directed_turn))
        {
            engine_started = Some(false);
        } else if entry.get("type").and_then(Value::as_str) == Some("custom")
            && entry.get("customType").and_then(Value::as_str) == Some(ENGINE_TURN_START_ENTRY_TYPE)
        {
            engine_started = Some(true);
        }
    }

    if engine_started != Some(true) {
        return RestartVerdict::Hold(None);
    }
    if continuations == 0 {
        return RestartVerdict::Continue;
    }
    if !progressed {
        return RestartVerdict::Hold(Some(RestartHoldReason::RepeatedRestartNoProgress));
    }
    let mut since_input = 0usize;
    for entry in branch.iter().rev() {
        if is_new_input(entry) {
            break;
        }
        if is_restart_continuation(entry) {
            since_input += 1;
        }
    }
    if since_input >= RESTART_CONTINUATION_LIMIT {
        RestartVerdict::Hold(Some(RestartHoldReason::RepeatedRestartLimit))
    } else {
        RestartVerdict::Continue
    }
}

pub fn restart_hold_data(continued: bool, branch: impl FnOnce() -> Vec<Value>) -> Option<Value> {
    if !continued {
        return None;
    }
    match restart_verdict(&branch()) {
        RestartVerdict::Hold(Some(reason)) => {
            Some(serde_json::json!({ "reason": reason.as_str() }))
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestartRelease {
    Resume { interrupted_run: bool },
    Release { keys: Vec<String> },
}

pub fn restart_release(
    holds: &[SessionHold],
    branch: impl FnOnce() -> Vec<Value>,
) -> Option<RestartRelease> {
    let mut restart_keys = vec![HOST_DRAIN_HOLD_KEY];
    if holds.iter().any(|h| h.key == INTERRUPTED_RUN_HOLD_KEY)
        && restart_verdict(&branch()).is_continue()
    {
        restart_keys.push(INTERRUPTED_RUN_HOLD_KEY);
    }
    let restart_holds: Vec<&SessionHold> = holds
        .iter()
        .filter(|h| restart_keys.contains(&h.key.as_str()))
        .collect();
    if restart_holds.is_empty() {
        return None;
    }
    if restart_holds.len() == holds.len() {
        return Some(RestartRelease::Resume {
            interrupted_run: restart_keys.contains(&INTERRUPTED_RUN_HOLD_KEY),
        });
    }
    if holds.iter().any(|h| h.key == HOST_DRAIN_HOLD_KEY) {
        Some(RestartRelease::Release {
            keys: vec![HOST_DRAIN_HOLD_KEY.to_owned()],
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_turn_hold::HoldKind;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn hold(key: &str) -> SessionHold {
        SessionHold {
            kind: HoldKind::SessionHold,
            key: key.to_owned(),
            since: String::new(),
            data: None,
            source_counts: BTreeMap::new(),
        }
    }

    #[test]
    fn continues_progressed_engine_run_until_limit() {
        let engine_start = json!({"type": "custom", "customType": ENGINE_TURN_START_ENTRY_TYPE});
        let cont = json!({
            "type": "custom_message",
            "customType": STOP_RESUME_CUSTOM_TYPE,
            "details": {"resumeCause": "interrupted"}
        });
        let progress =
            json!({"type": "message", "message": {"role": "assistant", "stopReason": "toolUse"}});

        // Engine-started run with no continuations yet -> resumes.
        let holds = vec![hold(HOST_DRAIN_HOLD_KEY), hold(INTERRUPTED_RUN_HOLD_KEY)];
        assert_eq!(
            restart_release(&holds, || vec![engine_start.clone()]),
            Some(RestartRelease::Resume {
                interrupted_run: true
            })
        );

        // Continuation with no progress -> holds with repeated_restart_no_progress.
        assert_eq!(
            restart_hold_data(true, || vec![engine_start.clone(), cont.clone()]),
            Some(json!({"reason": "repeated_restart_no_progress"}))
        );

        // Continuation with progress under limit -> continues.
        assert_eq!(
            restart_hold_data(true, || vec![
                engine_start.clone(),
                cont.clone(),
                progress.clone()
            ]),
            None
        );

        // Reaches limit of 3 continuations without user input -> holds with repeated_restart_limit.
        let at_limit = vec![
            engine_start,
            cont.clone(),
            progress.clone(),
            cont.clone(),
            progress.clone(),
            cont,
            progress,
        ];
        assert_eq!(
            restart_hold_data(true, || at_limit),
            Some(json!({"reason": "repeated_restart_limit"}))
        );
    }
}
