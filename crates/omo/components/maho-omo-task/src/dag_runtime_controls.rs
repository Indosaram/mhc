//! `dag-runtime-controls.ts`: retry / send / amend / guide for a run, paused or live.
//!
//! A run the USER stopped must not restart scheduling: send and guide go straight through the
//! record-based controls, and retry/amend take the record-based path instead of acquiring or adopting
//! a scheduler. Only a run that is not user-stopped reaches the live entry points.

pub type LiveRetryCallback = Arc<dyn Fn(&str, Option<&[String]>, Option<&str>) -> Result<DagRunSnapshot, DagToolExecutionError> + Send + Sync>;
pub type LiveSendCallback = Arc<dyn Fn(&str, &str, &str) -> Result<DagToolSendOutcome, DagToolExecutionError> + Send + Sync>;
pub type LiveAmendCallback = Arc<dyn Fn(&str, senpi_task::dag::graph::DagDefinition) -> Result<DagRunSnapshot, DagToolExecutionError> + Send + Sync>;
use std::sync::Arc;

use senpi_task::dag::{
    paused_controls::{DagPauseStore, DagRunPauseReason, PausedDagGuideCommand, apply_paused_dag_guidance},
    scheduler::DagSchedulerOptions,
    types::DagRunSnapshot,
};

use crate::dag_tool::{DagToolExecutionError, DagToolSendDelivery, DagToolSendOutcome};

pub struct DagRuntimeControlsPorts {
    pub pauses: Arc<DagPauseStore>,
    pub options: Arc<dyn Fn(&str) -> DagSchedulerOptions + Send + Sync>,
    /// Retry on a live run: acquire or adopt the scheduler and re-enter the nodes.
    pub live_retry: LiveRetryCallback,
    /// Retry on a user-stopped run: the record-based staged mutation, no scheduler.
    pub paused_retry: LiveRetryCallback,
    /// Send on a live run: steer a running child or revive a resident one.
    pub live_send: LiveSendCallback,
    /// Amend on a live run: edit the definition and re-enter.
    pub live_amend: LiveAmendCallback,
    /// Amend on a user-stopped run: journal the amendment, never re-enter.
    pub paused_amend: LiveAmendCallback,
    pub clear_admission_latch: Arc<dyn Fn(&str) + Send + Sync>,
}

pub struct DagRuntimeControls {
    ports: Arc<DagRuntimeControlsPorts>,
}

impl DagRuntimeControls {
    pub fn new(ports: Arc<DagRuntimeControlsPorts>) -> Self {
        Self { ports }
    }

    /// Whether the run is stopped BY THE USER, the only state that forbids scheduling.
    pub fn user_stopped(&self, run_id: &str) -> bool {
        self.ports
            .pauses
            .read(&run_id.to_string())
            .ok()
            .and_then(|state| state.pause)
            .is_some_and(|pause| pause.reason == DagRunPauseReason::UserStop)
    }

    /// `retry`: a user-stopped run retries from the record; a live run re-enters a scheduler.
    pub fn retry(&self, run_id: &str, node_ids: Option<&[String]>, prompt: Option<&str>) -> Result<DagRunSnapshot, DagToolExecutionError> {
        if self.user_stopped(run_id) {
            return (self.ports.paused_retry)(run_id, node_ids, prompt);
        }
        (self.ports.clear_admission_latch)(run_id);
        (self.ports.live_retry)(run_id, node_ids, prompt)
    }

    /// `send`: a user-stopped run stores the message as guidance; a live run steers or revives.
    pub fn send(&self, run_id: &str, node_id: &str, message: &str) -> Result<DagToolSendOutcome, DagToolExecutionError> {
        if self.user_stopped(run_id) {
            let guidance = apply_paused_dag_guidance(
                &(self.ports.options)(run_id),
                &run_id.to_string(),
                PausedDagGuideCommand::Add { text: message.to_string(), node_id: Some(node_id.to_string()) },
                self.ports.pauses.as_ref(),
            )
            .map_err(DagToolExecutionError::Control)?;
            return Ok(DagToolSendOutcome {
                node_id: node_id.to_string(),
                task_id: None,
                delivery: DagToolSendDelivery::Guidance,
                guidance_id: Some(guidance.id),
                effect_id: None,
                queue_position: None,
            });
        }
        (self.ports.live_send)(run_id, node_id, message)
    }

    /// `amend`: a user-stopped run journals the amendment and never re-enters.
    pub fn amend(&self, run_id: &str, definition: senpi_task::dag::graph::DagDefinition) -> Result<DagRunSnapshot, DagToolExecutionError> {
        let stopped = self.user_stopped(run_id);
        (self.ports.clear_admission_latch)(run_id);
        if stopped {
            return (self.ports.paused_amend)(run_id, definition);
        }
        (self.ports.live_amend)(run_id, definition)
    }

    /// `guide`: validated add/edit/remove against the run's stored guidance.
    pub fn guide(&self, run_id: &str, command: PausedDagGuideCommand) -> Result<senpi_task::pause::guidance::Guidance, DagToolExecutionError> {
        apply_paused_dag_guidance(&(self.ports.options)(run_id), &run_id.to_string(), command, self.ports.pauses.as_ref())
            .map_err(DagToolExecutionError::Control)
    }
}
