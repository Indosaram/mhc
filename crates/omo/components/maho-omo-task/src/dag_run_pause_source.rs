//! `dag-run-pause-source.ts`: a DAG runtime's own contribution to the parent's pause custody.
//!
//! Retires scheduling before the durable `user_stop` lands, dispatches the run's final guidance
//! revisions into the held input custody, and installs a resumed scheduler under the admission
//! credential. A run that is not the pause sequence's own run is left alone.

use std::sync::Arc;

use senpi_task::{
    dag::{
        manager::DagRunRecordV1,
        paused_controls::{DagPauseStore, DagRunPauseReason},
        scheduler::DagSchedulerOptions,
        types::DagRunStatus,
    },
    pause::{
        admission_barrier::{AdmissionBarrier, AdmissionEntry, ResumeCredential},
        guidance::{Guidance, GuidanceDispatchPorts, GuidanceInput, GuidanceTarget, dispatch_guidance},
        ports::RunRef,
        registry::{PauseParticipant, PauseParticipantKind, PauseParticipantState},
    },
};

use crate::engine_run_pause::RunPauseSource;

/// What a child engine answers when asked to hold one guidance revision.
pub type DagGuidanceDelivery = Arc<dyn Fn(&str, &GuidanceInput) -> Result<Option<String>, String> + Send + Sync>;

pub struct DagRunPauseSourcePorts {
    pub barrier: AdmissionBarrier,
    pub records: Arc<dyn Fn(&str) -> Vec<DagRunRecordV1> + Send + Sync>,
    pub options: Arc<dyn Fn(&DagRunRecordV1) -> DagSchedulerOptions + Send + Sync>,
    pub pauses: Arc<DagPauseStore>,
    pub retire: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
    pub prepare_resume: Arc<dyn Fn(&str) + Send + Sync>,
    /// `installResumedRun`: `Ok(true)` when a scheduler was installed.
    pub install_resumed_run: Arc<dyn Fn(&str, &str, u64, &ResumeCredential) -> Result<bool, String> + Send + Sync>,
    pub adopt: Arc<dyn Fn(&str) + Send + Sync>,
    pub cancel_run: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>,
    pub deliver_guidance: Option<DagGuidanceDelivery>,
}

pub struct DagRunPauseSource {
    ports: Arc<DagRunPauseSourcePorts>,
}

fn active(records: Vec<DagRunRecordV1>) -> Vec<DagRunRecordV1> {
    records
        .into_iter()
        .filter(|record| matches!(record.status, DagRunStatus::Pending | DagRunStatus::Running | DagRunStatus::Paused))
        .collect()
}

impl DagRunPauseSource {
    pub fn new(ports: Arc<DagRunPauseSourcePorts>) -> Self {
        Self { ports }
    }

    fn active(&self, parent: &str) -> Vec<DagRunRecordV1> {
        active((self.ports.records)(parent))
    }

    fn guidance_targets(&self, record: &DagRunRecordV1, guidance: &Guidance) -> Vec<GuidanceTarget> {
        record
            .nodes
            .iter()
            .filter(|node| match guidance.node_id() {
                Some(node_id) => node.id == node_id,
                None => node.state != senpi_task::dag::types::DagNodeState::Completed,
            })
            .map(|node| GuidanceTarget {
                node_id: node.id.clone(),
                task_id: matches!(node.state, senpi_task::dag::types::DagNodeState::Running | senpi_task::dag::types::DagNodeState::Paused)
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
    saved: std::sync::Mutex<Vec<Guidance>>,
}

impl GuidanceDispatchPorts for SourceGuidancePorts<'_> {
    fn save(&self, records: &[Guidance]) {
        *self.saved.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = records.to_vec();
        let mut state = self.source.ports.pauses.read(&self.record.run_id).unwrap_or_default();
        state.guidance = records.to_vec();
        let _ = self.source.ports.pauses.write(&self.record.run_id, &state);
    }

    fn deliver(&self, task_id: &str, input: GuidanceInput) -> Result<senpi_task::pause::guidance::GuidanceDeliveryReceipt, String> {
        let Some(deliver) = &self.source.ports.deliver_guidance else {
            return Err("run guidance custody port is not configured".into());
        };
        Ok(senpi_task::pause::guidance::GuidanceDeliveryReceipt { entry_id: deliver(task_id, &input)? })
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
                eprintln!("[omo-task] dag run retire failed: run={} error={error}", record.run_id);
            }
        }
        for record in records {
            let current = (self.ports.options)(&record).store.read_checkpoint::<DagRunRecordV1>(&record.run_id).ok().flatten();
            let Some(current) = current else {
                continue;
            };
            if !matches!(current.status, DagRunStatus::Pending | DagRunStatus::Running | DagRunStatus::Paused) {
                continue;
            }
            let mut state = self.ports.pauses.read(&current.run_id).unwrap_or_default();
            if state.pause.as_ref().and_then(|pause| pause.pause_seq).unwrap_or(0) >= pause_seq {
                continue;
            }
            // The durable user_stop lands beside the run, then the journal records it.
            state.pause = Some(senpi_task::dag::paused_controls::DagRunPause {
                reason: DagRunPauseReason::UserStop,
                pause_seq: Some(pause_seq),
            });
            let _ = self.ports.pauses.write(&current.run_id, &state);
        }
    }

