use std::sync::{Arc, Mutex};

use memory_core::recall::RecallNudge;

use super::*;
use crate::kibitzer_sidecar_outcome::{
    KibitzerCursorSpan, KibitzerWakeCause, KibitzerWakeConfiguration,
    KibitzerWakeConfigurationCause, KibitzerWakeStatus, KibitzerWakeUsage,
};

fn outcome(session: &str) -> KibitzerWakeOutcome {
    KibitzerWakeOutcome {
        session_id: session.to_string(),
        wake: 1,
        generation: 1,
        status: KibitzerWakeStatus::Completed,
        cause: None,
        reason: None,
        configuration: None,
        model: None,
        nudges: Vec::new(),
        candidate_count: 0,
        steered: 0,
        tool_calls: 0,
        duration_ms: 5,
        slot_wait_ms: 0,
        cursors: None,
        context_tokens: None,
        usage: None,
        diagnostic: false,
    }
}

struct Harness {
    lines: Arc<Mutex<Vec<String>>>,
    entries: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    reporter: WakeReporter,
}

impl Harness {
    fn new() -> Self {
        Self {
            lines: Arc::new(Mutex::new(Vec::new())),
            entries: Arc::new(Mutex::new(Vec::new())),
            reporter: WakeReporter::new(),
        }
    }

    fn report(&self, outcome: &KibitzerWakeOutcome) {
        self.report_in_category(outcome, None)
    }

    fn report_in_category(&self, outcome: &KibitzerWakeOutcome, category: Option<&str>) {
        let lines = Arc::clone(&self.lines);
        let append = move |_: &Path, _: &str, line: &str| {
            lines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(line.to_string());
        };
        let warn = |_: &str| {};
        let now = || 1_700_000_000_000i64;
        let entries = Arc::clone(&self.entries);
        let append_entry: AppendEntry = Arc::new(move |kind: &str, data: serde_json::Value| {
            entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((kind.to_string(), data));
        });
        self.reporter.report(
            outcome,
            &WakeLogDeps {
                recall: Path::new("/recall"),
                append: &append,
                warn: &warn,
                now_ms: &now,
            },
            &append_entry,
            category,
        );
    }

    fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn entries(&self) -> Vec<(String, serde_json::Value)> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn parse(line: &str) -> serde_json::Value {
    serde_json::from_str(line.trim()).expect("a record is one JSON line")
}

#[test]
fn a_completed_wake_writes_one_closed_record_line() {
    let harness = Harness::new();
    let mut wake = outcome("s1");
    wake.nudges = vec![RecallNudge::new("notes/a.md", "hint")];
    wake.candidate_count = 4;
    wake.context_tokens = Some(900);
    wake.usage = Some(KibitzerWakeUsage {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
    });
    wake.cursors = Some(KibitzerCursorSpan { first: 3, last: 9 });
    harness.report(&wake);

    let lines = harness.lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].ends_with('\n'), "one line per record");
    let record = parse(&lines[0]);
    assert_eq!(record["version"], 1);
    assert_eq!(record["sessionId"], "s1");
    assert_eq!(record["status"], "completed");
    assert_eq!(record["nudged"], serde_json::json!(["notes/a.md"]));
    assert_eq!(record["candidateCount"], 4);
    assert_eq!(record["contextTokens"], 900);
    assert_eq!(record["cursors"], serde_json::json!({"first":3,"last":9}));
    assert_eq!(record["usage"]["cacheRead"], 30);
    assert_eq!(record["diagnostic"], false);
    assert_eq!(
        record["at"], "2023-11-14T22:13:20.000Z",
        "the record carries the injected clock, not the system one"
    );
    assert!(
        record.get("cause").is_none(),
        "an absent cause is omitted, never null"
    );
}

#[test]
fn a_failed_wake_carries_its_cause_and_reason() {
    let harness = Harness::new();
    let mut wake = outcome("s1");
    wake.status = KibitzerWakeStatus::Failed;
    wake.cause = Some(KibitzerWakeCause::ChildFailedUpstream);
    wake.reason = Some("stream ended".into());
    wake.diagnostic = true;
    harness.report(&wake);

    let record = parse(&harness.lines()[0]);
    assert_eq!(record["status"], "failed");
    assert_eq!(record["cause"], "child_failed_upstream");
    assert_eq!(record["reason"], "stream ended");
}

#[test]
fn a_configuration_refusal_writes_its_configuration_and_one_unavailable_notice() {
    let harness = Harness::new();
    let mut wake = outcome("s1");
    wake.status = KibitzerWakeStatus::Failed;
    wake.cause = Some(KibitzerWakeCause::StartFailed);
    wake.configuration = Some(KibitzerWakeConfiguration {
        category: "recall".into(),
        cause: KibitzerWakeConfigurationCause::CategoryUnavailable,
        missing_providers: Some(vec!["anthropic".into()]),
    });
    harness.report(&wake);
    harness.report(&wake);

    let record = parse(&harness.lines()[0]);
    assert_eq!(record["configuration"]["category"], "recall");
    assert_eq!(record["configuration"]["cause"], "category_unavailable");
    assert_eq!(
        record["configuration"]["missingProviders"],
        serde_json::json!(["anthropic"])
    );
    let entries = harness.entries();
    assert_eq!(
        entries.len(),
        1,
        "the unavailable notice is once per session, not once per wake"
    );
    assert_eq!(entries[0].0, UNAVAILABLE_ENTRY_TYPE);
    assert_eq!(entries[0].1["version"], 1);
    assert_eq!(entries[0].1["category"], "recall");
    assert_eq!(entries[0].1["cause"], "category_unavailable");
    assert_eq!(harness.lines().len(), 2, "both wakes still log a record");
}

