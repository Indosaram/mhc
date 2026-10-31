use senpi_desktop_core::protocol_params::CaptureParams;
use senpi_desktop_core::types::DesktopSessionOptions;
use serde_json::json;

use super::reads_or_acts_on_an_app;
use crate::request::Op;
use crate::test_support::harness;

fn capture(target: &str) -> Op {
    Op::Capture(CaptureParams {
        target: target.to_owned(),
        caps: None,
    })
}

#[test]
fn only_requests_aimed_at_a_window_or_an_element_read_or_act_on_an_app() {
    for (op, app) in [
        (capture("101"), true),
        (capture("desktop"), false),
        (Op::AxFocused, true),
        (Op::Displays, false),
        (Op::Windows, false),
        (Op::ClipboardRead, false),
    ] {
        assert_eq!(reads_or_acts_on_an_app(&op), app, "{op:?}");
    }
}

#[test]
fn the_first_request_into_an_app_starts_the_watch_once() {
    // Given
    let mut harness = harness(&json!({}));
    harness.process(Op::Displays).unwrap();
    harness.process(Op::Windows).unwrap();
    harness.capture("desktop");
    assert_eq!(harness.watches.count(), 0, "nothing named an app yet");
    // When
    harness.capture("101");
    harness.focused_ref();
    harness.capture("101");
    // Then
    assert_eq!(harness.watches.count(), 1);
    assert!(harness.worker.watching_user_input);
}

#[test]
fn an_accessibility_read_starts_the_watch() {
    let mut harness = harness(&json!({}));
    harness.focused_ref();
    assert_eq!(harness.watches.count(), 1);
}

/// mhc base: every `session.open` builds a new backend (upstream keeps it on
/// a cursor-only change, which needs `showCursor`), so this keeps the
/// new-backend half of upstream's test.
#[test]
fn a_new_backend_starts_its_own_watch() {
    // Given
    let mut harness = harness(&json!({}));
    harness.capture("101");
    // When: reopening builds a new backend, whose watch has not started.
    harness.worker.open(DesktopSessionOptions {
        allow_host_relay_only_stop: true,
        ..DesktopSessionOptions::default()
    });
    assert!(!harness.worker.watching_user_input);
    harness.capture("101");
    // Then
    assert_eq!(harness.watches.count(), 2);
}
