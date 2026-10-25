//! Receiver half of the gateway: the only path that moves a row out of `queued`.
//!
//! Upstream counterpart: `thread/gateway/drain.ts`. Each pass starts with the store's `BEGIN
//! IMMEDIATE` barrier (`reconcile`), then admits queued rows in seq order with a two-phase claim:
//! T1 claims the row, the runtime call runs outside any transaction, T2 records the outcome.

pub type LogCallback = Option<Arc<dyn Fn(&str) + Send + Sync>>;
pub type OnAdmittedCallback = Option<Arc<dyn Fn(&str, &str) + Send + Sync>>;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::adapter::{Admission, DrainWakeEvent, GatewayAdmissionKind, SessionRuntimePort, WakeReason};
use super::decision::{DeliveryDecision, RefusalKind, TargetState, decide_delivery};
use super::provenance::{delivery_provenance_line, delivery_sender, render_delivery_text};
use super::store_api::GatewayStoreApi;
use super::types::{AdmissionLedger, ClaimOutcome, DeliveryRow, GatewayDeliveryMode, OutcomeRecord, ProcessIdentity, ReconcileRequest, RecordOutcomeRequest, RefusalReason};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxDrainResult {
    pub admitted: Vec<Admission>,
}

/// The notice a session shows when a delivery waits behind its running turn or the user's draft.
pub fn queued_notice(row: &DeliveryRow) -> String {
    format!("remote message from {} queued ({})", row.envelope.actor, row.delivery_id)
}

fn releasing_reason(reason: WakeReason) -> bool {
    matches!(reason, WakeReason::Submission | WakeReason::DraftCleared | WakeReason::Idle | WakeReason::Start | WakeReason::Continue)
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
        Self { options: Arc::new(options), held: Mutex::new(BTreeMap::new()), noticed: Mutex::new(BTreeMap::new()), pass: Mutex::new(()) }
    }

    pub fn held_deliveries(&self) -> BTreeMap<String, i64> {
        self.held.lock().map(|held| held.clone()).unwrap_or_default()
    }

    pub fn is_session_referenced(&self) -> bool {
        self.options.store.is_referenced(&self.options.durable_id).unwrap_or(false)
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
        let already = self.noticed.lock().map(|mut noticed| noticed.insert(row.delivery_id.clone(), true).is_some()).unwrap_or(true);
        if !already {
            notify(&queued_notice(row));
        }
    }

    fn log(&self, line: &str) {
        if let Some(log) = &self.options.log {
            log(line);
        }
    }

    /// One drain pass; `event.reason` decides whether a held row may be released.
    pub fn drain(&self, event: &DrainWakeEvent) -> Result<InboxDrainResult, super::store_ops::StoreError> {
        let _serialized = self.pass.lock();
        let self_identity = self.identity();
        let ledger: AdmissionLedger = (self.options.runtime.list_admitted_deliveries)();
        let reconciled = self.options.store.reconcile(&ReconcileRequest {
            now: self.now(),
            target_durable_id: self.options.durable_id.clone(),
            self_identity,
            ledger,
            session_path: (self.options.session_path)(),
        })?;
        for id in &reconciled.dual_runtime {
            self.log(&format!("dual_runtime: delivery {id} is claimed by another live process for {}", self.options.durable_id));
        }
        let mut admitted: Vec<Admission> = Vec::new();
        {
            let queued_ids: Vec<String> = reconciled.queued.iter().map(|row| row.delivery_id.clone()).collect();
            if let Ok(mut held) = self.held.lock() {
                held.retain(|id, _| queued_ids.contains(id));
            }
        }
        for row in &reconciled.queued {
            let gate = (self.options.runtime.admission_gate)();
            let held_revision = self.held.lock().ok().and_then(|held| held.get(&row.delivery_id).copied());
            if let Some(held_revision) = held_revision
                && !releasing_reason(event.reason)
                && held_revision == gate.editor_revision
            {
                break;
            }
            if !gate.can_admit {
                if let Ok(mut held) = self.held.lock() {
                    held.insert(row.delivery_id.clone(), gate.editor_revision);
                }
                self.notice_queued(row);
                admitted.push(Admission { delivery_id: row.delivery_id.clone(), kind: super::adapter::ExternalAdmissionKind::HeldDraft });
                break;
            }
            if let Ok(mut held) = self.held.lock() {
                held.remove(&row.delivery_id);
            }
            let decision = decide_delivery(TargetState::Phase((self.options.runtime.phase)()), row.mode_requested, row.expected_turn_id, Some(gate.turn_epoch));
            let deliver_as = match decision {
                DeliveryDecision::Admit { deliver_as, .. } => deliver_as,
                DeliveryDecision::Refuse { reason } => {
                    let refusal = if reason == RefusalKind::NotSteerable { RefusalReason::NotSteerable } else { RefusalReason::TurnConflict };
                    let _ = self.options.store.refuse_queued(self.now(), &row.delivery_id, refusal);
                    admitted.push(Admission { delivery_id: row.delivery_id.clone(), kind: super::adapter::ExternalAdmissionKind::TurnConflict });
                    continue;
                }
                DeliveryDecision::QueuedOffline => GatewayDeliveryMode::FollowUp,
            };
            let lane = if deliver_as == GatewayDeliveryMode::Steer { super::types::EffectiveLane::Steer } else { super::types::EffectiveLane::FollowUp };
            let claimed = self.options.store.claim(&super::types::ClaimRequest { now: self.now(), delivery_id: row.delivery_id.clone(), self_identity: self.identity(), lane })?;
            let ClaimOutcome::Claimed { row: claimed_row } = claimed else {
                continue;
            };
            let input = super::adapter::AdmitExternalMessageInput {
                delivery_id: claimed_row.delivery_id.clone(),
                text: render_delivery_text(&claimed_row),
                deliver_as,
                expected_turn_id: None,
                sender: Some(delivery_sender(&claimed_row)),
                display_text: Some(format!("{}\n{}", delivery_provenance_line(claimed_row.envelope.provenance.as_ref()), claimed_row.body)),
            };
            let result = match (self.options.runtime.admit_external_message)(input) {
                Ok(result) => result,
                Err(error) => {
                    let _ = self.options.store.record_outcome(&RecordOutcomeRequest {
                        now: self.now(),
                        delivery_id: row.delivery_id.clone(),
                        self_identity: self.identity(),
                        outcome: OutcomeRecord::Requeue,
                    });
                    self.log(&format!("admission closed for {}: {error}", self.options.durable_id));
                    break;
                }
            };
            if result.kind == super::adapter::ExternalAdmissionKind::HeldDraft {
                let _ = self.options.store.record_outcome(&RecordOutcomeRequest {
                    now: self.now(),
                    delivery_id: row.delivery_id.clone(),
                    self_identity: self.identity(),
                    outcome: OutcomeRecord::Requeue,
                });
                if let Ok(mut held) = self.held.lock() {
                    held.insert(row.delivery_id.clone(), (self.options.runtime.admission_gate)().editor_revision);
                }
                self.notice_queued(row);
                admitted.push(Admission { delivery_id: row.delivery_id.clone(), kind: result.kind });
                break;
            }
            if result.kind == super::adapter::ExternalAdmissionKind::Queued {
                self.notice_queued(&claimed_row);
            }
            if result.kind == super::adapter::ExternalAdmissionKind::TurnConflict {
                let _ = self.options.store.record_outcome(&RecordOutcomeRequest {
                    now: self.now(),
                    delivery_id: row.delivery_id.clone(),
                    self_identity: self.identity(),
                    outcome: OutcomeRecord::Refused { reason: RefusalReason::TurnConflict },
                });
            } else {
                let emitted = (self.options.runtime.list_admitted_deliveries)().emitted.contains(&row.delivery_id);
                let written = result.kind == super::adapter::ExternalAdmissionKind::AlreadyAdmitted && emitted;
                let outcome = if written {
                    OutcomeRecord::Applied { admission_kind: kind_label(result.kind).to_owned(), turn_epoch: result.turn_epoch }
                } else {
                    OutcomeRecord::Admitted { admission_kind: kind_label(result.kind).to_owned(), turn_epoch: result.turn_epoch }
                };
                let _ = self.options.store.record_outcome(&RecordOutcomeRequest { now: self.now(), delivery_id: row.delivery_id.clone(), self_identity: self.identity(), outcome });
            }
            if let Some(on_admitted) = &self.options.on_admitted
                && matches!(
                    result.kind,
                    super::adapter::ExternalAdmissionKind::Started | super::adapter::ExternalAdmissionKind::Steered | super::adapter::ExternalAdmissionKind::Queued
                )
            {
                on_admitted(&row.delivery_id, kind_label(result.kind));
            }
            admitted.push(Admission { delivery_id: row.delivery_id.clone(), kind: result.kind });
        }
        Ok(InboxDrainResult { admitted })
    }
}

