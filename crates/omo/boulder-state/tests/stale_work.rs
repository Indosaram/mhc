//! Port of `src/stale-work.test.ts` (`reconcileStaleWorks`, `isWorkStale`,
//! `resolveStaleWorkThresholdMs`, and the resume paths that clear a demotion).

use std::path::Path;
use std::time::{Duration, SystemTime};

use boulder_state::{
    BoulderSessionOrigin, DEFAULT_STALE_WORK_THRESHOLD_MS, ReconcileStaleWorksOptions,
    STALE_WORK_THRESHOLD_ENV_KEY, StaleWorkDemotion, StaleWorkReconcileResult,
    append_session_id_for_work, get_boulder_file_path, get_work_resume_options, is_work_stale,
    read_boulder_state, reconcile_stale_works, resolve_stale_work_threshold_ms, select_active_work,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const STALE_WORK_ID: &str = "omo-agent-toolkit-eval-sdk-20260913";
const STALE_SESSION_ID: &str = "senpi:01a09988-f0b5-7e7c-97b2-ea591aa4bdaf";
const STALE_TRANSCRIPT_FILE: &str =
    "2026-09-13T06-51-23-701Z_01a09988-f0b5-7e7c-97b2-ea591aa4bdaf.jsonl";
const HOUR_MS: i64 = 60 * 60 * 1000;
// `Date.parse("2026-09-17T06:00:00.000Z")`.
const NOW_MS: i64 = 1_789_624_800_000;

// The record observed in a real install: no `updated_at`, no `started_at`, and extra fields the
// ulw-execute writer keeps beside the tracked ones.
fn create_stale_work_record(overrides: Value) -> Value {
    let mut record = json!({
        "work_id": STALE_WORK_ID,
        "active_plan": ".omo/plans/omo-agent-toolkit-eval-sdk.md",
        "plan_name": "omo-agent-toolkit-eval-sdk",
        "session_ids": [STALE_SESSION_ID],
        "status": "active",
        "worktree_path": Value::Null,
        "ulw_loop_session": "01a09988-f0b5-7e7c-97b2-ea591aa4bdaf",
        "mode": "--ship",
    });
    if let Value::Object(extra) = overrides {
        for (key, value) in extra {
            record[key] = value;
        }
    }
    record
}

// One stale work under its id, as the `works` map the fixture writes.
fn stale_works(overrides: Value) -> Value {
    let mut works = serde_json::Map::new();
    works.insert(STALE_WORK_ID.to_string(), create_stale_work_record(overrides));
    Value::Object(works)
}

fn create_project(works: Value, active_work_id: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("tempdir");
    let boulder_directory = directory.path().join(".omo");
    std::fs::create_dir_all(&boulder_directory).expect("create .omo");
    let active = works.get(active_work_id).cloned().expect("active work");
    let state = json!({
        "schema_version": 2,
        "active_work_id": active_work_id,
        "works": works,
        "active_plan": active["active_plan"],
        "plan_name": active["plan_name"],
        "status": active["status"],
        "session_ids": active["session_ids"],
    });
    std::fs::write(boulder_directory.join("boulder.json"), state.to_string()).expect("write state");
    directory
}

fn create_sessions_directory() -> tempfile::TempDir {
    tempfile::tempdir().expect("sessions tempdir")
}

fn read_raw_state(directory: &Path) -> String {
    std::fs::read_to_string(get_boulder_file_path(directory)).expect("read state")
}

// Senpi encodes a session cwd into one directory segment; the fixture mirrors the production
// normalizer (`toLowerCase().replace(/[^a-z0-9]+/g, "-")`).
fn encode_session_cwd(directory: &Path) -> String {
    let lowered = directory.to_string_lossy().to_lowercase();
    let mut encoded = String::with_capacity(lowered.len());
    for character in lowered.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            encoded.push(character);
        } else if !encoded.ends_with('-') {
            encoded.push('-');
        }
    }
    encoded.trim_matches('-').to_string()
}

fn set_mtime(path: &Path, millis: i64) {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open transcript");
    let modified = SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(millis).expect("non-negative"));
    file.set_modified(modified).expect("set mtime");
}

