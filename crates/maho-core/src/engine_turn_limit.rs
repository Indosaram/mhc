//! Port of senpi packages/coding-agent/src/core/engine-turn-limit.ts.
//!
//! Bounds on turns the engine starts without a user message (senpi#2967). Session entries are untyped
//! JSON, matching session_manager. The reducer is a plain `add` fold: mhc has no `HistoryReducer` module.

use serde_json::Value;

use crate::manual_continue::MANUAL_CONTINUE_CUSTOM_TYPE;

pub const MAX_ENGINE_TURNS_PER_USER_INPUT: usize = 150;
pub const MAX_ENGINE_TURNS_PER_RESUME_WINDOW: usize = 8;
pub const MAX_TOOL_FREE_ENGINE_TURNS_PER_WINDOW: usize = 12;
pub const ENGINE_TURN_WINDOW_MS: i64 = 60_000;

pub const CONTINUE_FROM_LEAF_CUSTOM_TYPE: &str = "continue-from-leaf";
/// Appended when the engine starts a turn no user message asked for; the limits count these.
pub const ENGINE_TURN_START_ENTRY_TYPE: &str = "engine-turn-start";
pub const ENGINE_TURN_LIMIT_ENTRY_TYPE: &str = "engine-turn-limit";
pub const ENGINE_TURN_LIMIT_EVENT: &str = "engine:turn-limit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineTurnStopReason {
    PerUserInput,
    ToolFreeRate,
    ResumeWindow,
}

impl EngineTurnStopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerUserInput => "per-user-input",
            Self::ToolFreeRate => "tool-free-rate",
            Self::ResumeWindow => "resume-window",
        }
    }
}

/// A limit of 0 turns that limit off; `max_per_resume_window` of `None` is off too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTurnLimits {
    pub max_per_user_input: usize,
    pub max_tool_free_per_minute: usize,
    pub max_per_resume_window: Option<usize>,
}

/// An engine turn this process started since the user's last message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTurnStartRecord {
    pub at: i64,
    pub tool_used: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTurnStop {
    pub reason: EngineTurnStopReason,
    pub since_user_input: usize,
    pub tool_free_in_window: usize,
}

pub fn engine_turn_limit_notice(stop: &EngineTurnStop) -> String {
    match stop.reason {
        EngineTurnStopReason::ResumeWindow => format!(
            "Paused: the agent started {} turns on its own in this resume window. Send any message to continue.",
            stop.since_user_input
        ),
        EngineTurnStopReason::PerUserInput => format!(
            "Paused: the agent started {} turns on its own since your last message. Send any message to continue.",
            stop.since_user_input
        ),
        EngineTurnStopReason::ToolFreeRate => format!(
            "Paused: the agent started {} turns on its own in the last minute without doing any work. Send any message to continue.",
            stop.tool_free_in_window
        ),
    }
}

/// A `.` manual continue or a continue-from-leaf is the user's own request, not an automatic turn.
pub fn is_user_directed_turn(custom_type: &str) -> bool {
    custom_type == MANUAL_CONTINUE_CUSTOM_TYPE || custom_type == CONTINUE_FROM_LEAF_CUSTOM_TYPE || custom_type == "session-resumed"
}

fn entry_type(entry: &Value) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

fn is_user_message(entry: &Value) -> bool {
    match entry_type(entry) {
        Some("custom_message") => entry
            .get("customType")
            .and_then(Value::as_str)
            .is_some_and(is_user_directed_turn),
        Some("message") => entry.pointer("/message/role").and_then(Value::as_str) == Some("user"),
        _ => false,
    }
}

fn is_engine_turn_start(entry: &Value) -> bool {
    entry_type(entry) == Some("custom")
        && entry.get("customType").and_then(Value::as_str) == Some(ENGINE_TURN_START_ENTRY_TYPE)
}

fn calls_a_tool(entry: &Value) -> bool {
    entry_type(entry) == Some("message")
        && entry.pointer("/message/role").and_then(Value::as_str) == Some("assistant")
        && entry
            .pointer("/message/content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
            })
}

/// The session's engine turns since the user's last message, in order: when each started and whether its
/// reply called a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineTurnHistory {
    pub starts: Vec<EngineTurnStartRecord>,
}

impl EngineTurnHistory {
    /// The `engineTurnHistory` reducer step (senpi#2537): a user message resets; an engine-turn start
    /// pushes; a tool call marks the latest start.
    pub fn add(&mut self, entry: &Value) {
        if is_user_message(entry) {
            self.starts.clear();
        } else if is_engine_turn_start(entry) {
            let at = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .map_or(0, |time| time.timestamp_millis());
            self.starts.push(EngineTurnStartRecord {
                at,
                tool_used: false,
            });
        } else if calls_a_tool(entry)
            && let Some(latest) = self.starts.last_mut()
        {
            latest.tool_used = true;
        }
    }

    pub fn from_entries<'a>(entries: impl IntoIterator<Item = &'a Value>) -> Self {
        let mut history = Self::default();
        for entry in entries {
            history.add(entry);
        }
        history
    }
}

/// The `engineTurnHistory` reducer (senpi#2537), for `SessionManager::reduce_history`.
pub static ENGINE_TURN_HISTORY: crate::session_history_reduce::HistoryReducer<EngineTurnHistory> =
    crate::session_history_reduce::HistoryReducer {
        initial: EngineTurnHistory::default,
        add: |history, entry| history.add(entry),
    };

