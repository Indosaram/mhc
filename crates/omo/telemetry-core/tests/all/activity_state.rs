use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Barrier;

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
use telemetry_core::get_telemetry_day_claim_file_path;
use telemetry_core::resolve_telemetry_state_dir;
use telemetry_core::write_telemetry_diagnostic;

use crate::common::env;

const DAY_UTC: &str = "2026-05-25";

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

fn read_state(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn day_claim_path(state_dir: &Path, day_utc: &str) -> std::path::PathBuf {
    get_telemetry_day_claim_file_path(state_dir, day_utc)
}

fn read_dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Suite 1: activity-state.test.ts
// ---------------------------------------------------------------------------

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
    fs::write(&path, "{bad-json
").unwrap();
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

// ---------------------------------------------------------------------------
// Suite 2: activity-state.dedupe.test.ts
// ---------------------------------------------------------------------------

#[test]
fn fresh_day_captures_and_leaves_claim() {
    let dir = tempfile::tempdir().unwrap();
    let result = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: dir.path(),
        now: at(&format!("{DAY_UTC}T01:02:03.000Z")),
        diagnostics: None,
    });

    assert_eq!(result, state(DAY_UTC, true));
    assert!(read_dir_names(dir.path()).contains(&format!("daily-active.{DAY_UTC}.claim")));
}

#[test]
fn peer_claimed_today_does_not_capture() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(day_claim_path(dir.path(), DAY_UTC), "").unwrap();

    let result = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: dir.path(),
        now: at(&format!("{DAY_UTC}T01:02:03.000Z")),
        diagnostics: None,
    });

    assert_eq!(result, state(DAY_UTC, false));
}

#[test]
fn yesterday_claim_pruned_on_fresh_day() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(day_claim_path(dir.path(), "2026-05-24"), "").unwrap();

    let result = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: dir.path(),
        now: at(&format!("{DAY_UTC}T00:00:01.000Z")),
        diagnostics: None,
    });

    assert!(result.capture_daily);
    let entries = read_dir_names(dir.path());
    assert!(entries.contains(&format!("daily-active.{DAY_UTC}.claim")));
    assert!(!entries.contains(&"daily-active.2026-05-24.claim".to_string()));
}

#[test]
fn unwritable_state_dir_bounded_to_single_capture_in_process() {
    let root = tempfile::tempdir().unwrap();
    let blocker = root.path().join("blocker");
    fs::write(&blocker, "not a directory
").unwrap();
    let unwritable_state_dir = blocker.join("state");

    let first = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: &unwritable_state_dir,
        now: at(&format!("{DAY_UTC}T01:02:03.000Z")),
        diagnostics: None,
    });
    let second = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: &unwritable_state_dir,
        now: at(&format!("{DAY_UTC}T01:02:04.000Z")),
        diagnostics: None,
    });
    let third = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: &unwritable_state_dir,
        now: at(&format!("{DAY_UTC}T20:00:00.000Z")),
        diagnostics: None,
    });

    assert!(first.capture_daily);
    assert!(!second.capture_daily);
    assert!(!third.capture_daily);
}

#[test]
fn corrupt_state_file_captures_and_repairs_state() {
    let dir = tempfile::tempdir().unwrap();
    let state_file_path = get_telemetry_activity_state_file_path(dir.path());
    fs::write(&state_file_path, "{bad-json
").unwrap();

    let result = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: dir.path(),
        now: at(&format!("{DAY_UTC}T10:00:00.000Z")),
        diagnostics: None,
    });

    assert!(result.capture_daily);
    assert!(read_dir_names(dir.path()).contains(&format!("daily-active.{DAY_UTC}.claim")));
    assert_eq!(
        fs::read_to_string(&state_file_path).unwrap(),
        format!("{{\n  \"lastActiveDayUTC\": \"{DAY_UTC}\"\n}}\n")
    );
}

#[test]
fn state_already_stamped_today_reports_false_without_touching_dir() {
    let dir = tempfile::tempdir().unwrap();
    let state_file_path = get_telemetry_activity_state_file_path(dir.path());
    fs::write(&state_file_path, format!("{{\"lastActiveDayUTC\":\"{DAY_UTC}\"}}\n")).unwrap();
    let entries_before = read_dir_names(dir.path());
    let mtime_before = fs::metadata(&state_file_path).unwrap().modified().unwrap();

    let result = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: dir.path(),
        now: at(&format!("{DAY_UTC}T12:00:00.000Z")),
        diagnostics: None,
    });

    assert_eq!(result, state(DAY_UTC, false));
    assert_eq!(read_dir_names(dir.path()), entries_before);
    assert_eq!(
        fs::metadata(&state_file_path).unwrap().modified().unwrap(),
        mtime_before
    );
}

// ---------------------------------------------------------------------------
// Suite 3: activity-state.concurrency.test.ts
// ---------------------------------------------------------------------------

#[test]
fn many_concurrent_evaluations_on_fresh_day_exactly_one_captures() {
    const WORKER_COUNT: usize = 16;
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(WORKER_COUNT));
    let mut handles = Vec::with_capacity(WORKER_COUNT);

    for _ in 0..WORKER_COUNT {
        let b = Arc::clone(&barrier);
        let s = state_dir.clone();
        handles.push(std::thread::spawn(move || {
            b.wait();
            let result = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
                state_dir: &s,
                now: at(&format!("{DAY_UTC}T00:00:00.000Z")),
                diagnostics: None,
            });
            result.capture_daily
        }));
    }

    let results: Vec<bool> = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();

    let capture_count = results.iter().filter(|&&c| c).count();
    assert_eq!(capture_count, 1, "exactly one concurrent worker must capture");
    assert_eq!(results.len(), WORKER_COUNT);
}
