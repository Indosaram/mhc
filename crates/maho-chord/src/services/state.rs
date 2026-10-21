use super::{errors::{collected, contract}, state_internals::{register_replicated_state_internals, ReplicatedStateInternals, SourceListener, Unsubscribe}};
use crate::{context::BACKGROUND_CONTEXT, delta::{self, Op, Change}, types::*};
use futures::FutureExt;
use std::{collections::VecDeque, sync::{Arc, Mutex, MutexGuard, atomic::{AtomicBool, Ordering}}};

pub(crate) fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> { value.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
pub fn service_delivery_context() -> ContextRef { BACKGROUND_CONTEXT.clone() }
fn default_reporter() -> ErrorHandler { Arc::new(|error| { tokio::spawn(async move { std::panic::panic_any(error); }); }) }
pub(crate) fn report_error(report: &ErrorHandler, error: ServiceError) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| report(error))) {
        tokio::spawn(async move { std::panic::resume_unwind(panic); });
    }
}
#[derive(Clone)]
struct Frame<T> { value: Arc<T>, context: ContextRef, delivery: ReplicatedStateDelivery }
struct Pending<T> { frames: VecDeque<Frame<T>>, running: bool, started: bool, closed: bool }
struct Subscriber<T> { listener: StateListener<T>, report: ErrorHandler, pending: Mutex<Pending<T>> }
impl<T: Send + Sync + 'static> Subscriber<T> {
    fn new(listener: StateListener<T>, report: ErrorHandler) -> Arc<Self> {
        Arc::new(Self { listener, report, pending: Mutex::new(Pending { frames: VecDeque::new(), running: false, started: false, closed: false }) })
    }
    fn push(&self, frame: Frame<T>) {
        let mut pending = lock(&self.pending);
        if pending.closed { return; }
        if pending.frames.len() == 100 {
            let hydration = if pending.started { None } else { pending.frames.pop_front() };
            pending.frames.clear();
            if let Some(hydration) = hydration { pending.frames.push_back(hydration); }
        }
        pending.frames.push_back(frame);
    }
    fn drain(self: &Arc<Self>) {
        {
            let mut p = lock(&self.pending);
            if p.running || p.closed { return; }
            p.running = true;
        }
        loop {
            let frame = {
                let mut p = lock(&self.pending);
                if p.closed { p.running = false; return; }
                match p.frames.pop_front() { Some(frame) => { p.started = true; frame }, None => { p.running = false; return; } }
            };
            let future = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.listener)(frame.value, frame.context, frame.delivery))) {
                Ok(future) => future,
                Err(_) => { report_error(&self.report, contract("State listener panicked")); continue; }
            };
            let future: BoxFuture<'static, Result<(), ServiceError>> = Box::pin(async move {
                match std::panic::AssertUnwindSafe(future).catch_unwind().await { Ok(result) => result, Err(_) => Err(contract("State listener future panicked")) }
            });
            match future.now_or_never_preserving() {
                ListenerPoll::Ready(result) => { if let Err(error) = result { report_error(&self.report, error); } }
                ListenerPoll::Pending(future) => {
                    let subscriber = self.clone();
                    tokio::spawn(async move {
                        if let Err(error) = future.await { report_error(&subscriber.report, error); }
                        lock(&subscriber.pending).running = false;
                        subscriber.drain();
                    });
                    return;
                }
            }
        }
    }
    fn clear(&self) { lock(&self.pending).frames.clear(); }
    fn close(&self) { let mut p = lock(&self.pending); p.closed = true; p.frames.clear(); }
    fn closed(&self) -> bool { lock(&self.pending).closed }
}
enum ListenerPoll { Ready(Result<(), ServiceError>), Pending(BoxFuture<'static, Result<(), ServiceError>>) }
trait PollListener { fn now_or_never_preserving(self) -> ListenerPoll; }
impl PollListener for BoxFuture<'static, Result<(), ServiceError>> {
    fn now_or_never_preserving(mut self) -> ListenerPoll {
        match self.as_mut().now_or_never() { Some(result) => ListenerPoll::Ready(result), None => ListenerPoll::Pending(self) }
    }
}
struct Publication<T> { frame: Frame<T>, ops: Arc<[Op]> }
struct PublisherData<T> {
    value: Arc<T>, sequence: ServiceSequence,
    subscribers: Vec<(Arc<Subscriber<T>>, ServiceSequence)>, sources: Vec<(Arc<()>, SourceListener)>,
    publications: VecDeque<Publication<T>>, delivering: bool,
}
struct Publisher<T> { data: Mutex<PublisherData<T>>, report: ErrorHandler }
impl<T: Send + Sync + 'static> Publisher<T> {
    fn new(value: Arc<T>, report: ErrorHandler) -> Arc<Self> {
        Arc::new(Self { data: Mutex::new(PublisherData { value, sequence: ServiceSequence::ZERO, subscribers: Vec::new(), sources: Vec::new(), publications: VecDeque::new(), delivering: false }), report })
    }
    fn snapshot(&self) -> (Arc<T>, ServiceSequence) { let d = lock(&self.data); (d.value.clone(), d.sequence) }
    fn subscribe(self: &Arc<Self>, listener: StateListener<T>) -> Disposal {
        let subscriber = Subscriber::new(listener, self.report.clone());
        {
            let mut d = lock(&self.data);
            let sequence = d.sequence;
            subscriber.push(Frame { value: d.value.clone(), context: service_delivery_context(), delivery: ReplicatedStateDelivery { kind: ReplicatedStateDeliveryKind::Hydrate, sequence } });
            d.subscribers.push((subscriber.clone(), sequence));
        }
        subscriber.drain();
        let publisher = Arc::downgrade(self);
        Box::new(move || { subscriber.close(); if let Some(p) = publisher.upgrade() { lock(&p.data).subscribers.retain(|(s, _)| !Arc::ptr_eq(s, &subscriber)); } Box::pin(async { Ok(()) }) })
    }
    fn subscribe_source(self: &Arc<Self>, listener: SourceListener) -> Unsubscribe {
        let identity = Arc::new(());
        lock(&self.data).sources.push((identity.clone(), listener));
        let publisher = Arc::downgrade(self);
        Box::new(move || { if let Some(p) = publisher.upgrade() { lock(&p.data).sources.retain(|(id, _)| !Arc::ptr_eq(id, &identity)); } })
    }
    fn publish(&self, value: Arc<T>, ops: Arc<[Op]>, context: ContextRef) -> Result<(), ServiceError> {
        {
            let mut d = lock(&self.data);
            d.sequence = d.sequence.next();
            d.value = value.clone();
            let sequence = d.sequence;
            d.publications.push_back(Publication { frame: Frame { value, context, delivery: ReplicatedStateDelivery { kind: ReplicatedStateDeliveryKind::Update, sequence } }, ops });
            if d.delivering { return Ok(()); }
            d.delivering = true;
        }
        let mut errors = Vec::new();
        loop {
            let (publication, sources) = {
                let mut d = lock(&self.data);
                match d.publications.pop_front() { Some(p) => (p, d.sources.clone()), None => { d.delivering = false; break; } }
            };
            for (_, listener) in sources {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(&publication.ops, publication.frame.delivery.sequence, publication.frame.context.clone()))) {
                    Ok(Ok(())) => {}, Ok(Err(error)) => errors.push(error), Err(_) => errors.push(contract("State source listener panicked")),
                }
            }
            let subscribers = lock(&self.data).subscribers.clone();
            for (subscriber, hydrated) in subscribers {
                if publication.frame.delivery.sequence > hydrated {
                    subscriber.push(Frame { value: publication.frame.value.clone(), context: publication.frame.context.clone(), delivery: publication.frame.delivery });
                    subscriber.drain();
                }
            }
        }
        collected(errors, "Replicated state listeners failed")
    }
}
pub struct MutableReplicatedStateImpl { publisher: Arc<Publisher<JsonValue>>, changing: AtomicBool }
impl MutableReplicatedStateImpl {
    pub fn new(initial: JsonValue) -> Result<Arc<Self>, ServiceError> {
        delta::track(initial.clone())?;
        let state = Arc::new(Self { publisher: Publisher::new(Arc::new(initial), default_reporter()), changing: AtomicBool::new(false) });
        let internals: Arc<dyn ReplicatedStateInternals> = state.clone();
        register_replicated_state_internals(&internals, &internals);
        Ok(state)
    }
}
struct Changing<'a>(&'a AtomicBool);
impl Drop for Changing<'_> { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }
impl MutableReplicatedState for MutableReplicatedStateImpl {
    fn change(&self, context: ContextRef, mutate: &mut dyn FnMut(&mut Change) -> Result<(), ServiceError>) -> Result<(), ServiceError> {
        if self.changing.swap(true, Ordering::AcqRel) { return Err(contract("Replicated state cannot be changed reentrantly from a change callback")); }
        let guard = Changing(&self.changing);
        let (value, _) = self.publisher.snapshot();
        let mut tracker = delta::track((*value).clone())?;
        let mut change = tracker.begin_change();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mutate(&mut change))) {
            Ok(Ok(())) => {},
            Ok(Err(error)) => { change.abort(); return Err(error); }
            Err(panic) => { change.abort(); std::panic::resume_unwind(panic); }
        }
        let prepared = match change.prepare() { Ok(p) => p, Err(e) => { change.abort(); return Err(e.into()); } };
        tracker.adopt(&prepared)?;
        drop(guard);
        if prepared.ops().is_empty() { return Ok(()); }
        self.publisher.publish(Arc::new(prepared.value().clone()), prepared.ops().to_vec().into(), context)
    }
    fn replace(&self, context: ContextRef, value: JsonValue) -> Result<(), ServiceError> {
        if self.changing.load(Ordering::Acquire) { return Err(contract("Replicated state cannot be replaced from a change callback")); }
        let (previous, _) = self.publisher.snapshot();
        let mut tracker = delta::track((*previous).clone())?;
        let prepared = tracker.prepare_replace(value)?;
        tracker.adopt(&prepared)?;
        if prepared.ops().is_empty() { return Ok(()); }
        let ops = delta::diff_revisions(&previous, prepared.value());
        self.publisher.publish(Arc::new(prepared.value().clone()), ops.into(), context)
    }
}
impl ReplicatedState<JsonValue> for MutableReplicatedStateImpl {
    fn value(&self) -> Option<Arc<JsonValue>> { Some(self.publisher.snapshot().0) }
    fn subscribe(&self, listener: StateListener<JsonValue>) -> Disposal { self.publisher.subscribe(listener) }
}
impl ReplicatedStateInternals for MutableReplicatedStateImpl {
    fn snapshot(&self) -> (Arc<JsonValue>, ServiceSequence) { self.publisher.snapshot() }
    fn subscribe_source(&self, listener: SourceListener) -> Unsubscribe { self.publisher.subscribe_source(listener) }
}
pub struct ReplicatedStateReplica {
    revision: Mutex<Option<(Arc<JsonValue>, ServiceSequence)>>,
    validator: Mutex<delta::revision_validator::JsonRevisionValidator>,
    subscribers: Arc<Mutex<Vec<Arc<Subscriber<JsonValue>>>>>,
    report: ErrorHandler,
}
impl ReplicatedStateReplica {
    pub fn new(report: ErrorHandler) -> Arc<Self> { Arc::new(Self { revision: Mutex::new(None), validator: Mutex::new(delta::revision_validator::JsonRevisionValidator::new()), subscribers: Arc::new(Mutex::new(Vec::new())), report }) }
    pub fn hydrate(&self, sequence: ServiceSequence, ops: &[Op], context: ContextRef) -> Result<(), ServiceError> {
        if !delta::is_base(ops) { self.clear(); return Err(contract("Replicated state snapshot is not a base operation batch")); }
        self.install(sequence, delta::apply_immutable(None, ops), context, ReplicatedStateDeliveryKind::Hydrate)
    }
    pub fn update(&self, sequence: ServiceSequence, ops: &[Op], context: ContextRef) -> Result<(), ServiceError> {
        let (value, previous) = lock(&self.revision).clone().ok_or_else(|| contract("Replicated state received an update before hydration"))?;
        if previous.next() != sequence { self.clear(); return Err(contract("Replicated state update sequence has a gap")); }
        self.install(sequence, delta::apply_immutable(Some(&value), ops), context, ReplicatedStateDeliveryKind::Update)
    }
    fn install(&self, sequence: ServiceSequence, result: delta::Result<JsonValue>, context: ContextRef, kind: ReplicatedStateDeliveryKind) -> Result<(), ServiceError> {
        let value = match result { Ok(v) => Arc::new(v), Err(error) => { self.clear(); return Err(error.into()); } };
        lock(&self.validator).validate(&value);
        *lock(&self.revision) = Some((value.clone(), sequence));
        let subscribers = lock(&self.subscribers).clone();
        for subscriber in &subscribers { subscriber.push(Frame { value: value.clone(), context: context.clone(), delivery: ReplicatedStateDelivery { kind, sequence } }); }
        for subscriber in subscribers { subscriber.drain(); }
        Ok(())
    }
    pub fn clear(&self) { *lock(&self.revision) = None; for s in lock(&self.subscribers).iter() { s.clear(); } }
}
impl ReplicatedState<JsonValue> for ReplicatedStateReplica {
    fn value(&self) -> Option<Arc<JsonValue>> { lock(&self.revision).as_ref().map(|(v, _)| v.clone()) }
    fn subscribe(&self, listener: StateListener<JsonValue>) -> Disposal {
        let subscriber = Subscriber::new(listener, self.report.clone());
        { let mut subscribers = lock(&self.subscribers); subscribers.retain(|subscriber| !subscriber.closed()); subscribers.push(subscriber.clone()); }
        let revision = lock(&self.revision).clone();
        if let Some((value, sequence)) = revision { subscriber.push(Frame { value, context: service_delivery_context(), delivery: ReplicatedStateDelivery { kind: ReplicatedStateDeliveryKind::Hydrate, sequence } }); }
        subscriber.drain();
        let subscribers = Arc::downgrade(&self.subscribers);
        Box::new(move || { subscriber.close(); if let Some(subscribers) = subscribers.upgrade() { lock(&subscribers).retain(|s| !Arc::ptr_eq(s, &subscriber)); } Box::pin(async { Ok(()) }) })
    }
}
pub struct AttachedReplicatedStateImpl<T> {
    publisher: Arc<Publisher<T>>,
    attachment: Mutex<Option<Box<dyn ReplicatedStateSourceAttachment<T>>>>,
    cursor: Mutex<i64>,
    disposed: AtomicBool,
    report: ErrorHandler,
}
impl<T: Send + Sync + 'static> ReplicatedState<T> for AttachedReplicatedStateImpl<T> {
    fn value(&self) -> Option<Arc<T>> { Some(self.publisher.snapshot().0) }
    fn subscribe(&self, listener: StateListener<T>) -> Disposal { self.publisher.subscribe(listener) }
}
impl<T: Send + Sync + 'static> AttachedReplicatedState<T> for AttachedReplicatedStateImpl<T> {
    fn dispose(&self) {
        if !self.disposed.swap(true, Ordering::AcqRel) {
            let attachment = lock(&self.attachment).take();
            if let Some(mut attachment) = attachment { attachment.dispose(); }
        }
    }
}
impl ReplicatedStateInternals for AttachedReplicatedStateImpl<JsonValue> {
    fn snapshot(&self) -> (Arc<JsonValue>, ServiceSequence) { self.publisher.snapshot() }
    fn subscribe_source(&self, listener: SourceListener) -> Unsubscribe { self.publisher.subscribe_source(listener) }
}
pub fn attach_replicated_state_source<T: Send + Sync + 'static>(source: &dyn ReplicatedStateSource<T>, options: ReplicatedStateSourceOptions) -> Result<Arc<AttachedReplicatedStateImpl<T>>, ServiceError> {
    let mut attachment = source.attach()?;
    let snapshot = attachment.snapshot();
    if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&snapshot.cursor) {
        let error = contract("Replicated state source snapshot cursor must be a safe integer");
        return match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attachment.dispose())) {
            Ok(()) => Err(error),
            Err(_) => Err(Box::new(super::errors::CollectedErrors { message: "Failed to attach replicated state source", errors: vec![error, contract("Source attachment disposal panicked")] })),
        };
    }
    let report = options.on_error.unwrap_or_else(default_reporter);
    let state = Arc::new(AttachedReplicatedStateImpl { publisher: Publisher::new(snapshot.value.clone(), report.clone()), cursor: Mutex::new(snapshot.cursor), attachment: Mutex::new(None), disposed: AtomicBool::new(false), report });
    let erased: Arc<dyn std::any::Any + Send + Sync> = state.clone();
    if let Ok(json_state) = erased.downcast::<AttachedReplicatedStateImpl<JsonValue>>() {
        let internals: Arc<dyn ReplicatedStateInternals> = json_state;
        register_replicated_state_internals(&internals, &internals);
    }
    let target = Arc::downgrade(&state);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attachment.activate(Box::new(move |frame| {
        let Some(state) = target.upgrade() else { return; };
        if state.disposed.load(Ordering::Acquire) { return; }
        let result = {
            let mut cursor = lock(&state.cursor);
            if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&frame.cursor) || cursor.checked_add(1) != Some(frame.cursor) { Err(contract("Replicated state source cursor has a gap")) }
            else { *cursor = frame.cursor; Ok(()) }
        };
        match result {
            Ok(()) => { if let Err(error) = state.publisher.publish(frame.value, frame.ops, frame.context) { report_error(&state.report, error); } }
            Err(error) => {
                let disposal = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.dispose()));
                let error = match disposal {
                    Ok(()) => error,
                    Err(_) => Box::new(super::errors::CollectedErrors { message: "Replicated state source contract failed", errors: vec![error, contract("Source attachment disposal panicked")] }) as ServiceError,
                };
                report_error(&state.report, error);
            }
        }
    }))));
    let result = match result { Ok(result) => result, Err(_) => Err(contract("Source attachment activation panicked")) };
    if let Err(error) = result {
        state.disposed.store(true, Ordering::Release);
        let disposal = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attachment.dispose()));
        return match disposal {
            Ok(()) => Err(error),
            Err(_) => Err(Box::new(super::errors::CollectedErrors { message: "Failed to attach replicated state source", errors: vec![error, contract("Source attachment disposal panicked")] })),
        };
    }
    if state.disposed.load(Ordering::Acquire) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attachment.dispose())).is_err() { report_error(&state.report, contract("Source attachment disposal panicked")); }
    }
    else { *lock(&state.attachment) = Some(attachment); }
    Ok(state)
}
