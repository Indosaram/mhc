//! `dag/journal.ts`: durable WAL-backed run journal with a per-store commit pub/sub and a
//! per-journal bounded-ring live subscriber feed.
// allow: SIZE_OK - WAL append/replay, live subscriber delivery, and durable commit notification
// share one contract so a caller cannot see a checkpoint the WAL has not committed.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dag::events::dag_event_lane;
use crate::dag::store::{DagEventReadOptions, DagFileStore, DagStoreError};
use crate::dag::types::{
    DAG_SETTINGS_DEFAULTS, DagRunEvent, DagRunEventPayload, DagRunId, SchemaVersion1,
};

const REPLAY_PAGE_SIZE: usize = 1000;

/// Every checkpoint carries the WAL sequence it was built through.
pub trait DagJournalCheckpoint: Clone + Send + Sync + 'static {
    const SCHEMA_VERSION: SchemaVersion1 = SchemaVersion1;
    fn checkpoint_seq(&self) -> u64;
    fn with_checkpoint_seq(&self, checkpoint_seq: u64) -> Self;
}

pub type DagJournalListener = Arc<dyn Fn(&DagRunEvent) + Send + Sync>;
pub type DagJournalApplyEvent<C> = Arc<dyn Fn(&C, &DagRunEvent) -> C + Send + Sync>;
pub type DagJournalUnsubscribe = Box<dyn FnOnce() + Send>;

pub struct DagJournalOptions<C: DagJournalCheckpoint> {
    pub store: Arc<DagFileStore>,
    pub run_id: DagRunId,
    pub initial_checkpoint: C,
    pub apply_event: DagJournalApplyEvent<C>,
    pub subscriber_ring: Option<usize>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

/// Live per-journal-instance subscriber: an isolated ring queue drained by one dedicated thread so
/// a slow listener can never block `append`. `commit_overflow` durably appends the coalesced
/// overflow marker through the owning journal before the drain thread delivers it.
struct Subscriber {
    queue: Mutex<SubscriberQueue>,
    idle: Condvar,
    listener: DagJournalListener,
    ring_size: usize,
    active: std::sync::atomic::AtomicBool,
    commit_overflow: Box<dyn Fn(OverflowState) -> Option<DagRunEvent> + Send + Sync>,
}

#[derive(Default)]
struct SubscriberQueue {
    events: VecDeque<DagRunEvent>,
    overflow: Option<OverflowState>,
    last_delivered_seq: u64,
    running: bool,
}

#[derive(Clone, Copy)]
struct OverflowState {
    dropped_count: u64,
    recover_after_seq: u64,
}

impl Subscriber {
    fn enqueue(self: &Arc<Self>, event: DagRunEvent) {
        if !self.active.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let mut queue = self.lock();
        if queue.events.len() >= self.ring_size && queue.events.pop_front().is_some() {
            match &mut queue.overflow {
                Some(overflow) => overflow.dropped_count += 1,
                None => {
                    queue.overflow = Some(OverflowState {
                        dropped_count: 1,
                        recover_after_seq: queue.last_delivered_seq,
                    });
                }
            }
        }
        queue.events.push_back(event);
        if !queue.running {
            queue.running = true;
            self.spawn_drain(queue);
        }
    }

    fn lock(&self) -> MutexGuard<'_, SubscriberQueue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Spawns the drain thread. Called with the queue lock already held; the lock is dropped
    /// before the thread body runs so the worker can re-acquire it.
    fn spawn_drain(self: &Arc<Self>, queue: MutexGuard<'_, SubscriberQueue>) {
        drop(queue);
        let this = Arc::clone(self);
        std::thread::spawn(move || this.drain());
    }

