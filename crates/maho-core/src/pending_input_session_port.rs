//! Port of senpi `packages/coding-agent/src/core/pending-input-session-port.ts`.
//!
//! The session's side of the agent queues (senpi#3549). Every drain reserves the identified inputs it hands
//! to a turn, every consumption commits them, and every give-back releases them, each under the store's gate
//! together with the native buffer change. Input the store never admitted passes through.
//!
//! Upstream reads `clientMessageId` off the user message object; the Rust `UserMessage` carries no such
//! field, so the owner passes the identity reader (`client_message_id_of`). Upstream keys attempts by message
//! object identity; Rust messages are values, so attempts are matched by equality.

use std::sync::{Arc, Mutex, MutexGuard};

use maho_agent::agent::{QueuedDrain, QueuedGiveBack, QueuedInputPort};
use maho_agent::types::{AgentMessage, PendingQueue};
use maho_ai::types::BoxFuture;

use crate::pending_input_store::{
    PendingInputChange, PendingInputError, PendingInputStore, PendingInputTransaction,
};
use crate::pending_input_types::{CancelCause, DeliveryToken, PendingInputEvent};

pub type ClientMessageIdOf = Arc<dyn Fn(&AgentMessage) -> Option<String> + Send + Sync>;

#[derive(Clone)]
pub struct SessionQueuedInputPortOptions {
    pub store: Arc<PendingInputStore>,
    pub owner_id: String,
    pub writer_generation: u64,
    pub next_attempt_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub client_message_id_of: ClientMessageIdOf,
}

pub fn create_session_queued_input_port(
    options: SessionQueuedInputPortOptions,
) -> Arc<dyn QueuedInputPort> {
    Arc::new(SessionQueuedInputPort {
        options,
        attempts: Arc::new(Mutex::new(Vec::new())),
    })
}

type Attempts = Arc<Mutex<Vec<(AgentMessage, DeliveryToken)>>>;

struct SessionQueuedInputPort {
    options: SessionQueuedInputPortOptions,
    attempts: Attempts,
}

fn lock(attempts: &Attempts) -> MutexGuard<'_, Vec<(AgentMessage, DeliveryToken)>> {
    attempts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn attempt_of(attempts: &Attempts, message: &AgentMessage) -> Option<DeliveryToken> {
    lock(attempts)
        .iter()
        .find(|(held, _)| held == message)
        .map(|(_, token)| token.clone())
}

fn forget(attempts: &Attempts, message: &AgentMessage) {
    let mut held = lock(attempts);
    if let Some(index) = held.iter().position(|(candidate, _)| candidate == message) {
        held.remove(index);
    }
}

async fn reserve(
    options: &SessionQueuedInputPortOptions,
    attempts: &Attempts,
    tx: &PendingInputTransaction,
    message: &AgentMessage,
) -> Result<bool, PendingInputError> {
    let Some(client_message_id) = (options.client_message_id_of)(message) else {
        return Ok(true);
    };
    let Some(input) = tx.current(&client_message_id) else {
        return Ok(true);
    };
    let token = DeliveryToken {
        owner_id: options.owner_id.clone(),
        writer_generation: options.writer_generation,
        lifetime_id: input.handle.lifetime_id,
        attempt_id: (options.next_attempt_id)(),
        client_message_id: client_message_id.clone(),
    };
    tx.apply(
        &client_message_id,
        PendingInputEvent::Reserve {
            token: token.clone(),
        },
    )
    .await?;
    let reserved = tx
        .current(&client_message_id)
        .and_then(|input| input.attempt)
        .map(|attempt| attempt.attempt_id);
    if reserved.as_deref() != Some(token.attempt_id.as_str()) {
        return Ok(false);
    }
    lock(attempts).push((message.clone(), token));
    Ok(true)
}

/// One journal record for the whole batch: a failed write leaves every input as it was, never half delivered.
async fn settle(
    attempts: &Attempts,
    tx: &PendingInputTransaction,
    messages: &[AgentMessage],
    consumed: bool,
) -> Result<(), PendingInputError> {
    let tokens: Vec<DeliveryToken> = messages
        .iter()
        .filter_map(|message| attempt_of(attempts, message))
        .collect();
    if tokens.is_empty() {
        return Ok(());
    }
    let changes = tokens
        .into_iter()
        .map(|token| PendingInputChange {
            client_message_id: token.client_message_id.clone(),
            event: if consumed {
                PendingInputEvent::Consumed { token }
            } else {
                PendingInputEvent::Release { token }
            },
        })
        .collect();
    tx.apply_all(changes).await?;
    for message in messages {
        forget(attempts, message);
    }
    Ok(())
}

