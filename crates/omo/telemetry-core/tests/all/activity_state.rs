use std::fs;

use chrono::DateTime;
use chrono::Utc;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use telemetry_core::DailyActiveCaptureStateInput;
use telemetry_core::PostHogActivityCaptureState;
use telemetry_core::ResolveTelemetryStateDirOptions;
use telemetry_core::TelemetryDiagnosticEvent;
use telemetry_core::TelemetryDiagnosticInput;
use telemetry_core::WriteTelemetryDiagnosticOptions;
use telemetry_core::get_daily_active_capture_state;
use telemetry_core::get_telemetry_activity_state_file_path;
use telemetry_core::resolve_telemetry_state_dir;
use telemetry_core::write_telemetry_diagnostic;

use crate::common::env;

fn at(iso: &str) -> Option<DateTime<Utc>> {
    Some(
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc),
    )
}

fn state(day: &str, capture_daily: bool) -> PostHogActivityCaptureState {
    PostHogActivityCaptureState {
        day_utc: day.to_string(),
        capture_daily,
    }
}

fn read_state(path: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn xdg_data_home_state_dir_contains_product_dir_once() {
    let xdg = tempfile::tempdir().unwrap();
    let env = env(&[("XDG_DATA_HOME", xdg.path().to_str().unwrap())]);

    let state_dir = resolve_telemetry_state_dir(
        "omo-codex",
        &ResolveTelemetryStateDirOptions {
            env: Some(&env),
            os_provider: None,
        },
    );

    assert_eq!(state_dir, xdg.path().join("omo-codex"));
    assert_eq!(
        get_telemetry_activity_state_file_path(&state_dir),
        xdg.path().join("omo-codex").join("posthog-activity.json"),
    );
}

#[test]
fn missing_state_sends_once_and_writes_current_utc_day_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_telemetry_activity_state_file_path(dir.path());
    let input = |now| DailyActiveCaptureStateInput {
        state_dir: dir.path(),
        now,
        diagnostics: None,
    };

    let first = get_daily_active_capture_state(&input(at("2026-05-25T01:02:03.000Z")));
    let second = get_daily_active_capture_state(&input(at("2026-05-25T23:59:59.000Z")));

    assert_eq!(first, state("2026-05-25", true));
    assert_eq!(second, state("2026-05-25", false));
    assert_eq!(
        read_state(&path),
        json!({ "lastActiveDayUTC": "2026-05-25" })
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{\n  \"lastActiveDayUTC\": \"2026-05-25\"\n}\n"
    );
}

#[test]
fn stale_state_sends_next_day_and_preserves_schema_field_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_telemetry_activity_state_file_path(dir.path());
    fs::write(&path, "{\"lastActiveDayUTC\":\"2026-05-24\"}\n").unwrap();

    let result = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: dir.path(),
        now: at("2026-05-25T00:00:01.000Z"),
        diagnostics: None,
    });

    assert_eq!(result, state("2026-05-25", true));
    assert_eq!(
        read_state(&path),
        json!({ "lastActiveDayUTC": "2026-05-25" })
    );
}

#[test]
fn malformed_state_json_is_treated_as_missing_and_recorded_in_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let path = get_telemetry_activity_state_file_path(dir.path());
    fs::write(&path, "{bad-json\n").unwrap();
    let now = at("2026-05-25T10:20:30.000Z");
    let events = std::cell::RefCell::new(Vec::new());
    let sink = |input: &TelemetryDiagnosticInput| {
        events.borrow_mut().push(input.event);
        write_telemetry_diagnostic(
            input,
            &WriteTelemetryDiagnosticOptions {
                diagnostics_dir: dir.path(),
                now,
            },
        );
    };

    let result = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: dir.path(),
        now,
        diagnostics: Some(&sink),
    });

    assert_eq!(result, state("2026-05-25", true));
    assert_eq!(
        events.into_inner(),
        [TelemetryDiagnosticEvent::TelemetryActivityStateReadFailed]
    );
    let log = fs::read_to_string(dir.path().join("telemetry-diagnostics.jsonl")).unwrap();
    assert!(
        log.contains("\"event\":\"telemetry_activity_state_read_failed\""),
        "{log}"
    );
}
