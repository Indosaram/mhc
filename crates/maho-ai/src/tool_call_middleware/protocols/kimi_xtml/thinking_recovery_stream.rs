//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/thinking-recovery-stream.ts.

use crate::types::AssistantMessageEvent;
use crate::utils::event_stream::{AssistantMessageEventStream, StreamError};

use super::thinking_recovery::recover_kimi_xtml_thinking;

pub fn wrap_stream_with_kimi_thinking_recovery(inner_stream: AssistantMessageEventStream) -> AssistantMessageEventStream {
    let outer_stream = AssistantMessageEventStream::assistant();
    let outer_for_task = outer_stream.clone();

    tokio::spawn(async move {
        loop {
            match inner_stream.next().await {
                Ok(Some(AssistantMessageEvent::Done { reason, message })) => {
                    let recovered = recover_kimi_xtml_thinking(&message);
                    outer_for_task.push(AssistantMessageEvent::Done { reason, message: recovered });
                    outer_for_task.end(None);
                    return;
                }
                Ok(Some(event @ AssistantMessageEvent::Error { .. })) => {
                    outer_for_task.push(event);
                    outer_for_task.end(None);
                    return;
                }
                Ok(Some(event)) => outer_for_task.push(event),
                Ok(None) => {
                    let result = inner_stream.result().await;
                    match result {
                        Ok(message) => outer_for_task.end(Some(recover_kimi_xtml_thinking(&message))),
                        Err(error) => outer_for_task.fail(error),
                    }
                    return;
                }
                Err(error) => {
                    outer_for_task.fail(StreamError::new(error.message));
                    return;
                }
            }
        }
    });

    outer_stream
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, DoneReason, StopReason, ThinkingContent, Usage};

    fn message(content: Vec<ContentBlock>, stop_reason: StopReason) -> crate::types::AssistantMessage {
        crate::types::AssistantMessage {
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
        }
    }

    #[tokio::test]
    async fn recovers_thinking_on_the_done_event_and_forwards_other_events() {
        let inner = AssistantMessageEventStream::assistant();
        let outer = wrap_stream_with_kimi_thinking_recovery(inner.clone());

        let start_msg = message(vec![], StopReason::Pending);
        inner.push(AssistantMessageEvent::Start { partial: start_msg.clone() });
        let done_msg = message(vec![ContentBlock::Thinking(ThinkingContent { thinking: "r<|open|>response<|sep|>answer".into(), ..ThinkingContent::default() })], StopReason::Stop);
        inner.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message: done_msg });

        let first = outer.next().await.expect("first").expect("event");
        assert!(matches!(first, AssistantMessageEvent::Start { .. }));
        let second = outer.next().await.expect("second").expect("event");
        let AssistantMessageEvent::Done { message: recovered, .. } = second else { panic!("expected done event") };
        assert_eq!(recovered.content.len(), 2);

        assert_eq!(outer.next().await, Ok(None));
    }

    #[tokio::test]
    async fn propagates_error_events_without_recovery() {
        let inner = AssistantMessageEventStream::assistant();
        let outer = wrap_stream_with_kimi_thinking_recovery(inner.clone());

        let error_msg = message(vec![], StopReason::Error);
        inner.push(AssistantMessageEvent::Error { reason: crate::types::ErrorReason::Error, error: error_msg });

        let event = outer.next().await.expect("event").expect("some");
        assert!(matches!(event, AssistantMessageEvent::Error { .. }));
        assert_eq!(outer.next().await, Ok(None));
    }
}
