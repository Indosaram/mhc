//! Port of senpi packages/ai/src/utils/event-stream.ts.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tokio::sync::{oneshot, watch};

use crate::types::{AssistantMessage, AssistantMessageEvent};
use crate::utils::assistant_capture::{
    AssistantCapture, CaptureHint, CaptureSlot, Captured, FrozenProgress, ProgressPush, Record,
};
use crate::utils::provider_error::sanitize_provider_error_message;

/// Error a stream was failed with (`fail(error)`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct StreamError {
    pub message: String,
    pub kind: StreamErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamErrorKind {
    Other,
    ProviderFailure(crate::types::ProviderDiagnostic),
    OAuthRefreshUnavailable,
}

impl StreamError {
    pub fn diagnostic_from_status(status: Option<u16>) -> Option<crate::types::ProviderDiagnostic> {
        use crate::types::{
            ProviderDiagnostic, ProviderDiagnosticCategory as Category, ProviderDiagnosticEvidence,
        };
        let status = status.filter(|status| (400..=599).contains(status))?;
        Some(ProviderDiagnostic {
            category: match status {
                401 => Category::Auth,
                400 => Category::InvalidRequest,
                500..=599 => Category::ProviderUnavailable,
                _ => Category::Unknown,
            },
            http_status: Some(status),
            code: None,
            evidence: ProviderDiagnosticEvidence::StructuredStatus,
            resets_at: None,
        })
    }

    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: StreamErrorKind::Other,
        }
    }

    pub fn provider_failure(
        message: impl Into<String>,
        diagnostic: crate::types::ProviderDiagnostic,
    ) -> Self {
        Self {
            message: message.into(),
            kind: StreamErrorKind::ProviderFailure(diagnostic),
        }
    }

    pub fn oauth_refresh_unavailable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: StreamErrorKind::OAuthRefreshUnavailable,
        }
    }
}

type Waiter<T> = oneshot::Sender<Result<Option<Record<T>>, StreamError>>;
type CancellationHandler =
    Box<dyn FnOnce() -> crate::types::BoxFuture<'static, Result<(), StreamError>> + Send>;

struct State<T, R> {
    queue: VecDeque<Record<T>>,
    waiting: VecDeque<Waiter<T>>,
    /// Assistant capture bookkeeping; unused by non-assistant streams.
    capture: CaptureSlot,
    done: bool,
    error: Option<StreamError>,
    result: Option<Result<R, StreamError>>,
}

/// Runs under the queue lock before an event is queued: sanitize it and build its capture
/// (TS `AssistantMessageEventStream.push`). `None` on generic streams.
/// `captured` is true when the event already carries its source capture, which wins.
type PushHook<T> =
    fn(&mut T, &mut CaptureSlot, &CaptureHint, bool) -> Option<Arc<AssistantCapture>>;

struct Inner<T, R> {
    state: Mutex<State<T, R>>,
    push_hook: Option<PushHook<T>>,
    /// Applied to a final result passed to `end` (TS `AssistantMessageEventStream.end`).
    end_hook: Option<fn(&mut R)>,
    result_tx: watch::Sender<bool>,
    local_work_depth: AtomicUsize,
    cancellation_handler: Mutex<Option<CancellationHandler>>,
    cancellation: OnceLock<watch::Receiver<Option<Result<(), StreamError>>>>,
    is_complete: fn(&T) -> bool,
    extract_result: fn(&T) -> R,
}

/// Generic event stream for async iteration. Clones share the same stream.
pub struct EventStream<T, R = T> {
    inner: Arc<Inner<T, R>>,
}

impl<T, R> Clone for EventStream<T, R> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T, R> std::fmt::Debug for EventStream<T, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream").finish_non_exhaustive()
    }
}

impl<T: Clone + Send + 'static, R: Clone + Send + 'static> EventStream<T, R> {
    pub fn new(is_complete: fn(&T) -> bool, extract_result: fn(&T) -> R) -> Self {
        Self::with_hooks(is_complete, extract_result, None, None)
    }

