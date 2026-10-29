use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, Mutex, PoisonError},
};

use maho_omo_task::component::{TaskTerminalEdge, TaskTerminalObservers, TaskTerminalSubscription};
use senpi_task::state::TaskStatus;
use serde_json::Value;

use crate::{
    delegation_projection::{
        DelegationProjectionInput, DelegationSteerCounts, project_delegation_completed,
    },
    omo_native_parallel_summary::SummaryCapture,
};

const MAX_EVENT_LOG_BYTES: u64 = 4 * 1024 * 1024;
type SessionHash = Arc<dyn Fn(&str) -> String + Send + Sync>;

#[derive(Default)]
struct CaptureState {
    emitted: HashSet<(Option<u64>, i64)>,
    prior_status: HashMap<u64, TaskStatus>,
}

pub fn create_omo_native_delegation_capture(
    observers: &TaskTerminalObservers,
    capture: SummaryCapture,
    hash_session_id: SessionHash,
    allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    diagnostics: Option<telemetry_core::TelemetryDiagnostics>,
) -> TaskTerminalSubscription {
    let state = Mutex::new(CaptureState::default());
    observers.subscribe(Arc::new(move |edge| {
        if !allowed() {
            return;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let record = &edge.record;
            let epoch = record.notification.run_epoch;
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            if !state.emitted.insert((record.task_seq, epoch)) {
                return;
            }
            let events = read_task_events(&edge.state_dir, &record.task_id);
            let reason = start_reason_of(edge, &events, &state.prior_status);
            let steer_counts = count_steers(&events, epoch);
            let session_hash = hash_session_id(&record.parent_session_id);
            let properties = project_delegation_completed(&DelegationProjectionInput {
                edge: crate::delegation_projection::TaskTerminalEdge {
                    record,
                    previous_status: edge.previous_status,
                },
                session_hash: &session_hash,
                start_reason: reason,
                steer_counts,
            });
            capture("delegation_completed", Value::Object(properties));
            if let Some(seq) = record.task_seq {
                state.prior_status.insert(seq, record.status);
            }
        }));
        if result.is_err() {
            if let Some(diagnostics) = &diagnostics {
                diagnostics(&telemetry_core::TelemetryDiagnosticInput {
                    event: telemetry_core::TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                    source: "omo-native-delegation".into(),
                    error: Some(telemetry_core::TelemetryError::new(
                        "delegation capture panicked",
                    )),
                    error_kind: Some(telemetry_core::TelemetryDiagnosticErrorKind::Error),
                    entry_kind: None,
                    error_code: None,
                    count: None,
                });
            } else {
                maho_ext_api::diagnostic("telemetry_capture_failed: omo-native-delegation");
            }
        }
    }))
}

fn start_reason_of(
    edge: &TaskTerminalEdge,
    events: &[PersistedEventLine],
    prior: &HashMap<u64, TaskStatus>,
) -> &'static str {
    let record = &edge.record;
    let epoch = record.notification.run_epoch;
    let fallback = if record.owner.is_some() {
        "dag_retry"
    } else if epoch == 0 {
        "initial_spawn"
    } else {
        "unknown"
    };
    if epoch == 0
        || !events
            .iter()
            .any(|event| event.event_type == "revived" && event.run_epoch == Some(epoch))
    {
        return fallback;
    }
    match record.task_seq.and_then(|seq| prior.get(&seq)) {
        Some(TaskStatus::Completed) => "revive_after_completed",
        Some(TaskStatus::Error) => "revive_after_error",
        Some(TaskStatus::Cancelled) => "revive_after_cancelled",
        Some(TaskStatus::Interrupted) => "revive_after_interrupted",
        Some(TaskStatus::Lost) => "revive_after_lost",
        // A previous paused task resumed by this run: the epoch proves a resume, and a paused prior run
        // was never terminal, so the reason is `session_resume` exactly as for pending and running.
        Some(TaskStatus::Pending | TaskStatus::Running | TaskStatus::Paused) | None => "session_resume",
    }
}

