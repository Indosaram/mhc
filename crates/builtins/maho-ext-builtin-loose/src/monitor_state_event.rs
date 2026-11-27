use serde_json::Value;
pub const TERMINAL_MONITOR_STATE_EVENT: &str = "terminal_monitor_state";
pub const TERMINAL_MONITOR_ENDED_EVENT: &str = "terminal_monitor_ended";
pub const WAKE_SOURCE_STATE_EVENT: &str = "wake_source_state";
pub const CONTINUATION_HOLD_STATE_EVENT: &str = "continuation_hold_state";
pub const JOB_WIDGET_STATE_EVENT: &str = "job_widget_state";
pub const MONITOR_NOTIFICATION_CUSTOM_TYPE: &str = "senpi-monitor:notification";
pub const TERMINAL_BACKGROUND_AGE_EVENT: &str = "terminal_background_age";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalBackgroundAgeKind {
    Running,
    Reaped,
    ReapFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalBackgroundAgeStop {
    pub bash_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalBackgroundAgeEvent {
    pub kind: TerminalBackgroundAgeKind,
    pub id: String,
    pub description: String,
    pub command: Option<String>,
    pub started_at_ms: f64,
    pub age_ms: f64,
    pub age_minutes: f64,
    pub auto_reap_minutes: Option<f64>,
    pub reason: Option<String>,
    pub stop: TerminalBackgroundAgeStop,
}
fn source(value: &Value) -> bool {
    value
        .get("source")
        .and_then(Value::as_str)
        .is_some_and(|source| !source.is_empty())
}
fn finite(value: Option<&Value>) -> bool {
    value.and_then(Value::as_f64).is_some_and(f64::is_finite)
}
fn integer(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_f64)
        .is_some_and(|value| value.is_finite() && value.fract() == 0.0)
}
fn nonnegative_integer(value: Option<&Value>) -> bool {
    integer(value)
        && value
            .and_then(Value::as_f64)
            .is_some_and(|value| value >= 0.0)
}
pub fn is_wake_source_state_event(value: &Value) -> bool {
    value.is_object() && source(value) && finite(value.get("activeCount"))
}
pub fn is_continuation_hold_state_event(value: &Value) -> bool {
    value.is_object() && source(value) && value.get("active").is_some_and(Value::is_boolean)
}
pub fn is_terminal_monitor_state_event(value: &Value) -> bool {
    value.is_object() && nonnegative_integer(value.get("activeCount"))
}
pub fn is_terminal_monitor_ended_event(value: &Value) -> bool {
    value.is_object()
        && value.get("id").is_some_and(Value::is_string)
        && value.get("description").is_some_and(Value::is_string)
        && finite(value.get("startedAtMs"))
        && finite(value.get("endedAtMs"))
        && value
            .get("exitCode")
            .is_some_and(|code| code.is_null() || integer(Some(code)))
        && nonnegative_integer(value.get("fireCount"))
        && matches!(
            value.get("reason").and_then(Value::as_str),
            Some("exit" | "timeout" | "killed" | "disposed")
        )
}

pub fn is_terminal_background_age_event(value: &Value) -> bool {
    value.is_object()
        && matches!(
            value.get("kind").and_then(Value::as_str),
            Some("running" | "reaped" | "reap_failed")
        )
        && value.get("id").is_some_and(Value::is_string)
        && value.get("description").is_some_and(Value::is_string)
        && value.get("command").is_none_or(Value::is_string)
        && finite(value.get("startedAtMs"))
        && finite(value.get("ageMs"))
        && finite(value.get("ageMinutes"))
        && value
            .get("autoReapMinutes")
            .is_some_and(|v| v.is_null() || finite(Some(v)))
        && value.get("reason").is_none_or(Value::is_string)
        && value
            .get("stop")
            .and_then(|stop| stop.as_object())
            .is_some_and(|stop| {
                stop.get("tool").and_then(Value::as_str) == Some("kill_bash")
                    && stop
                        .get("args")
                        .and_then(|args| args.get("bash_id"))
                        .is_some_and(Value::is_string)
            })
}
