//! Port of senpi `packages/coding-agent/src/core/pending-input-store.ts`.
//!
//! The one owner of every pending input in a session: reduce, commit, then publish and dispatch
//! (senpi#3549). Each command runs under one async gate; the in-memory view sits behind a short sync lock
//! so `current`/`read`/`snapshot` stay synchronous like upstream.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::pending_input::reduce_pending_input;
use crate::pending_input_projection::{PendingInputProjection, project_pending_input};
use crate::pending_input_types::{
    CancelCause, DeliveryToken, PendingInput, PendingInputDecision, PendingInputEffect,
    PendingInputEvent, PendingInputHandle, PendingInputOperationResult, PendingInputPhase,
    SubmitEvent,
};

pub const PENDING_INPUT_TRANSITION: &str = "pending_input_transition";

/// One complete, durable transition: the input after it and the answer it gave, appended before anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInputJournalRecord {
    #[serde(rename = "type")]
    pub kind: String,
    pub cursor: u64,
    pub input: PendingInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<PendingInputOperationResult>,
    /// Further inputs changed in the same step (a consumer's batch), recorded atomically with `input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<Vec<PendingInput>>,
}

/// Errors of the journal and the store; `code()` is upstream's machine-readable `code`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingInputError {
    /// A line naming a transition was written whole but does not parse.
    JournalCorrupt { line_number: usize },
    /// The entry reached the file but could not be synced: it may or may not survive a crash.
    AppendUncertain { cursor: u64, message: String },
    /// The session file is not on disk yet.
    NotDurable,
    /// A transition is missing, which a skipped corrupt line leaves behind.
    JournalGap { missing_cursor: u64 },
    /// A transition entry does not have the record's shape.
    JournalInvalid { entry_id: String },
    /// A previous write may have landed without being synced; only a reopen can say what the journal holds.
    StoreUnavailable { cause: Box<PendingInputError> },
    /// Any other journal failure (an I/O error, a test journal refusing a write).
    Journal(String),
}

impl PendingInputError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::JournalCorrupt { .. } => "pending_input_journal_corrupt",
            Self::AppendUncertain { .. } => "pending_input_append_uncertain",
            Self::NotDurable => "pending_input_journal_not_durable",
            Self::JournalGap { .. } => "pending_input_journal_gap",
            Self::JournalInvalid { .. } => "pending_input_journal_invalid",
            Self::StoreUnavailable { .. } => "pending_input_store_unavailable",
            Self::Journal(_) => "pending_input_journal_error",
        }
    }
}

