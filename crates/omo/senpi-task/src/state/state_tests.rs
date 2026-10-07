use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::store::parse_task_record;

fn base_input() -> TaskRecordInput {
    TaskRecordInput {
        parent_session_id: "parent-session".into(),
        root_session_id: "root-session".into(),
        depth: 0,
        execution_mode: "direct".into(),
        model: "gpt-5.2".into(),
        ..TaskRecordInput::default()
    }
}

fn pending_record() -> TaskRecord {
    create_task_record(base_input(), None).expect("task id")
}

fn ts(value: &str) -> String {
    value.to_string()
}

fn start() -> TaskTransition {
    TaskTransition::Start {
        timestamp: ts("2026-07-06T00:00:00.000Z"),
        pid: Some(1234),
        child_session_id: None,
    }
}

fn complete(response: &str, run_stats: Option<TaskRunStats>) -> TaskTransition {
    TaskTransition::Complete {
        timestamp: ts("2026-07-06T00:00:01.000Z"),
        final_response: response.into(),
        run_stats,
    }
}

fn fail(message: &str) -> TaskTransition {
    TaskTransition::Fail {
        timestamp: ts("2026-07-06T00:00:01.000Z"),
        error_message: message.into(),
        killed: false,
        run_stats: None,
    }
}

fn cancel(message: Option<&str>) -> TaskTransition {
    TaskTransition::Cancel {
        timestamp: ts("2026-07-06T00:00:01.000Z"),
        error_message: message.map(str::to_string),
        run_stats: None,
    }
}

fn interrupt(message: &str) -> TaskTransition {
    TaskTransition::Interrupt {
        timestamp: ts("2026-07-06T00:00:01.000Z"),
        error_message: Some(message.into()),
        run_stats: None,
    }
}

fn lose(message: &str) -> TaskTransition {
    TaskTransition::Lose {
        timestamp: ts("2026-07-06T00:00:01.000Z"),
        error_message: message.into(),
    }
}

fn running() -> TaskRecord {
    transition_task_record(&pending_record(), &start()).record
}

fn with_status(status: TaskStatus) -> TaskRecord {
    TaskRecord {
        status,
        ..pending_record()
    }
}

// ---- state/record.test.ts ----

#[test]
fn record_notify_on_terminal_true_lands_on_record() {
    let record = create_task_record(
        TaskRecordInput {
            notify_on_terminal: true,
            ..base_input()
        },
        None,
    )
    .expect("id");
    assert!(record.notify_on_terminal);
}

#[test]
fn record_notify_on_terminal_false_is_explicit() {
    let record = pending_record();
    let json = serde_json::to_value(&record).expect("json");
    assert_eq!(json["notify_on_terminal"], json!(false));
}

#[test]
fn record_pending_steering_round_trips_through_json() {
    let steering = vec![
        PendingSteeringEntry {
            id: "msg-1".into(),
            message: "prefer the south gate".into(),
            deliver_as: DeliverAs::Steer,
        },
        PendingSteeringEntry {
            id: "msg-2".into(),
            message: "bring the ledger".into(),
            deliver_as: DeliverAs::FollowUp,
        },
    ];
    let record = create_task_record(
        TaskRecordInput {
            notify_on_terminal: true,
            pending_steering: Some(steering.clone()),
            ..base_input()
        },
        None,
    )
    .expect("id");
    let restored = parse_task_record(
        &serde_json::to_value(&record).expect("json"),
        "/tmp/r.json",
        &mut Vec::new(),
    )
    .expect("parse");
    assert_eq!(record.pending_steering, Some(steering.clone()));
    assert_eq!(restored.pending_steering, Some(steering));
    assert!(restored.notify_on_terminal);
}

#[test]
fn record_empty_pending_steering_is_omitted() {
    let record = create_task_record(
        TaskRecordInput {
            pending_steering: Some(Vec::new()),
            ..base_input()
        },
        None,
    )
    .expect("id");
    assert_eq!(record.pending_steering, None);
    let json = serde_json::to_value(&record).expect("json");
    assert!(json.get("pending_steering").is_none());
}

