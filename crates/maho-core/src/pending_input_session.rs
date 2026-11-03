//! Port of senpi `packages/coding-agent/src/core/pending-input-session.ts`.
//!
//! The pending inputs of one AgentSession (senpi#3549): the store, opened on the first identified input,
//! and the agent queue port over it. Input without a client id never touches the store.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::{BoxFuture, FutureExt, Shared};
use maho_agent::agent::{QueuedDrain, QueuedGiveBack, QueuedInputPort};
use maho_agent::types::{AgentMessage, PendingQueue};
use maho_ai::types::ImageContent;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::pending_input_journal::{JournalSession, SessionPendingInputJournal};
use crate::pending_input_projection::{PendingInputProjection, PendingInputState};
use crate::pending_input_session_port::{
    ClientMessageIdOf, SessionQueuedInputPortOptions, create_session_queued_input_port,
};
use crate::pending_input_store::{
    PENDING_INPUT_TRANSITION, PendingInputChange, PendingInputError, PendingInputJournal,
    PendingInputRead, PendingInputStore, PendingInputStoreOptions, SubmitRequest,
};
use crate::pending_input_types::{
    CancelCause, MutationTarget, OperationOutcome, PendingInputEvent, PendingInputMode,
    PendingInputOperationResult, PendingInputPhase,
};

#[derive(Debug, Clone, PartialEq)]
pub struct IdentifiedInput {
    pub client_message_id: String,
    pub text: String,
    pub images: Vec<ImageContent>,
    pub mode: PendingInputMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInputUpdate {
    pub lifetime_id: String,
    pub cursor: u64,
    pub changes: Vec<PendingInputProjection>,
}

#[derive(Clone)]
pub struct SessionPendingInputsOptions {
    pub session_manager: Arc<dyn JournalSession>,
    /// The session's own queue-id counter, so a store handle and the session's queued record share one id.
    pub next_queued_id: Arc<dyn Fn() -> String + Send + Sync>,
    /// Receives every committed change, in cursor order.
    pub on_change: Option<Arc<dyn Fn(PendingInputUpdate) + Send + Sync>>,
    /// Defaults to the session JSONL; injectable so tests can fail an append.
    pub journal: Option<Arc<dyn PendingInputJournal>>,
    /// Reads the client id a queued agent message carries (upstream reads `message.clientMessageId`).
    pub client_message_id_of: ClientMessageIdOf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingInputOutcome {
    Admitted {
        client_message_id: String,
        projection: Box<PendingInputProjection>,
    },
    NotAdmitted {
        client_message_id: String,
    },
}

/// Every input that still needs an outcome, plus an explicit answer for each id the caller already knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInputSnapshot {
    pub lifetime_id: String,
    pub through_cursor: u64,
    pub inputs: Vec<PendingInputProjection>,
    pub outcomes: Vec<PendingInputOutcome>,
}

/// The legacy answers of `dequeue_message` / `edit_queued_message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedInputChangeOutcome {
    Removed,
    Edited,
    AlreadyDelivered,
    NotFound,
    Conflict,
}

impl QueuedInputChangeOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::Edited => "edited",
            Self::AlreadyDelivered => "already_delivered",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInputChangeRequest {
    pub client_message_id: String,
    /// `None` removes the input; a string replaces its text.
    pub text: Option<String>,
    pub expected_revision: Option<u64>,
    pub effect_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreHandle {
    Queued { queued_id: String },
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPendingInputError {
    /// A reused client id with another payload.
    ClientMessageIdConflict {
        client_message_id: String,
    },
    /// The store changed an input that the native queue no longer held.
    Divergence {
        client_message_id: String,
        recorded: String,
        native: String,
    },
    Store(PendingInputError),
}

impl SessionPendingInputError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ClientMessageIdConflict { .. } => "client_message_id_conflict",
            Self::Divergence { .. } => "pending_input_divergence",
            Self::Store(error) => error.code(),
        }
    }
}

