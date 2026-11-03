//! The durable effect ledger (senpi `rpc/effect-ledger.ts`). Every non-input effect is written to the
//! transcript as `accepted` BEFORE it is applied, then moves forward by appending a newer `rpc_effect`
//! entry (the newest entry per id wins on read). Input effects are the admission receipts of
//! [`ClientAdmissions`], read through the same status.
use crate::client_admission_record::{CLIENT_ADMISSION_ENTRY, parse_client_admission_record};
use crate::client_admissions::ClientAdmissions;
use crate::effect_crash_point::{EffectCrashPhase, effect_crash_point};
use crate::effect_outcome_copy::{
    context_budget_outcome, effect_outcomes, model_switch_error_outcome,
};
use crate::effect_record::{
    EffectActor, EffectCommandKind, EffectIdConflict, EffectKind, EffectOutcome, EffectState,
    EffectStatus, QueuedChangeAnswer, RPC_EFFECT_ENTRY, RpcEffectRecord, effect_fingerprint,
    is_terminal_effect_state, parse_rpc_effect_record,
};
use crate::effect_session::{EffectSession, EffectSessionEvent, ModelChangeCancelReason};
use crate::effect_status::{project_admission, project_effect};
use crate::session_effect_status::note_session_effect;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

type CachedReceipt = (usize, Option<EffectStatus>);
type ReceiptCache = Mutex<HashMap<String, CachedReceipt>>;

/// The session's most recent effect receipt, read from the transcript tail WITHOUT opening a ledger:
/// answering a status subscriber must never seal a stale receipt or restore queued input as a side effect.
pub fn latest_session_effect(session: &dyn EffectSession) -> Option<EffectStatus> {
    static CACHE: OnceLock<ReceiptCache> = OnceLock::new();
    let entries = session.entries();
    let route = session.effect_route();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some((count, latest)) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&route)
        && *count == entries.len()
    {
        return latest.clone();
    }
    let latest = entries.iter().rev().find_map(|entry| {
        if entry["type"] != "custom" {
            return None;
        }
        if entry["customType"] == RPC_EFFECT_ENTRY {
            parse_rpc_effect_record(&entry["data"]).map(|record| project_effect(&record, None))
        } else if entry["customType"] == CLIENT_ADMISSION_ENTRY {
            parse_client_admission_record(&entry["data"])
                .map(|record| project_admission(&record, None))
        } else {
            None
        }
    });
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(route, (entries.len(), latest.clone()));
    latest
}

