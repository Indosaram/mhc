//! Confirmed end verification for daemon-hosted children (`lifecycle/host-session-end.ts`, `#11765`).

use std::collections::HashMap;
use std::sync::{Condvar, Mutex, PoisonError};

use serde_json::json;

use super::context::LifecycleContext;
use super::destroy::spawn_spec_cwd;
use super::host_session::{
    HostSessionEnd, HostSessionEndVia, HostSessionLiveness, is_host_session_record,
};
use super::host_session_close::{HostSessionCloseOutcome, close_host_session};
use crate::state::{HostSessionIdentity, TaskRecord};
use crate::store::PersistedTaskEvent;

#[derive(Clone)]
struct FlightEntry {
    owner: String,
    in_flight: bool,
    ended: Option<HostSessionEnd>,
}

/// Shared single-flight table per `TaskRecordStore` / `LifecycleContext` instance.
#[derive(Default)]
pub struct HostSessionEndFlights {
    table: Mutex<HashMap<String, FlightEntry>>,
    changed: Condvar,
}

fn owner_of(record: &TaskRecord, host_session: &HostSessionIdentity) -> String {
    json!([
        record.notification.run_epoch,
        host_session.socket,
        host_session.instance_id,
        host_session.session_path,
        host_session.routing_id,
    ])
    .to_string()
}

struct InFlightGuard<'a> {
    flights: &'a HostSessionEndFlights,
    task_id: &'a str,
    owner: &'a str,
    committed: bool,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut table = self
            .flights
            .table
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if table
            .get(self.task_id)
            .is_some_and(|e| e.in_flight && e.owner == self.owner)
        {
            table.remove(self.task_id);
        }
        self.flights.changed.notify_all();
    }
}

/// THE one way the lifecycle decides whether a daemon-hosted child it holds no handle for has
/// ended (`#11765`). Confirmed by an answered `close_session`, or - with a generation view - by
/// the three-part proof (listing absent, no handover-parked entry, generation gone, second listing
/// still absent). Reuses an in-flight or confirmed pass only for the same run & session (`#11783`).
pub fn end_host_session_child(context: &LifecycleContext, record: &TaskRecord) -> HostSessionEnd {
    if !is_host_session_record(record) {
        return HostSessionEnd::NotEnded {
            reason: "not_host_session".to_string(),
        };
    }
    let Some(host_session) = record.host_session.as_ref() else {
        return HostSessionEnd::NotEnded {
            reason: "not_host_session".to_string(),
        };
    };
    let owner = owner_of(record, host_session);
    let flights = &context.host_session_end_flights;
    {
        let mut table = flights.table.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(existing) = table.get(&record.task_id) {
                if existing.owner == owner {
                    if let Some(ended) = &existing.ended {
                        return ended.clone();
                    }
                    if existing.in_flight {
                        table = flights
                            .changed
                            .wait(table)
                            .unwrap_or_else(PoisonError::into_inner);
                        continue;
                    }
                } else if existing.in_flight {
                    return HostSessionEnd::NotEnded {
                        reason: "earlier_run_in_flight".to_string(),
                    };
                }
            }
            table.insert(
                record.task_id.clone(),
                FlightEntry {
                    owner: owner.clone(),
                    in_flight: true,
                    ended: None,
                },
            );
            break;
        }
    }

    let mut guard = InFlightGuard {
        flights,
        task_id: &record.task_id,
        owner: &owner,
        committed: false,
    };

    let outcome = run_pass(context, record, host_session);

    let mut table = flights.table.lock().unwrap_or_else(PoisonError::into_inner);
    if matches!(outcome, HostSessionEnd::Ended { .. }) {
        table.insert(
            record.task_id.clone(),
            FlightEntry {
                owner: owner.clone(),
                in_flight: false,
                ended: Some(outcome.clone()),
            },
        );
    } else if table
        .get(&record.task_id)
        .is_some_and(|e| e.owner == owner && e.in_flight)
    {
        table.remove(&record.task_id);
    }
    guard.committed = true;
    flights.changed.notify_all();
    outcome
}

