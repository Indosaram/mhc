use std::path::Path;
use crate::{
    commands::{
        doctor::handle_doctor,
        test_support::{fake_command_context, fake_deps, temp_identity, FakeContextOptions, FakeDepsOverrides},
    },
};

const NOW_MS: i64 = 1791288000000; // 2026-10-06T12:00:00.000Z
const HOUR_MS: i64 = 3_600_000;

#[test]
fn receipt_read_failure_is_not_a_successful_never_summary() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("receipts.jsonl")).unwrap();
    assert!(super::doctor_receipts::check_receipts(root.path(), NOW_MS).is_err());
}

#[test]
fn unicode_receipt_identifier_does_not_panic_in_doctor_summary() {
    let root = tempfile::tempdir().unwrap();
    write_receipts(root.path(), &[serde_json::json!({"at":at(0),"kind":"facts","batchId":"\u{ac00}".repeat(12),"trigger":"settle","event":"committed"})], "");
    let (check, _) = super::doctor_receipts::check_receipts(root.path(), NOW_MS).unwrap();
    assert!(check.detail.contains(&"\u{ac00}".repeat(8)));
}

fn at(ms_before_now: i64) -> String {
    chrono::DateTime::from_timestamp_millis(NOW_MS - ms_before_now)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn write_receipts(runtime_dir: &Path, lines: &[serde_json::Value], tail: &str) {
    std::fs::create_dir_all(runtime_dir).unwrap();
    let mut out = String::new();
    for line in lines {
        let mut obj = line.clone();
        if let Some(map) = obj.as_object_mut() {
            map.insert("v".into(), serde_json::json!(1));
            map.insert("host".into(), serde_json::json!("fixture-host"));
            map.insert("pid".into(), serde_json::json!(1));
        }
        out.push_str(&serde_json::to_string(&obj).unwrap());
        out.push('\n');
    }
    out.push_str(tail);
    std::fs::write(runtime_dir.join("receipts.jsonl"), out).unwrap();
}

#[tokio::test]
async fn given_receipts_for_dream_and_reflection_when_doctor_runs_then_each_kind_shows_newest_terminal() {
    let (_root, identity) = temp_identity();
    std::fs::create_dir_all(&identity.identity_paths.repo).unwrap();
    let git = memory_core::git::GitMemoryRepo::open(&identity.identity_paths.repo, "agent-test").unwrap();
    git.init(None).unwrap();

    let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides {
        now_ms: Some(NOW_MS),
        ..Default::default()
    });
    let context = fake_command_context(FakeContextOptions::default());

    write_receipts(&identity.identity_paths.runtime, &[
        serde_json::json!({ "at": at(72 * HOUR_MS), "kind": "reflection", "runId": "reflection-older", "trigger": "step-count", "event": "merged", "generation": at(73 * HOUR_MS) }),
        serde_json::json!({ "at": at(48 * HOUR_MS), "kind": "dream", "runId": "dream-run-abcdef", "trigger": "idle", "event": "merged", "generation": at(49 * HOUR_MS), "sha": "abc123" }),
        serde_json::json!({ "at": at(HOUR_MS), "kind": "reflection", "runId": "reflection-newer", "trigger": "step-count", "event": "failed", "generation": at(2 * HOUR_MS), "reason": "validation_failed" }),
        serde_json::json!({ "at": at(60_000), "kind": "dream", "runId": "dream-run-next", "trigger": "idle", "event": "launched", "generation": at(60_000) }),
    ], "");

    let response = handle_doctor(&fake.deps, &context.ctx, "").await;
    assert!(response.text.contains("[ok] receipts: dream merged 2d ago (run dream-ru); reflection failed 1h ago (validation_failed); facts never"));

    let json_response = handle_doctor(&fake.deps, &context.ctx, "--json").await;
    let report: serde_json::Value = serde_json::from_str(&json_response.text).unwrap();
    assert_eq!(report["receipts"]["dream"]["event"], "merged");
    assert_eq!(report["receipts"]["dream"]["sha"], "abc123");
    assert_eq!(report["receipts"]["reflection"]["event"], "failed");
    assert_eq!(report["receipts"]["reflection"]["reason"], "validation_failed");
    assert!(report["receipts"]["facts"].is_null());
}