#[test]
fn record_absent_pending_steering_stays_absent() {
    let json = serde_json::to_value(pending_record()).expect("json");
    assert!(json.get("pending_steering").is_none());
}

// ---- state/id.test.ts ----

#[test]
fn id_bump_carries_into_next_hex_group() {
    let bumped = bump_task_id(parse_task_id("st_0000ffff").expect("id")).expect("bump");
    assert_eq!(bumped.to_string(), "st_00010000");
}

#[test]
fn id_bump_increments_value() {
    let bumped = bump_task_id(parse_task_id("st_00000010").expect("id")).expect("bump");
    assert_eq!(bumped.to_string(), "st_00000011");
}

#[test]
fn id_mode_bump_exhaust() {
    assert_eq!(
        bump_task_id(parse_task_id("st_ffffffff").expect("id")),
        Err(TaskIdSpaceExhaustedError)
    );
}

// The TS "isolated module process" modes need a fresh process-global floor; `just test` runs
// nextest, which gives every test its own process, so these exercise the real global directly.
#[test]
fn id_mode_create_exhaust() {
    sync_task_id_floor(parse_task_id("st_ffffffff").expect("id"));
    assert_eq!(create_task_id(None), Err(TaskIdSpaceExhaustedError));
}

#[test]
fn id_mode_floor_raise() {
    sync_task_id_floor(parse_task_id("st_00000100").expect("id"));
    assert_eq!(
        create_task_id(Some(0x10 * 0x10000))
            .expect("id")
            .to_string(),
        "st_00000101"
    );
}

#[test]
fn id_mode_floor_never_lower() {
    sync_task_id_floor(parse_task_id("st_00000200").expect("id"));
    sync_task_id_floor(parse_task_id("st_00000100").expect("id"));
    assert_eq!(
        create_task_id(Some(0x10 * 0x10000))
            .expect("id")
            .to_string(),
        "st_00000201"
    );
}

#[test]
fn id_mode_nowms_record_uses_supplied_clock_bucket() {
    let record = create_task_record(base_input(), Some(0x123 * 0x10000)).expect("id");
    assert_eq!(record.task_id, "st_00000123");
}

fn is_canonical(id: &str) -> bool {
    parse_task_id(id).is_ok()
}

#[test]
fn id_deterministic_clock_ids_are_canonical_and_sortable() {
    let now_ms = 0x1234_5678;
    let ids: Vec<String> = [now_ms, now_ms, now_ms + 1]
        .into_iter()
        .map(|now| create_task_id(Some(now)).expect("id").to_string())
        .collect();
    assert!(ids.iter().all(|id| is_canonical(id)));
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len());
}

#[test]
fn id_same_millisecond_burst_is_unique() {
    let ids: Vec<String> = (0..300)
        .map(|_| create_task_id(Some(0x2233_4455)).expect("id").to_string())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    sorted.dedup();
    assert_eq!(sorted.len(), 300);
}

#[test]
fn id_factory_clock_seam_is_repeatable() {
    let mut first = create_task_id_factory(|| 0x1234_5678);
    let mut second = create_task_id_factory(|| 0x1234_5678);
    let first_ids: Vec<String> = (0..3)
        .map(|_| first.next_id().expect("id").to_string())
        .collect();
    let second_ids: Vec<String> = (0..3)
        .map(|_| second.next_id().expect("id").to_string())
        .collect();
    assert_eq!(first_ids, second_ids);
    assert!(first_ids.iter().all(|id| is_canonical(id)));
}

#[test]
fn id_wrap_pressure_never_wraps_or_repeats() {
    let mut factory = create_task_id_factory(|| 0x00ff_ffff);
    let ids: Vec<String> = (0..300)
        .map(|_| factory.next_id().expect("id").to_string())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    sorted.dedup();
    assert_eq!(sorted.len(), 300);
    assert!(!ids.contains(&"st_00000000".to_string()));
}

// ---- state/transitions-table.test.ts ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Start,
    Complete,
    Fail,
    Cancel,
    Interrupt,
    Lose,
}

