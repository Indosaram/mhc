//! `dag-run-pause-source.ts`: a DAG runtime's own contribution to the parent's pause custody.
//!
//! Retires scheduling before the durable `user_stop` lands, dispatches the run's final guidance
//! revisions into the held input custody, and installs a resumed scheduler under the admission
//! credential. A run that is not the pause sequence's own run is left alone.

pub type RecordsCallback = Arc<dyn Fn(&str) -> Vec<DagRunRecordV1> + Send + Sync>;
pub type RetireCallback = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
pub type InstallResumedRunCallback = Arc<
        dyn Fn(
                &str,
                &str,
                u64,
                &ResumeCredential,
            ) -> Result<senpi_task::dag::recovery::InstalledDagRun, String>
            + Send
            + Sync,
    >;
use std::sync::Arc;

use senpi_task::{
    dag::{
        manager::DagRunRecordV1, paused_controls::DagPauseStore, scheduler::DagSchedulerOptions,
        types::DagRunStatus,
    },
    pause::{
        admission_barrier::{AdmissionBarrier, AdmissionEntry, ResumeCredential},
        guidance::{Guidance, GuidancePorts, GuidanceTarget, dispatch_guidance},
        ports::{HoldInput as GuidanceInput, RunRef},
        registry::{ParticipantKind, ParticipantState, PauseParticipant},
    },
};

use crate::engine_run_pause::RunPauseSource;

/// What a child engine answers when asked to hold one guidance revision.
pub type DagGuidanceDelivery =
    Arc<dyn Fn(&str, &GuidanceInput) -> Result<Option<String>, String> + Send + Sync>;

pub struct DagRunPauseSourcePorts {
    pub barrier: AdmissionBarrier,
    pub records: RecordsCallback,
    pub options: Arc<dyn Fn(&DagRunRecordV1) -> DagSchedulerOptions + Send + Sync>,
    pub pauses: Arc<DagPauseStore>,
    pub retire: RetireCallback,
    pub prepare_resume: Arc<dyn Fn(&str) + Send + Sync>,
    /// `installResumedRun`: `Ok(true)` when a scheduler was installed.
    pub install_resumed_run: InstallResumedRunCallback,
    pub adopt: Arc<dyn Fn(senpi_task::dag::recovery::InstalledDagRun) + Send + Sync>,
    pub cancel_run: RetireCallback,
    pub deliver_guidance: Option<DagGuidanceDelivery>,
}

pub struct DagRunPauseSource {
    ports: Arc<DagRunPauseSourcePorts>,
}

fn active(records: Vec<DagRunRecordV1>) -> Vec<DagRunRecordV1> {
    records
        .into_iter()
        .filter(|record| {
            matches!(
                record.status,
                DagRunStatus::Pending | DagRunStatus::Running | DagRunStatus::Paused
            )
        })
        .collect()
}

impl DagRunPauseSource {
    pub fn new(ports: Arc<DagRunPauseSourcePorts>) -> Self {
        Self { ports }
    }

    fn active(&self, parent: &str) -> Vec<DagRunRecordV1> {
        active((self.ports.records)(parent))
    }

    fn guidance_targets(
        &self,
        record: &DagRunRecordV1,
        guidance: &Guidance,
    ) -> Vec<GuidanceTarget> {
        record
            .nodes
            .iter()
            .filter(|node| {
                match match &guidance.scope {
                    senpi_task::pause::guidance::GuidanceScope::Run => None,
                    senpi_task::pause::guidance::GuidanceScope::Node { node_id } => Some(node_id),
                } {
                    Some(node_id) => &node.id == node_id,
                    None => node.state != senpi_task::dag::types::DagNodeState::Completed,
                }
            })
            .map(|node| GuidanceTarget {
                node_id: node.id.clone(),
                task_id: matches!(
                    node.state,
                    senpi_task::dag::types::DagNodeState::Running
                        | senpi_task::dag::types::DagNodeState::Paused
                )
                .then(|| node.task_id.clone())
                .flatten(),
            })
            .collect()
    }
}

/// The ports the dispatch needs, built per run so the journal stays that run's own.
struct SourceGuidancePorts<'a> {
    source: &'a DagRunPauseSource,
    record: DagRunRecordV1,
}