impl fmt::Display for SessionPendingInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientMessageIdConflict { client_message_id } => {
                write!(
                    f,
                    "Client message id {client_message_id} was already used for a different message."
                )
            }
            Self::Divergence {
                client_message_id,
                recorded,
                native,
            } => {
                write!(
                    f,
                    "Queued input {client_message_id} was {recorded} in its record but {native} in the queue."
                )
            }
            Self::Store(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for SessionPendingInputError {}

impl From<PendingInputError> for SessionPendingInputError {
    fn from(error: PendingInputError) -> Self {
        Self::Store(error)
    }
}

type StoreOpening = Shared<BoxFuture<'static, Result<Arc<PendingInputStore>, PendingInputError>>>;

/// The pending inputs of one AgentSession (senpi#3549).
pub struct SessionPendingInputs {
    pub lifetime_id: String,
    options: SessionPendingInputsOptions,
    store_opening: Mutex<Option<StoreOpening>>,
    port: Mutex<Option<Arc<dyn QueuedInputPort>>>,
    attempts: Arc<AtomicU64>,
    /// Bumped synchronously by every clear, so a change can tell a clear it raced from a real divergence.
    clears: AtomicU64,
}

impl SessionPendingInputs {
    /// A session file that already holds pending inputs is recovered now: the agent's first drain must see
    /// them. The eager open is spawned when a tokio runtime is present; every later use awaits this same
    /// opening and surfaces its failure.
    pub fn new(options: SessionPendingInputsOptions) -> Arc<Self> {
        let inputs = Arc::new(Self {
            lifetime_id: uuid::Uuid::new_v4().to_string(),
            options,
            store_opening: Mutex::new(None),
            port: Mutex::new(None),
            attempts: Arc::new(AtomicU64::new(0)),
            clears: AtomicU64::new(0),
        });
        let journaled = inputs
            .options
            .session_manager
            .entries()
            .iter()
            .any(|entry| {
                entry.get("type").and_then(Value::as_str) == Some("custom")
                    && entry.get("customType").and_then(Value::as_str)
                        == Some(PENDING_INPUT_TRANSITION)
            });
        if journaled {
            let opening = inputs.store();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = opening.await;
                });
            }
        }
        inputs
    }

    /// How a restored input re-enters the native queue: under the store's rebound queue id while it is still
    /// queued; not at all once the store retired it; `None` when the store never held it.
    pub async fn restore_handle(
        &self,
        client_message_id: &str,
    ) -> Result<Option<RestoreHandle>, PendingInputError> {
        let Some(opening) = self.opening() else {
            return Ok(None);
        };
        let Some(input) = opening.await?.current(client_message_id) else {
            return Ok(None);
        };
        Ok(Some(if input.phase == PendingInputPhase::Queued {
            RestoreHandle::Queued {
                queued_id: input.handle.queued_id,
            }
        } else {
            RestoreHandle::Retired
        }))
    }

    /// Admits an identified input and, only when it is newly admitted, puts its native copy in the agent queue
    /// inside the same gated step. A repeated submit is answered from the record and enqueues nothing.
    pub async fn admit(
        &self,
        input: IdentifiedInput,
        enqueue: impl FnOnce(&str) + Send,
    ) -> Result<Option<PendingInputOperationResult>, SessionPendingInputError> {
        let store = self.store().await?;
        store
            .transaction(move |tx| async move {
                let request = SubmitRequest {
                    client_message_id: input.client_message_id.clone(),
                    fingerprint: admission_fingerprint(&input),
                    text: input.text.clone(),
                    attachments: input.images.iter().map(attachment_fingerprint).collect(),
                    requested_mode: input.mode,
                };
                let result = tx.submit(request).await?;
                let outcome = result.as_ref().map(|result| result.outcome);
                if outcome == Some(OperationOutcome::ClientMessageIdConflict) {
                    return Err(SessionPendingInputError::ClientMessageIdConflict {
                        client_message_id: input.client_message_id,
                    });
                }
                if outcome == Some(OperationOutcome::Admitted)
                    && let Some(admitted) = tx.current(&input.client_message_id)
                {
                    enqueue(&admitted.handle.queued_id);
                }
                Ok::<_, SessionPendingInputError>(result)
            })
            .await?
    }

    /// Edits or removes an identified input: the store transition is journaled first, and only when it answers
    /// `edited`/`removed` does `apply_native` change the native queue, inside the same gated step. Returns
    /// `None` when the store never admitted this input.
    pub async fn change(
        &self,
        request: QueuedInputChangeRequest,
        apply_native: impl FnOnce() -> QueuedInputChangeOutcome + Send,
    ) -> Result<Option<QueuedInputChangeOutcome>, SessionPendingInputError> {
        let Some(opening) = self.opening() else {
            return Ok(None);
        };
        let clears_at_start = self.clears.load(Ordering::SeqCst);
        let store = opening.await?;
        store
            .transaction(move |tx| async move {
                let Some(current) = tx.current(&request.client_message_id) else {
                    return Ok::<_, SessionPendingInputError>(None);
                };
                let target = MutationTarget {
                    effect_id: request
                        .effect_id
                        .clone()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    handle: current.handle.clone(),
                    expected_revision: request.expected_revision.unwrap_or(current.revision),
                };
                let event = match request.text {
                    None => PendingInputEvent::Remove { target },
                    Some(text) => PendingInputEvent::Edit {
                        target,
                        text,
                        attachments: current.attachments.clone(),
                    },
                };
                let result = tx.apply(&request.client_message_id, event).await?;
                let outcome = legacy_outcome(result.map(|result| result.outcome));
                if !matches!(
                    outcome,
                    QueuedInputChangeOutcome::Edited | QueuedInputChangeOutcome::Removed
                ) {
                    return Ok(Some(outcome));
                }
                let native = apply_native();
                // A synchronous clear can empty the queue while the change is being journaled; its own record of
                // the clear is queued behind this step and retires the input, so the change simply found nothing.
                if native == QueuedInputChangeOutcome::NotFound
                    && self.clears.load(Ordering::SeqCst) != clears_at_start
                {
                    return Ok(Some(QueuedInputChangeOutcome::NotFound));
                }
                if native != outcome {
                    return Err(SessionPendingInputError::Divergence {
                        client_message_id: request.client_message_id,
                        recorded: outcome.as_str().to_owned(),
                        native: native.as_str().to_owned(),
                    });
                }
                Ok(Some(outcome))
            })
            .await?
    }

    /// Bumps the clear counter synchronously; call it when the native queue is emptied, before awaiting
    /// [`Self::record_cleared`].
    pub fn note_cleared(&self) {
        self.clears.fetch_add(1, Ordering::SeqCst);
    }

    /// Records an explicit clear (Stop, `clear_queue`, unload with clearQueue) of identified inputs that just
    /// left the native queue: each still-queued one is removed, so a reopen never queues it again.
    pub async fn record_cleared(
        &self,
        client_message_ids: Vec<String>,
    ) -> Result<(), PendingInputError> {
        self.note_cleared();
        let Some(opening) = self.opening() else {
            return Ok(());
        };
        if client_message_ids.is_empty() {
            return Ok(());
        }
        let store = opening.await?;
        // One record for the whole clear: it lands or not as a unit, never split around a later entry.
        store
            .transaction(move |tx| async move {
                let queued: Vec<PendingInputChange> = client_message_ids
                    .into_iter()
                    .filter(|id| {
                        tx.current(id).map(|input| input.phase) == Some(PendingInputPhase::Queued)
                    })
                    .map(|client_message_id| PendingInputChange {
                        client_message_id,
                        event: PendingInputEvent::Cancel {
                            cause: CancelCause::Stop,
                            consumer_fenced: true,
                        },
                    })
                    .collect();
                if !queued.is_empty() {
                    tx.apply_all(queued).await?;
                }
                Ok::<(), PendingInputError>(())
            })
            .await?
    }

    /// Pending inputs (queued, sending, failed) and the outcome of each known id, consistent through
    /// `through_cursor`. Absence from `inputs` is never an outcome.
    pub async fn snapshot(
        &self,
        known_client_message_ids: &[String],
    ) -> Result<PendingInputSnapshot, PendingInputError> {
        let store = match self.opening() {
            Some(opening) => Some(opening.await?),
            None => None,
        };
        let inputs = store
            .as_ref()
            .map(|store| store.snapshot())
            .unwrap_or_default()
            .into_iter()
            .filter(|input| {
                !matches!(
                    input.state,
                    PendingInputState::Delivered | PendingInputState::Removed
                )
            })
            .collect();
        let outcomes = known_client_message_ids
            .iter()
            .map(|client_message_id| {
                match store.as_ref().map(|store| store.read(client_message_id)) {
                    Some(PendingInputRead::Admitted { projection }) => {
                        PendingInputOutcome::Admitted {
                            client_message_id: client_message_id.clone(),
                            projection,
                        }
                    }
                    _ => PendingInputOutcome::NotAdmitted {
                        client_message_id: client_message_id.clone(),
                    },
                }
            })
            .collect();
        Ok(PendingInputSnapshot {
            lifetime_id: self.lifetime_id.clone(),
            through_cursor: store.map(|store| store.through_cursor()).unwrap_or(0),
            inputs,
            outcomes,
        })
    }

    /// The agent's queue port. Before the store opens nothing identified is queued, so it drains natively.
    pub fn queued_input_port(self: &Arc<Self>) -> Arc<dyn QueuedInputPort> {
        Arc::new(SessionPort {
            inputs: self.clone(),
        })
    }

    fn opening(&self) -> Option<StoreOpening> {
        self.opening_slot().clone()
    }

    fn opening_slot(&self) -> MutexGuard<'_, Option<StoreOpening>> {
        self.store_opening
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn store(&self) -> StoreOpening {
        let mut slot = self.opening_slot();
        if let Some(opening) = slot.as_ref() {
            return opening.clone();
        }
        let journal: Arc<dyn PendingInputJournal> = match &self.options.journal {
            Some(journal) => journal.clone(),
            None => Arc::new(SessionPendingInputJournal::new(
                self.options.session_manager.clone(),
            )),
        };
        let options = PendingInputStoreOptions {
            journal,
            lifetime_id: self.lifetime_id.clone(),
            next_queued_id: self.options.next_queued_id.clone(),
            dispatch: None,
            restore_to_draft: None,
            // The agent port commits consumption before any provider sees an input, so an attempt left `sending`
            // by a crash was never sent and returns to the queue under its id (T28).
            uncommitted_attempts_are_unsent: true,
            on_listener_error: None,
        };
        let on_change = self.options.on_change.clone();
        let lifetime_id = self.lifetime_id.clone();
        let opening = async move {
            let store = PendingInputStore::open(options).await?;
            if let Some(on_change) = on_change {
                store.subscribe(Arc::new(
                    move |projections: &[PendingInputProjection], cursor: u64| {
                        on_change(PendingInputUpdate {
                            lifetime_id: lifetime_id.clone(),
                            cursor,
                            changes: projections.to_vec(),
                        });
                        Ok::<(), String>(())
                    },
                ));
            }
            Ok::<_, PendingInputError>(store)
        }
        .boxed()
        .shared();
        *slot = Some(opening.clone());
        opening
    }

    async fn open_port(&self) -> Result<Option<Arc<dyn QueuedInputPort>>, PendingInputError> {
        let Some(opening) = self.opening() else {
            return Ok(None);
        };
        let store = opening.await?;
        let mut port = self
            .port
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if port.is_none() {
            let attempts = self.attempts.clone();
            let lifetime_id = self.lifetime_id.clone();
            *port = Some(create_session_queued_input_port(
                SessionQueuedInputPortOptions {
                    store,
                    owner_id: format!("{}:{}", std::process::id(), self.lifetime_id),
                    writer_generation: 1,
                    next_attempt_id: Arc::new(move || {
                        format!(
                            "{lifetime_id}:{}",
                            attempts.fetch_add(1, Ordering::SeqCst) + 1
                        )
                    }),
                    client_message_id_of: self.options.client_message_id_of.clone(),
                },
            ));
        }
        Ok(port.clone())
    }
}