    fn drain(self: &Arc<Self>) {
        loop {
            if !self.active.load(std::sync::atomic::Ordering::SeqCst) {
                let mut queue = self.lock();
                queue.running = false;
                self.idle.notify_all();
                return;
            }
            let next = {
                let mut queue = self.lock();
                if let Some(overflow) = queue.overflow.take() {
                    Some(DeliveryItem::Overflow(overflow))
                } else {
                    queue.events.pop_front().map(DeliveryItem::Event)
                }
            };
            match next {
                Some(DeliveryItem::Overflow(overflow)) => {
                    // The TS drain records `lastDeliveredSeq` only for queued events, never for an
                    // overflow delivery: an overflow's `recoverAfterSeq` stays the last queued seq.
                    if let Some(event) = (self.commit_overflow)(overflow) {
                        (self.listener)(&event);
                    }
                }
                Some(DeliveryItem::Event(event)) => {
                    // `subscriber.lastDeliveredSeq = event.seq` before `await deliver(...)`, so an
                    // overflow raised while this listener is still running recovers from this seq.
                    self.lock().last_delivered_seq = event.seq;
                    (self.listener)(&event);
                }
                None => {
                    let mut queue = self.lock();
                    if queue.overflow.is_none() && queue.events.is_empty() {
                        queue.running = false;
                        self.idle.notify_all();
                        return;
                    }
                }
            }
        }
    }

