//! `dag/journal.ts`: durable WAL-backed run journal with a per-store commit pub/sub and a
//! per-journal bounded-ring live subscriber feed.
// allow: SIZE_OK - WAL append/replay, live subscriber delivery, and durable commit notification
// share one contract so a caller cannot see a checkpoint the WAL has not committed.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dag::events::dag_event_lane;
use crate::dag::store::{DagEventReadOptions, DagFileStore, DagStoreError, FileStamp};
use crate::dag::types::{
    DAG_SETTINGS_DEFAULTS, DagRunEvent, DagRunEventPayload, DagRunId, SchemaVersion1,
};

const REPLAY_PAGE_SIZE: usize = 1000;

/// Every checkpoint carries the WAL sequence it was built through.
pub trait DagJournalCheckpoint: Clone + Send + Sync + 'static {
    const SCHEMA_VERSION: SchemaVersion1 = SchemaVersion1;
    fn checkpoint_seq(&self) -> u64;
    fn with_checkpoint_seq(&self, checkpoint_seq: u64) -> Self;
    /// By-value variant for the journal hot path: implementors override it to restamp in place
    /// instead of deep-cloning a checkpoint the journal already owns.
    fn with_checkpoint_seq_owned(self, checkpoint_seq: u64) -> Self {
        self.with_checkpoint_seq(checkpoint_seq)
    }
}

pub type DagJournalListener = Arc<dyn Fn(&DagRunEvent) + Send + Sync>;
pub type DagJournalApplyEvent<C> = Arc<dyn Fn(&C, &DagRunEvent) -> C + Send + Sync>;
pub type DagJournalUnsubscribe = Box<dyn FnOnce() + Send>;
type DuplicatePredicate<C> = Arc<dyn Fn(&C, &DagRunEventPayload) -> bool + Send + Sync>;

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
    commit_overflow: Box<dyn Fn(Box<OverflowState>) -> Option<DagRunEvent> + Send + Sync>,
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
                    Some(DeliveryItem::Overflow(Box::new(overflow)))
                } else {
                    queue.events.pop_front().map(Box::new).map(DeliveryItem::Event)
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
    Overflow(Box<OverflowState>),
    Event(Box<DagRunEvent>),
}

/// Process-local durable-commit subscription: fires once per store, independent of any particular
/// `DagJournal` instance in this process.
type CommitListener = Arc<dyn Fn(&DagRunEvent) + Send + Sync>;
struct CommitDelivery {
    queue: Mutex<VecDeque<CommitQueueEntry>>,
    running: std::sync::atomic::AtomicBool,
    idle: Condvar,
}

static COMMIT_DELIVERY: std::sync::OnceLock<CommitDelivery> = std::sync::OnceLock::new();

fn commit_delivery() -> &'static CommitDelivery {
    COMMIT_DELIVERY.get_or_init(|| CommitDelivery {
        queue: Mutex::new(VecDeque::new()),
        running: std::sync::atomic::AtomicBool::new(false),
        idle: Condvar::new(),
    })
}

struct CommitSubscriber {
    listener: CommitListener,
    active: std::sync::atomic::AtomicBool,
}

type CommitQueueEntry = (Arc<CommitSubscriber>, Arc<DagRunEvent>);
type CommitRegistryEntry = (usize, DagRunId, Vec<Arc<CommitSubscriber>>);
type CommitRegistry = Mutex<Vec<CommitRegistryEntry>>;

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
            subscribers.retain(|entry| entry.active.load(std::sync::atomic::Ordering::SeqCst));
            if registry[index].2.is_empty() {
                registry.remove(index);
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
    let mut pushed = false;
    // One shared copy of the event serves every active commit subscriber.
    let mut shared: Option<Arc<DagRunEvent>> = None;
    for subscriber in subscribers {
        if subscriber.active.load(std::sync::atomic::Ordering::SeqCst) {
            let event = Arc::clone(shared.get_or_insert_with(|| Arc::new(event.clone())));
            commit_delivery().queue.lock().unwrap_or_else(PoisonError::into_inner).push_back((subscriber, event));
            pushed = true;
        }
    }
    // Nothing queued: a running worker drains what exists, and none is needed otherwise.
    if pushed && !commit_delivery().running.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::thread::spawn(|| loop {
            let next = commit_delivery().queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
            let Some((subscriber, event)) = next else {
                let queue = commit_delivery().queue.lock().unwrap_or_else(PoisonError::into_inner);
                if queue.is_empty() {
                    commit_delivery().running.store(false, std::sync::atomic::Ordering::SeqCst);
                    commit_delivery().idle.notify_all();
                    break;
                }
                drop(queue);
                continue;
            };
            if subscriber.active.load(std::sync::atomic::Ordering::SeqCst) {
                let event: &DagRunEvent = &event;
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (subscriber.listener)(event)));
            }
        });
    }
}