struct SessionPort {
    inputs: Arc<SessionPendingInputs>,
}

impl QueuedInputPort for SessionPort {
    fn claim(
        &self,
        queue: PendingQueue,
        drain: QueuedDrain,
    ) -> BoxFuture<'static, Vec<AgentMessage>> {
        let inputs = self.inputs.clone();
        Box::pin(async move {
            match inputs.open_port().await {
                Ok(Some(port)) => port.claim(queue, drain).await,
                Ok(None) => drain(),
                Err(error) => {
                    // The store is unavailable: nothing identified can be reserved, so nothing is delivered.
                    eprintln!("pending input claim failed: {error}");
                    Vec::new()
                }
            }
        })
    }

    fn commit_consumption(
        &self,
        queue: PendingQueue,
        messages: Vec<AgentMessage>,
    ) -> BoxFuture<'static, Result<(), String>> {
        let inputs = self.inputs.clone();
        Box::pin(async move {
            match inputs
                .open_port()
                .await
                .map_err(|error| error.to_string())?
            {
                Some(port) => port.commit_consumption(queue, messages).await,
                None => Ok(()),
            }
        })
    }

    fn release(
        &self,
        queue: PendingQueue,
        messages: Vec<AgentMessage>,
        give_back: QueuedGiveBack,
    ) -> BoxFuture<'static, ()> {
        let inputs = self.inputs.clone();
        Box::pin(async move {
            match inputs.open_port().await {
                Ok(Some(port)) => port.release(queue, messages, give_back).await,
                Ok(None) => {
                    (give_back.restore)();
                }
                Err(error) => eprintln!("pending input release failed: {error}"),
            }
        })
    }
}