    /// Blocks until the queue and overflow are both drained. Faithful counterpart of
    /// `Promise.all([...subscribers].map((s) => s.drainPromise))`.
    fn wait_idle(&self) {
        let mut queue = self.lock();
        while queue.running || queue.overflow.is_some() || !queue.events.is_empty() {
            queue = self
                .idle
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

enum DeliveryItem {
    Overflow(OverflowState),
    Event(DagRunEvent),
}

/// Process-local durable-commit subscription: fires once per store, independent of any particular
/// `DagJournal` instance in this process.
type CommitListener = Arc<dyn Fn(&DagRunEvent) + Send + Sync>;

struct CommitSubscriber {
    listener: CommitListener,
    active: std::sync::atomic::AtomicBool,
}

type CommitRegistry = Mutex<Vec<(usize, DagRunId, Vec<Arc<CommitSubscriber>>)>>;

static COMMIT_SUBSCRIBERS: std::sync::OnceLock<CommitRegistry> = std::sync::OnceLock::new();

fn commit_registry() -> &'static CommitRegistry {
    COMMIT_SUBSCRIBERS.get_or_init(|| Mutex::new(Vec::new()))
}

fn store_key(store: &Arc<DagFileStore>) -> usize {
    Arc::as_ptr(store) as *const () as usize
}

/// Process-local notification for durable commits, independent of a particular journal instance.
pub fn subscribe_dag_journal(
    store: &Arc<DagFileStore>,
    run_id: &DagRunId,
    listener: CommitListener,
) -> DagJournalUnsubscribe {
    let key = store_key(store);
    let subscriber = Arc::new(CommitSubscriber {
        listener,
        active: std::sync::atomic::AtomicBool::new(true),
    });
    {
        let mut registry = commit_registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let bucket = registry
            .iter_mut()
            .find(|(existing_key, existing_run, _)| *existing_key == key && existing_run == run_id);
        match bucket {
            Some((_, _, subscribers)) => subscribers.push(Arc::clone(&subscriber)),
            None => registry.push((key, run_id.clone(), vec![Arc::clone(&subscriber)])),
        }
    }
    let unsub_key = key;
    let unsub_run_id = run_id.clone();
    let weak: Weak<CommitSubscriber> = Arc::downgrade(&subscriber);
    Box::new(move || {
        if let Some(subscriber) = weak.upgrade() {
            subscriber
                .active
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        let mut registry = commit_registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = registry
            .iter()
            .position(|(existing_key, existing_run, _)| {
                *existing_key == unsub_key && *existing_run == unsub_run_id
            })
        {
            let subscribers = &mut registry[index].2;
            subscribers.retain(|entry| !entry.active.load(std::sync::atomic::Ordering::SeqCst));
            if registry[index].2.is_empty() {
                registry.remove(index);
            } else {
                // Keep only the still-active removal criterion: rebuild without the retained
                // inactive ones already pruned above.
            }
        }
    })
}

fn publish_commit(store: &Arc<DagFileStore>, run_id: &DagRunId, event: &DagRunEvent) {
    let key = store_key(store);
    let subscribers: Vec<Arc<CommitSubscriber>> = {
        let registry = commit_registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        registry
            .iter()
            .find(|(existing_key, existing_run, _)| *existing_key == key && existing_run == run_id)
            .map(|(_, _, subscribers)| subscribers.clone())
            .unwrap_or_default()
    };
    for subscriber in subscribers {
        if subscriber.active.load(std::sync::atomic::Ordering::SeqCst) {
            (subscriber.listener)(event);
        }
    }
}

/// A durable, replayable run journal: `append` commits one WAL event and advances the checkpoint
/// under the run lock; `subscribe` feeds live events through a bounded per-subscriber ring.
pub struct DagJournal<C: DagJournalCheckpoint> {
    store: Arc<DagFileStore>,
    run_id: DagRunId,
    apply_event: DagJournalApplyEvent<C>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    ring_size: usize,
    checkpoint: Mutex<C>,
    subscribers: Arc<Mutex<Vec<Arc<Subscriber>>>>,
}

pub fn create_dag_journal<C: DagJournalCheckpoint + DeserializeOwned + Serialize>(
    options: DagJournalOptions<C>,
) -> Result<Arc<DagJournal<C>>, DagStoreError> {
    let ring_size = options
        .subscriber_ring
        .unwrap_or(DAG_SETTINGS_DEFAULTS.subscriber_ring);
    if ring_size == 0 {
        return Err(DagStoreError::Message(
            "subscriber ring must be a positive integer".to_string(),
        ));
    }
    let now = options.now.unwrap_or_else(|| {
        Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX))
    });
    let store = options.store;
    let run_id = options.run_id;
    let apply_event = options.apply_event;
    let initial_checkpoint = options.initial_checkpoint;
    let recovered = store.with_run_lock(&run_id, || {
        recover_checkpoint(&store, &run_id, &initial_checkpoint, &apply_event)
    })??;
    Ok(Arc::new(DagJournal {
        store,
        run_id,
        apply_event,
        now,
        ring_size,
        checkpoint: Mutex::new(recovered),
        subscribers: Arc::new(Mutex::new(Vec::new())),
    }))
}

fn recover_checkpoint<C: DagJournalCheckpoint + DeserializeOwned + Serialize>(
    store: &DagFileStore,
    run_id: &DagRunId,
    initial_checkpoint: &C,
    apply_event: &DagJournalApplyEvent<C>,
) -> Result<C, DagStoreError> {
    let mut checkpoint = store
        .read_checkpoint::<C>(run_id)?
        .unwrap_or_else(|| initial_checkpoint.clone());
    let mut since_seq = checkpoint.checkpoint_seq();
    let mut replayed = false;
    loop {
        let page = store.read_events(
            run_id,
            since_seq,
            &DagEventReadOptions {
                limit: REPLAY_PAGE_SIZE,
                ..Default::default()
            },
        )?;
        for event in &page.events {
            let applied = apply_event(&checkpoint, event);
            checkpoint = applied.with_checkpoint_seq(event.seq);
            replayed = true;
        }
        if !page.has_more {
            break;
        }
        since_seq = page.next_since_seq;
    }
    if replayed {
        store.write_checkpoint(run_id, &checkpoint)?;
    }
    Ok(checkpoint)
}

impl<C: DagJournalCheckpoint + DeserializeOwned + Serialize> DagJournal<C> {
    pub fn append(&self, payload: DagRunEventPayload) -> Result<DagRunEvent, DagStoreError> {
        self.append_from(payload, None)
    }

    fn append_from(
        &self,
        payload: DagRunEventPayload,
        direct_subscriber: Option<&Arc<Subscriber>>,
    ) -> Result<DagRunEvent, DagStoreError> {
        let event = self.store.with_run_lock(&self.run_id, || {
            self.append_locked(payload)
        })??;
        let subscribers: Vec<Arc<Subscriber>> = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for subscriber in &subscribers {
            let is_direct = direct_subscriber.is_some_and(|direct| Arc::ptr_eq(direct, subscriber));
            if !is_direct {
                subscriber.enqueue(event.clone());
            }
        }
        publish_commit(&self.store, &self.run_id, &event);
        Ok(event)
    }