/// senpi `EffectBegin`: a replay of a stored status, or the freshly accepted record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectBegin {
    Replay(EffectStatus),
    Record(RpcEffectRecord),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectLedgerError {
    #[error(transparent)]
    Conflict(#[from] EffectIdConflict),
    #[error("No effect {effect_id} in session {durable_session_id}")]
    Unknown {
        effect_id: String,
        durable_session_id: String,
    },
    #[error("{0}")]
    Persist(String),
}

/// A transition's optional fields (senpi `transition(..., patch)`).
#[derive(Clone, Debug, Default)]
pub struct EffectPatch {
    pub disposition: Option<String>,
    pub outcome: Option<EffectOutcome>,
    pub answer: Option<QueuedChangeAnswer>,
}

pub struct EffectLedger {
    session: Arc<dyn EffectSession>,
    admissions: Arc<ClientAdmissions>,
    durable_session_id: String,
    records: Mutex<HashMap<String, RpcEffectRecord>>,
}

fn registry() -> MutexGuard<'static, HashMap<String, Arc<EffectLedger>>> {
    static LEDGERS: OnceLock<Mutex<HashMap<String, Arc<EffectLedger>>>> = OnceLock::new();
    LEDGERS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn applied() -> EffectPatch {
    EffectPatch {
        disposition: Some("applied".into()),
        ..Default::default()
    }
}

fn failed_with(outcome: EffectOutcome) -> EffectPatch {
    EffectPatch {
        outcome: Some(outcome),
        ..Default::default()
    }
}

impl EffectLedger {
    fn new(session: Arc<dyn EffectSession>, admissions: Arc<ClientAdmissions>) -> Self {
        let durable_session_id = session.durable_session_id();
        let mut records = HashMap::new();
        let mut switched_by = HashSet::new();
        let mut thinking_by = HashSet::new();
        for entry in session.entries() {
            if let Some(effect) = entry["effectId"].as_str() {
                if entry["type"] == "model_change" {
                    switched_by.insert(effect.to_owned());
                }
                if entry["type"] == "thinking_level_change" {
                    thinking_by.insert(effect.to_owned());
                }
            }
            if entry["type"] != "custom" || entry["customType"] != RPC_EFFECT_ENTRY {
                continue;
            }
            if let Some(record) = parse_rpc_effect_record(&entry["data"])
                && record.durable_session_id == durable_session_id
            {
                records.insert(record.effect_id.clone(), record);
            }
        }
        let ledger = Self {
            session,
            admissions,
            durable_session_id,
            records: Mutex::new(records),
        };
        ledger.seal_on_reopen(&switched_by, &thinking_by);
        ledger
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, RpcEffectRecord>> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn current(&self) -> bool {
        self.durable_session_id == self.session.durable_session_id()
    }

    /// The ledger of `session`, built on first use; a ledger of an earlier durable session under the same
    /// route is replaced.
    pub async fn for_session(session: &Arc<dyn EffectSession>) -> Arc<Self> {
        let admissions = ClientAdmissions::for_session(session).await;
        let route = session.effect_route();
        let durable = session.durable_session_id();
        let mut ledgers = registry();
        if let Some(existing) = ledgers.get(&route)
            && existing.durable_session_id == durable
        {
            return existing.clone();
        }
        let ledger = Arc::new(Self::new(session.clone(), admissions));
        ledgers.insert(route, ledger.clone());
        ledger
    }

    /// The ledger already built for `route`, if any.
    pub fn existing(route: &str) -> Option<Arc<Self>> {
        registry().get(route).cloned()
    }

    /// The host unloaded the session behind `route`.
    pub fn forget(route: &str) {
        registry().remove(route);
    }

    /// senpi `sealOnReplace`: settle existing held effects while the outgoing transcript can still be
    /// written. The host calls this from its before-replace hook.
    pub fn seal_on_replace(route: &str) {
        let Some(ledger) = Self::existing(route) else {
            return;
        };
        if !ledger.current() {
            return;
        }
        for record in ledger.snapshot() {
            if record.kind == EffectKind::SetModel && !is_terminal_effect_state(record.state) {
                ledger.transition_quiet(
                    &record.effect_id,
                    EffectState::Failed,
                    failed_with(effect_outcomes::SET_MODEL_DROPPED_BY_REPLACE.outcome()),
                );
            }
        }
    }

    fn snapshot(&self) -> Vec<RpcEffectRecord> {
        self.lock().values().cloned().collect()
    }

    /// One forwarded session event.
    pub fn observe(&self, event: &EffectSessionEvent) {
        if !self.current() {
            return;
        }
        match event {
            EffectSessionEvent::ModelChanged {
                effect_id: Some(effect_id),
            } if self.pending_kind(effect_id) == Some(EffectKind::SetModel) => {
                self.transition_quiet(effect_id, EffectState::Done, applied());
            }
            EffectSessionEvent::ModelChangeCancelled {
                effect_id: Some(effect_id),
                reason,
                detail,
            } if self.pending_kind(effect_id) == Some(EffectKind::SetModel) => {
                let outcome = match reason {
                    ModelChangeCancelReason::ContextBudget => context_budget_outcome(
                        detail
                            .as_deref()
                            .unwrap_or("the target model cannot hold the repaired context"),
                    ),
                    ModelChangeCancelReason::Error => model_switch_error_outcome(
                        detail.as_deref().unwrap_or("the model switch failed"),
                    ),
                    ModelChangeCancelReason::Superseded => {
                        effect_outcomes::SET_MODEL_SUPERSEDED.outcome()
                    }
                };
                self.transition_quiet(effect_id, EffectState::Failed, failed_with(outcome));
            }
            EffectSessionEvent::AgentIdle => {
                for record in self.snapshot() {
                    if record.kind == EffectKind::Abort && record.state == EffectState::Applied {
                        self.transition_quiet(
                            &record.effect_id,
                            EffectState::Done,
                            EffectPatch::default(),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn pending_kind(&self, effect_id: &str) -> Option<EffectKind> {
        self.lock()
            .get(effect_id)
            .filter(|record| !is_terminal_effect_state(record.state))
            .map(|record| record.kind)
    }

    /// A reopened session has no live turn, no teardown in flight and no pending model switch, so every
    /// nonterminal effect it finds is stale. Seal each one before the ledger is published.
    fn seal_on_reopen(&self, switched_by: &HashSet<String>, thinking_by: &HashSet<String>) {
        for record in self.snapshot() {
            if is_terminal_effect_state(record.state) {
                continue;
            }
            let id = &record.effect_id;
            let (state, patch) = match record.kind {
                EffectKind::Abort => (
                    EffectState::Done,
                    EffectPatch {
                        disposition: Some("no_active_turn".into()),
                        outcome: Some(effect_outcomes::ABORT_INTERRUPTED_BY_REOPEN.outcome()),
                        answer: None,
                    },
                ),
                EffectKind::SetModel if switched_by.contains(id) => (EffectState::Done, applied()),
                EffectKind::SetModel => (
                    EffectState::Failed,
                    failed_with(effect_outcomes::SET_MODEL_INTERRUPTED_BY_REOPEN.outcome()),
                ),
                EffectKind::Unload => (EffectState::Done, EffectPatch::default()),
                EffectKind::SetThinkingLevel if thinking_by.contains(id) => {
                    (EffectState::Done, applied())
                }
                EffectKind::SetThinkingLevel => (
                    EffectState::Failed,
                    failed_with(effect_outcomes::SET_THINKING_INTERRUPTED_BY_REOPEN.outcome()),
                ),
                EffectKind::Dequeue => (
                    EffectState::Failed,
                    failed_with(effect_outcomes::DEQUEUE_INTERRUPTED_BY_REOPEN.outcome()),
                ),
                EffectKind::EditQueued => (
                    EffectState::Failed,
                    failed_with(effect_outcomes::EDIT_QUEUED_INTERRUPTED_BY_REOPEN.outcome()),
                ),
                // `parse_rpc_effect_record` never admits an input kind into the ledger.
                EffectKind::Input => continue,
            };
            self.transition_quiet(id, state, patch);
        }
    }

    /// Record a new effect as `accepted` (durably, before the caller applies it), or answer a replay. Same
    /// id + same semantic payload replays the stored status; same id + a different payload conflicts.
    pub async fn begin(
        &self,
        command: EffectCommandKind,
        effect_id: &str,
        payload: &Value,
        by: EffectActor,
    ) -> Result<EffectBegin, EffectLedgerError> {
        self.session
            .persist_header_now()
            .await
            .map_err(EffectLedgerError::Persist)?;
        let fingerprint = effect_fingerprint(command, payload);
        // The check and the reservation share one guard, so two same-id begins cannot both apply: the
        // second one finds the reserved `accepted` record and replays it.
        let mut records = self.lock();
        if let Some(existing) = records.get(effect_id) {
            if existing.fingerprint != fingerprint {
                return Err(EffectIdConflict {
                    effect_id: effect_id.to_owned(),
                }
                .into());
            }
            return Ok(EffectBegin::Replay(project_effect(existing, None)));
        }
        let at = now();
        let kind = match command {
            EffectCommandKind::Abort => EffectKind::Abort,
            EffectCommandKind::SetModel => EffectKind::SetModel,
            EffectCommandKind::SetThinkingLevel => EffectKind::SetThinkingLevel,
            EffectCommandKind::Unload => EffectKind::Unload,
            EffectCommandKind::Dequeue => EffectKind::Dequeue,
            EffectCommandKind::EditQueued => EffectKind::EditQueued,
        };
        let record = RpcEffectRecord {
            effect_id: effect_id.to_owned(),
            kind,
            state: EffectState::Accepted,
            disposition: None,
            outcome: None,
            answer: None,
            by,
            accepted_at: at.clone(),
            updated_at: at,
            durable_session_id: self.durable_session_id.clone(),
            fingerprint,
        };
        records.insert(record.effect_id.clone(), record.clone());
        drop(records);
        self.append(record.clone());
        Ok(EffectBegin::Record(record))
    }

    pub fn transition(
        &self,
        effect_id: &str,
        state: EffectState,
        patch: EffectPatch,
    ) -> Result<EffectStatus, EffectLedgerError> {
        let unknown = || EffectLedgerError::Unknown {
            effect_id: effect_id.to_owned(),
            durable_session_id: self.durable_session_id.clone(),
        };
        let current = self.lock().get(effect_id).cloned().ok_or_else(unknown)?;
        self.append(RpcEffectRecord {
            state,
            disposition: patch.disposition.or(current.disposition.clone()),
            outcome: patch.outcome.or(current.outcome.clone()),
            answer: patch.answer.or(current.answer.clone()),
            updated_at: now(),
            ..current
        });
        self.status(effect_id, None).ok_or_else(unknown)
    }

    /// A transition on a record this ledger just read: it exists by construction.
    fn transition_quiet(&self, effect_id: &str, state: EffectState, patch: EffectPatch) {
        let _known = self.transition(effect_id, state, patch);
    }

    /// The newest status of `effect_id`: a non-input effect, or an input receipt under the same id.
    pub fn status(&self, effect_id: &str, session_id: Option<&str>) -> Option<EffectStatus> {
        if let Some(record) = self.lock().get(effect_id) {
            return Some(project_effect(record, session_id));
        }
        self.admissions
            .record(effect_id)
            .map(|admission| project_admission(&admission, session_id))
    }

    fn append(&self, record: RpcEffectRecord) {
        let kind = serde_json::to_value(record.kind)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let state = serde_json::to_value(record.state)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        effect_crash_point(EffectCrashPhase::Before, &kind, &state);
        self.session.append_custom_entry(
            RPC_EFFECT_ENTRY,
            serde_json::to_value(&record).unwrap_or(Value::Null),
        );
        let status = project_effect(&record, None);
        self.lock().insert(record.effect_id.clone(), record);
        note_session_effect(&self.session.effect_route(), status);
        effect_crash_point(EffectCrashPhase::After, &kind, &state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_admissions::test_session::FakeSession;
    use serde_json::json;

    fn session(route: &str) -> (Arc<FakeSession>, Arc<dyn EffectSession>) {
        let fake = Arc::new(FakeSession::new(route));
        let dynamic: Arc<dyn EffectSession> = fake.clone();
        (fake, dynamic)
    }

    fn actor() -> EffectActor {
        EffectActor {
            connection_id: "c1".into(),
        }
    }

    fn forget(route: &str) {
        EffectLedger::forget(route);
        ClientAdmissions::forget(route);
    }

    #[tokio::test]
    async fn begin_writes_accepted_first_and_a_same_id_retry_replays_or_conflicts() {
        let (fake, session) = session("el-begin");
        let ledger = EffectLedger::for_session(&session).await;
        let payload = json!({"provider":"anthropic","modelId":"claude"});
        let begun = ledger
            .begin(EffectCommandKind::SetModel, "e1", &payload, actor())
            .await
            .unwrap();
        assert!(
            matches!(begun, EffectBegin::Record(ref record) if record.state == EffectState::Accepted)
        );
        assert_eq!(fake.custom(RPC_EFFECT_ENTRY)[0]["state"], "accepted");
        let replay = ledger
            .begin(EffectCommandKind::SetModel, "e1", &payload, actor())
            .await
            .unwrap();
        assert!(matches!(replay, EffectBegin::Replay(ref status) if status.effect_id == "e1"));
        let conflict = ledger
            .begin(
                EffectCommandKind::SetModel,
                "e1",
                &json!({"provider":"openai","modelId":"gpt"}),
                actor(),
            )
            .await;
        assert!(matches!(conflict, Err(EffectLedgerError::Conflict(_))));
        assert_eq!(fake.custom(RPC_EFFECT_ENTRY).len(), 1);
        forget("el-begin");
    }

    #[tokio::test]
    async fn a_same_id_begin_during_the_first_append_replays() {
        let (fake, session) = session("el-race");
        let ledger = EffectLedger::for_session(&session).await;
        let payload = json!({"provider":"anthropic","modelId":"claude"});
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        *fake.append_gate.lock().unwrap() = Some((entered_tx, release_rx));
        // The first begin parks inside its `accepted` append, the window where a second check used to pass.
        let first = std::thread::spawn({
            let (ledger, payload) = (ledger.clone(), payload.clone());
            move || {
                futures::executor::block_on(ledger.begin(
                    EffectCommandKind::SetModel,
                    "e1",
                    &payload,
                    actor(),
                ))
                .unwrap()
            }
        });
        entered.recv().unwrap();
        let second = ledger
            .begin(EffectCommandKind::SetModel, "e1", &payload, actor())
            .await
            .unwrap();
        release.send(()).unwrap();
        let first = first.join().unwrap();
        assert!(matches!(first, EffectBegin::Record(_)));
        assert!(matches!(second, EffectBegin::Replay(ref status) if status.effect_id == "e1"));
        assert_eq!(fake.custom(RPC_EFFECT_ENTRY).len(), 1);
        forget("el-race");
    }

    #[tokio::test]
    async fn a_model_changed_event_carrying_the_id_moves_set_model_to_done() {
        let (_fake, session) = session("el-model");
        let ledger = EffectLedger::for_session(&session).await;
        ledger
            .begin(EffectCommandKind::SetModel, "e1", &json!({}), actor())
            .await
            .unwrap();
        ledger
            .begin(EffectCommandKind::SetModel, "e2", &json!({"x":1}), actor())
            .await
            .unwrap();
        ledger.observe(&EffectSessionEvent::ModelChanged {
            effect_id: Some("e1".into()),
        });
        ledger.observe(&EffectSessionEvent::ModelChangeCancelled {
            effect_id: Some("e2".into()),
            reason: ModelChangeCancelReason::ContextBudget,
            detail: None,
        });
        let done = ledger.status("e1", None).unwrap();
        assert_eq!(done.state, EffectState::Done);
        assert_eq!(done.disposition.as_deref(), Some("applied"));
        let failed = ledger.status("e2", None).unwrap();
        assert_eq!(failed.state, EffectState::Failed);
        assert_eq!(
            failed.outcome.unwrap().detail.as_deref(),
            Some("the target model cannot hold the repaired context")
        );
        forget("el-model");
    }

    #[tokio::test]
    async fn a_reopen_seals_every_nonterminal_effect() {
        let (fake, session) = session("el-reopen");
        let durable = session.durable_session_id();
        let record = |id: &str, kind: &str| json!({"effectId":id,"kind":kind,"state":"accepted","by":{"connectionId":"c"},"acceptedAt":"t","updatedAt":"t","durableSessionId":durable,"fingerprint":"f"});
        for (id, kind) in [
            ("a", "abort"),
            ("m1", "set_model"),
            ("m2", "set_model"),
            ("u", "unload"),
            ("t", "set_thinking_level"),
            ("d", "dequeue"),
        ] {
            session.append_custom_entry(RPC_EFFECT_ENTRY, record(id, kind));
        }
        fake.entries
            .lock()
            .unwrap()
            .push(json!({"type":"model_change","effectId":"m1"}));
        let ledger = EffectLedger::for_session(&session).await;
        let state = |id: &str| ledger.status(id, None).unwrap();
        assert_eq!(state("a").state, EffectState::Done);
        assert_eq!(state("a").disposition.as_deref(), Some("no_active_turn"));
        assert_eq!(state("m1").state, EffectState::Done);
        assert_eq!(state("m2").state, EffectState::Failed);
        assert_eq!(state("u").state, EffectState::Done);
        assert_eq!(state("t").state, EffectState::Failed);
        assert_eq!(state("d").state, EffectState::Failed);
        forget("el-reopen");
    }

    #[tokio::test]
    async fn seal_on_replace_fails_held_model_switches() {
        let (_fake, session) = session("el-replace");
        let ledger = EffectLedger::for_session(&session).await;
        ledger
            .begin(EffectCommandKind::SetModel, "e1", &json!({}), actor())
            .await
            .unwrap();
        EffectLedger::seal_on_replace("el-replace");
        let status = ledger.status("e1", None).unwrap();
        assert_eq!(status.state, EffectState::Failed);
        assert_eq!(
            status.outcome.unwrap().reason.as_deref(),
            Some(effect_outcomes::SET_MODEL_DROPPED_BY_REPLACE.reason)
        );
        forget("el-replace");
    }

    #[tokio::test]
    async fn latest_session_effect_reads_the_tail_without_opening_a_ledger() {
        let (_fake, session) = session("el-latest");
        assert_eq!(latest_session_effect(session.as_ref()), None);
        let durable = session.durable_session_id();
        session.append_custom_entry(
            RPC_EFFECT_ENTRY,
            json!({"effectId":"e9","kind":"abort","state":"done","by":{"connectionId":"c"},"acceptedAt":"t","updatedAt":"t","durableSessionId":durable,"fingerprint":"f"}),
        );
        let latest = latest_session_effect(session.as_ref()).unwrap();
        assert_eq!(latest.effect_id, "e9");
        assert!(EffectLedger::existing("el-latest").is_none());
    }
}
