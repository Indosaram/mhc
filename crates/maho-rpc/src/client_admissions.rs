//! The durable client-input admission ledger (senpi `rpc/client-admissions.ts`). One ledger per loaded
//! session; the transcript, not an attachment or request id, owns its history: every receipt is a
//! `rpc_client_message_admission` custom entry, and the newest entry per `clientMessageId` wins on read.
//!
//! The session is reached through [`EffectSession`]; the host adapter forwards the session events the
//! ledger observes to [`ClientAdmissions::observe`].
use crate::client_admission_record::{
    AdmissionDisposition, AdmissionState, CLIENT_ADMISSION_ENTRY, ClientAdmissionRecord,
    ClientMessageIdConflict, HOST_MINTED_ID_PREFIX, PreparedAdmissionInput, admission_snapshot,
    client_input_fingerprint, parse_client_admission_record,
};
use crate::effect_outcome_copy::{effect_outcomes, turn_error_outcome};
use crate::effect_record::{EffectActor, EffectOutcome, EffectOutcomeKind};
use crate::effect_session::{EffectSession, EffectSessionEvent};
use crate::effect_status::project_admission;
use crate::session_effect_status::note_session_effect;
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// The client (and abort effect) an `abortedBy` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientAbort {
    pub connection_id: String,
    pub effect_id: Option<String>,
    /// Identity token: `forget_abort` forgets only the abort it was handed.
    token: u64,
}

/// The identity to stamp on the input, and the two moments the caller reports.
pub struct AdmissionHooks {
    pub client_message_id: Option<String>,
    pub client_turn_id: Option<String>,
    admissions: Option<Arc<ClientAdmissions>>,
    base: Option<ClientAdmissionRecord>,
}

impl AdmissionHooks {
    /// The input went to a queue: persist it as `queued` with the prepared content.
    pub fn queued(&self, prepared: PreparedAdmissionInput) {
        if let (Some(admissions), Some(base)) = (&self.admissions, &self.base) {
            admissions.save(ClientAdmissionRecord {
                disposition: AdmissionDisposition::Queued,
                state: AdmissionState::Queued { prepared },
                ..base.clone()
            });
        }
    }