    fn with_hooks(
        is_complete: fn(&T) -> bool,
        extract_result: fn(&T) -> R,
        push_hook: Option<PushHook<T>>,
        end_hook: Option<fn(&mut R)>,
    ) -> Self {
        let (result_tx, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                push_hook,
                end_hook,
                state: Mutex::new(State {
                    queue: VecDeque::new(),
                    waiting: VecDeque::new(),
                    capture: CaptureSlot::default(),
                    done: false,
                    error: None,
                    result: None,
                }),
                result_tx,
                local_work_depth: AtomicUsize::new(0),
                cancellation_handler: Mutex::new(None),
                cancellation: OnceLock::new(),
                is_complete,
                extract_result,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State<T, R>> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_cancellation_handler(&self, handler: CancellationHandler) {
        *self
            .inner
            .cancellation_handler
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(handler);
    }

    pub async fn cancel(&self) -> Result<(), StreamError> {
        let mut completion = self
            .inner
            .cancellation
            .get_or_init(|| {
                let handler = self
                    .inner
                    .cancellation_handler
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                let stream = self.clone();
                let (completed, completion) = watch::channel(None);
                tokio::spawn(async move {
                    let result = match handler {
                        Some(handler) => handler().await,
                        None => Ok(()),
                    };
                    match &result {
                        Ok(()) => stream.end(None),
                        Err(error) => stream.fail(error.clone()),
                    }
                    completed.send_replace(Some(result));
                });
                completion
            })
            .clone();
        loop {
            if let Some(result) = completion.borrow().clone() {
                return result;
            }
            if completion.changed().await.is_err() {
                return Err(StreamError::new(
                    "Cancellation task ended without acknowledgement",
                ));
            }
        }
    }

    /// Join the owned relay after its cancellation handler, including terminal streams.
    /// Install the handler first and attach the task before exposing the stream.
    pub fn set_cancellation_task(&self, task: tokio::task::JoinHandle<()>) {
        let handler = self
            .inner
            .cancellation_handler
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        self.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                let cancellation = match handler {
                    Some(handler) => handler().await,
                    None => Ok(()),
                };
                let joined = task.await.map_err(|error| {
                    StreamError::new(format!("Stream relay task failed: {error}"))
                });
                cancellation.and(joined)
            })
        }));
    }

    /// Snapshot a settled result without waiting for an `end(None)` stream.
    pub fn result_if_ready(&self) -> Option<Result<R, StreamError>> {
        self.state().result.clone()
    }

    /// Relay ownership, not just events: abandoning the target closes the source first.
    /// This is for transparent forwarding only; projecting wrappers finalize their own state.
    pub fn forward_cancellation_from(&self, source: Self) {
        self.forward_cancellation_after(async move { Some(source) });
    }

    pub fn forward_cancellation_after(
        &self,
        source: impl Future<Output = Option<Self>> + Send + 'static,
    ) {
        let target = Arc::downgrade(&self.inner);
        self.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                let Some(source) = source.await else {
                    return Ok(());
                };
                let cancellation = source.cancel().await;
                let Some(inner) = target.upgrade() else {
                    return cancellation;
                };
                let target = Self { inner };
                match &cancellation {
                    Ok(()) => {
                        let result = source.state().result.clone();
                        match result {
                            Some(Ok(result)) => target.end(Some(result)),
                            Some(Err(error)) => target.fail(error),
                            None => target.end(None),
                        }
                    }
                    Err(error) => target.fail(error.clone()),
                }
                cancellation
            })
        }));
    }

    fn settle(&self, state: &mut State<T, R>, result: Result<R, StreamError>) {
        if state.result.is_none() {
            state.result = Some(result);
            self.inner.result_tx.send_replace(true);
        }
    }

    /// Events queued and not yet consumed (progress records are internal and never listed).
    pub fn queue(&self) -> Vec<T> {
        self.state()
            .queue
            .iter()
            .filter_map(|entry| match entry {
                Record::Event(event) => Some(event.value.clone()),
                Record::Progress(_) => None,
            })
            .collect()
    }

    pub fn push(&self, event: T) {
        self.push_with_hint(event, CaptureHint::default());
    }

    /// `push(event, hint)`: the hint describes the producer-time frontier of a captured block.
    pub fn push_with_hint(&self, event: T, hint: CaptureHint) {
        self.push_captured(Captured::bare(event), hint);
    }

    /// Push an event that may already carry its source capture (a relay). A carried capture wins
    /// over a fresh one (TS `captures.has(event)`); otherwise the stream captures it itself.
    pub fn push_captured(&self, event: Captured<T>, hint: CaptureHint) {
        let mut state = self.state();
        if state.done {
            return;
        }
        let Captured {
            value: mut event,
            capture: carried,
        } = event;
        let fresh = match self.inner.push_hook {
            // Classify raw provider errors and redact before capture or publication (senpi#4282).
            Some(hook) => hook(&mut event, &mut state.capture, &hint, carried.is_some()),
            None => None,
        };
        let event = Captured {
            value: event,
            capture: carried.or(fresh),
        };
        if (self.inner.is_complete)(&event.value) {
            state.done = true;
            let result = (self.inner.extract_result)(&event.value);
            self.settle(&mut state, Ok(result));
        }
        Self::deliver(&mut state, Record::Event(event));
    }

    fn deliver(state: &mut State<T, R>, entry: Record<T>) {
        let mut entry = Some(entry);
        while let Some(waiter) = state.waiting.pop_front() {
            let Some(value) = entry.take() else { break };
            match waiter.send(Ok(Some(value))) {
                Ok(()) => return,
                Err(Ok(Some(returned))) => entry = Some(returned),
                Err(_) => return,
            }
        }
        if let Some(value) = entry {
            state.queue.push_back(value);
        }
    }

    /// `enqueueProgress`: a frozen progress record takes its place in the ordered queue.
    fn enqueue_progress(&self, progress: ProgressPush) {
        let mut state = self.state();
        if state.done {
            return;
        }
        if let Some(frozen) = state.capture.freeze_progress(progress) {
            Self::deliver(&mut state, Record::Progress(frozen));
        }
    }

    pub(crate) fn push_progress(&self, progress: ProgressPush) {
        self.enqueue_progress(progress);
    }

    pub(crate) fn with_capture_slot(&self, f: impl FnOnce(&mut CaptureSlot)) {
        f(&mut self.state().capture);
    }

    pub fn end(&self, result: Option<R>) {
        let mut state = self.state();
        state.done = true;
        if let Some(mut result) = result {
            if let Some(hook) = self.inner.end_hook {
                hook(&mut result);
            }
            self.settle(&mut state, Ok(result));
        }
        for waiter in state.waiting.drain(..) {
            let _ = waiter.send(Ok(None));
        }
    }

    pub fn fail(&self, error: StreamError) {
        let mut state = self.state();
        if state.done {
            return;
        }
        state.done = true;
        state.error = Some(error.clone());
        self.settle(&mut state, Err(error.clone()));
        for waiter in state.waiting.drain(..) {
            let _ = waiter.send(Err(error.clone()));
        }
    }

    /// Next event; `Ok(None)` once the stream ended, `Err` once it failed and the queue drained.
    /// The public iterator is the capture downgrade boundary: it skips progress records and never
    /// yields a capture.
    pub async fn next(&self) -> Result<Option<T>, StreamError> {
        loop {
            match self.next_record().await? {
                None => return Ok(None),
                Some(Record::Event(event)) => return Ok(Some(event.value)),
                Some(Record::Progress(_)) => {}
            }
        }
    }

    /// Next ordered record (event with its capture, or progress) for internal readers
    /// (`assistant_records` / `assistant_events`).
    pub async fn next_record(&self) -> Result<Option<Record<T>>, StreamError> {
        let rx = {
            let mut state = self.state();
            if let Some(entry) = state.queue.pop_front() {
                return Ok(Some(entry));
            }
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            if state.done {
                return Ok(None);
            }
            let (tx, rx) = oneshot::channel();
            state.waiting.push_back(tx);
            rx
        };
        rx.await.unwrap_or(Ok(None))
    }

    /// Drains the stream, collecting every event until it ends or fails.
    pub async fn collect(&self) -> Result<Vec<T>, StreamError> {
        let mut events = Vec::new();
        while let Some(event) = self.next().await? {
            events.push(event);
        }
        Ok(events)
    }

    /// Final result. Pending forever if the stream ends without one, as in TS.
    pub async fn result(&self) -> Result<R, StreamError> {
        let mut rx = self.inner.result_tx.subscribe();
        loop {
            if let Some(result) = self.state().result.clone() {
                return result;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    /// Track locally executing work during which the stream legitimately emits no events.
    pub async fn track_local_work<W>(&self, work: impl Future<Output = W>) -> W {
        struct Guard<'a>(&'a AtomicUsize);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        self.inner.local_work_depth.fetch_add(1, Ordering::SeqCst);
        let _guard = Guard(&self.inner.local_work_depth);
        work.await
    }

    pub fn has_pending_local_work(&self) -> bool {
        self.inner.local_work_depth.load(Ordering::SeqCst) > 0
    }
}

fn is_terminal(event: &AssistantMessageEvent) -> bool {
    matches!(
        event,
        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
    )
}

pub(crate) fn extract_message(event: &AssistantMessageEvent) -> AssistantMessage {
    match event {
        AssistantMessageEvent::Done { message, .. } => message.clone(),
        AssistantMessageEvent::Error { error, .. } => error.clone(),
        AssistantMessageEvent::Start { partial }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => partial.clone(),
    }
}

pub type AssistantMessageEventStream = EventStream<AssistantMessageEvent, AssistantMessage>;

fn sanitize_event(event: &mut AssistantMessageEvent) {
    match event {
        AssistantMessageEvent::Done { message, .. } => sanitize_provider_error_message(message),
        AssistantMessageEvent::Error { error, .. } => sanitize_provider_error_message(error),
        AssistantMessageEvent::Start { partial }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => {
            sanitize_provider_error_message(partial)
        }
    }
}

fn assistant_push_hook(
    event: &mut AssistantMessageEvent,
    capture: &mut CaptureSlot,
    hint: &CaptureHint,
    captured: bool,
) -> Option<Arc<AssistantCapture>> {
    sanitize_event(event);
    if captured {
        capture.decide();
        return None;
    }
    match capture.relay_hint(event, hint) {
        Some(relay) => capture.capture_event(event, &relay),
        None => capture.capture_event(event, hint),
    }
}

impl AssistantMessageEventStream {
    pub fn assistant() -> Self {
        EventStream::with_hooks(
            is_terminal,
            extract_message,
            Some(assistant_push_hook),
            Some(sanitize_provider_error_message),
        )
    }

    /// `new AssistantMessageEventStream({ captureBlocks })`.
    pub fn with_capture_blocks(capture_blocks: bool) -> Self {
        let stream = Self::assistant();
        if capture_blocks {
            stream.with_capture_slot(CaptureSlot::enable);
        }
        stream
    }

    /// Runs a projection step that copies `source` 1:1 to a new index. The copy is built from
    /// source blocks read after later deltas landed, so its capture relays the source's
    /// emission-time record instead of freezing the copy. Only the step's first push of the
    /// source's type is that copy; any later one records nothing.
    pub fn relaying<V>(
        &self,
        source: std::mem::Discriminant<AssistantMessageEvent>,
        capture: Option<Arc<AssistantCapture>>,
        project: impl FnOnce() -> V,
    ) -> V {
        struct EndRelay<'a>(&'a AssistantMessageEventStream);
        impl Drop for EndRelay<'_> {
            fn drop(&mut self) {
                self.0.with_capture_slot(CaptureSlot::end_relay);
            }
        }
        self.with_capture_slot(|slot| slot.begin_relay(source, capture));
        let _end = EndRelay(self);
        project()
    }

    /// Relay a frozen progress record another stream captured, unchanged.
    pub fn push_frozen_progress(&self, progress: FrozenProgress) {
        self.enqueue_progress(ProgressPush::Frozen(progress));
    }
}