fn transition_for(event: Event) -> TaskTransition {
    match event {
        Event::Start => start(),
        Event::Complete => complete("done", None),
        Event::Fail => fail("failed"),
        Event::Cancel => cancel(Some("cancelled")),
        Event::Interrupt => interrupt("interrupted"),
        Event::Lose => lose("missing child"),
    }
}

fn expected_table(status: TaskStatus, event: Event) -> (bool, TaskStatus, &'static str) {
    const APPLIED: &str = "transition_applied";
    const INVALID: &str = "invalid_transition_ignored";
    const LATE: &str = "late_transition_ignored";
    match (status, event) {
        (TaskStatus::Pending, Event::Start) => (true, TaskStatus::Running, APPLIED),
        (TaskStatus::Pending, Event::Cancel) => (true, TaskStatus::Cancelled, APPLIED),
        (TaskStatus::Pending, _) => (false, TaskStatus::Pending, INVALID),
        (TaskStatus::Running, Event::Complete) => (true, TaskStatus::Completed, APPLIED),
        (TaskStatus::Running, Event::Fail) => (true, TaskStatus::Error, APPLIED),
        (TaskStatus::Running, Event::Cancel) => (true, TaskStatus::Cancelled, APPLIED),
        (TaskStatus::Running, Event::Interrupt) => (true, TaskStatus::Interrupted, APPLIED),
        (TaskStatus::Running, Event::Start | Event::Lose) => (false, TaskStatus::Running, INVALID),
        (terminal, _) => (false, terminal, LATE),
    }
}

#[test]
fn transitions_table_every_status_and_normal_event_matches_lifecycle() {
    let events = [
        Event::Start,
        Event::Complete,
        Event::Fail,
        Event::Cancel,
        Event::Interrupt,
        Event::Lose,
    ];
    let mut count = 0;
    for status in TASK_STATUSES {
        for event in events {
            let result = transition_task_record(&with_status(status), &transition_for(event));
            let key = format!("{status:?}/{event:?}");
            assert_eq!(
                (
                    key.clone(),
                    result.applied,
                    result.record.status,
                    result.audit.type_name()
                ),
                {
                    let (applied, next, audit) = expected_table(status, event);
                    (key, applied, next, audit)
                }
            );
            count += 1;
        }
    }
    assert_eq!(count, TASK_STATUSES.len() * events.len());
}

// ---- state/transitions.test.ts ----

#[test]
fn transitions_pending_rejects_running_only_terminals() {
    let results: Vec<_> = [
        complete("done", None),
        fail("failed"),
        interrupt("interrupted"),
    ]
    .iter()
    .map(|transition| transition_task_record(&pending_record(), transition))
    .collect();
    assert_eq!(
        results
            .iter()
            .map(|result| (
                result.applied,
                result.record.status,
                result.audit.type_name()
            ))
            .collect::<Vec<_>>(),
        vec![(false, TaskStatus::Pending, "invalid_transition_ignored"); 3]
    );
}

#[test]
fn transitions_running_applies_terminals() {
    let running = running();
    let statuses: Vec<_> = [
        complete("done", None),
        fail("failed"),
        cancel(Some("cancelled")),
        interrupt("interrupted"),
    ]
    .iter()
    .map(|transition| {
        let result = transition_task_record(&running, transition);
        (result.applied, result.record.status)
    })
    .collect();
    assert_eq!(
        statuses,
        vec![
            (true, TaskStatus::Completed),
            (true, TaskStatus::Error),
            (true, TaskStatus::Cancelled),
            (true, TaskStatus::Interrupted),
        ]
    );
}

#[test]
fn transitions_killed_fail_persists_killed_true() {
    let result = transition_task_record(
        &running(),
        &TaskTransition::Fail {
            timestamp: ts("2026-07-06T00:00:01.000Z"),
            error_message: "RPC child killed by signal SIGKILL (pid=1234)".into(),
            killed: true,
            run_stats: None,
        },
    );
    assert!(result.applied);
    assert_eq!(result.record.status, TaskStatus::Error);
    assert_eq!(result.record.killed, Some(true));
}