impl fmt::Display for PendingInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::JournalCorrupt { line_number } => write!(
                f,
                "Pending input journal line {line_number} is corrupt; the session cannot recover its queued input safely."
            ),
            Self::AppendUncertain { message, .. } => f.write_str(message),
            Self::NotDurable => {
                f.write_str("The session file is not on disk yet, so a pending input cannot be saved; try again.")
            }
            Self::JournalGap { missing_cursor } => write!(
                f,
                "Pending input journal is missing transition {missing_cursor}; the session cannot recover its queued input safely."
            ),
            Self::JournalInvalid { entry_id } => write!(
                f,
                "Pending input journal entry {entry_id} is malformed; the session cannot recover its queued input safely."
            ),
            Self::StoreUnavailable { .. } => f.write_str(
                "Pending inputs are unavailable after a failed save; reopen the session to recover them.",
            ),
            Self::Journal(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PendingInputError {}

#[async_trait::async_trait]
pub trait PendingInputJournal: Send + Sync {
    async fn append(&self, record: &PendingInputJournalRecord) -> Result<(), PendingInputError>;
    async fn read(&self) -> Result<Vec<PendingInputJournalRecord>, PendingInputError>;
}

pub type DispatchHook = Arc<dyn Fn(&DeliveryToken, &PendingInput) + Send + Sync>;
pub type RestoreToDraftHook = Arc<dyn Fn(&str) + Send + Sync>;
pub type ListenerErrorHook = Arc<dyn Fn(String) + Send + Sync>;
/// Called after each committed transition with its projections and the journal cursor it was written at.
/// An `Err` is a failing observer: it never undoes the commit.
pub type PendingInputListener =
    Arc<dyn Fn(&[PendingInputProjection], u64) -> Result<(), String> + Send + Sync>;

#[derive(Clone)]
pub struct PendingInputStoreOptions {
    pub journal: Arc<dyn PendingInputJournal>,
    pub lifetime_id: String,
    pub next_queued_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub dispatch: Option<DispatchHook>,
    pub restore_to_draft: Option<RestoreToDraftHook>,
    /// Passed to every reopen rebind; see the `Rebind` event (senpi#3549).
    pub uncommitted_attempts_are_unsent: bool,
    /// Receives a listener's failure. Without it the failure is written to stderr so it is never silently lost.
    pub on_listener_error: Option<ListenerErrorHook>,
}

/// A submission without the handle; the store assigns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitRequest {
    pub client_message_id: String,
    pub fingerprint: String,
    pub text: String,
    pub attachments: Vec<String>,
    pub requested_mode: crate::pending_input_types::PendingInputMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInputChange {
    pub client_message_id: String,
    pub event: PendingInputEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingInputRead {
    Admitted { projection: PendingInputProjection },
    NotAdmitted { client_message_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopItem {
    pub client_message_id: String,
    pub handle: PendingInputHandle,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopConflict {
    pub client_message_id: String,
    pub current_revision: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopReceipt {
    pub removed: Vec<String>,
    pub conflicts: Vec<StopConflict>,
    pub retained: Vec<String>,
    /// Named with a handle of another lifetime or queue slot: the caller saw another version and must refresh.
    pub stale: Vec<String>,
}

type OperationResult = Result<Option<PendingInputOperationResult>, PendingInputError>;

#[derive(Default)]
struct StoreState {
    inputs: IndexMap<String, PendingInput>,
    cursor: u64,
    unavailable: Option<PendingInputError>,
}

/// The one owner of every pending input in a session: reduce, commit, then publish and dispatch (senpi#3549).
pub struct PendingInputStore {
    options: PendingInputStoreOptions,
    gate: tokio::sync::Mutex<()>,
    state: Mutex<StoreState>,
    listeners: Mutex<Vec<(u64, PendingInputListener)>>,
    next_listener: Mutex<u64>,
}

/// Transitions inside one gated step: each is committed directly, since the caller already holds the gate.
#[derive(Clone)]
pub struct PendingInputTransaction {
    store: Arc<PendingInputStore>,
}

impl PendingInputTransaction {
    pub async fn submit(&self, request: SubmitRequest) -> OperationResult {
        self.store.admit(request).await
    }

    pub async fn apply(
        &self,
        client_message_id: &str,
        event: PendingInputEvent,
    ) -> OperationResult {
        self.store.commit(client_message_id, event).await
    }

    /// Applies every change in one journal record: all of them land or none does.
    pub async fn apply_all(
        &self,
        changes: Vec<PendingInputChange>,
    ) -> Result<Vec<Option<PendingInputOperationResult>>, PendingInputError> {
        self.store.commit_many(changes).await
    }

    pub fn current(&self, client_message_id: &str) -> Option<PendingInput> {
        self.store.current(client_message_id)
    }
}

impl PendingInputStore {
    /// Rebuilds from the complete journal, then gives every still-pending input a handle of this lifetime.
    pub async fn open(options: PendingInputStoreOptions) -> Result<Arc<Self>, PendingInputError> {
        let records = options.journal.read().await?;
        let store = Arc::new(Self {
            options,
            gate: tokio::sync::Mutex::new(()),
            state: Mutex::new(StoreState::default()),
            listeners: Mutex::new(Vec::new()),
            next_listener: Mutex::new(0),
        });
        let pending: Vec<String> = {
            let mut state = store.state();
            for record in records {
                state.cursor = state.cursor.max(record.cursor);
                for input in std::iter::once(record.input).chain(record.batch.unwrap_or_default()) {
                    state.inputs.insert(input.client_message_id.clone(), input);
                }
            }
            state
                .inputs
                .values()
                .filter(|input| {
                    !matches!(
                        input.phase,
                        PendingInputPhase::Delivered | PendingInputPhase::Removed
                    )
                })
                .map(|input| input.client_message_id.clone())
                .collect()
        };
        for client_message_id in pending {
            let handle = PendingInputHandle {
                lifetime_id: store.options.lifetime_id.clone(),
                queued_id: (store.options.next_queued_id)(),
            };
            let attempts_unsent = store.options.uncommitted_attempts_are_unsent;
            store
                .commit(
                    &client_message_id,
                    PendingInputEvent::Rebind {
                        handle,
                        attempts_unsent,
                    },
                )
                .await?;
        }
        Ok(store)
    }

    pub async fn submit(&self, request: SubmitRequest) -> OperationResult {
        let _gate = self.enter().await?;
        self.admit(request).await
    }

    pub async fn apply(
        &self,
        client_message_id: &str,
        event: PendingInputEvent,
    ) -> OperationResult {
        let _gate = self.enter().await?;
        self.commit(client_message_id, event).await
    }

    /// Explicit Stop over the inputs the caller saw: each named revision is removed or reported, never guessed.
    pub async fn stop(
        &self,
        items: &[StopItem],
        consumer_fenced: bool,
    ) -> Result<StopReceipt, PendingInputError> {
        let _gate = self.enter().await?;
        let mut receipt = StopReceipt::default();
        for item in items {
            let Some(current) = self.current(&item.client_message_id) else {
                continue;
            };
            // Revisions restart at 1 on every reopen, so the handle decides which version the caller saw.
            if current.handle != item.handle {
                receipt.stale.push(item.client_message_id.clone());
                continue;
            }
            if current.phase == PendingInputPhase::Queued
                && current.revision != item.expected_revision
            {
                receipt.conflicts.push(StopConflict {
                    client_message_id: item.client_message_id.clone(),
                    current_revision: current.revision,
                });
                continue;
            }
            let cancel = PendingInputEvent::Cancel {
                cause: CancelCause::Stop,
                consumer_fenced,
            };
            self.commit(&item.client_message_id, cancel).await?;
            let after = self
                .current(&item.client_message_id)
                .map(|input| input.phase);
            if after == Some(PendingInputPhase::Removed)
                && current.phase != PendingInputPhase::Removed
            {
                receipt.removed.push(item.client_message_id.clone());
            } else {
                receipt.retained.push(item.client_message_id.clone());
            }
        }
        Ok(receipt)
    }

    /// Runs `work` under the gate every command shares, so a consumer can change its native buffer and the
    /// journal in one step that no edit, removal or other consumer can interleave with.
    pub async fn transaction<T, F, Fut>(self: &Arc<Self>, work: F) -> Result<T, PendingInputError>
    where
        F: FnOnce(PendingInputTransaction) -> Fut,
        Fut: Future<Output = T>,
    {
        let _gate = self.enter().await?;
        Ok(work(PendingInputTransaction {
            store: self.clone(),
        })
        .await)
    }

    /// The full record of one input, for the session that owns this store.
    pub fn current(&self, client_message_id: &str) -> Option<PendingInput> {
        self.state().inputs.get(client_message_id).cloned()
    }

    pub fn read(&self, client_message_id: &str) -> PendingInputRead {
        match self.state().inputs.get(client_message_id) {
            Some(input) => PendingInputRead::Admitted {
                projection: project_pending_input(input),
            },
            None => PendingInputRead::NotAdmitted {
                client_message_id: client_message_id.to_owned(),
            },
        }
    }

    /// The cursor of the newest committed transition; a snapshot taken now covers everything through it.
    pub fn through_cursor(&self) -> u64 {
        self.state().cursor
    }

    pub fn snapshot(&self) -> Vec<PendingInputProjection> {
        self.state()
            .inputs
            .values()
            .map(project_pending_input)
            .collect()
    }

    /// Returns the id to pass to `unsubscribe`.
    pub fn subscribe(&self, listener: PendingInputListener) -> u64 {
        let mut next = self
            .next_listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *next += 1;
        let id = *next;
        self.listeners().push((id, listener));
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.listeners()
            .retain(|(listener_id, _)| *listener_id != id);
    }

    async fn enter(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, PendingInputError> {
        let gate = self.gate.lock().await;
        if let Some(cause) = self.state().unavailable.clone() {
            return Err(PendingInputError::StoreUnavailable {
                cause: Box::new(cause),
            });
        }
        Ok(gate)
    }

    fn state(&self) -> MutexGuard<'_, StoreState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn listeners(&self) -> MutexGuard<'_, Vec<(u64, PendingInputListener)>> {
        self.listeners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    async fn admit(&self, request: SubmitRequest) -> OperationResult {
        let handle = self
            .current(&request.client_message_id)
            .map(|input| input.handle)
            .unwrap_or_else(|| PendingInputHandle {
                lifetime_id: self.options.lifetime_id.clone(),
                queued_id: (self.options.next_queued_id)(),
            });
        let client_message_id = request.client_message_id.clone();
        let event = PendingInputEvent::Submit(SubmitEvent {
            client_message_id: request.client_message_id,
            fingerprint: request.fingerprint,
            handle,
            text: request.text,
            attachments: request.attachments,
            requested_mode: request.requested_mode,
        });
        self.commit(&client_message_id, event).await
    }

    async fn commit(&self, client_message_id: &str, event: PendingInputEvent) -> OperationResult {
        let change = PendingInputChange {
            client_message_id: client_message_id.to_owned(),
            event,
        };
        Ok(self
            .commit_many(vec![change])
            .await?
            .into_iter()
            .next()
            .flatten())
    }

    /// Reduces every change, then records all that changed in ONE journal record, so a crash or a failed write
    /// keeps either all of them or none (a consumer committing a batch never sees half of it delivered).
    async fn commit_many(
        &self,
        changes: Vec<PendingInputChange>,
    ) -> Result<Vec<Option<PendingInputOperationResult>>, PendingInputError> {
        let mut results = Vec::with_capacity(changes.len());
        let mut committed: Vec<(PendingInput, PendingInputDecision)> = Vec::new();
        let mut staged: IndexMap<String, PendingInput> = IndexMap::new();
        let cursor = {
            let state = self.state();
            for PendingInputChange {
                client_message_id,
                event,
            } in &changes
            {
                let current = staged
                    .get(client_message_id)
                    .or_else(|| state.inputs.get(client_message_id));
                let decision = reduce_pending_input(current, event);
                let result = decision.result.clone().map(|mut result| {
                    if result.client_message_id.is_empty() {
                        result.client_message_id = client_message_id.clone();
                    }
                    result
                });
                results.push(result.clone());
                let Some(next) = decision.next.clone() else {
                    continue;
                };
                if decision.effects.is_empty() {
                    continue;
                }
                staged.insert(client_message_id.clone(), next.clone());
                committed.push((next, PendingInputDecision { result, ..decision }));
            }
            state.cursor
        };
        let Some((first, first_decision)) = committed.first() else {
            return Ok(results);
        };
        let rest: Vec<PendingInput> = committed
            .iter()
            .skip(1)
            .map(|(next, _)| next.clone())
            .collect();
        let record = PendingInputJournalRecord {
            kind: PENDING_INPUT_TRANSITION.to_owned(),
            cursor: cursor + 1,
            input: first.clone(),
            result: first_decision.result.clone(),
            batch: (!rest.is_empty()).then_some(rest),
        };
        if let Err(error) = self.options.journal.append(&record).await {
            // A write that may have landed cannot be retried at the same cursor, or the next reopen would see a gap:
            // stop accepting commands until a reopen rebuilds the store from what the file actually holds.
            if matches!(error, PendingInputError::AppendUncertain { .. }) {
                self.state().unavailable = Some(error.clone());
            }
            return Err(error);
        }
        {
            let mut state = self.state();
            state.cursor = record.cursor;
            for (next, _) in &committed {
                state
                    .inputs
                    .insert(next.client_message_id.clone(), next.clone());
            }
        }
        // One notification per record: every input it changed, under its one cursor.
        let projections: Vec<PendingInputProjection> = committed
            .iter()
            .map(|(next, _)| project_pending_input(next))
            .collect();
        let listeners: Vec<PendingInputListener> =
            self.listeners().iter().map(|(_, l)| l.clone()).collect();
        for listener in listeners {
            if let Err(error) = listener(&projections, record.cursor) {
                self.report_listener_error(error);
            }
        }
        for (next, decision) in &committed {
            for effect in &decision.effects {
                match effect {
                    PendingInputEffect::Dispatch { token } => {
                        if let Some(dispatch) = &self.options.dispatch {
                            dispatch(token, next);
                        }
                    }
                    PendingInputEffect::RestoreToDraft { text } => {
                        if let Some(restore) = &self.options.restore_to_draft {
                            restore(text);
                        }
                    }
                    PendingInputEffect::Persist => {}
                }
            }
        }
        Ok(results)
    }

    fn report_listener_error(&self, error: String) {
        match &self.options.on_listener_error {
            Some(report) => report(error),
            None => eprintln!("pending input listener failed: {error}"),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// senpi's `MemoryJournal` test double.
    #[derive(Default)]
    pub struct MemoryJournal {
        pub records: Mutex<Vec<PendingInputJournalRecord>>,
        #[allow(clippy::type_complexity)]
        pub fail_when: Mutex<Option<Box<dyn Fn(&PendingInputJournalRecord) -> bool + Send + Sync>>>,
    }

    impl MemoryJournal {
        pub fn records(&self) -> Vec<PendingInputJournalRecord> {
            self.records
                .lock()
                .map(|records| records.clone())
                .unwrap_or_default()
        }

        pub fn fail_when(
            &self,
            predicate: impl Fn(&PendingInputJournalRecord) -> bool + Send + Sync + 'static,
        ) {
            if let Ok(mut slot) = self.fail_when.lock() {
                *slot = Some(Box::new(predicate));
            }
        }

        pub fn phases(&self, client_message_id: &str) -> Vec<PendingInputPhase> {
            self.records()
                .iter()
                .flat_map(|record| {
                    std::iter::once(&record.input)
                        .chain(record.batch.iter().flatten())
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .filter(|input| input.client_message_id == client_message_id)
                .map(|input| input.phase)
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl PendingInputJournal for MemoryJournal {
        async fn append(
            &self,
            record: &PendingInputJournalRecord,
        ) -> Result<(), PendingInputError> {
            let fails = self
                .fail_when
                .lock()
                .ok()
                .and_then(|slot| slot.as_ref().map(|p| p(record)))
                .unwrap_or(false);
            if fails {
                return Err(PendingInputError::Journal("disk full".into()));
            }
            if let Ok(mut records) = self.records.lock() {
                records.push(record.clone());
            }
            Ok(())
        }

        async fn read(&self) -> Result<Vec<PendingInputJournalRecord>, PendingInputError> {
            Ok(self.records())
        }
    }

    pub fn options(
        journal: Arc<dyn PendingInputJournal>,
        lifetime_id: &str,
    ) -> PendingInputStoreOptions {
        let next = Arc::new(Mutex::new(0u64));
        PendingInputStoreOptions {
            journal,
            lifetime_id: lifetime_id.to_owned(),
            next_queued_id: Arc::new(move || {
                let mut next = next.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                *next += 1;
                format!("q_{next}")
            }),
            dispatch: None,
            restore_to_draft: None,
            uncommitted_attempts_are_unsent: false,
            on_listener_error: None,
        }
    }

    pub fn submit_request() -> SubmitRequest {
        SubmitRequest {
            client_message_id: "c-1".into(),
            fingerprint: "fp".into(),
            text: "check the logs".into(),
            attachments: Vec::new(),
            requested_mode: crate::pending_input_types::PendingInputMode::FollowUp,
        }
    }
}

#[cfg(test)]
mod tests {
    //! senpi `test/suite/pending-input-store.test.ts`.
    use super::test_support::*;
    use super::*;
    use crate::pending_input_projection::PendingInputState;
    use crate::pending_input_types::{MutationTarget, OperationOutcome};

    struct Opened {
        store: Arc<PendingInputStore>,
        dispatched: Arc<Mutex<Vec<DeliveryToken>>>,
        drafts: Arc<Mutex<Vec<String>>>,
    }

    async fn open_store(journal: &Arc<MemoryJournal>, lifetime_id: &str) -> Opened {
        let dispatched = Arc::new(Mutex::new(Vec::new()));
        let drafts = Arc::new(Mutex::new(Vec::new()));
        let mut options = options(journal.clone(), lifetime_id);
        let sink = dispatched.clone();
        options.dispatch = Some(Arc::new(move |token: &DeliveryToken, _: &PendingInput| {
            if let Ok(mut sink) = sink.lock() {
                sink.push(token.clone());
            }
        }));
        let sink = drafts.clone();
        options.restore_to_draft = Some(Arc::new(move |text: &str| {
            if let Ok(mut sink) = sink.lock() {
                sink.push(text.to_owned());
            }
        }));
        let store = PendingInputStore::open(options)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        Opened {
            store,
            dispatched,
            drafts,
        }
    }

    fn handle(lifetime_id: &str, queued_id: &str) -> PendingInputHandle {
        PendingInputHandle {
            lifetime_id: lifetime_id.into(),
            queued_id: queued_id.into(),
        }
    }

    fn edit(
        effect_id: &str,
        handle: PendingInputHandle,
        expected_revision: u64,
        text: &str,
    ) -> PendingInputEvent {
        PendingInputEvent::Edit {
            target: MutationTarget {
                effect_id: effect_id.into(),
                handle,
                expected_revision,
            },
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    fn token() -> DeliveryToken {
        DeliveryToken {
            owner_id: "o".into(),
            writer_generation: 1,
            lifetime_id: "life-1".into(),
            attempt_id: "a-1".into(),
            client_message_id: "c-1".into(),
        }
    }

    fn projection(store: &PendingInputStore, id: &str) -> PendingInputProjection {
        match store.read(id) {
            PendingInputRead::Admitted { projection } => projection,
            PendingInputRead::NotAdmitted { .. } => panic!("{id} not admitted"),
        }
    }

    fn outcome(result: &OperationResult) -> Option<OperationOutcome> {
        result
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().map(|r| r.outcome))
    }

    #[tokio::test]
    async fn does_not_publish_or_dispatch_an_uncommitted_transition_t40() {
        let journal = Arc::new(MemoryJournal::default());
        let opened = open_store(&journal, "life-1").await;
        let _ = opened.store.submit(submit_request()).await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        opened.store.subscribe(Arc::new(
            move |projections: &[PendingInputProjection], _: u64| {
                if let Ok(mut sink) = sink.lock() {
                    sink.extend(projections.iter().map(|p| p.state));
                }
                Ok::<(), String>(())
            },
        ));
        journal.fail_when(|_| true);
        let result = opened
            .store
            .apply("c-1", PendingInputEvent::Reserve { token: token() })
            .await;
        assert_eq!(
            result.map_err(|e| e.to_string()),
            Err("disk full".to_owned())
        );
        assert_eq!(
            projection(&opened.store, "c-1").state,
            PendingInputState::Queued
        );
        assert!(seen.lock().map(|s| s.is_empty()).unwrap_or(false));
        assert!(
            opened
                .dispatched
                .lock()
                .map(|d| d.is_empty())
                .unwrap_or(false)
        );
    }

    #[tokio::test]
    async fn rebinds_lifetime_handles_without_losing_text_or_edited_history_t29() {
        let journal = Arc::new(MemoryJournal::default());
        let first = open_store(&journal, "life-1").await;
        let _ = first.store.submit(submit_request()).await;
        let _ = first
            .store
            .apply("c-1", edit("e1", handle("life-1", "q_1"), 1, "edited"))
            .await;
        let reopened = open_store(&journal, "life-2").await;
        let projection = projection(&reopened.store, "c-1");
        assert_eq!(
            (
                projection.state,
                projection.revision,
                projection.text.as_str()
            ),
            (PendingInputState::Edited, 1, "edited")
        );
    }

    #[tokio::test]
    async fn replays_the_complete_original_effect_answer_across_reopen_t43() {
        let journal = Arc::new(MemoryJournal::default());
        let first = open_store(&journal, "life-1").await;
        let _ = first.store.submit(submit_request()).await;
        let live = first
            .store
            .apply("c-1", edit("e1", handle("life-1", "q_1"), 1, "edited"))
            .await;
        let stale = first
            .store
            .apply("c-1", edit("e2", handle("life-1", "q_1"), 1, "stale"))
            .await;
        let reopened = open_store(&journal, "life-2").await;
        let replayed = |result: OperationResult| {
            result
                .ok()
                .flatten()
                .map(|result| PendingInputOperationResult {
                    replayed: Some(true),
                    ..result
                })
        };
        let again = reopened
            .store
            .apply("c-1", edit("e1", handle("life-1", "q_1"), 1, "edited"))
            .await;
        assert_eq!(again.ok().flatten(), replayed(live));
        let again_stale = reopened
            .store
            .apply("c-1", edit("e2", handle("life-1", "q_1"), 1, "stale"))
            .await;
        assert_eq!(again_stale.ok().flatten(), replayed(stale.clone()));
        let stale = stale
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("stale answer"));
        assert_eq!(
            (stale.outcome, stale.current_revision),
            (OperationOutcome::Conflict, Some(2))
        );
    }

    #[tokio::test]
    async fn answers_unknown_identity_without_inventing_text_or_delivery_t36() {
        let opened = open_store(&Arc::new(MemoryJournal::default()), "life-1").await;
        assert_eq!(
            opened.store.read("never-sent"),
            PendingInputRead::NotAdmitted {
                client_message_id: "never-sent".into()
            }
        );
    }

    #[tokio::test]
    async fn serializes_concurrent_edits_on_one_revision_into_one_winner_and_one_conflict() {
        let journal = Arc::new(MemoryJournal::default());
        let opened = open_store(&journal, "life-1").await;
        let _ = opened.store.submit(submit_request()).await;
        let (a, b) = tokio::join!(
            opened
                .store
                .apply("c-1", edit("a", handle("life-1", "q_1"), 1, "from A")),
            opened
                .store
                .apply("c-1", edit("b", handle("life-1", "q_1"), 1, "from B")),
        );
        assert_eq!(
            (outcome(&a), outcome(&b)),
            (
                Some(OperationOutcome::Edited),
                Some(OperationOutcome::Conflict)
            )
        );
        let projection = projection(&opened.store, "c-1");
        assert_eq!(
            (projection.text.as_str(), projection.revision),
            ("from A", 2)
        );
    }

    #[tokio::test]
    async fn reports_partial_stop_conflicts_without_dropping_concurrent_edits_t44() {
        let journal = Arc::new(MemoryJournal::default());
        let opened = open_store(&journal, "life-1").await;
        let _ = opened.store.submit(submit_request()).await;
        let second = SubmitRequest {
            client_message_id: "c-2".into(),
            fingerprint: "fp2".into(),
            text: "second".into(),
            ..submit_request()
        };
        let _ = opened.store.submit(second).await;
        let _ = opened
            .store
            .apply(
                "c-2",
                edit("e", handle("life-1", "q_2"), 1, "second, edited"),
            )
            .await;
        let receipt = opened
            .store
            .stop(
                &[
                    StopItem {
                        client_message_id: "c-1".into(),
                        handle: handle("life-1", "q_1"),
                        expected_revision: 1,
                    },
                    StopItem {
                        client_message_id: "c-2".into(),
                        handle: handle("life-1", "q_2"),
                        expected_revision: 1,
                    },
                ],
                false,
            )
            .await;
        assert_eq!(
            receipt,
            Ok(StopReceipt {
                removed: vec!["c-1".into()],
                conflicts: vec![StopConflict {
                    client_message_id: "c-2".into(),
                    current_revision: 2
                }],
                retained: Vec::new(),
                stale: Vec::new(),
            })
        );
        assert_eq!(
            opened.drafts.lock().map(|d| d.clone()).unwrap_or_default(),
            vec!["check the logs".to_owned()]
        );
        let projection = projection(&opened.store, "c-2");
        assert_eq!(
            (projection.state, projection.text.as_str()),
            (PendingInputState::Edited, "second, edited")
        );
    }

    #[tokio::test]
    async fn does_not_stop_an_input_through_a_handle_of_an_earlier_lifetime() {
        let journal = Arc::new(MemoryJournal::default());
        let first = open_store(&journal, "life-1").await;
        let _ = first.store.submit(submit_request()).await;
        let _ = first
            .store
            .apply("c-1", edit("e", handle("life-1", "q_1"), 1, "edited"))
            .await;
        let opened = open_store(&journal, "life-2").await;
        let item = |lifetime: &str| StopItem {
            client_message_id: "c-1".into(),
            handle: handle(lifetime, "q_1"),
            expected_revision: 1,
        };
        let stale = opened.store.stop(&[item("life-1")], false).await;
        assert_eq!(
            stale,
            Ok(StopReceipt {
                stale: vec!["c-1".into()],
                ..StopReceipt::default()
            })
        );
        let projection = projection(&opened.store, "c-1");
        assert_eq!(
            (projection.state, projection.text.as_str()),
            (PendingInputState::Edited, "edited")
        );
        let current = opened.store.stop(&[item("life-2")], false).await;
        assert_eq!(
            current.map(|receipt| receipt.removed),
            Ok(vec!["c-1".to_owned()])
        );
    }

    #[tokio::test]
    async fn does_not_stop_an_input_through_a_handle_naming_another_queue_slot() {
        let opened = open_store(&Arc::new(MemoryJournal::default()), "life-1").await;
        let _ = opened.store.submit(submit_request()).await;
        let receipt = opened
            .store
            .stop(
                &[StopItem {
                    client_message_id: "c-1".into(),
                    handle: handle("life-1", "q_9"),
                    expected_revision: 1,
                }],
                false,
            )
            .await;
        assert_eq!(
            receipt,
            Ok(StopReceipt {
                stale: vec!["c-1".into()],
                ..StopReceipt::default()
            })
        );
    }

    #[tokio::test]
    async fn still_dispatches_and_answers_when_a_listener_throws_and_reports_the_throw() {
        let journal = Arc::new(MemoryJournal::default());
        let dispatched = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let mut options = options(journal, "life-1");
        options.next_queued_id = Arc::new(|| "q_1".to_owned());
        let sink = dispatched.clone();
        options.dispatch = Some(Arc::new(move |token: &DeliveryToken, _: &PendingInput| {
            if let Ok(mut sink) = sink.lock() {
                sink.push(token.attempt_id.clone());
            }
        }));
        let sink = errors.clone();
        options.on_listener_error = Some(Arc::new(move |error: String| {
            if let Ok(mut sink) = sink.lock() {
                sink.push(error);
            }
        }));
        let store = PendingInputStore::open(options)
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let _ = store.submit(submit_request()).await;
        store.subscribe(Arc::new(|_: &[PendingInputProjection], _: u64| {
            Err::<(), String>("renderer crashed".to_owned())
        }));
        let _ = store
            .apply("c-1", PendingInputEvent::Reserve { token: token() })
            .await;
        assert_eq!(
            dispatched.lock().map(|d| d.clone()).unwrap_or_default(),
            vec!["a-1".to_owned()]
        );
        assert_eq!(
            errors.lock().map(|e| e.clone()).unwrap_or_default(),
            vec!["renderer crashed".to_owned()]
        );
    }

    #[tokio::test]
    async fn answers_a_change_to_an_unknown_input_with_that_inputs_id() {
        let opened = open_store(&Arc::new(MemoryJournal::default()), "life-1").await;
        let remove = PendingInputEvent::Remove {
            target: MutationTarget {
                effect_id: "e".into(),
                handle: handle("life-1", "q_9"),
                expected_revision: 1,
            },
        };
        let answer = opened
            .store
            .apply("c-9", remove)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("answer"));
        assert_eq!(
            (answer.outcome, answer.client_message_id.as_str()),
            (OperationOutcome::NotFound, "c-9")
        );
    }

    #[tokio::test]
    async fn holds_every_other_command_until_a_transaction_finishes() {
        let journal = Arc::new(MemoryJournal::default());
        let opened = open_store(&journal, "life-1").await;
        let _ = opened.store.submit(submit_request()).await;
        let (finish, held) = tokio::sync::oneshot::channel::<()>();
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let push = |order: &Arc<Mutex<Vec<&'static str>>>, step| {
            if let Ok(mut order) = order.lock() {
                order.push(step);
            }
        };
        let tx_order = order.clone();
        let transaction = opened.store.transaction(move |_tx| async move {
            push(&tx_order, "transaction start");
            let _ = held.await;
            push(&tx_order, "transaction end");
        });
        let edit_order = order.clone();
        let store = opened.store.clone();
        let edit_future = async move {
            let _ = store
                .apply("c-1", edit("e-1", handle("life-1", "q_1"), 1, "edited"))
                .await;
            push(&edit_order, "edit");
        };
        let journal_probe = journal.clone();
        let release = async move {
            // join! polls the transaction, then the edit, then this: an ungated edit (its journal is always
            // ready) would have journaled during that same poll.
            let journaled_while_held = journal_probe.records().len();
            let _ = finish.send(());
            journaled_while_held
        };
        let (transaction, (), journaled_while_held) =
            tokio::join!(transaction, edit_future, release);
        assert!(transaction.is_ok());
        assert_eq!(journaled_while_held, 1);
        assert_eq!(
            order.lock().map(|o| o.clone()).unwrap_or_default(),
            vec!["transaction start", "transaction end", "edit"]
        );
    }

    #[tokio::test]
    async fn dispatches_only_after_the_reservation_is_journaled() {
        let journal = Arc::new(MemoryJournal::default());
        let opened = open_store(&journal, "life-1").await;
        let _ = opened.store.submit(submit_request()).await;
        let _ = opened
            .store
            .apply("c-1", PendingInputEvent::Reserve { token: token() })
            .await;
        assert_eq!(
            opened
                .dispatched
                .lock()
                .map(|d| d.clone())
                .unwrap_or_default(),
            vec![token()]
        );
        assert_eq!(
            journal.records().last().map(|record| record.input.phase),
            Some(PendingInputPhase::Sending)
        );
    }

    #[tokio::test]
    async fn refuses_every_command_after_an_uncertain_append_until_reopen() {
        let journal = Arc::new(MemoryJournal::default());
        let opened = open_store(&journal, "life-1").await;
        struct Uncertain;
        #[async_trait::async_trait]
        impl PendingInputJournal for Uncertain {
            async fn append(
                &self,
                record: &PendingInputJournalRecord,
            ) -> Result<(), PendingInputError> {
                Err(PendingInputError::AppendUncertain {
                    cursor: record.cursor,
                    message: "EIO".into(),
                })
            }
            async fn read(&self) -> Result<Vec<PendingInputJournalRecord>, PendingInputError> {
                Ok(Vec::new())
            }
        }
        drop(opened);
        let store = PendingInputStore::open(options(Arc::new(Uncertain), "life-1"))
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let first = store.submit(submit_request()).await;
        assert_eq!(
            first.err().map(|e| e.code()),
            Some("pending_input_append_uncertain")
        );
        let second = store.submit(submit_request()).await;
        assert_eq!(
            second.err().map(|e| e.code()),
            Some("pending_input_store_unavailable")
        );
    }
}
