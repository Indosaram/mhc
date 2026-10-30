//! Port of upstream `test/forward-compat.test.ts`.
//!
//! A host and an engine from different releases still talk: codes, actions and capability fields
//! one side does not know are tolerated, never fatal (ErrorCode amendment 1, #11461).

use std::sync::{Arc, Mutex};

use senpi_desktop_protocol::{AuditCode, AuditEvent, JsonRpcErrorData};
use senpi_desktop_service::{NotificationHub, is_desktop_capabilities, parse_error_data};
use serde_json::{Value, json};

fn click() -> Value {
    json!({ "action": "click", "target": "101", "delivery": "background", "durationMs": 3 })
}

fn with(base: Value, fields: Value) -> Value {
    let mut merged = base;
    for (key, value) in fields.as_object().expect("fields") {
        merged[key] = value.clone();
    }
    merged
}

fn dispatch_audit(params: &Value) -> (Vec<AuditEvent>, Vec<String>) {
    let hub = NotificationHub::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let event_sink = events.clone();
    let error_sink = errors.clone();
    let _audit = hub.on_audit(move |event| event_sink.lock().expect("events").push(event));
    let _errors =
        hub.on_error(move |error| error_sink.lock().expect("errors").push(error.to_string()));
    hub.dispatch("audit", params);
    let events = events.lock().expect("events").clone();
    let errors = errors.lock().expect("errors").clone();
    (events, errors)
}

fn event(value: Value) -> AuditEvent {
    serde_json::from_value(value).expect("audit event")
}

#[test]
fn keeps_an_event_whose_code_this_host_does_not_know_as_unknown() {
    let (events, errors) = dispatch_audit(&with(click(), json!({ "code": "SomeFutureCode" })));
    let mut expected = event(click());
    expected.code = Some(AuditCode::UNKNOWN);
    assert_eq!((events, errors), (vec![expected], vec![]));
}

#[test]
fn keeps_an_event_whose_action_this_host_does_not_know_as_unknown() {
    let (events, errors) = dispatch_audit(&with(
        click(),
        json!({ "action": "window.someFutureMethod", "code": null }),
    ));
    let mut expected = event(click());
    expected.action = "unknown".to_owned();
    assert_eq!((events, errors), (vec![expected], vec![]));
}

#[test]
fn passes_the_amended_codes_through_unchanged() {
    let codes = [
        "UserIntervened",
        "AxUnconfirmed",
        "SpaceUnsupported",
        "SpaceMoveDenied",
    ];
    let seen: Vec<Option<&str>> = codes
        .iter()
        .map(|code| {
            dispatch_audit(&with(click(), json!({ "code": code })))
                .0
                .first()
                .and_then(|event| event.code)
                .map(AuditCode::as_str)
        })
        .collect();
    assert_eq!(seen, codes.map(Some).to_vec());
}

#[test]
fn still_reports_a_structurally_malformed_event() {
    let (events, errors) = dispatch_audit(&with(click(), json!({ "durationMs": "3" })));
    assert_eq!(
        (events.len(), errors),
        (
            0,
            vec!["desktop engine notification \"audit\" is malformed".to_owned()]
        )
    );
}

#[test]
fn reads_an_amended_code_as_itself_and_an_unknown_code_as_no_data() {
    let amended = parse_error_data(&json!({ "code": "UserIntervened", "hint": null }));
    assert!(matches!(
        amended,
        Some(JsonRpcErrorData::Engine(ref data)) if data.code.as_str() == "UserIntervened" && data.hint.is_none()
    ));
    assert_eq!(
        parse_error_data(&json!({ "code": "SomeFutureCode", "hint": null })),
        None
    );
}

fn capabilities() -> Value {
    json!({
        "backend": "fake",
        "capture": true,
        "input": true,
        "ax": true,
        "backgroundWindowInput": true,
        "deliveryModes": ["background"],
        "capturePermission": "granted",
        "inputPermission": "granted",
        "axPermission": "granted",
        "displayCount": 1,
        "focusGuard": true,
        "stopPath": "global",
        "screenLocked": false,
    })
}

#[test]
fn accepts_capabilities_with_and_without_intervention_watch_and_display_asleep() {
    let newer = with(
        capabilities(),
        json!({ "interventionWatch": "on", "displayAsleep": false }),
    );
    assert_eq!(
        [
            is_desktop_capabilities(&capabilities()),
            is_desktop_capabilities(&newer)
        ],
        [true, true]
    );
}

#[test]
fn rejects_mistyped_intervention_watch_or_display_asleep() {
    let wrong = [
        with(capabilities(), json!({ "interventionWatch": true })),
        with(capabilities(), json!({ "displayAsleep": "no" })),
    ];
    assert_eq!(
        wrong.map(|value| is_desktop_capabilities(&value)),
        [false, false]
    );
}