#[test]
fn transitions_plain_fail_leaves_killed_absent() {
    let result = transition_task_record(&running(), &fail("boom"));
    assert_eq!(result.record.status, TaskStatus::Error);
    assert_eq!(result.record.killed, None);
}

#[test]
fn transitions_normal_lose_is_rejected() {
    let pending = transition_task_record(&pending_record(), &lose("missing on resume"));
    let running = transition_task_record(&running(), &lose("missing on resume"));
    assert_eq!(
        (
            pending.applied,
            pending.record.status,
            pending.audit.type_name()
        ),
        (false, TaskStatus::Pending, "invalid_transition_ignored")
    );
    assert_eq!(
        (
            running.applied,
            running.record.status,
            running.audit.type_name()
        ),
        (false, TaskStatus::Running, "invalid_transition_ignored")
    );
}

#[test]
fn transitions_reconciliation_marks_lost_explicitly() {
    let pending = mark_record_lost_for_reconciliation(
        &pending_record(),
        "2026-07-06T00:00:01.000Z",
        "missing pending child",
        LostReason::KeepExisting,
    );
    let running = mark_record_lost_for_reconciliation(
        &running(),
        "2026-07-06T00:00:01.000Z",
        "missing running child",
        LostReason::KeepExisting,
    );
    assert_eq!(
        (
            pending.applied,
            pending.record.status,
            pending.record.error_message.as_deref()
        ),
        (true, TaskStatus::Lost, Some("missing pending child"))
    );
    assert_eq!(
        (
            running.applied,
            running.record.status,
            running.record.error_message.as_deref()
        ),
        (true, TaskStatus::Lost, Some("missing running child"))
    );
}

#[test]
fn transitions_terminal_residency_only_changes_residency() {
    let completed = transition_task_record(&running(), &complete("done", None)).record;
    let evicted = transition_task_record(&completed, &TaskTransition::Evict { timestamp: ts("t") });
    let disposed =
        transition_task_record(&completed, &TaskTransition::Dispose { timestamp: ts("t") });
    assert_eq!(
        (
            evicted.applied,
            evicted.record.status,
            evicted.record.residency_state
        ),
        (true, TaskStatus::Completed, ResidencyState::Evicted)
    );
    assert_eq!(
        (
            disposed.applied,
            disposed.record.status,
            disposed.record.residency_state
        ),
        (true, TaskStatus::Completed, ResidencyState::Disposed)
    );
}

#[test]
fn transitions_every_terminal_every_residency_transition() {
    let terminal = [
        TaskStatus::Completed,
        TaskStatus::Error,
        TaskStatus::Cancelled,
        TaskStatus::Interrupted,
        TaskStatus::Lost,
    ];
    let residency = [
        (
            TaskTransition::Evict { timestamp: ts("t") },
            ResidencyState::Evicted,
        ),
        (
            TaskTransition::Dispose { timestamp: ts("t") },
            ResidencyState::Disposed,
        ),
        (
            TaskTransition::PersistOnly { timestamp: ts("t") },
            ResidencyState::PersistedOnly,
        ),
        (
            TaskTransition::DetachRpc { timestamp: ts("t") },
            ResidencyState::RpcDetached,
        ),
        (
            TaskTransition::MarkResident { timestamp: ts("t") },
            ResidencyState::Resident,
        ),
    ];
    let mut count = 0;
    for status in terminal {
        for (transition, expected) in &residency {
            let result = transition_task_record(&with_status(status), transition);
            assert_eq!(
                (
                    result.applied,
                    result.record.status,
                    result.record.residency_state
                ),
                (true, status, *expected)
            );
            count += 1;
        }
    }
    assert_eq!(count, 25);
}

#[test]
fn transitions_interrupted_ignores_late_completion() {
    let interrupted = transition_task_record(&running(), &interrupt("operator interrupt")).record;
    let late = transition_task_record(&interrupted, &complete("too late", None));
    assert_eq!(
        (late.applied, late.record.status, late.audit.type_name()),
        (false, TaskStatus::Interrupted, "late_transition_ignored")
    );
}