/// Factory function for AssistantMessageEventStream (for use in extensions).
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::assistant()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DoneReason, ErrorReason, StopReason, Usage};

    fn message(stop_reason: StopReason) -> AssistantMessage {
        AssistantMessage {
            error_cause: None,
            prompt_cache_breakpoint: None,
            prompt_cache_diagnostics: None,
            thinking_level: None,
            provider_diagnostic: None,
            content: Vec::new(),
            api: "faux".into(),
            provider: "faux".into(),
            model: "m".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
            failure_source: None,
            credential_served_recently: None,
            timing: None,
        }
    }

    #[tokio::test]
    async fn terminal_cancellation_joins_relay_and_preserves_result() {
        let stream: EventStream<u32, u32> = EventStream::new(|value| *value == 9, |value| *value);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        stream.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                entered_tx.send(()).expect("entered subscriber");
                release_rx.await.expect("release");
                Ok(())
            })
        }));
        let resource = Arc::new(());
        let owned = resource.clone();
        let (exit_tx, exit_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _owned = owned;
            exit_rx.await.expect("relay exit");
        });
        stream.set_cancellation_task(task);
        stream.push(1);
        stream.push(9);
        let bound = std::time::Duration::from_secs(5);
        let mut cancellation = Box::pin(stream.cancel());
        assert!(futures::poll!(cancellation.as_mut()).is_pending());
        tokio::time::timeout(bound, entered_rx)
            .await
            .expect("entered deadline")
            .expect("entered");
        release_tx.send(()).expect("release subscriber");
        assert!(futures::poll!(cancellation.as_mut()).is_pending());
        exit_tx.send(()).expect("relay subscriber");
        tokio::time::timeout(bound, cancellation)
            .await
            .expect("cancel deadline")
            .expect("cancel");
        assert_eq!(Arc::strong_count(&resource), 1);
        assert_eq!(stream.collect().await.expect("events"), vec![1, 9]);
        assert_eq!(stream.result_if_ready(), Some(Ok(9)));
    }

    #[tokio::test]
    async fn dropped_cancellation_waiter_does_not_lose_cleanup() {
        let stream: EventStream<u32, u32> = EventStream::new(|_| false, |value| *value);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        stream.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                entered_tx.send(()).expect("entered subscriber");
                release_rx.await.expect("release");
                Ok(())
            })
        }));
        let bound = std::time::Duration::from_secs(5);
        let mut first = Box::pin(stream.cancel());
        assert!(futures::poll!(first.as_mut()).is_pending());
        tokio::time::timeout(bound, entered_rx)
            .await
            .expect("entered deadline")
            .expect("entered");
        drop(first);
        let mut second = Box::pin(stream.cancel());
        assert!(futures::poll!(second.as_mut()).is_pending());
        release_tx.send(()).expect("release subscriber");
        tokio::time::timeout(bound, second)
            .await
            .expect("cancel deadline")
            .expect("cancel");
        assert_eq!(stream.next().await, Ok(None));
    }

    #[tokio::test]
    async fn terminal_failure_keeps_original_result_and_reports_cleanup_failure() {
        let stream: EventStream<u32, u32> = EventStream::new(|_| false, |value| *value);
        let failure = StreamError::new("provider failure");
        let cleanup = StreamError::new("cleanup failure");
        let error = cleanup.clone();
        stream.set_cancellation_handler(Box::new(move || Box::pin(async move { Err(error) })));
        stream.fail(failure.clone());
        assert_eq!(stream.cancel().await, Err(cleanup));
        assert_eq!(stream.result_if_ready(), Some(Err(failure)));
    }

    #[tokio::test]
    async fn forwarding_cancellation_waits_for_setup_and_inner_close() {
        let outer = create_assistant_message_event_stream();
        let inner = create_assistant_message_event_stream();
        let (source_tx, source_rx) = oneshot::channel();
        outer.forward_cancellation_after(async move { source_rx.await.ok() });
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let final_message = message(StopReason::Error);
        let final_inner = inner.clone();
        let expected = final_message.clone();
        inner.set_cancellation_handler(Box::new(move || {
            Box::pin(async move {
                let _ = entered_tx.send(());
                release_rx.await.expect("release");
                final_inner.end(Some(final_message));
                Ok(())
            })
        }));
        let cancel_outer = outer.clone();
        let task = tokio::spawn(async move { cancel_outer.cancel().await });
        source_tx.send(inner).expect("setup subscriber");
        let bound = std::time::Duration::from_secs(5);
        tokio::time::timeout(bound, entered_rx)
            .await
            .expect("entered deadline")
            .expect("entered");
        assert!(!task.is_finished());
        assert!(!outer.state().done);
        release_tx.send(()).expect("release subscriber");
        tokio::time::timeout(bound, task)
            .await
            .expect("close deadline")
            .expect("task")
            .expect("close");
        assert_eq!(outer.result().await, Ok(expected));
    }

    #[tokio::test]
    async fn forwarding_cancellation_preserves_inner_close_failure() {
        let outer = create_assistant_message_event_stream();
        let inner = create_assistant_message_event_stream();
        let error = StreamError::new("inner close failed");
        let failure = error.clone();
        inner.set_cancellation_handler(Box::new(move || Box::pin(async move { Err(failure) })));
        outer.forward_cancellation_from(inner);
        assert_eq!(outer.cancel().await, Err(error.clone()));
        assert_eq!(outer.result().await, Err(error));
    }

    #[tokio::test]
    async fn queued_events_are_delivered_in_order_and_done_sets_result() {
        let stream = create_assistant_message_event_stream();
        stream.push(AssistantMessageEvent::Start {
            partial: message(StopReason::Pending),
        });
        stream.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: message(StopReason::Stop),
        });
        stream.push(AssistantMessageEvent::Start {
            partial: message(StopReason::Pending),
        });
        stream.end(None);
        let events = stream.collect().await.expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(
            stream.result().await.expect("result").stop_reason,
            StopReason::Stop
        );
    }

    #[tokio::test]
    async fn waiting_consumer_receives_pushed_event() {
        let stream = create_assistant_message_event_stream();
        let consumer = stream.clone();
        let task = tokio::spawn(async move { consumer.next().await });
        tokio::task::yield_now().await;
        stream.push(AssistantMessageEvent::Error {
            reason: ErrorReason::Error,
            error: message(StopReason::Error),
        });
        let event = task.await.expect("join").expect("ok").expect("event");
        assert!(matches!(event, AssistantMessageEvent::Error { .. }));
        assert_eq!(
            stream.result().await.expect("result").stop_reason,
            StopReason::Error
        );
        assert_eq!(stream.next().await, Ok(None));
    }

    #[tokio::test]
    async fn fail_rejects_waiters_and_result() {
        let stream: EventStream<u32, u32> = EventStream::new(|_| false, |v| *v);
        let consumer = stream.clone();
        let task = tokio::spawn(async move { consumer.next().await });
        tokio::task::yield_now().await;
        stream.fail(StreamError::new("boom"));
        assert_eq!(task.await.expect("join"), Err(StreamError::new("boom")));
        assert_eq!(stream.result().await, Err(StreamError::new("boom")));
        stream.push(1);
        assert!(stream.queue().is_empty());
    }

    #[tokio::test]
    async fn typed_failures_survive_iteration_and_result() {
        let diagnostic = StreamError::diagnostic_from_status(Some(401)).expect("status diagnostic");
        for error in [
            StreamError::provider_failure("denied", diagnostic),
            StreamError::oauth_refresh_unavailable("refresh unavailable"),
        ] {
            let stream: EventStream<u32, u32> = EventStream::new(|_| false, |value| *value);
            let mut next = Box::pin(stream.next());
            assert!(futures::poll!(next.as_mut()).is_pending());
            stream.fail(error.clone());
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(1), next)
                    .await
                    .expect("bounded iterator"),
                Err(error.clone())
            );
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(1), stream.result())
                    .await
                    .expect("bounded result"),
                Err(error)
            );
        }
    }

    #[tokio::test]
    async fn end_with_result_and_local_work_tracking() {
        let stream: EventStream<u32, u32> = EventStream::new(|_| false, |v| *v);
        let observer = stream.clone();
        let seen = stream
            .track_local_work(async move { observer.has_pending_local_work() })
            .await;
        assert!(seen);
        assert!(!stream.has_pending_local_work());
        stream.end(Some(9));
        assert_eq!(stream.result().await, Ok(9));
    }

    // event-stream.test.ts: "queues adjacent delta events immediately without throttling or merging"
    #[tokio::test]
    async fn queues_adjacent_delta_events_immediately_without_throttling_or_merging() {
        let stream = create_assistant_message_event_stream();
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "a".into(),
            partial: message(StopReason::Pending),
        });
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "b".into(),
            partial: message(StopReason::Pending),
        });
        let queue = stream.queue();
        assert_eq!(queue.len(), 2);
        match &queue[0] {
            AssistantMessageEvent::TextDelta { delta, .. } => assert_eq!(delta, "a"),
            other => panic!("expected text_delta, got {other:?}"),
        }
        match &queue[1] {
            AssistantMessageEvent::TextDelta { delta, .. } => assert_eq!(delta, "b"),
            other => panic!("expected text_delta, got {other:?}"),
        }
    }

    // event-stream.test.ts: "preserves FIFO across the compaction boundary when more events are pushed later"
    #[tokio::test]
    async fn preserves_fifo_across_the_compaction_boundary_when_more_events_are_pushed_later() {
        let stream: EventStream<i64, i64> = EventStream::new(|n| *n == -1, |n| *n);
        let total = 3000i64;
        for i in 0..1500 {
            stream.push(i);
        }
        let mut seen = Vec::new();
        for _ in 0..1300 {
            let value = stream.next().await.expect("ok").expect("value");
            seen.push(value);
        }
        for i in 1500..total {
            stream.push(i);
        }
        stream.push(total);
        stream.end(Some(total));
        while let Some(value) = stream.next().await.expect("ok") {
            seen.push(value);
        }
        assert_eq!(seen.len(), (total + 1) as usize);
        for (i, value) in seen.iter().enumerate() {
            assert_eq!(*value, i as i64);
        }
        assert_eq!(stream.result().await, Ok(total));
    }

    // event-stream.test.ts: "returns a defensive queue snapshot that cannot desync internal state"
    #[tokio::test]
    async fn returns_a_defensive_queue_snapshot_that_cannot_desync_internal_state() {
        let stream: EventStream<i64, i64> = EventStream::new(|_| false, |n| *n);
        for i in 0..5 {
            stream.push(i);
        }
        let mut snapshot = stream.queue();
        assert_eq!(snapshot.len(), 5);
        snapshot.clear();
        for i in 0..5 {
            let value = stream.next().await.expect("ok").expect("value");
            assert_eq!(value, i);
        }
        assert!(stream.queue().is_empty());
    }

    // event-stream.test.ts: "preserves order when events arrive after buffered draining starts"
    #[tokio::test]
    async fn preserves_order_when_events_arrive_after_buffered_draining_starts() {
        let stream: EventStream<i64, i64> = EventStream::new(|_| false, |n| *n);
        stream.push(1);
        stream.push(2);
        assert_eq!(stream.next().await, Ok(Some(1)));
        stream.push(3);
        assert_eq!(stream.next().await, Ok(Some(2)));
        assert_eq!(stream.next().await, Ok(Some(3)));
        stream.end(Some(3));
        assert_eq!(stream.next().await, Ok(None));
    }

    // event-stream.test.ts: "delivers events to waiting consumers in registration order"
    #[tokio::test]
    async fn delivers_events_to_waiting_consumers_in_registration_order() {
        let stream: EventStream<i64, i64> = EventStream::new(|_| false, |n| *n);
        let first_consumer = stream.clone();
        let second_consumer = stream.clone();
        let first_task = tokio::spawn(async move { first_consumer.next().await });
        tokio::task::yield_now().await;
        let second_task = tokio::spawn(async move { second_consumer.next().await });
        tokio::task::yield_now().await;
        stream.push(1);
        stream.push(2);
        assert_eq!(first_task.await.expect("join"), Ok(Some(1)));
        assert_eq!(second_task.await.expect("join"), Ok(Some(2)));
    }

    // event-stream.test.ts: "wakes all waiting consumers when ended without a result"
    #[tokio::test]
    async fn wakes_all_waiting_consumers_when_ended_without_a_result() {
        let stream: EventStream<i64, i64> = EventStream::new(|_| false, |n| *n);
        let first_consumer = stream.clone();
        let second_consumer = stream.clone();
        let first_task = tokio::spawn(async move { first_consumer.next().await });
        tokio::task::yield_now().await;
        let second_task = tokio::spawn(async move { second_consumer.next().await });
        tokio::task::yield_now().await;
        stream.end(None);
        assert_eq!(first_task.await.expect("join"), Ok(None));
        assert_eq!(second_task.await.expect("join"), Ok(None));
    }
}
