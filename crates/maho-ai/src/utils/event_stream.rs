//! Port of senpi packages/ai/src/utils/event-stream.ts.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::{oneshot, watch};

use crate::types::{AssistantMessage, AssistantMessageEvent};

/// Error a stream was failed with (`fail(error)`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct StreamError {
    pub message: String,
}

impl StreamError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

type Waiter<T> = oneshot::Sender<Result<Option<T>, StreamError>>;

struct State<T, R> {
    queue: VecDeque<T>,
    waiting: VecDeque<Waiter<T>>,
    done: bool,
    error: Option<StreamError>,
    result: Option<Result<R, StreamError>>,
}

struct Inner<T, R> {
    state: Mutex<State<T, R>>,
    result_tx: watch::Sender<bool>,
    local_work_depth: AtomicUsize,
    is_complete: fn(&T) -> bool,
    extract_result: fn(&T) -> R,
}

/// Generic event stream for async iteration. Clones share the same stream.
pub struct EventStream<T, R = T> {
    inner: Arc<Inner<T, R>>,
}

impl<T, R> Clone for EventStream<T, R> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<T, R> std::fmt::Debug for EventStream<T, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream").finish_non_exhaustive()
    }
}

impl<T: Clone + Send + 'static, R: Clone + Send + 'static> EventStream<T, R> {
    pub fn new(is_complete: fn(&T) -> bool, extract_result: fn(&T) -> R) -> Self {
        let (result_tx, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    queue: VecDeque::new(),
                    waiting: VecDeque::new(),
                    done: false,
                    error: None,
                    result: None,
                }),
                result_tx,
                local_work_depth: AtomicUsize::new(0),
                is_complete,
                extract_result,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State<T, R>> {
        self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn settle(&self, state: &mut State<T, R>, result: Result<R, StreamError>) {
        if state.result.is_none() {
            state.result = Some(result);
            self.inner.result_tx.send_replace(true);
        }
    }

    /// Events queued and not yet consumed.
    pub fn queue(&self) -> Vec<T> {
        self.state().queue.iter().cloned().collect()
    }

    pub fn push(&self, event: T) {
        let mut state = self.state();
        if state.done {
            return;
        }
        if (self.inner.is_complete)(&event) {
            state.done = true;
            let result = (self.inner.extract_result)(&event);
            self.settle(&mut state, Ok(result));
        }
        let mut event = Some(event);
        while let Some(waiter) = state.waiting.pop_front() {
            let Some(value) = event.take() else { break };
            match waiter.send(Ok(Some(value))) {
                Ok(()) => return,
                Err(Ok(Some(returned))) => event = Some(returned),
                Err(_) => return,
            }
        }
        if let Some(value) = event {
            state.queue.push_back(value);
        }
    }

    pub fn end(&self, result: Option<R>) {
        let mut state = self.state();
        state.done = true;
        if let Some(result) = result {
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
    pub async fn next(&self) -> Result<Option<T>, StreamError> {
        let rx = {
            let mut state = self.state();
            if let Some(event) = state.queue.pop_front() {
                return Ok(Some(event));
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
    matches!(event, AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. })
}

fn extract_message(event: &AssistantMessageEvent) -> AssistantMessage {
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

impl AssistantMessageEventStream {
    pub fn assistant() -> Self {
        EventStream::new(is_terminal, extract_message)
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
        }
    }

    #[tokio::test]
    async fn queued_events_are_delivered_in_order_and_done_sets_result() {
        let stream = create_assistant_message_event_stream();
        stream.push(AssistantMessageEvent::Start { partial: message(StopReason::Pending) });
        stream.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message: message(StopReason::Stop) });
        stream.push(AssistantMessageEvent::Start { partial: message(StopReason::Pending) });
        stream.end(None);
        let events = stream.collect().await.expect("events");
        assert_eq!(events.len(), 2);
        assert_eq!(stream.result().await.expect("result").stop_reason, StopReason::Stop);
    }

    #[tokio::test]
    async fn waiting_consumer_receives_pushed_event() {
        let stream = create_assistant_message_event_stream();
        let consumer = stream.clone();
        let task = tokio::spawn(async move { consumer.next().await });
        tokio::task::yield_now().await;
        stream.push(AssistantMessageEvent::Error { reason: ErrorReason::Error, error: message(StopReason::Error) });
        let event = task.await.expect("join").expect("ok").expect("event");
        assert!(matches!(event, AssistantMessageEvent::Error { .. }));
        assert_eq!(stream.result().await.expect("result").stop_reason, StopReason::Error);
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
}