impl GuidancePorts for SourceGuidancePorts<'_> {
    type Error = String;
    fn save(&mut self, records: &[Guidance]) -> Result<(), String> {
        let journal = senpi_task::dag::node_control_context::control_journal(
            &(self.source.ports.options)(&self.record),
            self.record.clone(),
            std::collections::BTreeMap::new(),
        )
        .map_err(|error| error.to_string())?;
        for record in records {
            let guidance = serde_json::from_value(
                serde_json::to_value(record).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let payload = if record.state == senpi_task::pause::guidance::GuidanceState::Delivered {
                senpi_task::dag::types::DagRunEventPayload::GuidanceDelivered { guidance }
            } else {
                senpi_task::dag::types::DagRunEventPayload::GuidanceDispatching { guidance }
            };
            journal.append(payload).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    fn deliver(&mut self, task: &str, input: GuidanceInput) -> Result<Option<String>, String> {
        self.source
            .ports
            .deliver_guidance
            .as_ref()
            .ok_or("run guidance custody port is not configured")?(task, &input)
    }
}

impl RunPauseSource for DagRunPauseSource {
    fn refs(&self, parent_session_id: &str) -> Vec<RunRef> {
        self.active(parent_session_id)
            .into_iter()
            .map(|record| RunRef::new(senpi_task::pause::ports::RunRefKind::Dag, record.run_id))
            .collect()
    }

    fn quiesce(&self, parent_session_id: &str, pause_seq: u64) {
        let records = self.active(parent_session_id);
        for record in &records {
            if let Err(error) = (self.ports.retire)(&record.run_id) {
                eprintln!(
                    "[omo-task] dag run retire failed: run={} error={error}",
                    record.run_id
                );
            }
        }
        for record in records {
            let current = (self.ports.options)(&record)
                .store
                .read_checkpoint::<DagRunRecordV1>(&record.run_id)
                .ok()
                .flatten();
            let Some(current) = current else {
                continue;
            };
            if !matches!(
                current.status,
                DagRunStatus::Pending | DagRunStatus::Running | DagRunStatus::Paused
            ) {
                continue;
            }
            let state = self.ports.pauses.read(&current.run_id).unwrap_or_default();
            if state
                .pause
                .as_ref()
                .and_then(|pause| pause.pause_seq)
                .unwrap_or(0)
                >= pause_seq
            {
                continue;
            }
            let options = (self.ports.options)(&current);
            let journal = senpi_task::dag::node_control_context::control_journal(
                &options,
                current.clone(),
                std::collections::BTreeMap::new(),
            );
            match journal.and_then(|journal| {
                journal
                    .append(senpi_task::dag::types::DagRunEventPayload::RunPaused {
                        reason: Some("user_stop".into()),
                        pause_seq: Some(pause_seq),
                    })
                    .map(|_| ())
            }) {
                Ok(()) => {}
                Err(error) => {
                    maho_ext_api::diagnostic(&format!("DAG Stop checkpoint failed: {error}"))
                }
            }
        }
    }

    fn dispatch_guidance(&self, parent_session_id: &str, _pause_seq: u64, _resume_id: &str) {
        for record in self.active(parent_session_id) {
            let state = self.ports.pauses.read(&record.run_id).unwrap_or_default();
            let mut guidance = state.guidance.clone();
            guidance.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
            for entry in guidance {
                let targets = self.guidance_targets(&record, &entry);
                let mut ports = SourceGuidancePorts {
                    source: self,
                    record: record.clone(),
                };
                if let Err(error) = dispatch_guidance(std::slice::from_ref(&entry), &targets, &mut ports) {
                    eprintln!(
                        "[omo-task] dag guidance dispatch failed: run={} guidance={} error={error}",
                        record.run_id, entry.id
                    );
                }
            }
        }
    }

    fn install(
        &self,
        parent_session_id: &str,
        pause_seq: u64,
        credential: &ResumeCredential,
    ) -> Result<(), String> {
        for record in self.active(parent_session_id) {
            let state = self.ports.pauses.read(&record.run_id).unwrap_or_default();
            if state.pause.as_ref().and_then(|pause| pause.pause_seq) != Some(pause_seq) {
                continue;
            }
            let entered = self.ports.barrier.enter(
                parent_session_id,
                senpi_task::pause::admission_barrier::AdmissionParticipant {
                    key: format!("run:dag:{}", record.run_id),
                    run_ref: RunRef::new(
                        senpi_task::pause::ports::RunRefKind::Dag,
                        record.run_id.clone(),
                    ),
                },
                Some(credential.clone()),
            );
            let ticket = match entered {
                AdmissionEntry::Held { .. } => {
                    return Err(senpi_task::manager::run_pause::RunPauseAdmissionHeld.to_string());
                }
                AdmissionEntry::Entered { ticket } => ticket,
            };
            (self.ports.prepare_resume)(&record.run_id);
            let installed = (self.ports.install_resumed_run)(
                &record.run_id,
                &record.parent_session_id,
                pause_seq,
                credential,
            );
            if ticket.check() != senpi_task::pause::admission_barrier::AdmissionCheck::Allowed {
                if let Ok(installed) = installed {
                    installed.scheduler.suspend();
                    let _ = installed.settled.join();
                }
                ticket.leave();
                return Err(senpi_task::manager::run_pause::RunPauseAdmissionHeld.to_string());
            }
            match installed {
                Ok(installed) => (self.ports.adopt)(installed),
                Err(error) => {
                    ticket.leave();
                    return Err(format!("DAG installation refused: {error}"));
                }
            }
            ticket.leave();
        }
        Ok(())
    }

    fn cancel(&self, parent_session_id: &str, run_ref: &RunRef) -> Result<(), String> {
        if run_ref.kind != senpi_task::pause::ports::RunRefKind::Dag {
            return Ok(());
        }
        if !self
            .active(parent_session_id)
            .iter()
            .any(|record| record.run_id == run_ref.id)
        {
            return Ok(());
        }
        (self.ports.cancel_run)(&run_ref.id)
    }

    fn snapshot(&self, parent_session_id: &str) -> Vec<PauseParticipant> {
        self.active(parent_session_id)
            .into_iter()
            .map(|record| PauseParticipant {
                key: format!("run:dag:{}", record.run_id),
                kind: ParticipantKind::DagNode,
                run_ref: RunRef::new(
                    senpi_task::pause::ports::RunRefKind::Dag,
                    record.run_id.clone(),
                ),
                task_id: None,
                node_id: None,
                run_epoch: None,
                admission: None,
                state: ParticipantState::Pending,
                ack_seq: None,
                observed_child_seq: None,
                resume_turn_id: None,
                lost_reason: None,
                at: record.updated_at.clone(),
            })
            .collect()
    }
}