#[tokio::test]
async fn given_a_run_reconciliation_quarantined_when_doctor_runs_then_it_is_listed_for_manual_disposal() {
    let (_root, identity) = temp_identity();
    std::fs::create_dir_all(&identity.identity_paths.repo).unwrap();
    let git = memory_core::git::GitMemoryRepo::open(&identity.identity_paths.repo, "agent-test").unwrap();
    git.init(None).unwrap();

    let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides {
        now_ms: Some(NOW_MS),
        ..Default::default()
    });
    let context = fake_command_context(FakeContextOptions::default());

    let run_dir = identity.identity_paths.reflection.join("runs").join("run-quarantined");
    std::fs::create_dir_all(&run_dir).unwrap();
    let record = serde_json::json!({
        "version": 1,
        "runId": "run-quarantined",
        "kind": "reflection",
        "trigger": "step-count",
        "generation": at(3 * HOUR_MS),
        "reason": "invalid_generation_timestamps",
        "quarantinedAt": at(2 * HOUR_MS),
        "evidence": ["final.json", "ledger.json"],
    });
    std::fs::write(run_dir.join("quarantined.json"), serde_json::to_string(&record).unwrap()).unwrap();

    let response = handle_doctor(&fake.deps, &context.ctx, "").await;
    assert!(response.text.contains(&format!("[warn] quarantined-runs: 1 run needs manual disposal: {} (invalid_generation_timestamps)", run_dir.display())));

    let json_response = handle_doctor(&fake.deps, &context.ctx, "--json").await;
    let report: serde_json::Value = serde_json::from_str(&json_response.text).unwrap();
    assert_eq!(report["quarantinedRuns"][0]["runId"], "run-quarantined");
    assert_eq!(report["quarantinedRuns"][0]["reason"], "invalid_generation_timestamps");
    assert_eq!(report["level"], "warn");
}

#[tokio::test]
async fn given_a_receipts_file_ending_in_a_partial_line_when_doctor_runs_then_receipts_warns_and_counts_skipped() {
    let (_root, identity) = temp_identity();
    std::fs::create_dir_all(&identity.identity_paths.repo).unwrap();
    let git = memory_core::git::GitMemoryRepo::open(&identity.identity_paths.repo, "agent-test").unwrap();
    git.init(None).unwrap();

    let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides {
        now_ms: Some(NOW_MS),
        ..Default::default()
    });
    let context = fake_command_context(FakeContextOptions::default());

    write_receipts(&identity.identity_paths.runtime, &[
        serde_json::json!({ "at": at(HOUR_MS), "kind": "facts", "batchId": "batch-0001", "trigger": "settle", "event": "committed" }),
    ], "{\"v\":1,\"at\":\"2026-10-06T11:59");

    let response = handle_doctor(&fake.deps, &context.ctx, "").await;
    assert!(response.text.contains("[warn] receipts: dream never; reflection never; facts committed 1h ago (batch batch-00) (1 partial line skipped)"));
}

#[tokio::test]
async fn given_a_launch_the_reconciler_recorded_as_interrupted_when_doctor_runs_then_not_needing_manual_disposal() {
    let (_root, identity) = temp_identity();
    std::fs::create_dir_all(&identity.identity_paths.repo).unwrap();
    let git = memory_core::git::GitMemoryRepo::open(&identity.identity_paths.repo, "agent-test").unwrap();
    git.init(None).unwrap();

    let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides {
        now_ms: Some(NOW_MS),
        ..Default::default()
    });
    let context = fake_command_context(FakeContextOptions::default());

    let run_dir = identity.identity_paths.reflection.join("runs").join("run-interrupted");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("abandoned.json"), serde_json::to_string(&serde_json::json!({
        "version": 1,
        "runId": "run-interrupted",
        "abandonedAt": at(HOUR_MS),
        "reason": "launch_interrupted",
        "generation": at(2 * HOUR_MS),
        "kind": "reflection",
        "trigger": "step-count",
    })).unwrap()).unwrap();

    let response = handle_doctor(&fake.deps, &context.ctx, "").await;
    assert!(response.text.contains("[ok] abandoned-runs: no abandoned runs"));
}