fn write_transcript_into(
    sessions_directory: &Path,
    project_directory: &Path,
    file_name: &str,
    mtime_ms: i64,
) {
    let project_sessions_directory = sessions_directory
        .join(format!("--{}--", encode_session_cwd(project_directory)));
    std::fs::create_dir_all(&project_sessions_directory).expect("create session dir");
    let transcript_path = project_sessions_directory.join(file_name);
    std::fs::write(
        &transcript_path,
        format!("{}\n", json!({ "type": "session", "id": "01a09988-f0b5-7e7c-97b2-ea591aa4bdaf" })),
    )
    .expect("write transcript");
    set_mtime(&transcript_path, mtime_ms);
}

fn write_transcript(project_directory: &Path, file_name: &str, mtime_ms: i64) -> tempfile::TempDir {
    let sessions_directory = create_sessions_directory();
    write_transcript_into(sessions_directory.path(), project_directory, file_name, mtime_ms);
    sessions_directory
}

fn options(sessions_directory: &Path) -> ReconcileStaleWorksOptions {
    ReconcileStaleWorksOptions {
        now: Some(NOW_MS),
        sessions_directory: Some(sessions_directory.to_path_buf()),
        ..Default::default()
    }
}

fn no_changes() -> StaleWorkReconcileResult {
    StaleWorkReconcileResult {
        demoted: Vec::new(),
        written: false,
    }
}

#[test]
fn windows_shaped_session_cwd_encodes_without_colon_or_separator() {
    // A drive colon and a backslash are both illegal inside a Windows path segment.
    let encoded = encode_session_cwd(Path::new(
        r"C:\Users\RUNNER~1\AppData\Local\Temp\boulder-stale-work-z5FTDO",
    ));
    assert_eq!(
        encoded,
        "c-users-runner-1-appdata-local-temp-boulder-stale-work-z5ftdo"
    );
    assert!(!encoded.contains([':', '\\', '/']));
}

#[test]
fn active_work_with_only_a_41_hour_old_transcript_is_paused_and_stamped() {
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    let sessions_directory = write_transcript(directory.path(), STALE_TRANSCRIPT_FILE, NOW_MS - 41 * HOUR_MS);

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert_eq!(
        result.demoted.iter().map(|demotion| demotion.work_id.as_str()).collect::<Vec<_>>(),
        vec![STALE_WORK_ID]
    );
    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    let work = state.work(STALE_WORK_ID).expect("work");
    assert_eq!(work.status().map(|status| status.as_str()), Some("paused"));
    assert_eq!(work.stale_since(), Some("2026-09-17T06:00:00.000Z"));
    assert_eq!(work.session_ids(), vec![STALE_SESSION_ID.to_string()]);
}

#[test]
fn demoted_work_keeps_every_other_field() {
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    let sessions_directory = write_transcript(directory.path(), STALE_TRANSCRIPT_FILE, NOW_MS - 41 * HOUR_MS);

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert!(result.written);
    assert_eq!(
        result.demoted[0].last_activity_at.as_deref(),
        Some("2026-09-15T13:00:00.000Z")
    );
    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    let work = state.work(STALE_WORK_ID).expect("work").to_json_value();
    assert_eq!(work["active_plan"], json!(".omo/plans/omo-agent-toolkit-eval-sdk.md"));
    assert_eq!(work["plan_name"], json!("omo-agent-toolkit-eval-sdk"));
    assert_eq!(work["ulw_loop_session"], json!("01a09988-f0b5-7e7c-97b2-ea591aa4bdaf"));
    assert_eq!(work["mode"], json!("--ship"));
}

#[test]
fn demoted_work_is_still_resumable() {
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    let sessions_directory = write_transcript(directory.path(), STALE_TRANSCRIPT_FILE, NOW_MS - 41 * HOUR_MS);
    reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    let resume_options = get_work_resume_options(directory.path()).expect("resume options");

    assert_eq!(
        resume_options
            .iter()
            .map(|option| (option.work_id.as_str(), option.status.as_str()))
            .collect::<Vec<_>>(),
        vec![(STALE_WORK_ID, "paused")]
    );
}

#[test]
fn one_hour_old_transcript_writes_nothing() {
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    let sessions_directory = write_transcript(directory.path(), STALE_TRANSCRIPT_FILE, NOW_MS - HOUR_MS);
    let raw_before = read_raw_state(directory.path());

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert_eq!(result, no_changes());
    assert_eq!(read_raw_state(directory.path()), raw_before);
    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    assert_eq!(
        state.work(STALE_WORK_ID).expect("work").status().map(|status| status.as_str()),
        Some("active")
    );
}