fn admission_fingerprint(input: &IdentifiedInput) -> String {
    let images: Vec<String> = input.images.iter().map(attachment_fingerprint).collect();
    let mode = serde_json::to_value(input.mode).unwrap_or(Value::Null);
    sha256(&serde_json::json!([mode, input.text, images]).to_string())
}

fn attachment_fingerprint(image: &ImageContent) -> String {
    format!("{}:{}", image.mime_type, sha256(&image.data))
}

fn sha256(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

/// `sending` keeps the legacy spelling `already_delivered` (the handoff began); the snapshot says `sending`.
fn legacy_outcome(outcome: Option<OperationOutcome>) -> QueuedInputChangeOutcome {
    match outcome {
        Some(OperationOutcome::Edited) => QueuedInputChangeOutcome::Edited,
        Some(OperationOutcome::Removed) => QueuedInputChangeOutcome::Removed,
        Some(OperationOutcome::Conflict) => QueuedInputChangeOutcome::Conflict,
        Some(OperationOutcome::AlreadyDelivered | OperationOutcome::Sending) => {
            QueuedInputChangeOutcome::AlreadyDelivered
        }
        _ => QueuedInputChangeOutcome::NotFound,
    }
}

#[cfg(test)]
mod tests {
    //! senpi `test/suite/pending-input-session.test.ts` (the `SessionPendingInputs` cases; the AgentSession
    //! harness cases need the session wiring, which lives in the drift-owned `agent_session.rs`).
    use super::*;
    use crate::pending_input_journal::JournalSession;
    use crate::pending_input_session_port::test_support::{id_by_text, queued_user};
    use crate::pending_input_store::test_support::MemoryJournal;

    /// An in-memory session (upstream `SessionManager.inMemory()`).
    #[derive(Default)]
    struct MemorySession {
        entries: Mutex<Vec<Value>>,
    }

    #[async_trait::async_trait]
    impl JournalSession for MemorySession {
        fn append_custom_entry(&self, custom_type: &str, data: Value) {
            if let Ok(mut entries) = self.entries.lock() {
                entries.push(
                    serde_json::json!({"type": "custom", "customType": custom_type, "data": data}),
                );
            }
        }
        async fn persist_header_now(&self) -> Result<(), String> {
            Ok(())
        }
        fn entries(&self) -> Vec<Value> {
            self.entries
                .lock()
                .map(|entries| entries.clone())
                .unwrap_or_default()
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn is_persisted(&self) -> bool {
            false
        }
        fn is_transcript_flushed(&self) -> bool {
            false
        }
    }

    fn inputs_over(
        session: Arc<MemorySession>,
        journal: Option<Arc<dyn PendingInputJournal>>,
    ) -> Arc<SessionPendingInputs> {
        let next = Arc::new(AtomicU64::new(0));
        SessionPendingInputs::new(SessionPendingInputsOptions {
            session_manager: session,
            next_queued_id: Arc::new(move || {
                format!("q_{}", next.fetch_add(1, Ordering::SeqCst) + 1)
            }),
            on_change: None,
            journal,
            client_message_id_of: id_by_text(&[
                ("queued", "queued"),
                ("cut", "cut"),
                ("done", "done"),
            ]),
        })
    }

    fn identified(id: &str, text: &str) -> IdentifiedInput {
        IdentifiedInput {
            client_message_id: id.into(),
            text: text.into(),
            images: Vec::new(),
            mode: PendingInputMode::FollowUp,
        }
    }

    fn edit_request(text: &str) -> QueuedInputChangeRequest {
        QueuedInputChangeRequest {
            client_message_id: "c-1".into(),
            text: Some(text.into()),
            expected_revision: None,
            effect_id: None,
        }
    }

    #[tokio::test]
    async fn leaves_the_native_queue_alone_when_the_record_of_the_change_cannot_be_saved() {
        let journal = Arc::new(MemoryJournal::default());
        let inputs = inputs_over(Arc::default(), Some(journal.clone()));
        assert!(
            inputs
                .admit(identified("c-1", "draft"), |_| {})
                .await
                .is_ok()
        );
        journal.fail_when(|_| true);
        let native_changed = AtomicU64::new(0);
        let result = inputs
            .change(edit_request("final"), || {
                native_changed.fetch_add(1, Ordering::SeqCst);
                QueuedInputChangeOutcome::Edited
            })
            .await;
        assert_eq!(
            result.map_err(|error| error.to_string()),
            Err("disk full".to_owned())
        );
        assert_eq!(native_changed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn answers_not_found_when_a_clear_emptied_the_queue_while_the_edit_was_being_saved() {
        let journal = Arc::new(MemoryJournal::default());
        let inputs = inputs_over(Arc::default(), Some(journal.clone()));
        assert!(
            inputs
                .admit(identified("c-1", "draft"), |_| {})
                .await
                .is_ok()
        );
        let native_queued = Arc::new(Mutex::new(true));
        // The clear lands while the edit is journaled: the native copy is gone and the clear is counted.
        let (clearing, flag) = (inputs.clone(), native_queued.clone());
        journal.fail_when(move |_| {
            if let Ok(mut queued) = flag.lock() {
                *queued = false;
            }
            clearing.note_cleared();
            false
        });
        let outcome = inputs
            .change(edit_request("final"), || {
                if native_queued.lock().map(|queued| *queued).unwrap_or(false) {
                    QueuedInputChangeOutcome::Edited
                } else {
                    QueuedInputChangeOutcome::NotFound
                }
            })
            .await;
        assert_eq!(outcome, Ok(Some(QueuedInputChangeOutcome::NotFound)));
        journal.fail_when(|_| false);
        assert!(inputs.record_cleared(vec!["c-1".into()]).await.is_ok());
        let last = journal
            .records()
            .last()
            .map(|record| (record.input.client_message_id.clone(), record.input.phase));
        assert_eq!(last, Some(("c-1".to_owned(), PendingInputPhase::Removed)));
    }

    #[tokio::test]
    async fn still_refuses_a_missing_native_copy_that_no_clear_explains() {
        let inputs = inputs_over(Arc::default(), Some(Arc::new(MemoryJournal::default())));
        assert!(
            inputs
                .admit(identified("c-1", "draft"), |_| {})
                .await
                .is_ok()
        );
        let result = inputs
            .change(edit_request("final"), || QueuedInputChangeOutcome::NotFound)
            .await;
        assert_eq!(
            result.err().map(|error| error.code()),
            Some("pending_input_divergence")
        );
    }

    #[tokio::test]
    async fn queues_a_repeated_identical_submit_once_and_refuses_a_reused_id() {
        let inputs = inputs_over(Arc::default(), Some(Arc::new(MemoryJournal::default())));
        let enqueued = Mutex::new(Vec::<String>::new());
        let enqueue = |id: &str| {
            if let Ok(mut enqueued) = enqueued.lock() {
                enqueued.push(id.to_owned());
            }
        };
        let first = inputs.admit(identified("c-1", "draft"), enqueue).await;
        let again = inputs.admit(identified("c-1", "draft"), enqueue).await;
        assert_eq!(
            first.ok().flatten().map(|r| r.outcome),
            Some(OperationOutcome::Admitted)
        );
        assert_eq!(
            again.ok().flatten().map(|r| r.outcome),
            Some(OperationOutcome::Replayed)
        );
        assert_eq!(
            enqueued.lock().map(|e| e.clone()).unwrap_or_default(),
            vec!["q_1".to_owned()]
        );
        let conflict = inputs.admit(identified("c-1", "other"), enqueue).await;
        assert_eq!(
            conflict.err().map(|error| error.code()),
            Some("client_message_id_conflict")
        );
    }

    #[tokio::test]
    async fn reports_a_failed_eager_open_to_the_next_caller() {
        let journal = Arc::new(MemoryJournal::default());
        let session = Arc::new(MemorySession::default());
        let before = inputs_over(session.clone(), Some(journal.clone()));
        assert!(before.admit(identified("c-1", "q"), |_| {}).await.is_ok());
        let record = journal
            .records()
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("record"));
        session.append_custom_entry(
            PENDING_INPUT_TRANSITION,
            serde_json::to_value(&record).unwrap_or_default(),
        );
        journal.fail_when(|_| true);
        let after = inputs_over(session, Some(journal.clone()));
        let snapshot = after.snapshot(&[]).await;
        assert_eq!(
            snapshot.err().map(|error| error.to_string()),
            Some("disk full".to_owned())
        );
    }

    #[tokio::test]
    async fn recovers_a_queued_input_and_an_attempt_cut_off_before_delivery_and_retires_a_delivered_one()
     {
        let journal = Arc::new(MemoryJournal::default());
        let before = inputs_over(Arc::default(), Some(journal.clone()));
        for id in ["queued", "cut", "done"] {
            assert!(before.admit(identified(id, id), |_| {}).await.is_ok());
        }
        let port = before.queued_input_port();
        let (cut, done) = (queued_user("cut"), queued_user("done"));
        let drained = vec![cut, done.clone()];
        let claimed = port
            .claim(PendingQueue::FollowUp, Arc::new(move || drained.clone()))
            .await;
        assert_eq!(claimed.len(), 2);
        assert_eq!(
            port.commit_consumption(PendingQueue::FollowUp, vec![done])
                .await,
            Ok(())
        );

        let reopened_session = Arc::new(MemorySession::default());
        for record in journal.records() {
            reopened_session.append_custom_entry(
                PENDING_INPUT_TRANSITION,
                serde_json::to_value(&record).unwrap_or_default(),
            );
        }
        let after = inputs_over(reopened_session, Some(journal.clone()));
        let queued_handle = |handle: Result<Option<RestoreHandle>, PendingInputError>| matches!(handle, Ok(Some(RestoreHandle::Queued { queued_id })) if queued_id.starts_with("q_"));
        assert!(queued_handle(after.restore_handle("queued").await));
        assert!(queued_handle(after.restore_handle("cut").await));
        assert_eq!(
            after.restore_handle("done").await,
            Ok(Some(RestoreHandle::Retired))
        );
        assert_eq!(after.restore_handle("never-admitted").await, Ok(None));
    }
}