#[test]
fn transitions_w2trans_pending_cancel_applies() {
    let result = transition_task_record(&pending_record(), &cancel(None));
    assert_eq!(
        (
            result.applied,
            result.record.status,
            result.audit.type_name()
        ),
        (true, TaskStatus::Cancelled, "transition_applied")
    );
}

#[test]
fn transitions_w2trans_pending_interrupt_is_invalid() {
    let result = transition_task_record(&pending_record(), &interrupt("interrupted"));
    assert_eq!(
        (
            result.applied,
            result.record.status,
            result.audit.type_name()
        ),
        (false, TaskStatus::Pending, "invalid_transition_ignored")
    );
}

#[test]
fn transitions_w2trans_running_cancel_still_applies() {
    let result = transition_task_record(&running(), &cancel(Some("cancelled")));
    assert_eq!(
        (result.applied, result.record.status),
        (true, TaskStatus::Cancelled)
    );
}

#[test]
fn transitions_w2trans_terminal_cancel_is_idempotent() {
    let completed = transition_task_record(&running(), &complete("done", None)).record;
    let cancelled = transition_task_record(&running(), &cancel(Some("cancelled"))).record;
    let again = cancel(Some("double cancel"));
    let completed_cancel = transition_task_record(&completed, &again);
    let cancelled_cancel = transition_task_record(&cancelled, &again);
    assert_eq!(
        (
            completed_cancel.applied,
            completed_cancel.record.status,
            completed_cancel.audit.type_name()
        ),
        (false, TaskStatus::Completed, "late_transition_ignored")
    );
    assert_eq!(
        (
            cancelled_cancel.applied,
            cancelled_cancel.record.status,
            cancelled_cancel.audit.type_name()
        ),
        (false, TaskStatus::Cancelled, "late_transition_ignored")
    );
}

#[test]
fn transitions_w2trans_pending_cancel_stamps_reason() {
    let result = transition_task_record(&pending_record(), &cancel(Some("cancelled while queued")));
    assert_eq!(
        (
            result.applied,
            result.record.status,
            result.record.error_message.as_deref()
        ),
        (true, TaskStatus::Cancelled, Some("cancelled while queued"))
    );
}

#[test]
fn transitions_persist_only_clears_both_pids_and_keeps_epochs() {
    let running = TaskRecord {
        host_pid: Some(777),
        notification: TaskNotification {
            run_epoch: 3,
            notified_epoch: 1,
            ..TaskNotification::default()
        },
        ..transition_task_record(
            &pending_record(),
            &TaskTransition::Start {
                timestamp: ts("t"),
                pid: Some(4321),
                child_session_id: None,
            },
        )
        .record
    };
    let result = transition_task_record(
        &running,
        &TaskTransition::PersistOnly {
            timestamp: ts("t1"),
        },
    );
    assert!(result.applied);
    assert_eq!(result.record.status, TaskStatus::Running);
    assert_eq!(result.record.residency_state, ResidencyState::PersistedOnly);
    let json = serde_json::to_value(&result.record).expect("json");
    assert!(json.get("host_pid").is_none());
    assert!(json.get("pid").is_none());
    assert_eq!(
        json["notification"],
        json!({ "run_epoch": 3, "notified_epoch": 1 })
    );
}

#[test]
fn transitions_detach_rpc_retains_pid_and_clears_host() {
    let running = TaskRecord {
        host_pid: Some(777),
        ..transition_task_record(
            &pending_record(),
            &TaskTransition::Start {
                timestamp: ts("t"),
                pid: Some(4321),
                child_session_id: None,
            },
        )
        .record
    };
    let result = transition_task_record(
        &running,
        &TaskTransition::DetachRpc {
            timestamp: ts("t1"),
        },
    );
    assert_eq!(
        (
            result.applied,
            result.record.status,
            result.record.residency_state
        ),
        (true, TaskStatus::Running, ResidencyState::RpcDetached)
    );
    assert_eq!(result.record.host_pid, None);
    assert_eq!(result.record.pid, Some(4321));
}