#[test]
fn the_gate_notice_waits_for_three_consecutive_diagnostic_failures() {
    let harness = Harness::new();
    let mut wake = outcome("s1");
    wake.status = KibitzerWakeStatus::Failed;
    wake.cause = Some(KibitzerWakeCause::ChildFailed);
    wake.diagnostic = true;
    harness.report(&wake);
    harness.report(&wake);
    assert!(harness.entries().is_empty(), "two failures stay quiet");
    harness.report(&wake);

    let entries = harness.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, GATE_ENTRY_TYPE);
    assert_eq!(entries[0].1["status"], "failed");
    assert_eq!(entries[0].1["cause"], "child_failed");
    assert_eq!(entries[0].1["consecutiveFailures"], 3);
    assert_eq!(entries[0].1["wake"], 1);

    harness.report(&wake);
    assert_eq!(
        harness.entries().len(),
        1,
        "the gate notice is raised once, not on every later failure"
    );
}

#[test]
fn a_non_diagnostic_wake_resets_the_failure_streak() {
    let harness = Harness::new();
    let mut failing = outcome("s1");
    failing.status = KibitzerWakeStatus::Failed;
    failing.cause = Some(KibitzerWakeCause::ChildFailed);
    failing.diagnostic = true;
    harness.report(&failing);
    harness.report(&failing);
    harness.report(&outcome("s1"));
    harness.report(&failing);
    harness.report(&failing);

    assert!(
        harness.entries().is_empty(),
        "the two failures after the reset are only the second consecutive pair"
    );
    harness.report(&failing);
    assert_eq!(harness.entries().len(), 1, "the third failure raises it");
    assert_eq!(harness.entries()[0].1["consecutiveFailures"], 3);
}

#[test]
fn a_cancelled_wake_is_not_diagnostic_and_raises_nothing() {
    let harness = Harness::new();
    for _ in 0..5 {
        let mut wake = outcome("s1");
        wake.status = KibitzerWakeStatus::Cancelled;
        wake.cause = Some(KibitzerWakeCause::Shutdown);
        harness.report(&wake);
    }
    assert!(harness.entries().is_empty());
    assert_eq!(parse(&harness.lines()[0])["cause"], "shutdown");
}

#[test]
fn forgetting_a_session_clears_both_its_streak_and_its_notice_memory() {
    let harness = Harness::new();
    let mut failing = outcome("s1");
    failing.status = KibitzerWakeStatus::Failed;
    failing.cause = Some(KibitzerWakeCause::ChildFailed);
    failing.diagnostic = true;
    harness.report(&failing);
    harness.report(&failing);
    harness.reporter.forget("s1");
    harness.report(&failing);
    harness.report(&failing);
    assert!(
        harness.entries().is_empty(),
        "the streak restarted after the session was forgotten"
    );
    harness.report(&failing);
    assert_eq!(harness.entries().len(), 1);
}

#[test]
fn a_record_over_the_cap_loses_its_reason_and_nudges_and_is_marked_truncated() {
    let mut wake = outcome("s1");
    wake.reason = Some("r".repeat(5000));
    wake.nudges = vec![RecallNudge::new("x".repeat(300), "hint")];
    let line = wake_record(&wake, 0);
    assert!(line.len() < WAKE_RECORD_MAX_CHARS || line.contains("truncated"));
    let record = parse(&line);
    assert_eq!(record["truncated"], true);
    assert_eq!(record["nudged"], serde_json::json!([]));
    assert!(
        record.get("reason").is_none(),
        "the reason is the first thing dropped"
    );
}

#[test]
fn a_record_under_the_cap_keeps_every_field() {
    let mut wake = outcome("s1");
    wake.reason = Some("short reason".into());
    let record = parse(&wake_record(&wake, 0));
    assert_eq!(record["reason"], "short reason");
    assert!(record.get("truncated").is_none());
}

#[test]
fn secret_like_text_in_a_model_or_category_is_redacted_not_truncated() {
    let mut wake = outcome("s1");
    wake.model = Some("sk-ant-api03-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into());
    let record = parse(&wake_record(&wake, 0));
    assert_eq!(record["model"], "redacted");
}

#[test]
fn the_wakes_file_lives_in_the_encoded_session_directory() {
    let path = wakes_file(Path::new("/recall"), "ses-1");
    assert!(path.ends_with(WAKES_FILE_NAME));
    assert_eq!(
        path.parent(),
        Some(sidecar_dir(Path::new("/recall"), "ses-1").as_path())
    );
}

#[test]
fn the_gate_notice_carries_the_pinned_category_when_one_is_known() {
    let harness = Harness::new();
    let mut wake = outcome("s1");
    wake.status = KibitzerWakeStatus::Failed;
    wake.cause = Some(KibitzerWakeCause::ChildFailed);
    wake.diagnostic = true;
    harness.report_in_category(&wake, None);
    harness.report_in_category(&wake, None);
    harness.report_in_category(&wake, Some("recall"));

    let entries = harness.entries();
    assert_eq!(entries[0].1["category"], "recall");
}

#[test]
fn two_sessions_keep_independent_streaks_and_notice_memory() {
    let harness = Harness::new();
    let mut failing = outcome("s1");
    failing.status = KibitzerWakeStatus::Failed;
    failing.cause = Some(KibitzerWakeCause::ChildFailed);
    failing.diagnostic = true;
    let mut other = failing.clone();
    other.session_id = "s2".into();
    harness.report(&failing);
    harness.report(&failing);
    harness.report(&other);
    harness.report(&other);
    assert!(harness.entries().is_empty(), "s2 only reached two");
    harness.report(&other);
    let entries = harness.entries();
    assert_eq!(entries.len(), 1, "only s2 crossed the threshold");
}