fn count_steers(events: &[PersistedEventLine], epoch: i64) -> DelegationSteerCounts {
    let mut counts = DelegationSteerCounts::default();
    for event in events.iter().filter(|event| event.run_epoch == Some(epoch)) {
        match event.event_type.as_str() {
            "steered" => counts.running += 1.0,
            "steer_queued" => counts.queued += 1.0,
            _ => {}
        }
    }
    counts
}

struct PersistedEventLine {
    event_type: String,
    run_epoch: Option<i64>,
}

fn read_task_events(state_dir: &Path, task_id: &str) -> Vec<PersistedEventLine> {
    let read = || -> std::io::Result<String> {
        let mut file =
            std::fs::File::open(state_dir.join("logs").join(format!("{task_id}.jsonl")))?;
        let start = file.metadata()?.len().saturating_sub(MAX_EVENT_LOG_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(MAX_EVENT_LOG_BYTES).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    let Ok(text) = read() else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let parsed: Value = serde_json::from_str(line).ok()?;
            Some(PersistedEventLine {
                event_type: parsed.get("type")?.as_str()?.to_owned(),
                run_epoch: parsed
                    .get("payload")
                    .and_then(|payload| payload.get("run_epoch"))
                    .and_then(Value::as_i64),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use senpi_task::{
        state::{TaskRecordInput, create_task_record},
        store::{PersistedTaskEvent, StateDirConfig, TaskRecordStore},
    };

    fn harness() -> (
        tempfile::TempDir,
        TaskRecordStore,
        TaskTerminalObservers,
        Arc<Mutex<Vec<Value>>>,
        TaskTerminalSubscription,
    ) {
        let root = tempfile::tempdir().expect("temporary state");
        let store = TaskRecordStore::new(&StateDirConfig {
            project_dir: root.path().into(),
            task_state_dir: None,
        });
        let observers = TaskTerminalObservers::default();
        let ledger = observers.clone();
        let state_dir = store.state_dir().to_path_buf();
        store.set_write_listener(Some(Arc::new(move |record, previous_status| {
            if record.status.is_terminal() && !previous_status.is_some_and(TaskStatus::is_terminal)
            {
                ledger.notify(&TaskTerminalEdge {
                    record: record.clone(),
                    previous_status,
                    state_dir: state_dir.clone(),
                });
            }
        })));
        let rows = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&rows);
        let subscription = create_omo_native_delegation_capture(
            &observers,
            Arc::new(move |name, properties| {
                assert_eq!(name, "delegation_completed");
                output.lock().expect("rows").push(properties);
            }),
            Arc::new(|raw| format!("hash:{raw}")),
            Arc::new(|| true),
            None,
        );
        (root, store, observers, rows, subscription)
    }

    fn record() -> senpi_task::state::TaskRecord {
        create_task_record(
            TaskRecordInput {
                task_seq: Some(7),
                parent_session_id: "older-parent".into(),
                execution_mode: "in-process".into(),
                ..Default::default()
            },
            Some(0),
        )
        .expect("record")
    }

    #[test]
    fn captures_epoch_counts_and_owner_when_reconciled_by_mutate() {
        // Given
        let (_root, store, _observers, rows, _subscription) = harness();
        let record = record();
        store.save(&record).expect("save");
        for (kind, epoch) in [("steered", 0), ("steered", 1), ("steer_queued", 0)] {
            store
                .append_event(
                    &record.task_id,
                    &PersistedTaskEvent {
                        event_type: kind.into(),
                        payload: serde_json::json!({"run_epoch":epoch}),
                    },
                )
                .expect("event");
        }
        // When
        store
            .clone()
            .mutate(&record.task_id, |old| {
                let mut next = old.clone();
                next.status = TaskStatus::Lost;
                next
            })
            .expect("reconcile");
        // Then
        let rows = rows.lock().expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["$session_id"], "hash:older-parent");
        assert_eq!(rows[0]["task_send_running_count"], 1.0);
        assert_eq!(rows[0]["task_send_queued_count"], 1.0);
    }

    #[test]
    fn deduplicates_terminal_rewrites_and_reports_revived_outcome() {
        // Given
        let (_root, store, observers, rows, _subscription) = harness();
        let mut record = record();
        record.status = TaskStatus::Completed;
        store.save(&record).expect("terminal save");
        store.replace(&record).expect("terminal rewrite");
        observers.notify(&TaskTerminalEdge {
            record: record.clone(),
            previous_status: None,
            state_dir: store.state_dir().into(),
        });
        record.status = TaskStatus::Running;
        record.notification.run_epoch = 1;
        store.replace(&record).expect("revive");
        store
            .append_event(
                &record.task_id,
                &PersistedTaskEvent {
                    event_type: "revived".into(),
                    payload: serde_json::json!({"run_epoch":1}),
                },
            )
            .expect("revived marker");
        // When
        store
            .transition(
                &record.task_id,
                &senpi_task::state::TaskTransition::Fail {
                    timestamp: record.updated_at.clone(),
                    error_message: "failed".into(),
                    killed: false,
                    run_stats: None,
                },
            )
            .expect("fail");
        // Then
        let rows = rows.lock().expect("rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["start_reason"], "revive_after_completed");
        assert_eq!(rows[1]["run_epoch"], 1);
    }

    #[test]
    fn stops_capture_when_subscription_is_dropped() {
        // Given
        let (_root, store, _observers, rows, subscription) = harness();
        drop(subscription);
        let mut record = record();
        record.status = TaskStatus::Lost;
        // When
        store.save(&record).expect("terminal save");
        // Then
        assert!(rows.lock().expect("rows").is_empty());
    }

    #[test]
    fn isolates_panicking_observer_without_starving_capture() {
        // Given
        let (_root, store, observers, rows, _subscription) = harness();
        let _broken = observers.subscribe(Arc::new(|_| panic!("observer failure")));
        let delivered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tail = delivered.clone();
        let _tail = observers.subscribe(Arc::new(move |_| {
            tail.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        let mut record = record();
        record.status = TaskStatus::Lost;
        // When
        store
            .save(&record)
            .expect("terminal save survives observer failure");
        // Then
        assert_eq!(rows.lock().expect("rows").len(), 1);
        assert!(delivered.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn skips_log_reads_hashing_and_capture_when_consent_is_off() {
        // Given
        let observers = TaskTerminalObservers::default();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let captures = calls.clone();
        let hashes = calls.clone();
        let _subscription = create_omo_native_delegation_capture(
            &observers,
            Arc::new(move |_, _| {
                captures.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
            Arc::new(move |_| {
                hashes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                "hash".into()
            }),
            Arc::new(|| false),
            None,
        );
        let edge = TaskTerminalEdge {
            record: record(),
            previous_status: None,
            state_dir: "/unused".into(),
        };
        // When
        observers.notify(&edge);
        // Then
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn diagnoses_capture_failure_without_failing_the_store() {
        // Given
        let (_root, store, observers, rows, _subscription) = harness();
        let failures = Arc::new(Mutex::new(Vec::new()));
        let diagnostics = failures.clone();
        let _broken = create_omo_native_delegation_capture(
            &observers,
            Arc::new(|_, _| panic!("transport failure")),
            Arc::new(|_| "hash".into()),
            Arc::new(|| true),
            Some(Arc::new(move |input| {
                diagnostics
                    .lock()
                    .expect("diagnostics")
                    .push(input.source.clone());
            })),
        );
        let mut record = record();
        record.status = TaskStatus::Lost;
        // When
        store
            .save(&record)
            .expect("committed despite capture failure");
        // Then
        assert_eq!(
            *failures.lock().expect("diagnostics"),
            vec!["omo-native-delegation"]
        );
        assert_eq!(rows.lock().expect("rows").len(), 1);
    }
}