/// A durable, replayable run journal: `append` commits one WAL event and advances the checkpoint
/// under the run lock; `subscribe` feeds live events through a bounded per-subscriber ring.
pub struct DagJournal<C: DagJournalCheckpoint> {
    store: Arc<DagFileStore>,
    run_id: DagRunId,
    apply_event: DagJournalApplyEvent<C>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    skip_duplicate: Mutex<Option<DuplicatePredicate<C>>>,
    ring_size: usize,
    checkpoint: Mutex<C>,
    /// `(event log length, seq)` right after this journal's last append. While the log still has
    /// that length under the run lock, no foreign append happened, so replay can be skipped.
    appended_tail: Mutex<Option<(u64, u64)>>,
    /// Identity of the checkpoint file right after this journal's own last write; `None` whenever
    /// that write is unconfirmed or the platform cannot stamp files.
    checkpoint_stamp: Mutex<Option<FileStamp>>,
    /// Committed appends; `when_idle` repeats its drain pass until a pass sees none, because an
    /// overflow committed by one subscriber's drain re-enqueues into subscribers already waited on.
    appends: std::sync::atomic::AtomicU64,
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
        recover_checkpoint(&store, &run_id, || initial_checkpoint, &apply_event, None)
    })??;
    Ok(Arc::new(DagJournal {
        store,
        run_id,
        apply_event,
        now,
        skip_duplicate: Mutex::new(None),
        ring_size,
        checkpoint: Mutex::new(recovered),
        appended_tail: Mutex::new(None),
        checkpoint_stamp: Mutex::new(None),
        appends: std::sync::atomic::AtomicU64::new(0),
        subscribers: Arc::new(Mutex::new(Vec::new())),
    }))
}