pub fn clear_host_session_end(context: &LifecycleContext, task_id: &str) {
    let mut table = context
        .host_session_end_flights
        .table
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if table
        .get(task_id)
        .is_some_and(|entry| entry.ended.is_some())
    {
        table.remove(task_id);
    }
}

fn run_pass(
    context: &LifecycleContext,
    record: &TaskRecord,
    host_session: &HostSessionIdentity,
) -> HostSessionEnd {
    let close = || -> HostSessionEnd {
        let cwd = spawn_spec_cwd(record);
        match close_host_session(context, &record.task_id, host_session, cwd.as_deref()) {
            HostSessionCloseOutcome::Closed => HostSessionEnd::Ended {
                via: HostSessionEndVia::CloseSession,
            },
            HostSessionCloseOutcome::Refused => HostSessionEnd::NotEnded {
                reason: "close_refused".to_string(),
            },
        }
    };

    match listing(context, host_session) {
        HostSessionLiveness::Live => return close(),
        HostSessionLiveness::Unknown => {
            return HostSessionEnd::NotEnded {
                reason: "host_unknown".to_string(),
            };
        }
        HostSessionLiveness::Gone => {}
    }

    let Some(view) = context.host_generation_view.as_ref() else {
        return HostSessionEnd::Ended {
            via: HostSessionEndVia::NotListed,
        };
    };

    if view.parked_entry_present(host_session) {
        return close();
    }
    if view.generation_present(host_session) {
        return HostSessionEnd::NotEnded {
            reason: "generation_cannot_cancel".to_string(),
        };
    }

    match listing(context, host_session) {
        HostSessionLiveness::Live => close(),
        HostSessionLiveness::Unknown => HostSessionEnd::NotEnded {
            reason: "host_unknown".to_string(),
        },
        HostSessionLiveness::Gone => {
            record_generation_exited(context, &record.task_id, host_session);
            HostSessionEnd::Ended {
                via: HostSessionEndVia::GenerationExited,
            }
        }
    }
}

fn listing(context: &LifecycleContext, host_session: &HostSessionIdentity) -> HostSessionLiveness {
    context
        .host_session_probe
        .refresh(Some(&host_session.socket));
    context.host_session_probe.session_liveness(host_session)
}

