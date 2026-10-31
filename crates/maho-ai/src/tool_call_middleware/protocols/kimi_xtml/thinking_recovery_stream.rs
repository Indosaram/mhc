//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/thinking-recovery-stream.ts.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, ThinkingContent};
use crate::utils::assistant_capture::{
    CaptureHint, assistant_events, carry_assistant_capture, event_content_index,
};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::thinking_recovery::recover_kimi_xtml_thinking;

/// Live events keep source indices; only the terminal is rewritten. A record (event capture or
/// control) crosses only while its source index names an existing block that no earlier split can
/// move; otherwise it is dropped, never guessed.
#[derive(Default)]
struct RelayIndexes {
    partial: Option<AssistantMessage>,
    /// Ended thinking index -> whether the terminal rewrite inserts a recovered response block
    /// after it.
    splits_by_thinking_index: HashMap<usize, bool>,
    /// Blocks that lost a record once lose all later ones, so a block never carries records
    /// without its start.
    dropped_indexes: HashSet<usize>,
}

impl RelayIndexes {
    fn keeps_index(&mut self, content_index: usize) -> bool {
        let Some(partial) = &self.partial else {
            return false;
        };
        if content_index >= partial.content.len() || self.dropped_indexes.contains(&content_index) {
            return false;
        }
        for index in 0..content_index {
            if matches!(partial.content.get(index), Some(ContentBlock::Thinking(_)))
                && self.splits_by_thinking_index.get(&index) != Some(&false)
            {
                self.dropped_indexes.insert(content_index);
                return false;
            }
        }
        true
    }
}

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
        let indexes = Arc::new(Mutex::new(RelayIndexes::default()));
        let progress_indexes = indexes.clone();
        let progress_sink = outer_for_task.clone();
        // Every event but the terminal crosses unchanged: same events, same indices, controls in
        // order.
        let mut events = assistant_events(&inner_stream, move |progress| {
            let keeps = progress_indexes
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .keeps_index(progress.content_index());
            if keeps {
                progress_sink.push_frozen_progress(progress);
            }
        });
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
                next = events.next() => next,
            };
            let mut event = match next {
                Ok(Some(event)) => event,
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
            };
            {
                let mut indexes = indexes.lock().unwrap_or_else(|error| error.into_inner());
                if let Some(event_partial) = event_partial(&event.value) {
                    indexes.partial = Some(event_partial.clone());
                }
                // The same event crosses, so public bytes stay identical; only its record is
                // withheld.
                if let Some(content_index) = event_content_index(&event.value)
                    && !indexes.keeps_index(content_index)
                {
                    event.capture = None;
                }
                if let AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content,
                    partial,
                } = &event.value
                {
                    let alone = AssistantMessage {
                        content: vec![ContentBlock::Thinking(ThinkingContent {
                            thinking: content.clone(),
                            ..ThinkingContent::default()
                        })],
                        ..partial.clone()
                    };
                    indexes.splits_by_thinking_index.insert(
                        *content_index,
                        recover_kimi_xtml_thinking(&alone).content.len() > 1,
                    );
                }
            }
            match &event.value {
                AssistantMessageEvent::Done { reason, message } => {
                    let recovered = recover_kimi_xtml_thinking(message);
                    let done = AssistantMessageEvent::Done {
                        reason: *reason,
                        message: recovered.clone(),
                    };
                    outer_for_task.push_captured(
                        carry_assistant_capture(&event, done, Some(&recovered)),
                        CaptureHint::default(),
                    );
                    outer_for_task.end(None);
                    return;
                }
                AssistantMessageEvent::Error { .. } => {
                    outer_for_task.push_captured(event, CaptureHint::default());
                    outer_for_task.end(None);
                    return;
                }
                _ => {
                    partial = Some(crate::utils::event_stream::extract_message(&event.value));
                    outer_for_task.push_captured(event, CaptureHint::default());
                }
            }
        }
    });
    outer_stream.set_cancellation_task(task);

    outer_stream
}

fn event_partial(event: &AssistantMessageEvent) -> Option<&AssistantMessage> {
    match event {
        AssistantMessageEvent::Start { partial }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => Some(partial),
        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => None,
    }
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
            credential_served_recently: None,
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