/// Consumption was never committed, so no consumer can hold these: removing them is fenced (senpi#3549).
fn cancel_of(client_message_id: String) -> PendingInputChange {
    PendingInputChange {
        client_message_id,
        event: PendingInputEvent::Cancel {
            cause: CancelCause::Stop,
            consumer_fenced: true,
        },
    }
}

async fn cancel_claims(
    attempts: &Attempts,
    tx: &PendingInputTransaction,
    messages: &[AgentMessage],
) -> Result<(), PendingInputError> {
    let ids: Vec<String> = messages
        .iter()
        .filter_map(|message| {
            let token = attempt_of(attempts, message);
            forget(attempts, message);
            token.map(|token| token.client_message_id)
        })
        .collect();
    if !ids.is_empty() {
        tx.apply_all(ids.into_iter().map(cancel_of).collect())
            .await?;
    }
    Ok(())
}

async fn cancel_released(
    options: &SessionQueuedInputPortOptions,
    tx: &PendingInputTransaction,
    messages: &[AgentMessage],
) -> Result<(), PendingInputError> {
    let ids: Vec<String> = messages
        .iter()
        .filter_map(|message| (options.client_message_id_of)(message))
        .filter(|client_message_id| tx.current(client_message_id).is_some())
        .collect();
    if !ids.is_empty() {
        tx.apply_all(ids.into_iter().map(cancel_of).collect())
            .await?;
    }
    Ok(())
}

fn report(stage: &str, error: &PendingInputError) {
    eprintln!("pending input {stage} failed: {error}");
}