fn record_generation_exited(
    context: &LifecycleContext,
    task_id: &str,
    host_session: &HostSessionIdentity,
) {
    let event = PersistedTaskEvent {
        event_type: "host_session_closed".to_string(),
        payload: json!({
            "session_path": host_session.session_path,
            "socket": host_session.socket,
            "via": "generation_exited",
            "instance_id": host_session.instance_id,
        }),
    };
    if let Err(error) = context.store.append_event(task_id, &event) {
        utils::logger::log(
            "senpi-task could not record a proven host session end",
            Some(&json!({ "taskId": task_id, "error": error.to_string() })),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::lifecycle::host_session::{HostGenerationView, HostSessionProbe};
    use crate::lifecycle::host_session_close::HostSessionCloseRequest;
    use crate::lifecycle::lifecycle_tests::{FakeRegistry, default_settings, temp_store};
    use crate::lifecycle::{LifecycleDeps, resolve_context};
    use crate::state::{RunnerKind, TaskRecord, TaskStatus};

    struct StubProbe {
        liveness: Arc<dyn Fn() -> HostSessionLiveness + Send + Sync>,
    }

    impl HostSessionProbe for StubProbe {
        fn daemon_alive(&self, _: &HostSessionIdentity) -> bool {
            true
        }
        fn session_live(&self, _: &HostSessionIdentity) -> bool {
            (self.liveness)() == HostSessionLiveness::Live
        }
        fn session_liveness(&self, _: &HostSessionIdentity) -> HostSessionLiveness {
            (self.liveness)()
        }
        fn refresh(&self, _: Option<&str>) {}
    }

    struct StubView {
        gen_present: Arc<AtomicBool>,
        parked_present: Arc<AtomicBool>,
    }

    impl HostGenerationView for StubView {
        fn generation_present(&self, _: &HostSessionIdentity) -> bool {
            self.gen_present.load(Ordering::Acquire)
        }
        fn parked_entry_present(&self, _: &HostSessionIdentity) -> bool {
            self.parked_present.load(Ordering::Acquire)
        }
    }

    fn host_record(run_epoch: i64, task_id: Option<&str>) -> TaskRecord {
        let mut rec = crate::state::create_task_record(
            crate::state::TaskRecordInput {
                parent_session_id: "parent-1".to_string(),
                root_session_id: "parent-1".to_string(),
                execution_mode: "rpc".to_string(),
                ..crate::state::TaskRecordInput::default()
            },
            Some(1_000),
        )
        .expect("record");
        if let Some(id) = task_id {
            rec.task_id = id.to_string();
        }
        rec.status = TaskStatus::Running;
        rec.residency_state = crate::state::ResidencyState::Resident;
        rec.runner_kind = Some(RunnerKind::HostSession);
        rec.host_session = Some(HostSessionIdentity {
            socket: "/tmp/sock".to_string(),
            instance_id: "gen-1".to_string(),
            session_path: "/tmp/s.jsonl".to_string(),
            routing_id: "r1".to_string(),
            daemon_pid: None,
            effect_seq: None,
            adopted_entry_id: None,
        });
        rec.notification.run_epoch = run_epoch;
        rec
    }

    #[test]
    fn generation_view_blocks_premature_cancel_closes_parked_and_proves_generation_exit() {
        let temp = temp_store();
        let rec = host_record(1, None);
        let task_id = rec.task_id.clone();
        temp.store.save(&rec).expect("save");
        let closes = Arc::new(AtomicUsize::new(0));
        let gen_present = Arc::new(AtomicBool::new(true));
        let parked_present = Arc::new(AtomicBool::new(false));
        let closes_clone = Arc::clone(&closes);

        let mut deps = LifecycleDeps::new(
            temp.store.clone(),
            Arc::new(FakeRegistry::default()),
            default_settings(),
        );
        deps.host_session_probe = Some(Arc::new(StubProbe {
            liveness: Arc::new(|| HostSessionLiveness::Gone),
        }));
        deps.host_generation_view = Some(Arc::new(StubView {
            gen_present: Arc::clone(&gen_present),
            parked_present: Arc::clone(&parked_present),
        }));
        deps.host_session_close = Some(Arc::new(move |_: &HostSessionCloseRequest| {
            closes_clone.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }));
        let ctx = resolve_context(deps);

        // Listed gone while old generation directory still present -> cannot cancel.
        assert_eq!(
            end_host_session_child(&ctx, &rec),
            HostSessionEnd::NotEnded {
                reason: "generation_cannot_cancel".to_string()
            }
        );

        // Parked entry present -> falls back to close_session.
        parked_present.store(true, Ordering::Release);
        assert_eq!(
            end_host_session_child(&ctx, &rec),
            HostSessionEnd::Ended {
                via: HostSessionEndVia::CloseSession
            }
        );
        assert_eq!(closes.load(Ordering::Acquire), 1);

        // Next run does NOT reuse earlier run's confirmed end, and proves generation exited.
        parked_present.store(false, Ordering::Release);
        gen_present.store(false, Ordering::Release);
        let rec2 = host_record(2, Some(&task_id));
        assert_eq!(
            end_host_session_child(&ctx, &rec2),
            HostSessionEnd::Ended {
                via: HostSessionEndVia::GenerationExited
            }
        );
        let log_path = temp
            .store
            .state_dir()
            .join("logs")
            .join(format!("{task_id}.jsonl"));
        let log_raw = std::fs::read_to_string(&log_path).expect("events log");
        let events: Vec<serde_json::Value> = log_raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("json event"))
            .collect();
        assert!(events.iter().any(|ev| {
            ev["type"] == "host_session_closed" && ev["payload"]["via"] == "generation_exited"
        }));
    }
}
