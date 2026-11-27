//! Receiver half of the gateway: the only path that moves a row out of `queued`.
//!
//! Upstream counterpart: `thread/gateway/drain.ts`. Each pass starts with the store's `BEGIN
//! IMMEDIATE` barrier (`reconcile`), then admits queued rows in seq order with a two-phase claim:
//! T1 claims the row, the runtime call runs outside any transaction, T2 records the outcome.

pub type LogCallback = Option<Arc<dyn Fn(&str) + Send + Sync>>;
pub type OnAdmittedCallback = Option<Arc<dyn Fn(&str, &str) + Send + Sync>>;
pub type AdmitIntakeFn =
    Option<Arc<dyn Fn(&str) -> super::intake_admission::IntakeAdmission + Send + Sync>>;
pub type WakeGatedFn = Option<Arc<dyn Fn() -> bool + Send + Sync>>;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use super::adapter::{
    Admission, DrainWakeEvent, ExternalAdmissionKind, GatewayAdmissionKind, SessionRuntimePort,
    WakeReason, to_session_control_drain_result,
};
use super::decision::{
    DeliveryDecision, RefusalKind, TargetState, decide_delivery, effective_lane_of,
};
use super::intake_admission::IntakeAdmission;
use super::provenance::{delivery_sender, render_delivery_display_text, render_delivery_text};
use super::store_api::GatewayStoreApi;
use super::types::{
    AdmissionLedger, ClaimOutcome, DeliveryRow, GatewayDeliveryMode, OutcomeRecord,
    ProcessIdentity, ReconcileRequest, RecordOutcomeRequest, RefusalReason,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxDrainResult {
    pub admitted: Vec<Admission>,
}

/// The notice a session shows when a delivery waits behind its running turn or the user's draft.
pub fn queued_notice(row: &DeliveryRow) -> String {
    format!(
        "remote message from {} queued ({})",
        row.envelope.actor, row.delivery_id
    )
}

fn releasing_reason(reason: WakeReason) -> bool {
    matches!(
        reason,
        WakeReason::Submission
            | WakeReason::DraftCleared
            | WakeReason::Idle
            | WakeReason::Start
            | WakeReason::Continue
    )
}

pub struct InboxDrainOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub runtime: SessionRuntimePort,
    pub durable_id: String,
    pub session_path: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub log: LogCallback,
    /// Shows the queued notice in the session's own UI.
    pub notify: LogCallback,
    /// A delivery this drain started, steered or queued into the session.
    pub on_admitted: OnAdmittedCallback,
    /// Asks whether an intake row's sending device is admitted now (`#13037` 5.6); `None` skips intake rows.
    pub admit_intake: AdmitIntakeFn,
    /// True while a woken session is opening under a live lease (A3.1.5): admits nothing.
    pub wake_gated: WakeGatedFn,
}

/// A held row (the user is composing) costs no store write and stops the pass, so nothing behind it
/// overtakes it.
pub struct InboxDrain {
    options: Arc<InboxDrainOptions>,
    held: Mutex<BTreeMap<String, i64>>,
    noticed: Mutex<BTreeMap<String, bool>>,
    /// One pass at a time, the way the upstream promise chain serializes them.
    pass: Mutex<()>,
}

impl InboxDrain {
    pub fn new(options: InboxDrainOptions) -> Self {
        Self {
            options: Arc::new(options),
            held: Mutex::new(BTreeMap::new()),
            noticed: Mutex::new(BTreeMap::new()),
            pass: Mutex::new(()),
        }
    }