impl QueuedInputPort for SessionQueuedInputPort {
    fn claim(
        &self,
        _queue: PendingQueue,
        drain: QueuedDrain,
    ) -> BoxFuture<'static, Vec<AgentMessage>> {
        let options = self.options.clone();
        let attempts = self.attempts.clone();
        Box::pin(async move {
            let store = options.store.clone();
            let claimed = store
                .transaction(move |tx| async move {
                    let mut claimed = Vec::new();
                    // A copy the store no longer authorizes (removed, delivered, failed) is stale and not delivered.
                    for message in drain() {
                        if reserve(&options, &attempts, &tx, &message).await? {
                            claimed.push(message);
                        }
                    }
                    Ok::<_, PendingInputError>(claimed)
                })
                .await
                .and_then(|claimed| claimed);
            // The Rust port cannot fail a claim; a store that refuses it delivers nothing it could not reserve.
            claimed.unwrap_or_else(|error| {
                report("claim", &error);
                Vec::new()
            })
        })
    }

    fn commit_consumption(
        &self,
        _queue: PendingQueue,
        messages: Vec<AgentMessage>,
    ) -> BoxFuture<'static, Result<(), String>> {
        let store = self.options.store.clone();
        let attempts = self.attempts.clone();
        Box::pin(async move {
            store
                .transaction(move |tx| async move { settle(&attempts, &tx, &messages, true).await })
                .await
                .and_then(|settled| settled)
                .map_err(|error| error.to_string())
        })
    }

    /// Journal first: if the release cannot be recorded the input stays `sending` (uncertain on reopen) and is
    /// not put back natively, so it can never be delivered twice. A clear that won keeps the messages out of the
    /// queue and their records removed, in one record. Only a clear landing during the release's own append
    /// needs the second, removing record.
    fn release(
        &self,
        _queue: PendingQueue,
        messages: Vec<AgentMessage>,
        give_back: QueuedGiveBack,
    ) -> BoxFuture<'static, ()> {
        let options = self.options.clone();
        let attempts = self.attempts.clone();
        Box::pin(async move {
            let store = options.store.clone();
            let released = store
                .transaction(move |tx| async move {
                    if !(give_back.restorable)() {
                        return cancel_claims(&attempts, &tx, &messages).await;
                    }
                    settle(&attempts, &tx, &messages, false).await?;
                    if !(give_back.restore)() {
                        cancel_released(&options, &tx, &messages).await?;
                    }
                    Ok(())
                })
                .await
                .and_then(|released| released);
            if let Err(error) = released {
                report("release", &error);
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use maho_ai::types::{Message, UserContent, UserMessage};

    /// A queued user message whose text doubles as its client id in these tests.
    pub fn queued_user(text: &str) -> AgentMessage {
        AgentMessage::Llm(Message::User(UserMessage {
            content: UserContent::Text(text.to_owned()),
            timestamp: 1,
        }))
    }

    pub fn id_by_text(ids: &[(&str, &str)]) -> ClientMessageIdOf {
        let ids: Vec<(String, String)> = ids
            .iter()
            .map(|(t, id)| ((*t).to_owned(), (*id).to_owned()))
            .collect();
        Arc::new(move |message: &AgentMessage| match message {
            AgentMessage::Llm(Message::User(UserMessage {
                content: UserContent::Text(text),
                ..
            })) => ids
                .iter()
                .find(|(candidate, _)| candidate == text)
                .map(|(_, id)| id.clone()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    //! senpi `test/suite/pending-input-session-port.test.ts`, driven through the port directly.
    use super::test_support::*;
    use super::*;
    use crate::pending_input_projection::PendingInputState;
    use crate::pending_input_store::PendingInputRead;
    use crate::pending_input_store::SubmitRequest;
    use crate::pending_input_store::test_support::{MemoryJournal, options, submit_request};
    use crate::pending_input_types::{MutationTarget, PendingInputHandle, PendingInputPhase};
    use PendingInputPhase::{Delivered, Queued, Removed, Sending};

    struct Setup {
        journal: Arc<MemoryJournal>,
        store: Arc<PendingInputStore>,
        port: Arc<dyn QueuedInputPort>,
    }

    async fn setup() -> Setup {
        let journal = Arc::new(MemoryJournal::default());
        let store = PendingInputStore::open(options(journal.clone(), "life-1"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let attempts = Arc::new(Mutex::new(0));
        let port = create_session_queued_input_port(SessionQueuedInputPortOptions {
            store: store.clone(),
            owner_id: "owner-1".into(),
            writer_generation: 1,
            next_attempt_id: Arc::new(move || {
                let mut next = attempts.lock().unwrap_or_else(|p| p.into_inner());
                *next += 1;
                format!("a-{next}")
            }),
            client_message_id_of: id_by_text(&[("later", "c-1"), ("also", "c-2")]),
        });
        let _ = store
            .submit(SubmitRequest {
                text: "later".into(),
                ..submit_request()
            })
            .await;
        Setup {
            journal,
            store,
            port,
        }
    }

    fn drain_of(messages: Vec<AgentMessage>) -> QueuedDrain {
        Arc::new(move || messages.clone())
    }

    fn give_back(restorable: bool, restore: bool) -> QueuedGiveBack {
        QueuedGiveBack {
            restorable: Arc::new(move || restorable),
            restore: Arc::new(move || restore),
        }
    }

    fn state(store: &PendingInputStore, id: &str) -> Option<PendingInputState> {
        match store.read(id) {
            PendingInputRead::Admitted { projection } => Some(projection.state),
            PendingInputRead::NotAdmitted { .. } => None,
        }
    }

    async fn submit_second(store: &PendingInputStore) {
        let request = SubmitRequest {
            client_message_id: "c-2".into(),
            fingerprint: "fp2".into(),
            text: "also".into(),
            ..submit_request()
        };
        let _ = store.submit(request).await;
    }

    #[tokio::test]
    async fn journals_the_reservation_and_the_consumption_before_the_input_is_sent() {
        let s = setup().await;
        let claimed = s
            .port
            .claim(PendingQueue::FollowUp, drain_of(vec![queued_user("later")]))
            .await;
        assert_eq!(claimed, vec![queued_user("later")]);
        assert_eq!(
            s.port
                .commit_consumption(PendingQueue::FollowUp, claimed)
                .await,
            Ok(())
        );
        assert_eq!(s.journal.phases("c-1"), vec![Queued, Sending, Delivered]);
        assert_eq!(state(&s.store, "c-1"), Some(PendingInputState::Delivered));
    }

    #[tokio::test]
    async fn does_not_deliver_a_native_copy_the_store_already_removed() {
        let s = setup().await;
        let remove = PendingInputEvent::Remove {
            target: MutationTarget {
                effect_id: "e-1".into(),
                handle: PendingInputHandle {
                    lifetime_id: "life-1".into(),
                    queued_id: "q_1".into(),
                },
                expected_revision: 1,
            },
        };
        let _ = s.store.apply("c-1", remove).await;
        let claimed = s
            .port
            .claim(PendingQueue::FollowUp, drain_of(vec![queued_user("later")]))
            .await;
        assert!(claimed.is_empty());
        assert_eq!(state(&s.store, "c-1"), Some(PendingInputState::Removed));
    }

    #[tokio::test]
    async fn returns_a_claimed_input_to_its_queue_when_its_consumption_cannot_be_committed_t28() {
        let s = setup().await;
        let claimed = s
            .port
            .claim(PendingQueue::FollowUp, drain_of(vec![queued_user("later")]))
            .await;
        s.journal
            .fail_when(|record| record.input.phase == Delivered);
        assert!(
            s.port
                .commit_consumption(PendingQueue::FollowUp, claimed.clone())
                .await
                .is_err()
        );
        s.port
            .release(PendingQueue::FollowUp, claimed, give_back(true, true))
            .await;
        assert_eq!(s.journal.phases("c-1"), vec![Queued, Sending, Queued]);
        assert_eq!(state(&s.store, "c-1"), Some(PendingInputState::Queued));
    }

    #[tokio::test]
    async fn records_a_claimed_input_removed_in_one_record_when_a_clear_won_before_the_give_back() {
        let s = setup().await;
        let claimed = s
            .port
            .claim(PendingQueue::FollowUp, drain_of(vec![queued_user("later")]))
            .await;
        let before = s.journal.records().len();
        s.port
            .release(PendingQueue::FollowUp, claimed, give_back(false, false))
            .await;
        assert_eq!(s.journal.phases("c-1"), vec![Queued, Sending, Removed]);
        assert_eq!(s.journal.records().len() - before, 1);
        assert_eq!(state(&s.store, "c-1"), Some(PendingInputState::Removed));
    }

    #[tokio::test]
    async fn removes_a_released_input_when_a_clear_lands_during_the_releases_own_write() {
        let s = setup().await;
        let claimed = s
            .port
            .claim(PendingQueue::FollowUp, drain_of(vec![queued_user("later")]))
            .await;
        s.port
            .release(PendingQueue::FollowUp, claimed, give_back(true, false))
            .await;
        assert_eq!(
            s.journal.phases("c-1"),
            vec![Queued, Sending, Queued, Removed]
        );
        assert_eq!(state(&s.store, "c-1"), Some(PendingInputState::Removed));
    }

    #[tokio::test]
    async fn commits_a_batchs_consumption_in_one_record_a_failed_write_leaves_every_input_queued() {
        let s = setup().await;
        submit_second(&s.store).await;
        let claimed = s
            .port
            .claim(
                PendingQueue::FollowUp,
                drain_of(vec![queued_user("later"), queued_user("also")]),
            )
            .await;
        s.journal.fail_when(|record| {
            std::iter::once(&record.input)
                .chain(record.batch.iter().flatten())
                .any(|input| input.client_message_id == "c-2" && input.phase == Delivered)
        });
        assert!(
            s.port
                .commit_consumption(PendingQueue::FollowUp, claimed.clone())
                .await
                .is_err()
        );
        s.port
            .release(PendingQueue::FollowUp, claimed, give_back(true, true))
            .await;
        assert_eq!(state(&s.store, "c-1"), Some(PendingInputState::Queued));
        assert_eq!(state(&s.store, "c-2"), Some(PendingInputState::Queued));
    }

    #[tokio::test]
    async fn notifies_and_delivers_a_batch_once_with_one_record_naming_every_input() {
        let s = setup().await;
        submit_second(&s.store).await;
        let notifications = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let sink = notifications.clone();
        s.store.subscribe(Arc::new(
            move |projections: &[crate::pending_input_projection::PendingInputProjection],
                  _: u64| {
                if let Ok(mut sink) = sink.lock() {
                    sink.push(
                        projections
                            .iter()
                            .map(|p| format!("{}:{:?}", p.client_message_id, p.state))
                            .collect(),
                    );
                }
                Ok::<(), String>(())
            },
        ));
        let claimed = s
            .port
            .claim(
                PendingQueue::FollowUp,
                drain_of(vec![queued_user("later"), queued_user("also")]),
            )
            .await;
        assert_eq!(
            s.port
                .commit_consumption(PendingQueue::FollowUp, claimed)
                .await,
            Ok(())
        );
        let delivered: Vec<Vec<String>> = notifications
            .lock()
            .map(|n| n.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|changes| changes.iter().any(|change| change.ends_with(":Delivered")))
            .collect();
        assert_eq!(
            delivered,
            vec![vec!["c-1:Delivered".to_owned(), "c-2:Delivered".to_owned()]]
        );
        let records: Vec<_> = s
            .journal
            .records()
            .into_iter()
            .filter(|r| r.input.phase == Delivered)
            .collect();
        assert_eq!(records.len(), 1);
        let ids: Vec<String> = std::iter::once(records[0].input.clone())
            .chain(records[0].batch.clone().unwrap_or_default())
            .map(|input| input.client_message_id)
            .collect();
        assert_eq!(ids, vec!["c-1".to_owned(), "c-2".to_owned()]);
    }

    #[tokio::test]
    async fn passes_input_the_store_never_admitted_straight_through() {
        let s = setup().await;
        let before = s.journal.records().len();
        let claimed = s
            .port
            .claim(
                PendingQueue::FollowUp,
                drain_of(vec![queued_user("legacy")]),
            )
            .await;
        assert_eq!(claimed, vec![queued_user("legacy")]);
        assert_eq!(
            s.port
                .commit_consumption(PendingQueue::FollowUp, claimed)
                .await,
            Ok(())
        );
        assert_eq!(s.journal.records().len(), before);
    }
}