#[test]
fn transitions_persist_only_keeps_terminal_facts() {
    let run_stats = TaskRunStats {
        runtime_ms: 1200,
        turns: 2,
        tool_calls: 3,
        output_tokens: Some(10),
        ..TaskRunStats::default()
    };
    let completed = TaskRecord {
        host_pid: Some(777),
        ..transition_task_record(&running(), &complete("shipped", Some(run_stats.clone()))).record
    };
    let result = transition_task_record(
        &completed,
        &TaskTransition::PersistOnly {
            timestamp: ts("t2"),
        },
    );
    assert_eq!(
        (
            result.applied,
            result.record.status,
            result.record.residency_state
        ),
        (true, TaskStatus::Completed, ResidencyState::PersistedOnly)
    );
    assert_eq!(result.record.final_response.as_deref(), Some("shipped"));
    assert_eq!(result.record.run_stats, Some(run_stats));
    assert_eq!(result.record.host_pid, None);
}

#[test]
fn transitions_suspended_running_accepts_late_complete() {
    let suspended = transition_task_record(
        &TaskRecord {
            host_pid: Some(777),
            ..running()
        },
        &TaskTransition::PersistOnly {
            timestamp: ts("t1"),
        },
    )
    .record;
    let stats = TaskRunStats {
        runtime_ms: 50,
        turns: 1,
        tool_calls: 0,
        ..TaskRunStats::default()
    };
    let late = transition_task_record(
        &suspended,
        &complete("finished while suspended", Some(stats.clone())),
    );
    assert_eq!(
        (
            late.applied,
            late.record.status,
            late.record.residency_state
        ),
        (true, TaskStatus::Completed, ResidencyState::PersistedOnly)
    );
    assert_eq!(
        late.record.final_response.as_deref(),
        Some("finished while suspended")
    );
    assert_eq!(late.record.run_stats, Some(stats));
}

// ---- state/model.test.ts ----

fn expected_messageability(status: TaskStatus, residency: ResidencyState) -> Messageability {
    match (status, residency) {
        (TaskStatus::Pending | TaskStatus::Running, ResidencyState::Resident) => {
            Messageability::Steer
        }
        (
            TaskStatus::Completed | TaskStatus::Error | TaskStatus::Interrupted,
            ResidencyState::Resident,
        ) => Messageability::Revive,
        _ => Messageability::NotContinuable,
    }
}

#[test]
fn model_messageability_table_is_exhaustive() {
    let mut count = 0;
    for status in TASK_STATUSES {
        for residency in RESIDENCY_STATES {
            assert_eq!(
                (status, residency, messageability(status, residency)),
                (
                    status,
                    residency,
                    expected_messageability(status, residency)
                )
            );
            count += 1;
        }
    }
    assert_eq!(count, 35);
}

#[test]
fn model_suspended_residencies_are_not_continuable() {
    for status in TASK_STATUSES {
        for residency in [ResidencyState::PersistedOnly, ResidencyState::RpcDetached] {
            assert_eq!(
                messageability(status, residency),
                Messageability::NotContinuable
            );
        }
    }
}

#[test]
fn model_cancelled_ignores_late_failure() {
    let cancelled = transition_task_record(&running(), &cancel(Some("user cancelled"))).record;
    let late = transition_task_record(&cancelled, &fail("process exited later"));
    assert!(!late.applied);
    assert_eq!(late.record.status, TaskStatus::Cancelled);
    assert_eq!(late.record.error_message.as_deref(), Some("user cancelled"));
    assert_eq!(
        late.audit,
        TaskTransitionAudit::LateTransitionIgnored {
            attempted_status: TaskStatus::Error,
            current_status: TaskStatus::Cancelled,
        }
    );
}

// ---- state/resolved-reasoning.test.ts ----

fn base_model() -> ResolvedModelRecord {
    ResolvedModelRecord {
        display: "GPT-5.6 Sol".into(),
        ..ResolvedModelRecord::new(ResolvedModelSource::Category, "openai", "gpt-5.6-sol")
    }
}

#[test]
fn reasoning_canonical_wins_over_legacy() {
    let record = ResolvedModelRecord {
        reasoning: Some("xhigh".into()),
        reasoning_effort: Some("low".into()),
        variant: Some("medium".into()),
        ..base_model()
    };
    assert_eq!(read_resolved_reasoning(&record), Some("xhigh"));
}

