//! Port of senpi packages/ai/src/api/responses-completion-grace.ts.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::Stream;

pub const RESPONSES_COMPLETION_GRACE_MS: u64 = 60_000;

pub fn format_responses_completion_stall(grace_ms: u64) -> String {
    format!("Provider stream stalled after the last output item: response.completed timed out after {grace_ms}ms")
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", format_responses_completion_stall(*grace_ms))]
pub struct ResponsesCompletionStallError {
    pub grace_ms: u64,
}

pub trait TypedEvent {
    fn event_type(&self) -> &str;
}

/// Wraps a Responses event stream so that, in the all-items-done phase, waiting for the next event is
/// bounded by `grace_ms`. On expiry the wrapper yields [`ResponsesCompletionStallError`] and drops
/// the source, which is the Rust equivalent of releasing the async generator without awaiting it.
pub struct ResponsesCompletionGrace<S> {
    source: Pin<Box<S>>,
    grace_ms: u64,
    open_items: usize,
    saw_item_done: bool,
    finished: bool,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
}

pub fn with_responses_completion_grace<S>(source: S, grace_ms: u64) -> ResponsesCompletionGrace<S> {
    ResponsesCompletionGrace {
        source: Box::pin(source),
        grace_ms,
        open_items: 0,
        saw_item_done: false,
        finished: false,
        deadline: None,
    }
}

impl<S, T, E> Stream for ResponsesCompletionGrace<S>
where
    S: Stream<Item = Result<T, E>>,
    T: TypedEvent,
{
    type Item = Result<T, ResponsesCompletionStallError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        let bounded = this.saw_item_done && this.open_items == 0 && this.grace_ms > 0;
        if bounded {
            let deadline = this
                .deadline
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(Duration::from_millis(this.grace_ms))));
            if deadline.as_mut().poll(cx).is_ready() {
                this.finished = true;
                return Poll::Ready(Some(Err(ResponsesCompletionStallError { grace_ms: this.grace_ms })));
            }
        }

        match this.source.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                match event.event_type() {
                    "response.output_item.added" => this.open_items += 1,
                    "response.output_item.done" => {
                        this.open_items = this.open_items.saturating_sub(1);
                        this.saw_item_done = true;
                    }
                    _ => {}
                }
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(_))) | Poll::Ready(None) => {
                this.finished = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use futures::StreamExt;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Event(String);

    impl TypedEvent for Event {
        fn event_type(&self) -> &str {
            &self.0
        }
    }

    fn events(names: &[&str]) -> impl Stream<Item = Result<Event, ()>> {
        let items: Vec<Result<Event, ()>> = names.iter().map(|name| Ok(Event((*name).to_owned()))).collect();
        stream::iter(items)
    }

    fn receiver_stream(
        receiver: tokio::sync::mpsc::UnboundedReceiver<Result<Event, ()>>,
    ) -> impl Stream<Item = Result<Event, ()>> {
        stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|item| (item, receiver))
        })
    }

    #[test]
    fn stall_error_message_matches_the_ts_format() {
        assert_eq!(
            format_responses_completion_stall(60_000),
            "Provider stream stalled after the last output item: response.completed timed out after 60000ms"
        );
    }

    #[tokio::test]
    async fn events_before_the_last_item_done_are_forwarded() {
        let source = events(&[
            "response.output_item.added",
            "response.output_text.delta",
            "response.output_item.done",
            "response.completed",
        ]);
        let collected: Vec<Event> = with_responses_completion_grace(source, RESPONSES_COMPLETION_GRACE_MS)
            .map(|item| item.expect("ok"))
            .collect()
            .await;
        assert_eq!(collected.len(), 4);
        assert_eq!(collected[3].0, "response.completed");
    }

    #[tokio::test]
    async fn a_stalled_terminal_event_reports_the_stall_error() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Event, ()>>();
        let mut guarded = Box::pin(with_responses_completion_grace(receiver_stream(rx), 20));

        tx.send(Ok(Event("response.output_item.added".into()))).expect("send");
        tx.send(Ok(Event("response.output_item.done".into()))).expect("send");
        assert_eq!(guarded.next().await.expect("event").expect("ok").0, "response.output_item.added");
        assert_eq!(guarded.next().await.expect("event").expect("ok").0, "response.output_item.done");

        let stalled = tokio::time::timeout(Duration::from_secs(5), guarded.next())
            .await
            .expect("bounded wait")
            .expect("item")
            .expect_err("stall");
        assert_eq!(stalled.grace_ms, 20);
        assert!(guarded.next().await.is_none());
    }

    #[tokio::test]
    async fn an_open_item_keeps_the_wait_unbounded() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Event, ()>>();
        let mut guarded = Box::pin(with_responses_completion_grace(receiver_stream(rx), 20));

        tx.send(Ok(Event("response.output_item.added".into()))).expect("send");
        assert_eq!(guarded.next().await.expect("event").expect("ok").0, "response.output_item.added");

        let next = tokio::time::timeout(Duration::from_millis(80), guarded.next()).await;
        assert!(next.is_err(), "an open item must not be bounded by the completion grace");

        tx.send(Ok(Event("response.output_item.done".into()))).expect("send");
        assert_eq!(guarded.next().await.expect("event").expect("ok").0, "response.output_item.done");
    }
}
