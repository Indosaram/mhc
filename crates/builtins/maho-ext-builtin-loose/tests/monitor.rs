use maho_ext_builtin_loose::monitor_state_event::*;
use serde_json::json;
#[test]
fn wake_fields() {
    assert!(is_wake_source_state_event(
        &json!({"source":"timer","activeCount":-1.5})
    ));
    for value in [
        json!(null),
        json!({"source":"","activeCount":1}),
        json!({"source":"timer","activeCount":"1"}),
    ] {
        assert!(!is_wake_source_state_event(&value));
    }
}
#[test]
fn continuation_fields() {
    assert!(is_continuation_hold_state_event(
        &json!({"source":"goal","active":false})
    ));
    assert!(!is_continuation_hold_state_event(
        &json!({"source":"goal","active":1})
    ));
}
#[test]
fn monitor_integer_fields() {
    assert!(is_terminal_monitor_state_event(
        &json!({"activeCount":2.0,"monitors":[]})
    ));
    for value in [
        json!({"activeCount":-1}),
        json!({"activeCount":1.5}),
        json!({"activeCount":"2"}),
    ] {
        assert!(!is_terminal_monitor_state_event(&value));
    }
}
#[test]
fn ended_fields() {
    let mut value = json!({"id":"","description":"","startedAtMs":0,"endedAtMs":1,"exitCode":null,"fireCount":0,"reason":"exit"});
    assert!(is_terminal_monitor_ended_event(&value));
    value["reason"] = json!("unknown");
    assert!(!is_terminal_monitor_ended_event(&value));
    value["reason"] = json!("disposed");
    value["exitCode"] = json!(1.5);
    assert!(!is_terminal_monitor_ended_event(&value));
}
#[test]
fn background_age_fields_and_constants() {
    assert_eq!(JOB_WIDGET_STATE_EVENT, "job_widget_state");
    assert_eq!(TERMINAL_BACKGROUND_AGE_EVENT, "terminal_background_age");
    let mut value = json!({
        "kind": "running",
        "id": "bash-1",
        "description": "sleep",
        "startedAtMs": 1000,
        "ageMs": 600000,
        "ageMinutes": 10,
        "autoReapMinutes": null,
        "stop": { "tool": "kill_bash", "args": { "bash_id": "bash-1" } }
    });
    assert!(is_terminal_background_age_event(&value));
    value["kind"] = json!("reaped");
    value["autoReapMinutes"] = json!(30);
    assert!(is_terminal_background_age_event(&value));
    value["kind"] = json!("bogus");
    assert!(!is_terminal_background_age_event(&value));
}