#[test]
fn reasoning_legacy_effort_is_used() {
    let record = ResolvedModelRecord {
        reasoning_effort: Some("minimal".into()),
        variant: Some("high".into()),
        ..base_model()
    };
    assert_eq!(read_resolved_reasoning(&record), Some("minimal"));
}

#[test]
fn reasoning_variant_only_is_used() {
    let record = ResolvedModelRecord {
        variant: Some("high".into()),
        ..base_model()
    };
    assert_eq!(read_resolved_reasoning(&record), Some("high"));
}

#[test]
fn reasoning_absent_is_none() {
    assert_eq!(read_resolved_reasoning(&base_model()), None);
}

#[test]
fn reasoning_fields_mirror_legacy_effort() {
    let record = ResolvedModelRecord {
        reasoning_effort: Some("minimal".into()),
        variant: Some("high".into()),
        ..base_model()
    };
    assert_eq!(
        resolved_reasoning_fields(&record),
        (Some("minimal".to_string()), Some("minimal".to_string()))
    );
}

#[test]
fn reasoning_fields_invent_nothing() {
    assert_eq!(resolved_reasoning_fields(&base_model()), (None, None));
}

// ---- state/spawn-spec.test.ts ----

fn parse_spec(value: serde_json::Value) -> TaskSpawnSpec {
    let mut record = serde_json::to_value(pending_record()).expect("json");
    record["spawn_spec"] = value;
    parse_task_record(&record, "/tmp/r.json", &mut Vec::new())
        .expect("parse")
        .spawn_spec
        .expect("spec")
}

#[test]
fn spawn_spec_legacy_is_not_v1() {
    let legacy = TaskSpawnSpec::LegacyProcess {
        cwd: "/tmp/project".into(),
        extensions: None,
        member_env: None,
    };
    assert!(!is_spawn_spec_v1(&legacy));
}

#[test]
fn spawn_spec_legacy_with_extensions_stays_legacy() {
    let legacy = TaskSpawnSpec::LegacyProcess {
        cwd: "/tmp/project".into(),
        extensions: Some(vec!["/tmp/member-extension.ts".into()]),
        member_env: Some(vec![("SENPI_TASK_MEMBER".into(), "run-1::alpha".into())]),
    };
    assert!(!is_spawn_spec_v1(&legacy));
}

#[test]
fn spawn_spec_v1_narrows_to_rebuildable_fields() {
    let spec = parse_spec(json!({
        "version": 1,
        "cwd": "/tmp/project",
        "prompt": "implement the south gate",
        "instructions": "keep the ledger intact",
        "member_scoped_tool_names": ["task", "read"],
    }));
    assert!(is_spawn_spec_v1(&spec));
    assert_eq!(
        spec.as_v1(),
        Some(&SpawnSpecV1 {
            cwd: "/tmp/project".into(),
            prompt: "implement the south gate".into(),
            instructions: Some("keep the ledger intact".into()),
            member_scoped_tool_names: Some(vec!["task".into(), "read".into()]),
            isolation: None,
        })
    );
}

#[test]
fn spawn_spec_unknown_version_is_not_v1() {
    let spec = parse_spec(json!({ "version": 2, "cwd": "/tmp/project", "prompt": "sneaky" }));
    assert!(!is_spawn_spec_v1(&spec));
}

// ---- state/run-stats-record.test.ts ----

fn run_stats() -> TaskRunStats {
    TaskRunStats {
        runtime_ms: 12_500,
        turns: 3,
        tool_calls: 5,
        output_tokens: Some(900),
        total_tokens: Some(4_200),
        generation_ms: Some(7_600),
        tokens_per_second: Some(118.0),
        cost_usd: Some(0.4213),
        cache_hit_rate_last: Some(0.9123),
        cache_hit_rate_run: Some(0.8712),
    }
}