#[test]
fn completed_work_with_no_activity_for_days_is_untouched() {
    let directory = create_project(
        stale_works(json!({
            "status": "completed",
            "ended_at": "2026-09-13T08:00:00.000Z",
        })),
        STALE_WORK_ID,
    );
    let sessions_directory = create_sessions_directory();
    let raw_before = read_raw_state(directory.path());

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert_eq!(result, no_changes());
    assert_eq!(read_raw_state(directory.path()), raw_before);
    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    assert_eq!(
        state.work(STALE_WORK_ID).expect("work").status().map(|status| status.as_str()),
        Some("completed")
    );
}

#[test]
fn no_transcript_but_a_fresh_updated_at_stays_active() {
    let directory = create_project(
        stale_works(json!({
            "updated_at": "2026-09-17T05:00:00.000Z",
        })),
        STALE_WORK_ID,
    );
    let sessions_directory = create_sessions_directory();
    let raw_before = read_raw_state(directory.path());

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert_eq!(result, no_changes());
    assert_eq!(read_raw_state(directory.path()), raw_before);
    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    assert_eq!(
        state.work(STALE_WORK_ID).expect("work").status().map(|status| status.as_str()),
        Some("active")
    );
}

#[test]
fn no_transcript_and_no_timestamps_demotes_with_no_last_activity() {
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    let sessions_directory = create_sessions_directory();

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert_eq!(
        result.demoted,
        vec![StaleWorkDemotion {
            work_id: STALE_WORK_ID.to_string(),
            stale_since: "2026-09-17T06:00:00.000Z".to_string(),
            last_activity_at: None,
        }]
    );
    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    assert_eq!(
        state.work(STALE_WORK_ID).expect("work").status().map(|status| status.as_str()),
        Some("paused")
    );
}

#[test]
fn unresolved_session_id_is_demoted_and_no_unrelated_transcript_matches() {
    let directory = create_project(
        stale_works(json!({ "session_ids": ["senpi:unknown"] })),
        STALE_WORK_ID,
    );
    let sessions_directory = write_transcript(directory.path(), STALE_TRANSCRIPT_FILE, NOW_MS - 60 * 1000);
    write_transcript_into(
        sessions_directory.path(),
        directory.path(),
        "2026-09-13T06-51-23-701Z_not-unknown.jsonl",
        NOW_MS - 60 * 1000,
    );

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert_eq!(
        result.demoted,
        vec![StaleWorkDemotion {
            work_id: STALE_WORK_ID.to_string(),
            stale_since: "2026-09-17T06:00:00.000Z".to_string(),
            last_activity_at: None,
        }]
    );
    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    assert_eq!(
        state.work(STALE_WORK_ID).expect("work").status().map(|status| status.as_str()),
        Some("paused")
    );
}

#[test]
fn work_quiet_for_31_days_is_demoted_with_its_last_activity_recorded() {
    let last_activity_ms = NOW_MS - 31 * 24 * HOUR_MS;
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    let sessions_directory = write_transcript(directory.path(), STALE_TRANSCRIPT_FILE, last_activity_ms);

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert_eq!(
        result.demoted,
        vec![StaleWorkDemotion {
            work_id: STALE_WORK_ID.to_string(),
            stale_since: "2026-09-17T06:00:00.000Z".to_string(),
            last_activity_at: Some("2026-08-17T06:00:00.000Z".to_string()),
        }]
    );
    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    assert_eq!(
        state.work(STALE_WORK_ID).expect("work").status().map(|status| status.as_str()),
        Some("paused")
    );
}

#[test]
fn no_boulder_file_writes_nothing_and_throws_nothing() {
    let directory = tempfile::tempdir().expect("tempdir");

    let result = reconcile_stale_works(directory.path(), &options(create_sessions_directory().path()));

    assert_eq!(result, no_changes());
    assert!(!get_boulder_file_path(directory.path()).exists());
}

#[test]
fn unreadable_boulder_file_writes_nothing_and_throws_nothing() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(directory.path().join(".omo")).expect("create .omo");
    std::fs::write(get_boulder_file_path(directory.path()), "{not-json").expect("write broken");

    let result = reconcile_stale_works(directory.path(), &options(create_sessions_directory().path()));

    assert_eq!(result, no_changes());
    assert_eq!(read_raw_state(directory.path()), "{not-json");
}