/// Whether one more engine-started turn may begin. Only turns since the user's last message count, so any
/// message the user sends lifts a pause at once. The per-minute breaker counts only engine turns whose reply
/// called no tool.
pub fn engine_turn_stop(
    history: &EngineTurnHistory,
    now: i64,
    limits: &EngineTurnLimits,
    in_process: &[EngineTurnStartRecord],
) -> Option<EngineTurnStop> {
    let in_window = |at: i64| now - at < ENGINE_TURN_WINDOW_MS;
    let from_history = history
        .starts
        .iter()
        .filter(|start| in_window(start.at) && !start.tool_used)
        .count();
    // The session record and this process's own record: the larger count wins, so a file that refuses
    // writes cannot lift the bound for the process that is running the turns.
    let since_user_input = history.starts.len().max(in_process.len());
    let tool_free_in_window = from_history.max(
        in_process
            .iter()
            .filter(|record| !record.tool_used && in_window(record.at))
            .count(),
    );
    let stop = |reason| {
        Some(EngineTurnStop {
            reason,
            since_user_input,
            tool_free_in_window,
        })
    };
    let resume_limit = limits.max_per_resume_window.unwrap_or(0);
    if resume_limit > 0 && since_user_input >= resume_limit {
        return stop(EngineTurnStopReason::ResumeWindow);
    }
    if limits.max_per_user_input > 0 && since_user_input >= limits.max_per_user_input {
        return stop(EngineTurnStopReason::PerUserInput);
    }
    if limits.max_tool_free_per_minute > 0 && tool_free_in_window >= limits.max_tool_free_per_minute
    {
        return stop(EngineTurnStopReason::ToolFreeRate);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits(resume: Option<usize>) -> EngineTurnLimits {
        EngineTurnLimits {
            max_per_user_input: MAX_ENGINE_TURNS_PER_USER_INPUT,
            max_tool_free_per_minute: MAX_TOOL_FREE_ENGINE_TURNS_PER_WINDOW,
            max_per_resume_window: resume,
        }
    }

    fn start(timestamp: &str) -> Value {
        json!({"type": "custom", "customType": ENGINE_TURN_START_ENTRY_TYPE, "timestamp": timestamp})
    }

    #[test]
    fn history_resets_on_user_message_and_marks_tool_use() {
        let entries = vec![
            start("2026-01-01T00:00:00.000Z"),
            json!({"type": "message", "message": {"role": "assistant", "content": [{"type": "toolCall"}]}}),
            start("2026-01-01T00:00:01.000Z"),
        ];
        let history = EngineTurnHistory::from_entries(&entries);
        assert_eq!(
            history
                .starts
                .iter()
                .map(|s| s.tool_used)
                .collect::<Vec<_>>(),
            vec![true, false]
        );
        let mut reset = history;
        reset.add(&json!({"type": "message", "message": {"role": "user", "content": "hi"}}));
        assert!(reset.starts.is_empty());
        reset.add(&start("2026-01-01T00:00:02.000Z"));
        reset.add(&json!({"type": "custom_message", "customType": "manual-continue"}));
        assert!(reset.starts.is_empty());
    }

    #[test]
    fn resume_window_is_checked_first_and_zero_disables() {
        let history = EngineTurnHistory {
            starts: (0..8)
                .map(|i| EngineTurnStartRecord {
                    at: i,
                    tool_used: true,
                })
                .collect(),
        };
        let stop = engine_turn_stop(
            &history,
            1_000_000,
            &limits(Some(MAX_ENGINE_TURNS_PER_RESUME_WINDOW)),
            &[],
        );
        assert_eq!(
            stop.map(|s| (s.reason, s.since_user_input)),
            Some((EngineTurnStopReason::ResumeWindow, 8))
        );
        assert_eq!(
            engine_turn_stop(&history, 1_000_000, &limits(Some(0)), &[]),
            None
        );
        assert_eq!(
            engine_turn_stop(&history, 1_000_000, &limits(None), &[]),
            None
        );
    }

    #[test]
    fn tool_free_rate_counts_only_recent_toolless_turns_and_in_process_wins() {
        let history = EngineTurnHistory::default();
        let in_process: Vec<_> = (0..12)
            .map(|i| EngineTurnStartRecord {
                at: 1_000 + i,
                tool_used: false,
            })
            .collect();
        let stop = engine_turn_stop(&history, 2_000, &limits(None), &in_process);
        assert_eq!(
            stop.map(|s| s.reason),
            Some(EngineTurnStopReason::ToolFreeRate)
        );
        assert_eq!(
            engine_turn_stop(
                &history,
                1_000 + ENGINE_TURN_WINDOW_MS + 100,
                &limits(None),
                &in_process
            ),
            None
        );
    }

    #[test]
    fn notices_match_upstream_wording() {
        let stop = EngineTurnStop {
            reason: EngineTurnStopReason::ResumeWindow,
            since_user_input: 8,
            tool_free_in_window: 0,
        };
        assert_eq!(
            engine_turn_limit_notice(&stop),
            "Paused: the agent started 8 turns on its own in this resume window. Send any message to continue."
        );
    }
}