fn recover_checkpoint<C: DagJournalCheckpoint + DeserializeOwned + Serialize>(
    store: &DagFileStore,
    run_id: &DagRunId,
    initial_checkpoint: impl FnOnce() -> C,
    apply_event: &DagJournalApplyEvent<C>,
    known_tail_seq: Option<u64>,
) -> Result<C, DagStoreError> {
    let mut checkpoint = store
        .read_checkpoint::<C>(run_id)?
        .unwrap_or_else(initial_checkpoint);
    if known_tail_seq == Some(checkpoint.checkpoint_seq()) {
        // The checkpoint already covers the log tail: nothing to replay.
        return Ok(checkpoint);
    }
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
            checkpoint = applied.with_checkpoint_seq_owned(event.seq);
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
    pub fn set_skip_duplicate(&self, predicate: DuplicatePredicate<C>) {
        *self.skip_duplicate.lock().unwrap_or_else(PoisonError::into_inner) = Some(predicate);
    }
    pub fn append_maybe(&self, payload: DagRunEventPayload) -> Result<Option<DagRunEvent>, DagStoreError> {
        self.append_from(payload, None)
    }

    pub fn append(&self, payload: DagRunEventPayload) -> Result<DagRunEvent, DagStoreError> {
        self.append_maybe(payload)?.ok_or_else(|| DagStoreError::Message("DAG journal mutation completed without an event".to_string()))
    }

    pub fn refresh(&self) -> Result<C, DagStoreError> {
        let checkpoint = self.store.with_run_lock(&self.run_id, || {
            recover_checkpoint(&self.store, &self.run_id, || self.snapshot(), &self.apply_event, None)
        })??;
        *self.checkpoint.lock().unwrap_or_else(PoisonError::into_inner) = checkpoint.clone();
        Ok(checkpoint)
    }

    fn append_from(
        &self,
        payload: DagRunEventPayload,
        direct_subscriber: Option<&Arc<Subscriber>>,
    ) -> Result<Option<DagRunEvent>, DagStoreError> {
        let event = self.store.with_run_lock(&self.run_id, || {
            self.append_locked(payload)
        })??;
        let Some(event) = event else { return Ok(None) };
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
        // Counted after the fan-out: a `when_idle` pass that misses this count already waits on
        // the subscribers it was enqueued into.
        self.appends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        publish_commit(&self.store, &self.run_id, &event);
        Ok(Some(event))
    }

    /// Seq of this journal's last append when the event log is still exactly as it left it.
    fn unchanged_tail_seq(&self) -> Option<u64> {
        let (len, seq) = (*self.appended_tail.lock().unwrap_or_else(PoisonError::into_inner))?;
        let current = std::fs::metadata(self.store.paths.event(&self.run_id)).ok()?.len();
        (current == len).then_some(seq)
    }

    /// The in-memory checkpoint when the on-disk checkpoint is provably this journal's own last
    /// write and the log tail is still its own last append; otherwise `None`.
    fn trusted_snapshot(&self) -> Option<C> {
        let tail = self.unchanged_tail_seq()?;
        let stamp = (*self.checkpoint_stamp.lock().unwrap_or_else(PoisonError::into_inner))?;
        if FileStamp::of(&self.store.paths.run(&self.run_id))? != stamp {
            return None;
        }
        let snapshot = self.snapshot();
        (snapshot.checkpoint_seq() == tail).then_some(snapshot)
    }

    fn append_locked(&self, payload: DagRunEventPayload) -> Result<Option<DagRunEvent>, DagStoreError> {
        // PERF-DIVERGENCE(senpi dag-journal.ts): upstream re-reads and re-parses the whole checkpoint from disk on every append; this port reuses its in-memory checkpoint when the file's len/mtime/ctime/inode still match its own last write and the log tail is unchanged, else it reads as before. Any foreign write changes the stamp, so the result is identical.
        let recovered = match self.trusted_snapshot() {
            Some(snapshot) => snapshot,
            None => recover_checkpoint(
                &self.store,
                &self.run_id,
                || self.snapshot(),
                &self.apply_event,
                self.unchanged_tail_seq(),
            )?,
        };
        if self.skip_duplicate.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_some_and(|predicate| predicate(&recovered, &payload)) {
            *self.checkpoint.lock().unwrap_or_else(PoisonError::into_inner) = recovered;
            return Ok(None);
        }
        // `recover_checkpoint` replayed every event past the checkpoint, so its seq is the log tail.
        let seq = recovered.checkpoint_seq() + 1;
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
        let log_len = self.store.append_validated_event(&event)?;
        *self.appended_tail.lock().unwrap_or_else(PoisonError::into_inner) = Some((log_len, seq));
        *self.checkpoint_stamp.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let applied = prepared.unwrap_or_else(|| (self.apply_event)(&recovered, &event));
        let next = applied.with_checkpoint_seq_owned(seq);
        self.store.write_checkpoint(&self.run_id, &next)?;
        *self.checkpoint_stamp.lock().unwrap_or_else(PoisonError::into_inner) =
            FileStamp::of(&self.store.paths.run(&self.run_id));
        *self.checkpoint.lock().unwrap_or_else(PoisonError::into_inner) = next;
        Ok(Some(event))
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
        )?.ok_or_else(|| DagStoreError::Message("overflow append was unexpectedly skipped".to_string()))
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
        let commit_overflow = move |overflow: Box<OverflowState>| -> Option<DagRunEvent> {
            let journal = journal_weak.upgrade()?;
            let subscriber = commit_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()?;
            journal.commit_overflow(&subscriber, *overflow).ok()
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
        loop {
            let appends = self.appends.load(std::sync::atomic::Ordering::SeqCst);
            let subscribers: Vec<Arc<Subscriber>> = self
                .subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            for subscriber in &subscribers {
                subscriber.wait_idle();
            }
            if self.appends.load(std::sync::atomic::Ordering::SeqCst) == appends {
                break;
            }
        }
        let delivery = commit_delivery();
        loop {
            let mut queue = delivery.queue.lock().unwrap_or_else(PoisonError::into_inner);
            if !delivery.running.load(std::sync::atomic::Ordering::SeqCst) && queue.is_empty() {
                break;
            }
            queue = delivery.idle.wait(queue).unwrap_or_else(PoisonError::into_inner);
            drop(queue);
        }
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