#[test]
fn selecting_a_demoted_work_reactivates_it_without_the_stale_stamp() {
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    reconcile_stale_works(
        directory.path(),
        &options(create_sessions_directory().path()),
    );

    select_active_work(directory.path(), STALE_WORK_ID).expect("select");

    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    let work = state.work(STALE_WORK_ID).expect("work");
    assert_eq!(work.status().map(|status| status.as_str()), Some("active"));
    assert_eq!(work.stale_since(), None);
}

#[test]
fn appending_a_session_to_a_demoted_work_reactivates_it_without_the_stale_stamp() {
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    reconcile_stale_works(
        directory.path(),
        &options(create_sessions_directory().path()),
    );

    append_session_id_for_work(
        directory.path(),
        STALE_WORK_ID,
        "senpi:01a0ae2a-0000-7000-8000-000000000000",
        BoulderSessionOrigin::Appended,
    )
    .expect("append");

    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    let work = state.work(STALE_WORK_ID).expect("work");
    assert_eq!(work.status().map(|status| status.as_str()), Some("active"));
    assert_eq!(work.stale_since(), None);
    assert_eq!(
        work.session_ids(),
        vec![
            STALE_SESSION_ID.to_string(),
            "senpi:01a0ae2a-0000-7000-8000-000000000000".to_string(),
        ]
    );
}

#[test]
fn a_work_paused_without_a_stale_stamp_keeps_its_status() {
    let directory = create_project(
        stale_works(json!({ "status": "paused" })),
        STALE_WORK_ID,
    );

    select_active_work(directory.path(), STALE_WORK_ID).expect("select");

    let state = read_boulder_state(directory.path()).expect("readable").expect("state");
    assert_eq!(
        state.work(STALE_WORK_ID).expect("work").status().map(|status| status.as_str()),
        Some("paused")
    );
}

#[test]
fn activity_exactly_at_the_threshold_is_stale() {
    assert!(is_work_stale(Some(1000), 1000 + HOUR_MS, HOUR_MS));
}

#[test]
fn activity_one_millisecond_inside_the_threshold_is_not_stale() {
    assert!(!is_work_stale(Some(1001), 1000 + HOUR_MS, HOUR_MS));
}

#[test]
fn no_activity_evidence_is_stale() {
    assert!(is_work_stale(None, 1000, HOUR_MS));
}

#[test]
fn activity_stamped_in_the_future_is_not_stale() {
    assert!(!is_work_stale(Some(2000 + HOUR_MS), 2000, HOUR_MS));
}

#[test]
fn no_env_knob_uses_the_six_hour_default() {
    assert_eq!(resolve_stale_work_threshold_ms(&Default::default()), DEFAULT_STALE_WORK_THRESHOLD_MS);
    assert_eq!(DEFAULT_STALE_WORK_THRESHOLD_MS, 6 * HOUR_MS);
}

#[test]
fn env_knob_overrides_the_default() {
    let env = std::collections::BTreeMap::from([(
        STALE_WORK_THRESHOLD_ENV_KEY.to_string(),
        " 900000 ".to_string(),
    )]);
    assert_eq!(resolve_stale_work_threshold_ms(&env), 900_000);
}

#[test]
fn non_positive_or_unparsable_env_knob_uses_the_default() {
    for raw in ["0", "soon"] {
        let env = std::collections::BTreeMap::from([(
            STALE_WORK_THRESHOLD_ENV_KEY.to_string(),
            raw.to_string(),
        )]);
        assert_eq!(resolve_stale_work_threshold_ms(&env), DEFAULT_STALE_WORK_THRESHOLD_MS);
    }
}

#[test]
fn transcript_scan_reads_only_the_matching_session_directory() {
    let directory = create_project(
        stale_works(json!({})),
        STALE_WORK_ID,
    );
    let sessions_directory = create_sessions_directory();
    // A fresh transcript under an unrelated cwd must not keep the work alive.
    let unrelated = tempfile::tempdir().expect("unrelated project");
    write_transcript_into(
        sessions_directory.path(),
        unrelated.path(),
        STALE_TRANSCRIPT_FILE,
        NOW_MS - 60 * 1000,
    );

    let result = reconcile_stale_works(directory.path(), &options(sessions_directory.path()));

    assert_eq!(
        result.demoted.iter().map(|demotion| demotion.work_id.as_str()).collect::<Vec<_>>(),
        vec![STALE_WORK_ID]
    );
}
