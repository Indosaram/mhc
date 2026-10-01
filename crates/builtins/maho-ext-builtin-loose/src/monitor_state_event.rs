use serde_json::Value;
pub const TERMINAL_MONITOR_STATE_EVENT:&str="terminal_monitor_state";
pub const TERMINAL_MONITOR_ENDED_EVENT:&str="terminal_monitor_ended";
pub const WAKE_SOURCE_STATE_EVENT:&str="wake_source_state";
pub const CONTINUATION_HOLD_STATE_EVENT:&str="continuation_hold_state";
fn source(value:&Value)->bool{value.get("source").and_then(Value::as_str).is_some_and(|source|!source.is_empty())}
fn finite(value:Option<&Value>)->bool{value.and_then(Value::as_f64).is_some_and(f64::is_finite)}
fn integer(value:Option<&Value>)->bool{value.and_then(Value::as_f64).is_some_and(|value|value.is_finite()&&value.fract()==0.0)}
fn nonnegative_integer(value:Option<&Value>)->bool{integer(value)&&value.and_then(Value::as_f64).is_some_and(|value|value>=0.0)}
pub fn is_wake_source_state_event(value:&Value)->bool{value.is_object()&&source(value)&&finite(value.get("activeCount"))}
pub fn is_continuation_hold_state_event(value:&Value)->bool{value.is_object()&&source(value)&&value.get("active").is_some_and(Value::is_boolean)}
pub fn is_terminal_monitor_state_event(value:&Value)->bool{value.is_object()&&nonnegative_integer(value.get("activeCount"))}
pub fn is_terminal_monitor_ended_event(value:&Value)->bool{
    value.is_object()&&value.get("id").is_some_and(Value::is_string)&&value.get("description").is_some_and(Value::is_string)&&finite(value.get("startedAtMs"))&&finite(value.get("endedAtMs"))&&value.get("exitCode").is_some_and(|code|code.is_null()||integer(Some(code)))&&nonnegative_integer(value.get("fireCount"))&&matches!(value.get("reason").and_then(Value::as_str),Some("exit"|"timeout"|"killed"|"disposed"))
}