    fn dispatch_guidance(&self, parent_session_id: &str, _pause_seq: u64, _resume_id: &str) {
        for record in self.active(parent_session_id) {
            let state = self.ports.pauses.read(&record.run_id).unwrap_or_default();
            let mut guidance = state.guidance.clone();
            guidance.sort_by(|left, right| left.created_at.cmp(&right.created_at).then_with(|| left.id.cmp(&right.id)));
            for entry in guidance {
                let targets = self.guidance_targets(&record, &entry);
                let ports = SourceGuidancePorts { source: self, record: record.clone(), saved: std::sync::Mutex::new(Vec::new()) };
                if let Err(error) = dispatch_guidance(std::slice::from_ref(&entry), &targets, &ports) {
                    eprintln!("[omo-task] dag guidance dispatch failed: run={} guidance={} error={error}", record.run_id, entry.id);
                }
            }
        }
    }

    fn install(&self, parent_session_id: &str, pause_seq: u64, credential: &ResumeCredential) -> Result<(), String> {
        for record in self.active(parent_session_id) {
            let state = self.ports.pauses.read(&record.run_id).unwrap_or_default();
            if state.pause.as_ref().and_then(|pause| pause.pause_seq) != Some(pause_seq) {
                continue;
            }
            let entered = self.ports.barrier.enter(
                parent_session_id,
                senpi_task::pause::admission_barrier::AdmissionParticipant {
                    key: format!("run:dag:{}", record.run_id),
                    run_ref: RunRef::new(senpi_task::pause::ports::RunRefKind::Dag, record.run_id.clone()),
                },
                Some(credential.clone()),
            );
            let ticket = match entered {
                AdmissionEntry::Held { .. } => {
                    return Err(senpi_task::manager::run_pause::RunPauseAdmissionHeld.to_string());
                }
                AdmissionEntry::Entered { ticket } => ticket,
            };
            let installed = (self.ports.install_resumed_run)(&record.run_id, &record.parent_session_id, pause_seq, credential);
            if ticket.check() != senpi_task::pause::admission_barrier::AdmissionCheck::Allowed {
                ticket.leave();
                return Err(senpi_task::manager::run_pause::RunPauseAdmissionHeld.to_string());
            }
            (self.ports.prepare_resume)(&record.run_id);
            match installed {
                Ok(true) => (self.ports.adopt)(&record.run_id),
                Ok(false) => {
                    ticket.leave();
                    return Err("DAG installation refused: not_installed".into());
                }
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
        if !self.active(parent_session_id).iter().any(|record| record.run_id == run_ref.id) {
            return Ok(());
        }
        (self.ports.cancel_run)(&run_ref.id)
    }

    fn snapshot(&self, parent_session_id: &str) -> Vec<PauseParticipant> {
        self.active(parent_session_id)
            .into_iter()
            .map(|record| PauseParticipant {
                key: format!("run:dag:{}", record.run_id),
                kind: PauseParticipantKind::DagNode,
                run_ref: RunRef::new(senpi_task::pause::ports::RunRefKind::Dag, record.run_id.clone()),
                task_id: None,
                node_id: None,
                run_epoch: None,
                admission: None,
                state: PauseParticipantState::Pending,
                ack_seq: None,
                observed_child_seq: None,
                resume_turn_id: None,
                lost_reason: None,
                at: record.updated_at.clone(),
            })
            .collect()
    }
}
