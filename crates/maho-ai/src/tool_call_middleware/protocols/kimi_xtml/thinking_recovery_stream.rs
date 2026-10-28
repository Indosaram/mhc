//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/thinking-recovery-stream.ts.

use crate::types::AssistantMessageEvent;
use crate::utils::event_stream::AssistantMessageEventStream;

use super::thinking_recovery::recover_kimi_xtml_thinking;

pub fn wrap_stream_with_kimi_thinking_recovery(
    inner_stream: AssistantMessageEventStream,
) -> AssistantMessageEventStream {
    let outer_stream = AssistantMessageEventStream::assistant();
    let outer_for_task = outer_stream.clone();
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
    let cancellation_source = inner_stream.clone();
    outer_stream.set_cancellation_handler(Box::new(move || {
        Box::pin(async move {
            let _ = cancel_tx.send(());
            cancellation_source.cancel().await
        })
    }));

    let task = tokio::spawn(async move {
        let mut partial = None;
        loop {
            let next = tokio::select! {
                biased;
                _ = &mut cancel_rx => {
                    let cancellation = inner_stream.cancel().await;
                    match inner_stream.result_if_ready() {
                        Some(Ok(message)) => outer_for_task.end(Some(recover_kimi_xtml_thinking(&message))),
                        Some(Err(error)) => outer_for_task.fail(error),
                        None => outer_for_task.end(partial.as_ref().map(recover_kimi_xtml_thinking)),
                    }
                    if let Err(error) = cancellation { outer_for_task.fail(error); }
                    return;
                }
                next = inner_stream.next() => next,
            };
            match next {
                Ok(Some(AssistantMessageEvent::Done { reason, message })) => {
                    let recovered = recover_kimi_xtml_thinking(&message);
                    outer_for_task.push(AssistantMessageEvent::Done {
                        reason,
                        message: recovered,
                    });
                    outer_for_task.end(None);
                    return;
                }
                Ok(Some(event @ AssistantMessageEvent::Error { .. })) => {
                    outer_for_task.push(event);
                    outer_for_task.end(None);
                    return;
                }
                Ok(Some(event)) => {
                    partial = Some(crate::utils::event_stream::extract_message(&event));
                    outer_for_task.push(event);
                }
                Ok(None) => {
                    match inner_stream.result_if_ready() {
                        Some(Ok(message)) => {
                            outer_for_task.end(Some(recover_kimi_xtml_thinking(&message)))
                        }
                        Some(Err(error)) => outer_for_task.fail(error),
                        None => {
                            outer_for_task.end(partial.as_ref().map(recover_kimi_xtml_thinking))
                        }
                    }
                    return;
                }
                Err(error) => {
                    outer_for_task.fail(error);
                    return;
                }
            }
        }
    });
    outer_stream.set_cancellation_task(task);

    outer_stream
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, DoneReason, StopReason, ThinkingContent, Usage};

    fn message(
        content: Vec<ContentBlock>,
        stop_reason: StopReason,
    ) -> crate::types::AssistantMessage {
        crate::types::AssistantMessage {
            error_cause: None,
            prompt_cache_breakpoint: None,
            prompt_cache_diagnostics: None,
            thinking_level: None,
            provider_diagnostic: None,
            content,
            api: "openai-completions".into(),
            provider: "openai".into(),
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
            timing: None,
        }
    }

    #[tokio::test]
    async fn recovers_thinking_on_the_done_event_and_forwards_other_events() {
        let inner = AssistantMessageEventStream::assistant();
        let outer = wrap_stream_with_kimi_thinking_recovery(inner.clone());

        let start_msg = message(vec![], StopReason::Pending);
        inner.push(AssistantMessageEvent::Start {
            partial: start_msg.clone(),
        });
        let done_msg = message(
            vec![ContentBlock::Thinking(ThinkingContent {
                thinking: "r<|open|>response<|sep|>answer".into(),
                ..ThinkingContent::default()
            })],
            StopReason::Stop,
        );
        inner.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: done_msg,
        });

        let first = outer.next().await.expect("first").expect("event");
        assert!(matches!(first, AssistantMessageEvent::Start { .. }));
        let second = outer.next().await.expect("second").expect("event");
        let AssistantMessageEvent::Done {
            message: recovered, ..
        } = second
        else {
            panic!("expected done event")
        };
        assert_eq!(recovered.content.len(), 2);

        assert_eq!(outer.next().await, Ok(None));
    }

    #[tokio::test]
    async fn propagates_error_events_without_recovery() {
        let inner = AssistantMessageEventStream::assistant();
        let outer = wrap_stream_with_kimi_thinking_recovery(inner.clone());

        let error_msg = message(vec![], StopReason::Error);
        inner.push(AssistantMessageEvent::Error {
            reason: crate::types::ErrorReason::Error,
            error: error_msg,
        });

        let event = outer.next().await.expect("event").expect("some");
        assert!(matches!(event, AssistantMessageEvent::Error { .. }));
        assert_eq!(outer.next().await, Ok(None));
    }
}