    pub fn held_deliveries(&self) -> BTreeMap<String, i64> {
        self.held
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    pub fn is_session_referenced(&self) -> bool {
        self.options
            .store
            .is_referenced(&self.options.durable_id)
            .unwrap_or(false)
    }

    fn now(&self) -> i64 {
        match &self.options.now {
            Some(now) => now(),
            None => self.options.store.now(),
        }
    }

    fn identity(&self) -> ProcessIdentity {
        self.options.store.identity().unwrap_or(ProcessIdentity {
            pid: std::process::id() as i64,
            process_start_time: None,
            instance_id: String::new(),
            runtime_instance: None,
        })
    }

    fn notice_queued(&self, row: &DeliveryRow) {
        let Some(notify) = &self.options.notify else {
            return;
        };
        let already = self
            .noticed
            .lock()
            .map(|mut noticed| noticed.insert(row.delivery_id.clone(), true).is_some())
            .unwrap_or(true);
        if !already {
            notify(&queued_notice(row));
        }
    }

    fn log(&self, line: &str) {
        if let Some(log) = &self.options.log {
            log(line);
        }
    }

    /// One drain pass returning host-level admissions (`to_session_control_drain_result`).
    pub fn drain(
        &self,
        event: &DrainWakeEvent,
    ) -> Result<InboxDrainResult, super::store_ops::StoreError> {
        let raw = self.drain_gateway(event)?;
        let converted = to_session_control_drain_result(&raw);
        Ok(InboxDrainResult {
            admitted: converted.admitted.unwrap_or_default(),
        })
    }

    /// One drain pass returning all gateway outcomes (including gateway-only refusals).
    pub fn drain_gateway(
        &self,
        event: &DrainWakeEvent,
    ) -> Result<Vec<(String, GatewayAdmissionKind)>, super::store_ops::StoreError> {
        let _serialized = self.pass.lock();
        let self_identity = self.identity();
        let ledger: AdmissionLedger = (self.options.runtime.list_admitted_deliveries)();
        let reconciled = self.options.store.reconcile(&ReconcileRequest {
            now: self.now(),
            target_durable_id: self.options.durable_id.clone(),
            self_identity: self_identity.clone(),
            ledger,
            session_path: (self.options.session_path)(),
        })?;
        for id in &reconciled.dual_runtime {
            self.log(&format!(
                "dual_runtime: delivery {id} is claimed by another live process for {}",
                self.options.durable_id
            ));
        }
        let mut admitted: Vec<(String, GatewayAdmissionKind)> = Vec::new();
        {
            let queued_ids: Vec<String> = reconciled
                .queued
                .iter()
                .map(|row| row.delivery_id.clone())
                .collect();
            if let Ok(mut held) = self.held.lock() {
                held.retain(|id, _| queued_ids.contains(id));
            }
        }
        if self.options.wake_gated.as_ref().is_some_and(|f| f()) {
            for row in &reconciled.queued {
                admitted.push((row.delivery_id.clone(), GatewayAdmissionKind::HeldDraft));
            }
            return Ok(admitted);
        }
        let mut intake_answers: HashMap<String, IntakeAdmission> = HashMap::new();
        for row in &reconciled.queued {
            let gate = (self.options.runtime.admission_gate)();
            let held_revision = self
                .held
                .lock()
                .ok()
                .and_then(|held| held.get(&row.delivery_id).copied());
            if let Some(held_revision) = held_revision
                && !releasing_reason(event.reason)
                && held_revision == gate.editor_revision
            {
                admitted.push((row.delivery_id.clone(), GatewayAdmissionKind::HeldDraft));
                break;
            }
            if !gate.can_admit {
                if let Ok(mut held) = self.held.lock() {
                    held.insert(row.delivery_id.clone(), gate.editor_revision);
                }
                self.notice_queued(row);
                admitted.push((row.delivery_id.clone(), GatewayAdmissionKind::HeldDraft));
                break;
            }
            if let Ok(mut held) = self.held.lock() {
                held.remove(&row.delivery_id);
            }
            if row.ledger_id.is_some() {
                let device = row
                    .envelope
                    .provenance
                    .as_ref()
                    .and_then(|p| p.device.as_deref());
                let verdict = match (device, &self.options.admit_intake) {
                    (Some(dev), Some(admit_fn)) => *intake_answers
                        .entry(dev.to_owned())
                        .or_insert_with(|| admit_fn(dev)),
                    _ => IntakeAdmission::Unknown,
                };
                match verdict {
                    IntakeAdmission::Unknown => continue,
                    IntakeAdmission::Refused => {
                        let _ = self.options.store.refuse_queued(
                            self.now(),
                            &row.delivery_id,
                            RefusalReason::SenderNotAdmitted,
                        );
                        admitted.push((
                            row.delivery_id.clone(),
                            GatewayAdmissionKind::SenderNotAdmitted,
                        ));
                        continue;
                    }
                    IntakeAdmission::Admitted => {}
                }
            }
            let decision = decide_delivery(
                TargetState::Phase((self.options.runtime.phase)()),
                row.mode_requested,
                row.expected_turn_id,
                Some(gate.turn_epoch),
            );
            let (deliver_as, expect, expected_turn_id) = match decision {
                DeliveryDecision::Admit {
                    deliver_as,
                    expect,
                    expected_turn_id,
                } => (deliver_as, expect, expected_turn_id),
                DeliveryDecision::Refuse { reason } => {
                    let (refusal, kind) = if reason == RefusalKind::NotSteerable {
                        (
                            RefusalReason::NotSteerable,
                            GatewayAdmissionKind::NotSteerable,
                        )
                    } else {
                        (
                            RefusalReason::TurnConflict,
                            GatewayAdmissionKind::TurnConflict,
                        )
                    };
                    let _ = self
                        .options
                        .store
                        .refuse_queued(self.now(), &row.delivery_id, refusal);
                    admitted.push((row.delivery_id.clone(), kind));
                    continue;
                }
                DeliveryDecision::QueuedOffline => (
                    GatewayDeliveryMode::FollowUp,
                    super::decision::Expect::Queued,
                    None,
                ),
            };
            let lane = effective_lane_of(deliver_as, expect);
            let claimed = self.options.store.claim(&super::types::ClaimRequest {
                now: self.now(),
                delivery_id: row.delivery_id.clone(),
                self_identity: self_identity.clone(),
                lane,
            })?;
            let ClaimOutcome::Claimed { row: claimed_row } = claimed else {
                continue;
            };
            let engine_id = claimed_row
                .ledger_id
                .clone()
                .unwrap_or_else(|| claimed_row.delivery_id.clone());
            let (text, display_text) = if claimed_row.ledger_id.is_some() {
                (
                    claimed_row
                        .verbatim_text
                        .clone()
                        .unwrap_or_else(|| claimed_row.body.clone()),
                    claimed_row
                        .verbatim_display_text
                        .clone()
                        .unwrap_or_else(|| claimed_row.body.clone()),
                )
            } else {
                (
                    render_delivery_text(&claimed_row),
                    render_delivery_display_text(&claimed_row),
                )
            };
            let input = super::adapter::AdmitExternalMessageInput {
                delivery_id: engine_id.clone(),
                text,
                deliver_as,
                expected_turn_id,
                sender: Some(delivery_sender(&claimed_row)),
                display_text: Some(display_text),
            };
            let result = match (self.options.runtime.admit_external_message)(input) {
                Ok(result) => result,
                Err(error) => {
                    let _ = self.options.store.record_outcome(&RecordOutcomeRequest {
                        now: self.now(),
                        delivery_id: row.delivery_id.clone(),
                        self_identity: self_identity.clone(),
                        outcome: OutcomeRecord::Requeue,
                    });
                    self.log(&format!(
                        "admission closed for {}: {error}",
                        self.options.durable_id
                    ));
                    break;
                }
            };
            if result.kind == ExternalAdmissionKind::HeldDraft {
                let _ = self.options.store.record_outcome(&RecordOutcomeRequest {
                    now: self.now(),
                    delivery_id: row.delivery_id.clone(),
                    self_identity: self_identity.clone(),
                    outcome: OutcomeRecord::Requeue,
                });
                if let Ok(mut held) = self.held.lock() {
                    held.insert(
                        row.delivery_id.clone(),
                        (self.options.runtime.admission_gate)().editor_revision,
                    );
                }
                self.notice_queued(row);
                admitted.push((row.delivery_id.clone(), GatewayAdmissionKind::HeldDraft));
                break;
            }
            if result.kind == ExternalAdmissionKind::Queued {
                self.notice_queued(&claimed_row);
            }
            if result.kind == ExternalAdmissionKind::TurnConflict {
                let _ = self.options.store.record_outcome(&RecordOutcomeRequest {
                    now: self.now(),
                    delivery_id: row.delivery_id.clone(),
                    self_identity: self_identity.clone(),
                    outcome: OutcomeRecord::Refused {
                        reason: RefusalReason::TurnConflict,
                    },
                });
            } else {
                let emitted = (self.options.runtime.list_admitted_deliveries)()
                    .emitted
                    .contains(&engine_id);
                let written = result.kind == ExternalAdmissionKind::AlreadyAdmitted && emitted;
                let wait_kind = (result.kind == ExternalAdmissionKind::Queued
                    && (self.options.runtime.phase)()
                        == super::adapter::RuntimePhase::WaitingQuestion)
                    .then(|| "approval".to_owned());
                let outcome = if written {
                    OutcomeRecord::Applied {
                        admission_kind: kind_label(result.kind).to_owned(),
                        turn_epoch: result.turn_epoch,
                        wait_kind,
                    }
                } else {
                    OutcomeRecord::Admitted {
                        admission_kind: kind_label(result.kind).to_owned(),
                        turn_epoch: result.turn_epoch,
                        wait_kind,
                    }
                };
                let _ = self.options.store.record_outcome(&RecordOutcomeRequest {
                    now: self.now(),
                    delivery_id: row.delivery_id.clone(),
                    self_identity: self_identity.clone(),
                    outcome,
                });
            }
            if let Some(on_admitted) = &self.options.on_admitted
                && matches!(
                    result.kind,
                    ExternalAdmissionKind::Started
                        | ExternalAdmissionKind::Steered
                        | ExternalAdmissionKind::Queued
                )
            {
                on_admitted(&row.delivery_id, kind_label(result.kind));
            }
            admitted.push((
                row.delivery_id.clone(),
                GatewayAdmissionKind::from_external(result.kind),
            ));
        }
        Ok(admitted)
    }
}

fn kind_label(kind: ExternalAdmissionKind) -> &'static str {
    match kind {
        ExternalAdmissionKind::Started => "started",
        ExternalAdmissionKind::Queued => "queued",
        ExternalAdmissionKind::Steered => "steered",
        ExternalAdmissionKind::TurnConflict => "turn_conflict",
        ExternalAdmissionKind::HeldDraft => "held_draft",
        ExternalAdmissionKind::Held => "held",
        ExternalAdmissionKind::AlreadyAdmitted => "already_admitted",
    }
}