fn kind_label(kind: super::adapter::ExternalAdmissionKind) -> &'static str {
    match kind {
        super::adapter::ExternalAdmissionKind::Started => "started",
        super::adapter::ExternalAdmissionKind::Queued => "queued",
        super::adapter::ExternalAdmissionKind::Steered => "steered",
        super::adapter::ExternalAdmissionKind::TurnConflict => "turn_conflict",
        super::adapter::ExternalAdmissionKind::HeldDraft => "held_draft",
        super::adapter::ExternalAdmissionKind::AlreadyAdmitted => "already_admitted",
    }
}

/// The gateway admission kind a drain result reports for one delivery.
pub fn gateway_kind(kind: super::adapter::ExternalAdmissionKind) -> GatewayAdmissionKind {
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
                origin: EnvelopeOrigin::Session { session: "sender".into() },
                actor: "planner".into(),
                via: vec![],
                root_id: "root".into(),
                hop: 1,
                delivery_id: "d-1".into(),
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
            turn_epoch: None,
            root_id: "root".into(),
            hop: 1,
            created_at: 1,
            updated_at: 1,
            expires_at: 9,
            binding_id: None,
            binding_revision: None,
            actor_user_id: None,
        }
    }

    #[test]
    fn the_queued_notice_names_the_sender_and_the_delivery() {
        assert_eq!(queued_notice(&row()), "remote message from planner queued (d-1)");
    }

    #[test]
    fn only_a_releasing_reason_lets_a_held_row_go() {
        assert!(releasing_reason(WakeReason::Submission));
        assert!(releasing_reason(WakeReason::DraftCleared));
        assert!(!releasing_reason(WakeReason::Inbox));
        assert!(!releasing_reason(WakeReason::Emitted));
    }
}