    fn append_locked(&self, payload: DagRunEventPayload) -> Result<DagRunEvent, DagStoreError> {
        let recovered = recover_checkpoint(
            &self.store,
            &self.run_id,
            &self.checkpoint.lock().unwrap_or_else(PoisonError::into_inner).clone(),
            &self.apply_event,
        )?;
        let tail_seq = self
            .store
            .read_events(
                &self.run_id,
                recovered.checkpoint_seq(),
                &DagEventReadOptions {
                    limit: 1,
                    ..Default::default()
                },
            )?
            .head_seq;
        let seq = recovered.checkpoint_seq().max(tail_seq) + 1;
        let event = DagRunEvent {
            schema_version: SchemaVersion1,
            run_id: self.run_id.clone(),
            seq,
            at: crate::shared::iso_from_ms((self.now)()),
            lane: dag_event_lane(payload.event_type()),
            payload,
        };
        // Completed-node reducers synchronously stage result artifacts; stage them before the WAL
        // so replay can rebuild metadata even if the process dies before checkpoint replacement.
        let prepared = matches!(
            &event.payload,
            DagRunEventPayload::NodeTransitioned { to, .. } if to.as_str() == "completed"
        )
        .then(|| (self.apply_event)(&recovered, &event));
        self.store.append_event(&event)?;
        let applied = prepared.unwrap_or_else(|| (self.apply_event)(&recovered, &event));
        let next = applied.with_checkpoint_seq(seq);
        self.store.write_checkpoint(&self.run_id, &next)?;
        *self.checkpoint.lock().unwrap_or_else(PoisonError::into_inner) = next;
        Ok(event)
    }

    /// Durable-commit overflow event: appended through the normal WAL path so the coalesced
    /// overflow marker itself gets a seq and survives replay, exactly like every other event.
    fn commit_overflow(
        &self,
        subscriber: &Arc<Subscriber>,
        overflow: OverflowState,
    ) -> Result<DagRunEvent, DagStoreError> {
        self.append_from(
            DagRunEventPayload::StreamOverflow {
                dropped_count: overflow.dropped_count,
                recover_after_seq: overflow.recover_after_seq,
            },
            Some(subscriber),
        )
    }

    pub fn snapshot(&self) -> C {
        self.checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Registers a live subscriber and returns its unsubscribe handle. The listener runs on a
    /// dedicated drain thread per subscriber so a slow listener never blocks `append`.
    pub fn subscribe(self: &Arc<Self>, listener: DagJournalListener) -> DagJournalUnsubscribe {
        let checkpoint_seq = self.snapshot().checkpoint_seq();
        let journal_weak: Weak<DagJournal<C>> = Arc::downgrade(self);
        let subscriber_slot: Arc<Mutex<Option<Arc<Subscriber>>>> = Arc::new(Mutex::new(None));
        let commit_slot = Arc::clone(&subscriber_slot);
        let commit_overflow = move |overflow: OverflowState| -> Option<DagRunEvent> {
            let journal = journal_weak.upgrade()?;
            let subscriber = commit_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()?;
            journal.commit_overflow(&subscriber, overflow).ok()
        };
        let subscriber = Arc::new(Subscriber {
            queue: Mutex::new(SubscriberQueue {
                last_delivered_seq: checkpoint_seq,
                ..Default::default()
            }),
            idle: Condvar::new(),
            listener,
            ring_size: self.ring_size,
            active: std::sync::atomic::AtomicBool::new(true),
            commit_overflow: Box::new(commit_overflow),
        });
        *subscriber_slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&subscriber));
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&subscriber));
        let subscribers = Arc::clone(&self.subscribers);
        let removed = subscriber;
        Box::new(move || {
            removed.active.store(false, std::sync::atomic::Ordering::SeqCst);
            let mut queue = removed.lock();
            queue.events.clear();
            queue.overflow = None;
            drop(queue);
            subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|entry| !Arc::ptr_eq(entry, &removed));
        })
    }

    /// Blocks until every current subscriber has drained its queue (including any coalesced
    /// overflow). Faithful counterpart of `Promise.all([...subscribers].map(drainPromise))`.
    pub fn when_idle(&self) {
        let subscribers: Vec<Arc<Subscriber>> = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for subscriber in &subscribers {
            subscriber.wait_idle();
        }
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