    /// The input was accepted with `disposition`; returns the admission snapshot answered to the client.
    pub fn accepted(&self, disposition: AdmissionDisposition) -> Option<Value> {
        let (admissions, base) = (self.admissions.as_ref()?, self.base.as_ref()?);
        let existing = admissions.record(&base.client_message_id);
        let record = existing.clone().unwrap_or_else(|| ClientAdmissionRecord {
            disposition,
            state: if disposition == AdmissionDisposition::Handled {
                AdmissionState::Completed { outcome: None }
            } else {
                AdmissionState::Running
            },
            ..base.clone()
        });
        if existing.is_none() {
            admissions.save(record.clone());
        }
        Some(admission_snapshot(&record))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmitError {
    #[error(transparent)]
    Conflict(#[from] ClientMessageIdConflict),
    #[error("{0}")]
    Failed(String),
}

type PendingResult = Option<Result<Value, AdmitError>>;

#[derive(Default)]
struct LedgerState {
    records: HashMap<String, ClientAdmissionRecord>,
    pending: HashMap<String, (String, tokio::sync::watch::Receiver<PendingResult>)>,
    restoring: bool,
    turn_outcome: Option<EffectOutcome>,
    turn_aborted_by: Option<EffectActor>,
    last_abort: Option<ClientAbort>,
    next_abort_token: u64,
}

/// The owner's reservation of one `clientMessageId`. Dropping it (the owner finished, or its future was
/// cancelled) frees the id; a cancelled owner publishes a failure so its joiners do not wait forever.
struct PendingAdmission {
    admissions: Arc<ClientAdmissions>,
    client_message_id: String,
    publish: tokio::sync::watch::Sender<PendingResult>,
}

impl Drop for PendingAdmission {
    fn drop(&mut self) {
        self.publish.send_if_modified(|result| {
            if result.is_some() {
                return false;
            }
            *result = Some(Err(AdmitError::Failed(
                "RPC input admission was cancelled".into(),
            )));
            true
        });
        self.admissions
            .lock()
            .pending
            .remove(&self.client_message_id);
    }
}

pub struct ClientAdmissions {
    session: Arc<dyn EffectSession>,
    durable_session_id: String,
    state: Mutex<LedgerState>,
    /// The restore of persisted queued input; a failure is answered by every later `admit`.
    ready: tokio::sync::OnceCell<Result<(), String>>,
    has_identities: std::sync::atomic::AtomicBool,
}

fn registry() -> MutexGuard<'static, HashMap<String, Arc<ClientAdmissions>>> {
    static LEDGERS: OnceLock<Mutex<HashMap<String, Arc<ClientAdmissions>>>> = OnceLock::new();
    LEDGERS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ClientAdmissions {
    fn new(session: Arc<dyn EffectSession>) -> Self {
        let durable_session_id = session.durable_session_id();
        let mut records = HashMap::new();
        for entry in session.entries() {
            if entry["type"] != "custom" || entry["customType"] != CLIENT_ADMISSION_ENTRY {
                continue;
            }
            // A custom entry is opaque transcript data: one that does not parse as an admission is not one
            // and must not keep the session from opening.
            if let Some(record) = parse_client_admission_record(&entry["data"])
                && record.durable_session_id == durable_session_id
            {
                records.insert(record.client_message_id.clone(), record);
            }
        }
        // Only ids a client sent switch on event correlation; ids the host minted stay internal.
        let has_identities = records
            .values()
            .any(|record| record.host_minted != Some(true));
        let ledger = Self {
            session,
            durable_session_id,
            state: Mutex::new(LedgerState {
                records,
                restoring: true,
                ..Default::default()
            }),
            ready: tokio::sync::OnceCell::new(),
            has_identities: has_identities.into(),
        };
        // A ledger is built once per loaded session, so no turn can be live for a `running` record it reads:
        // that turn belonged to a process or load that is gone. Seal it before queued input is restored.
        let running: Vec<ClientAdmissionRecord> = ledger
            .lock()
            .records
            .values()
            .filter(|record| matches!(record.state, AdmissionState::Running))
            .cloned()
            .collect();
        for record in running {
            ledger.save(ClientAdmissionRecord {
                state: AdmissionState::Completed {
                    outcome: Some(effect_outcomes::INPUT_INTERRUPTED_BY_REOPEN.outcome()),
                },
                ..record
            });
        }
        ledger
    }

    fn lock(&self) -> MutexGuard<'_, LedgerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn current(&self) -> bool {
        self.durable_session_id == self.session.durable_session_id()
    }

    /// The ledger of `session`, built (and its queued input restored) on first use. A ledger of an earlier
    /// durable session under the same route is replaced.
    pub async fn for_session(session: &Arc<dyn EffectSession>) -> Arc<Self> {
        let ledger = Self::existing_or_new(session);
        // A failed restore is not lost: `admit` answers it to the client that sends the next input.
        let _answered_by_admit = ledger.ensure_ready().await;
        ledger
    }

    fn existing_or_new(session: &Arc<dyn EffectSession>) -> Arc<Self> {
        let route = session.effect_route();
        let durable = session.durable_session_id();
        let mut ledgers = registry();
        if let Some(existing) = ledgers.get(&route)
            && existing.durable_session_id == durable
        {
            return existing.clone();
        }
        // Publish the ledger before restoration emits queue events.
        let ledger = Arc::new(Self::new(session.clone()));
        ledgers.insert(route, ledger.clone());
        ledger
    }

    /// The ledger already built for `route`, if any; never builds one (that would restore queued input).
    pub fn existing(route: &str) -> Option<Arc<Self>> {
        registry().get(route).cloned()
    }

    /// The host unloaded the session behind `route`.
    pub fn forget(route: &str) {
        registry().remove(route);
    }

    /// Restored input keeps its place ahead of anything delivered after the ledger opened.
    pub async fn ensure_ready(&self) -> Result<(), String> {
        self.ready.get_or_init(|| self.restore()).await.clone()
    }

    pub fn has_identities(&self) -> bool {
        self.has_identities
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Mark queued input cleared on a session whose ledger exists; never builds one.
    pub fn clear_queued_for(route: &str) {
        if let Some(ledger) = Self::existing(route) {
            ledger.clear_queued();
        }
    }

    /// senpi `sealQueuedOnReplace`: the host calls this from its before-replace hook. Only a ledger that
    /// already exists is touched.
    pub fn seal_queued_on_replace(route: &str) {
        if let Some(ledger) = Self::existing(route) {
            ledger.drop_queued_by_replace();
        }
    }

    /// The newest record for `client_message_id` in this session, for effect status queries.
    pub fn record(&self, client_message_id: &str) -> Option<ClientAdmissionRecord> {
        self.lock().records.get(client_message_id).cloned()
    }

    /// An edit of a still-queued input: its stored prepared content takes the new text, so a reopen restores
    /// what the client edited. The fingerprint stays the original send's, so a retry of that send replays.
    pub fn edit_queued_text(&self, client_message_id: &str, text: &str) {
        let Some(record) = self.record(client_message_id) else {
            return;
        };
        let AdmissionState::Queued { prepared } = &record.state else {
            return;
        };
        let prepared = PreparedAdmissionInput {
            text: text.to_owned(),
            ..prepared.clone()
        };
        self.save(ClientAdmissionRecord {
            state: AdmissionState::Queued { prepared },
            ..record
        });
    }

    /// Record who aborted; the returned record is the token `forget_abort` takes.
    pub fn note_abort(&self, by: EffectActor, effect_id: Option<String>) -> ClientAbort {
        let streaming = self.session.is_streaming();
        let mut state = self.lock();
        if streaming {
            state.turn_aborted_by = Some(by.clone());
        }
        state.next_abort_token += 1;
        let abort = ClientAbort {
            connection_id: by.connection_id,
            effect_id,
            token: state.next_abort_token,
        };
        state.last_abort = Some(abort.clone());
        abort
    }

    /// The client (and abort effect) that aborted the current turn, for `agent_end`/`session_abort.abortedBy`.
    pub fn aborted_by(&self) -> Option<ClientAbort> {
        self.lock().last_abort.clone()
    }

    /// A client abort that reached no turn has settled: only that abort is forgotten.
    pub fn forget_abort(&self, abort: &ClientAbort) {
        let mut state = self.lock();
        if state
            .last_abort
            .as_ref()
            .is_some_and(|last| last.token == abort.token)
        {
            state.last_abort = None;
        }
    }

    fn complete_queued(&self, outcome: &EffectOutcome) {
        if !self.current() {
            return;
        }
        let queued: Vec<ClientAdmissionRecord> = self
            .lock()
            .records
            .values()
            .filter(|record| matches!(record.state, AdmissionState::Queued { .. }))
            .cloned()
            .collect();
        for record in queued {
            self.save(ClientAdmissionRecord {
                state: AdmissionState::Completed {
                    outcome: Some(outcome.clone()),
                },
                ..record
            });
        }
    }

    /// The queues are about to be drained (`clear_queue`, `abort` or `unload_session` with `clearQueue`): give
    /// every still-queued input an honest `cleared` receipt first (senpi#3412).
    pub fn clear_queued(&self) {
        self.complete_queued(&effect_outcomes::INPUT_CLEARED.outcome());
    }

    /// The session is being replaced and its queue discarded with it.
    pub fn drop_queued_by_replace(&self) {
        self.complete_queued(&effect_outcomes::INPUT_DROPPED_BY_REPLACE.outcome());
    }

    fn end_outcome(&self, assistant_error: Option<Option<String>>) -> EffectOutcome {
        if let Some(abort_source) = self.session.current_abort_source() {
            return EffectOutcome {
                kind: EffectOutcomeKind::Aborted,
                reason: Some("aborted".into()),
                detail: Some("the turn was aborted before it finished".into()),
                action: None,
                abort_source: Some(abort_source),
                aborted_by: self.lock().turn_aborted_by.clone(),
            };
        }
        match assistant_error {
            Some(message) => turn_error_outcome(message.as_deref()),
            None => completed(),
        }
    }

    /// One forwarded session event (senpi's agent and session subscriptions).
    pub fn observe(&self, event: &EffectSessionEvent) {
        if !self.current() {
            return;
        }
        if let EffectSessionEvent::UserMessageStart { client_message_id } = event {
            // Agent listeners run before the next provider call: a consumed queue record cannot turn back
            // into a replayable queued admission.
            if let Some(id) = client_message_id
                && let Some(record) = self.record(id)
                && matches!(record.state, AdmissionState::Queued { .. })
            {
                self.save(ClientAdmissionRecord {
                    state: AdmissionState::Running,
                    ..record
                });
            }
            return;
        }
        if self.lock().restoring {
            return;
        }
        match event {
            // A new turn: an abort noted before it belongs to nothing this turn will report.
            EffectSessionEvent::AgentStart => self.lock().last_abort = None,
            EffectSessionEvent::AgentEnd {
                will_retry: false,
                assistant_error,
            } => {
                let outcome = self.end_outcome(assistant_error.clone());
                self.lock().turn_outcome = Some(outcome);
            }
            EffectSessionEvent::AgentIdle => {
                let (outcome, running) = {
                    let mut state = self.lock();
                    let outcome = state.turn_outcome.take().unwrap_or_else(completed);
                    state.turn_aborted_by = None;
                    state.last_abort = None;
                    let running: Vec<ClientAdmissionRecord> = state
                        .records
                        .values()
                        .filter(|record| matches!(record.state, AdmissionState::Running))
                        .cloned()
                        .collect();
                    (outcome, running)
                };
                for record in running {
                    self.save(ClientAdmissionRecord {
                        state: AdmissionState::Completed {
                            outcome: Some(outcome.clone()),
                        },
                        ..record
                    });
                }
            }
            EffectSessionEvent::QueueUpdate { queued_ids } => {
                let consuming = self.session.streaming_client_message_id();
                let gone: Vec<ClientAdmissionRecord> = self
                    .lock()
                    .records
                    .values()
                    .filter(|record| {
                        matches!(record.state, AdmissionState::Queued { .. })
                            && !queued_ids
                                .iter()
                                .any(|id| id.as_deref() == Some(record.client_message_id.as_str()))
                    })
                    .cloned()
                    .collect();
                for record in gone {
                    let state = if consuming.as_deref() == Some(record.client_message_id.as_str()) {
                        AdmissionState::Running
                    } else {
                        AdmissionState::Completed { outcome: None }
                    };
                    self.save(ClientAdmissionRecord { state, ..record });
                }
            }
            _ => {}
        }
    }

    async fn restore(&self) -> Result<(), String> {
        let mut queued: Vec<PreparedAdmissionInput> = self
            .lock()
            .records
            .values()
            .filter_map(|record| match &record.state {
                AdmissionState::Queued { prepared } => Some(prepared.clone()),
                _ => None,
            })
            .collect();
        queued.sort_by(|left, right| left.enqueue_order.total_cmp(&right.enqueue_order));
        let mut restored = Ok(());
        for prepared in queued {
            // senpi stops at the first failure; `restoring` always ends (`finally`).
            restored = self.session.restore_queued_input(prepared).await;
            if restored.is_err() {
                break;
            }
        }
        self.lock().restoring = false;
        restored
    }

    fn save(&self, record: ClientAdmissionRecord) {
        let data = serde_json::to_value(&record).unwrap_or(Value::Null);
        self.session
            .append_custom_entry(CLIENT_ADMISSION_ENTRY, data);
        let status = project_admission(&record, None);
        self.lock()
            .records
            .insert(record.client_message_id.clone(), record);
        note_session_effect(&self.session.effect_route(), status);
    }

    /// Admit one `prompt`/`steer`/`follow_up` command. `invoke` delivers it and reports through the hooks.
    /// `Ok(None)` for an id-less prompt and for a host-minted id (never answered back as the client's own).
    pub async fn admit(
        self: &Arc<Self>,
        command: &Value,
        invoke: impl FnOnce(AdmissionHooks) -> BoxFuture<'static, Result<AdmissionDisposition, String>>,
    ) -> Result<Option<Value>, AdmitError> {
        self.ensure_ready().await.map_err(AdmitError::Failed)?;
        let client_message_id = command["clientMessageId"].as_str().map(str::to_owned);
        let client_turn_id = command["clientTurnId"].as_str().map(str::to_owned);
        if client_message_id.is_some() || client_turn_id.is_some() {
            self.has_identities
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        // An id-less steer/follow_up is still made durable under a host-minted id, so a reopen restores it.
        let host_minted = client_message_id.is_none() && command["type"] != "prompt";
        if client_message_id.is_none() && !host_minted {
            invoke(AdmissionHooks {
                client_message_id: None,
                client_turn_id: None,
                admissions: None,
                base: None,
            })
            .await
            .map_err(AdmitError::Failed)?;
            return Ok(None);
        }
        let client_message_id = client_message_id.unwrap_or_else(|| {
            format!(
                "{HOST_MINTED_ID_PREFIX}{}:{}",
                self.session.durable_session_id(),
                uuid::Uuid::new_v4()
            )
        });
        let fingerprint = client_input_fingerprint(command);
        let conflict = || {
            AdmitError::Conflict(ClientMessageIdConflict {
                client_message_id: client_message_id.clone(),
            })
        };
        // The check and the pending reservation share one guard, so two same-id admits cannot both
        // reach `admit_new`.
        let reserved = {
            let mut state = self.lock();
            let known = state
                .pending
                .get(&client_message_id)
                .map(|(stored, _)| stored.clone())
                .or_else(|| {
                    state
                        .records
                        .get(&client_message_id)
                        .map(|record| record.fingerprint.clone())
                });
            if known.is_some_and(|stored| stored != fingerprint) {
                return Err(conflict());
            }
            if let Some((_, pending)) = state.pending.get(&client_message_id) {
                Err(pending.clone())
            } else if let Some(existing) = state.records.get(&client_message_id) {
                return Ok(Some(admission_snapshot(existing)));
            } else {
                let (publish, pending) = tokio::sync::watch::channel::<PendingResult>(None);
                state
                    .pending
                    .insert(client_message_id.clone(), (fingerprint.clone(), pending));
                Ok(PendingAdmission {
                    admissions: self.clone(),
                    client_message_id: client_message_id.clone(),
                    publish,
                })
            }
        };
        let pending = match reserved {
            Ok(pending) => pending,
            Err(mut pending) => {
                // The guard above is gone; joiners wait on the owner's published result.
                let result = pending
                    .wait_for(Option::is_some)
                    .await
                    .map_err(|_| AdmitError::Failed("RPC input admission was dropped".into()))?
                    .clone();
                return result
                    .expect("wait_for returned a published result")
                    .map(Some);
            }
        };
        let result = self
            .admit_new(
                &client_message_id,
                client_turn_id,
                fingerprint,
                host_minted,
                invoke,
            )
            .await;
        pending.publish.send_replace(Some(result.clone()));
        drop(pending);
        let snapshot = result?;
        Ok((!host_minted).then_some(snapshot))
    }

    async fn admit_new(
        self: &Arc<Self>,
        client_message_id: &str,
        client_turn_id: Option<String>,
        fingerprint: String,
        host_minted: bool,
        invoke: impl FnOnce(AdmissionHooks) -> BoxFuture<'static, Result<AdmissionDisposition, String>>,
    ) -> Result<Value, AdmitError> {
        self.session
            .persist_header_now()
            .await
            .map_err(AdmitError::Failed)?;
        let base = ClientAdmissionRecord {
            client_message_id: client_message_id.to_owned(),
            client_turn_id: client_turn_id.clone(),
            durable_session_id: self.session.durable_session_id(),
            fingerprint,
            disposition: AdmissionDisposition::Queued,
            host_minted: host_minted.then_some(true),
            state: AdmissionState::Running,
        };
        let disposition = invoke(AdmissionHooks {
            client_message_id: Some(client_message_id.to_owned()),
            client_turn_id,
            admissions: Some(self.clone()),
            base: Some(base),
        })
        .await
        .map_err(AdmitError::Failed)?;
        let record = self.record(client_message_id).ok_or_else(|| {
            AdmitError::Failed(format!(
                "RPC input was {} without a durable admission",
                serde_json::to_value(disposition)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ))
        })?;
        Ok(admission_snapshot(&record))
    }
}

fn completed() -> EffectOutcome {
    EffectOutcome {
        kind: EffectOutcomeKind::Completed,
        reason: None,
        detail: None,
        action: None,
        abort_source: None,
        aborted_by: None,
    }
}

#[cfg(test)]
pub(crate) mod test_session {
    use super::*;
    use crate::effect_session::{QueuedChangeStatus, QueuedInputView, SetModelError};
    use futures::FutureExt;

    /// An in-memory [`EffectSession`]: a transcript vector plus scripted queue and model behaviour.
    pub struct FakeSession {
        pub route: String,
        pub durable: Mutex<String>,
        pub entries: Mutex<Vec<Value>>,
        pub streaming: std::sync::atomic::AtomicBool,
        pub abort_source: Mutex<Option<String>>,
        pub streaming_id: Mutex<Option<String>>,
        pub restored: Mutex<Vec<PreparedAdmissionInput>>,
        pub queued: Mutex<Vec<QueuedInputView>>,
        pub change: Mutex<Option<QueuedChangeStatus>>,
        pub models: Vec<(String, String)>,
        pub set_model_error: Mutex<Option<SetModelError>>,
        pub thinking_levels: Vec<String>,
        pub thinking: Mutex<Vec<(String, bool, Option<String>)>>,
        pub cleared: std::sync::atomic::AtomicUsize,
        /// When set, the next `append_custom_entry` reports on the first channel and parks until the second fires.
        pub append_gate: Mutex<Option<AppendGate>>,
    }

    pub type AppendGate = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);

    impl FakeSession {
        pub fn new(route: &str) -> Self {
            Self {
                route: route.into(),
                durable: Mutex::new(format!("durable-{route}")),
                entries: Mutex::default(),
                streaming: false.into(),
                abort_source: Mutex::default(),
                streaming_id: Mutex::default(),
                restored: Mutex::default(),
                queued: Mutex::default(),
                change: Mutex::default(),
                models: vec![("anthropic".into(), "claude".into())],
                set_model_error: Mutex::default(),
                thinking_levels: vec!["low".into(), "high".into()],
                thinking: Mutex::default(),
                cleared: 0.into(),
                append_gate: Mutex::default(),
            }
        }
        pub fn custom(&self, custom_type: &str) -> Vec<Value> {
            self.entries
                .lock()
                .unwrap()
                .iter()
                .filter(|entry| entry["customType"] == custom_type)
                .map(|entry| entry["data"].clone())
                .collect()
        }
    }

    impl EffectSession for FakeSession {
        fn durable_session_id(&self) -> String {
            self.durable.lock().unwrap().clone()
        }
        fn effect_route(&self) -> String {
            self.route.clone()
        }
        fn entries(&self) -> Vec<Value> {
            self.entries.lock().unwrap().clone()
        }
        fn append_custom_entry(&self, custom_type: &str, data: Value) {
            let gate = self.append_gate.lock().unwrap().take();
            if let Some((entered, release)) = gate {
                entered.send(()).unwrap();
                release.recv().unwrap();
            }
            self.entries
                .lock()
                .unwrap()
                .push(serde_json::json!({"type":"custom","customType":custom_type,"data":data}));
        }
        fn persist_header_now(&self) -> BoxFuture<'_, Result<(), String>> {
            async { Ok(()) }.boxed()
        }
        fn session_file(&self) -> Option<String> {
            Some(format!("/sessions/{}.jsonl", self.route))
        }
        fn is_streaming(&self) -> bool {
            self.streaming.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn current_abort_source(&self) -> Option<String> {
            self.abort_source.lock().unwrap().clone()
        }
        fn streaming_client_message_id(&self) -> Option<String> {
            self.streaming_id.lock().unwrap().clone()
        }
        fn restore_queued_input(
            &self,
            prepared: PreparedAdmissionInput,
        ) -> BoxFuture<'_, Result<(), String>> {
            self.restored.lock().unwrap().push(prepared);
            async { Ok(()) }.boxed()
        }
        fn clear_queue(&self, _abort_will_follow: bool) -> usize {
            self.cleared
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::mem::take(&mut *self.queued.lock().unwrap()).len()
        }
        fn queued_inputs(&self) -> Vec<QueuedInputView> {
            self.queued.lock().unwrap().clone()
        }
        fn change_queued_input(
            &self,
            queued_id: &str,
            text: Option<String>,
            _expected_revision: Option<u64>,
            _effect_id: Option<String>,
        ) -> BoxFuture<'_, Result<QueuedChangeStatus, String>> {
            let scripted = *self.change.lock().unwrap();
            let status = scripted.unwrap_or_else(|| {
                let mut queued = self.queued.lock().unwrap();
                match queued.iter().position(|input| input.queued_id == queued_id) {
                    None => QueuedChangeStatus::NotFound,
                    Some(index) => match text {
                        None => {
                            queued.remove(index);
                            QueuedChangeStatus::Removed
                        }
                        Some(text) => {
                            queued[index].text = text;
                            queued[index].revision += 1;
                            QueuedChangeStatus::Edited
                        }
                    },
                }
            });
            async move { Ok(status) }.boxed()
        }
        fn find_available_model(
            &self,
            provider: &str,
            model_id: &str,
        ) -> BoxFuture<'_, Result<Option<Value>, String>> {
            let found = self
                .models
                .iter()
                .any(|(p, m)| p == provider && m == model_id)
                .then(|| serde_json::json!({"provider":provider,"id":model_id}));
            async move { Ok(found) }.boxed()
        }
        fn set_model(
            &self,
            _provider: &str,
            _model_id: &str,
            _effect_id: Option<String>,
        ) -> BoxFuture<'_, Result<Option<String>, SetModelError>> {
            let error = self.set_model_error.lock().unwrap().clone();
            async move {
                match error {
                    Some(error) => Err(error),
                    None => Ok(None),
                }
            }
            .boxed()
        }
        fn available_thinking_levels(&self) -> Vec<String> {
            self.thinking_levels.clone()
        }
        fn set_thinking_level(&self, level: &str, turn_scope: bool, effect_id: Option<String>) {
            self.thinking
                .lock()
                .unwrap()
                .push((level.to_owned(), turn_scope, effect_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_session::FakeSession;
    use super::*;
    use futures::FutureExt;
    use serde_json::json;

    fn session(route: &str) -> (Arc<FakeSession>, Arc<dyn EffectSession>) {
        let fake = Arc::new(FakeSession::new(route));
        let dynamic: Arc<dyn EffectSession> = fake.clone();
        (fake, dynamic)
    }

    fn start(hooks: AdmissionHooks) -> BoxFuture<'static, Result<AdmissionDisposition, String>> {
        assert!(hooks.accepted(AdmissionDisposition::Started).is_some());
        async { Ok(AdmissionDisposition::Started) }.boxed()
    }

    #[tokio::test]
    async fn a_same_id_retry_replays_and_a_changed_payload_conflicts() {
        let (fake, session) = session("ca-replay");
        let ledger = ClientAdmissions::for_session(&session).await;
        let command = json!({"type":"prompt","message":"hi","clientMessageId":"m1"});
        let first = ledger.admit(&command, start).await.unwrap().unwrap();
        assert_eq!(first["state"], "running");
        let replay = ledger
            .admit(
                &command,
                |_| -> BoxFuture<'static, Result<AdmissionDisposition, String>> {
                    panic!("a replay never delivers")
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replay, first);
        let changed = json!({"type":"prompt","message":"other","clientMessageId":"m1"});
        assert!(matches!(
            ledger.admit(&changed, start).await,
            Err(AdmitError::Conflict(_))
        ));
        assert_eq!(fake.custom(CLIENT_ADMISSION_ENTRY).len(), 1);
        ClientAdmissions::forget("ca-replay");
    }

    #[tokio::test]
    async fn an_idle_seals_running_inputs_with_the_turn_outcome() {
        let (fake, session) = session("ca-idle");
        let ledger = ClientAdmissions::for_session(&session).await;
        ledger
            .admit(
                &json!({"type":"prompt","message":"hi","clientMessageId":"m1"}),
                start,
            )
            .await
            .unwrap();
        ledger.observe(&EffectSessionEvent::AgentEnd {
            will_retry: false,
            assistant_error: Some(Some("rate limited".into())),
        });
        ledger.observe(&EffectSessionEvent::AgentIdle);
        let last = fake.custom(CLIENT_ADMISSION_ENTRY).pop().unwrap();
        assert_eq!(last["state"], "completed");
        assert_eq!(last["outcome"]["kind"], "error");
        assert_eq!(last["outcome"]["detail"], "rate limited");
        ClientAdmissions::forget("ca-idle");
    }

    #[tokio::test]
    async fn a_reopen_seals_running_records_and_restores_queued_input_in_order() {
        let (fake, session) = session("ca-reopen");
        let durable = session.durable_session_id();
        let prepared =
            |text: &str, order: f64| json!({"text":text,"mode":"followUp","enqueueOrder":order});
        for data in [
            json!({"clientMessageId":"r","durableSessionId":durable,"fingerprint":"f","disposition":"started","state":"running"}),
            json!({"clientMessageId":"q2","durableSessionId":durable,"fingerprint":"f","disposition":"queued","state":"queued","prepared":prepared("second",2.0)}),
            json!({"clientMessageId":"q1","durableSessionId":durable,"fingerprint":"f","disposition":"queued","state":"queued","prepared":prepared("first",1.0)}),
            json!({"note":"not an admission"}),
        ] {
            session.append_custom_entry(CLIENT_ADMISSION_ENTRY, data);
        }
        let ledger = ClientAdmissions::for_session(&session).await;
        let sealed = ledger.record("r").unwrap();
        assert!(matches!(
            sealed.state,
            AdmissionState::Completed { outcome: Some(ref outcome) }
                if outcome.kind == EffectOutcomeKind::InterruptedByReopen
        ));
        let restored: Vec<String> = fake
            .restored
            .lock()
            .unwrap()
            .iter()
            .map(|prepared| prepared.text.clone())
            .collect();
        assert_eq!(restored, ["first", "second"]);
        assert!(ledger.has_identities());
        ClientAdmissions::forget("ca-reopen");
    }

    #[tokio::test]
    async fn queued_inputs_get_cleared_or_running_receipts_as_they_leave_the_queue() {
        let (fake, session) = session("ca-queue");
        let ledger = ClientAdmissions::for_session(&session).await;
        for id in ["a", "b", "c"] {
            ledger
                .admit(
                    &json!({"type":"steer","message":id,"clientMessageId":id}),
                    |hooks| {
                        hooks.queued(PreparedAdmissionInput {
                            client_message_id: hooks.client_message_id.clone(),
                            client_turn_id: None,
                            text: "t".into(),
                            images: None,
                            mode: crate::client_admission_record::PreparedInputMode::Steer,
                            enqueue_order: 1.0,
                        });
                        async { Ok(AdmissionDisposition::Queued) }.boxed()
                    },
                )
                .await
                .unwrap();
        }
        *fake.streaming_id.lock().unwrap() = Some("a".into());
        ledger.observe(&EffectSessionEvent::QueueUpdate {
            queued_ids: vec![Some("c".into())],
        });
        assert!(matches!(
            ledger.record("a").unwrap().state,
            AdmissionState::Running
        ));
        assert!(matches!(
            ledger.record("b").unwrap().state,
            AdmissionState::Completed { outcome: None }
        ));
        ledger.clear_queued();
        assert!(matches!(
            ledger.record("c").unwrap().state,
            AdmissionState::Completed { outcome: Some(ref outcome) } if outcome.kind == EffectOutcomeKind::Cleared
        ));
        ClientAdmissions::forget("ca-queue");
    }

    #[tokio::test]
    async fn an_id_less_steer_is_durable_under_a_host_minted_id_that_is_never_answered() {
        let (fake, session) = session("ca-minted");
        let ledger = ClientAdmissions::for_session(&session).await;
        let answer = ledger
            .admit(&json!({"type":"steer","message":"x"}), start)
            .await
            .unwrap();
        assert_eq!(answer, None);
        let stored = fake.custom(CLIENT_ADMISSION_ENTRY).pop().unwrap();
        assert!(
            stored["clientMessageId"]
                .as_str()
                .unwrap()
                .starts_with("host:durable-ca-minted:")
        );
        assert_eq!(stored["hostMinted"], true);
        assert!(!ledger.has_identities());
        ClientAdmissions::forget("ca-minted");
    }

    #[tokio::test]
    async fn an_abort_is_forgotten_only_by_its_own_token() {
        let (_fake, session) = session("ca-abort");
        let ledger = ClientAdmissions::for_session(&session).await;
        let by = EffectActor {
            connection_id: "c1".into(),
        };
        let first = ledger.note_abort(by.clone(), None);
        let second = ledger.note_abort(by, Some("e2".into()));
        ledger.forget_abort(&first);
        assert_eq!(ledger.aborted_by(), Some(second.clone()));
        ledger.forget_abort(&second);
        assert_eq!(ledger.aborted_by(), None);
        ClientAdmissions::forget("ca-abort");
    }

    /// Hooks that count deliveries and hold the owner inside `invoke` until `release` fires.
    fn held(
        delivered: Arc<std::sync::atomic::AtomicUsize>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> impl FnOnce(AdmissionHooks) -> BoxFuture<'static, Result<AdmissionDisposition, String>>
    {
        move |hooks| {
            delivered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                let _ = release.await;
                hooks.accepted(AdmissionDisposition::Started);
                Ok(AdmissionDisposition::Started)
            }
            .boxed()
        }
    }

    #[tokio::test]
    async fn a_same_id_admit_during_delivery_joins_the_owner() {
        let (fake, session) = session("ca-join");
        let ledger = ClientAdmissions::for_session(&session).await;
        let command = json!({"type":"prompt","message":"hi","clientMessageId":"m1"});
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (release, held_owner) = tokio::sync::oneshot::channel();
        let (_unused, held_second) = tokio::sync::oneshot::channel();
        // Explicit polls fix the order: the owner parks inside `invoke`, then the second admit finds the id
        // taken and waits on the owner, and only then is the owner released.
        let mut owner = Box::pin(ledger.admit(&command, held(delivered.clone(), held_owner)));
        assert!(futures::poll!(&mut owner).is_pending());
        let mut joiner = Box::pin(ledger.admit(&command, held(delivered.clone(), held_second)));
        assert!(futures::poll!(&mut joiner).is_pending());
        release.send(()).unwrap();
        let (first, second) = (owner.await, joiner.await);
        assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(first.unwrap(), second.unwrap());
        assert_eq!(fake.custom(CLIENT_ADMISSION_ENTRY).len(), 1);
        ClientAdmissions::forget("ca-join");
    }

    #[tokio::test]
    async fn same_id_admits_from_two_threads_deliver_once() {
        let (fake, session) = session("ca-threads");
        let ledger = ClientAdmissions::for_session(&session).await;
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let answers: Vec<Value> = std::thread::scope(|scope| {
            let admits: Vec<_> = (0..2)
                .map(|_| {
                    let (ledger, delivered, barrier) =
                        (ledger.clone(), delivered.clone(), barrier.clone());
                    scope.spawn(move || {
                        let command =
                            json!({"type":"prompt","message":"hi","clientMessageId":"m1"});
                        barrier.wait();
                        futures::executor::block_on(ledger.admit(&command, |hooks| {
                            delivered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            start(hooks)
                        }))
                        .unwrap()
                        .unwrap()
                    })
                })
                .collect();
            admits
                .into_iter()
                .map(|admit| admit.join().unwrap())
                .collect()
        });
        assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(answers[0], answers[1]);
        assert_eq!(fake.custom(CLIENT_ADMISSION_ENTRY).len(), 1);
        ClientAdmissions::forget("ca-threads");
    }

    #[tokio::test]
    async fn a_cancelled_owner_frees_its_id_and_fails_its_joiners() {
        let (_fake, session) = session("ca-cancel");
        let ledger = ClientAdmissions::for_session(&session).await;
        let command = json!({"type":"prompt","message":"hi","clientMessageId":"m1"});
        let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (_never, held_owner) = tokio::sync::oneshot::channel();
        let mut owner = Box::pin(ledger.admit(&command, held(delivered.clone(), held_owner)));
        assert!(futures::poll!(&mut owner).is_pending());
        let mut joiner = Box::pin(ledger.admit(&command, start));
        assert!(futures::poll!(&mut joiner).is_pending());
        drop(owner);
        assert!(matches!(joiner.await, Err(AdmitError::Failed(_))));
        let retried = ledger.admit(&command, start).await.unwrap().unwrap();
        assert_eq!(retried["state"], "running");
        assert_eq!(delivered.load(std::sync::atomic::Ordering::SeqCst), 1);
        ClientAdmissions::forget("ca-cancel");
    }
}