fn running_fixture() -> serde_json::Value {
    json!({
        "task_id": "st_deadbeef",
        "status": "running",
        "residency_state": "resident",
        "parent_session_id": "parent-session",
        "root_session_id": "root-session",
        "depth": 1,
        "execution_mode": "in-process",
        "model": "gpt-5.2",
        "created_at": "2026-07-06T01:00:00.000Z",
        "updated_at": "2026-07-06T01:00:01.000Z",
        "notify_on_terminal": false,
        "notification": { "run_epoch": 0, "notified_epoch": -1 },
    })
}

fn running_record_fixture() -> TaskRecord {
    parse_task_record(&running_fixture(), "/tmp/record.json", &mut Vec::new()).expect("parse")
}

#[test]
fn run_stats_complete_carries_stats() {
    let result = transition_task_record(
        &running_record_fixture(),
        &complete("done", Some(run_stats())),
    );
    assert!(result.applied);
    assert_eq!(result.record.run_stats, Some(run_stats()));
}

#[test]
fn run_stats_fail_cancel_interrupt_persist_stats() {
    let record = running_record_fixture();
    let failed = transition_task_record(
        &record,
        &TaskTransition::Fail {
            timestamp: ts("t"),
            error_message: "boom".into(),
            killed: false,
            run_stats: Some(run_stats()),
        },
    );
    let cancelled = transition_task_record(
        &record,
        &TaskTransition::Cancel {
            timestamp: ts("t"),
            error_message: None,
            run_stats: Some(run_stats()),
        },
    );
    let interrupted = transition_task_record(
        &record,
        &TaskTransition::Interrupt {
            timestamp: ts("t"),
            error_message: None,
            run_stats: Some(run_stats()),
        },
    );
    for result in [failed, cancelled, interrupted] {
        assert_eq!(result.record.run_stats, Some(run_stats()));
    }
}

#[test]
fn run_stats_round_trip_through_parse() {
    let mut record = running_fixture();
    record["status"] = json!("completed");
    record["run_stats"] = serde_json::to_value(run_stats()).expect("json");
    let parsed = parse_task_record(&record, "/tmp/record.json", &mut Vec::new()).expect("parse");
    assert_eq!(parsed.run_stats, Some(run_stats()));
}

#[test]
fn run_stats_malformed_rejects_record() {
    let mut record = running_fixture();
    record["run_stats"] = json!({ "runtime_ms": "12" });
    assert!(parse_task_record(&record, "/tmp/record.json", &mut Vec::new()).is_err());
}

#[test]
fn run_stats_both_cache_rates_round_trip() {
    let mut record = running_fixture();
    record["status"] = json!("completed");
    record["run_stats"] = json!({
        "runtime_ms": 10, "turns": 2, "tool_calls": 0,
        "cost_usd": 0.4213, "cache_hit_rate_last": 0.9, "cache_hit_rate_run": 0.4,
    });
    let stats = parse_task_record(&record, "/tmp/record.json", &mut Vec::new())
        .expect("parse")
        .run_stats
        .expect("stats");
    assert_eq!(
        (
            stats.cost_usd,
            stats.cache_hit_rate_last,
            stats.cache_hit_rate_run
        ),
        (Some(0.4213), Some(0.9), Some(0.4))
    );
}

#[test]
fn run_stats_legacy_cache_hit_rate_is_whole_run() {
    let mut record = running_fixture();
    record["status"] = json!("completed");
    record["run_stats"] =
        json!({ "runtime_ms": 10, "turns": 1, "tool_calls": 0, "cache_hit_rate": 0.6 });
    let stats = parse_task_record(&record, "/tmp/record.json", &mut Vec::new())
        .expect("parse")
        .run_stats
        .expect("stats");
    assert_eq!(stats.cache_hit_rate_run, Some(0.6));
    assert_eq!(stats.cache_hit_rate_last, None);
    assert!(
        serde_json::to_value(&stats)
            .expect("json")
            .get("cache_hit_rate")
            .is_none()
    );
}

#[test]
fn run_stats_non_numeric_cost_rejects_record() {
    let mut record = running_fixture();
    record["run_stats"] =
        json!({ "runtime_ms": 10, "turns": 1, "tool_calls": 0, "cost_usd": "0.42" });
    assert!(parse_task_record(&record, "/tmp/record.json", &mut Vec::new()).is_err());
}