/// The gateway admission kind a drain result reports for one delivery.
pub fn gateway_kind(kind: ExternalAdmissionKind) -> GatewayAdmissionKind {
    GatewayAdmissionKind::from_external(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::types::{EnvelopeOrigin, StoredDeliveryEnvelope};

    fn row() -> DeliveryRow {
        DeliveryRow {
            delivery_id: "d-1".into(),
            target_durable_id: "receiver".into(),
            seq: 1,
            sender: "session:sender".into(),
            sender_turn: None,
            envelope: StoredDeliveryEnvelope {
                provenance: None,
                origin: EnvelopeOrigin::Session {
                    session: "sender".into(),
                },
                actor: "planner".into(),
                via: vec![],
                root_id: "root".into(),
                hop: 1,
                delivery_id: "d-1".into(),
                authority: None,
            },
            body: "hi".into(),
            bytes: 2,
            mode_requested: GatewayDeliveryMode::Auto,
            mode_effective: None,
            expected_turn_id: None,
            state: super::super::types::DeliveryState::Queued,
            reason: None,
            admitted_by: None,
            claimed_at: None,
            attempt: 0,
            admission_kind: None,
            wait_kind: None,
            turn_epoch: None,
            root_id: "root".into(),
            hop: 1,
            created_at: 1,
            updated_at: 1,
            expires_at: 9,
            binding_id: None,
            binding_revision: None,
            actor_user_id: None,
            ledger_id: None,
            verbatim_text: None,
            verbatim_display_text: None,
            collapse_key: None,
        }
    }

    #[test]
    fn the_queued_notice_names_the_sender_and_the_delivery() {
        assert_eq!(
            queued_notice(&row()),
            "remote message from planner queued (d-1)"
        );
    }

    #[test]
    fn only_a_releasing_reason_lets_a_held_row_go() {
        assert!(releasing_reason(WakeReason::Submission));
        assert!(releasing_reason(WakeReason::DraftCleared));
        assert!(!releasing_reason(WakeReason::Inbox));
        assert!(!releasing_reason(WakeReason::Emitted));
    }
}
