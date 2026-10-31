//! Port of senpi packages/agent/test/internal-events-recovery.test.ts (#4272, the
//! empty-assistant recovery cases): a discarded attempt discards its capture records with its
//! events, and the kept attempt's records and terminal carrier cross unchanged.

use super::*;

use maho_ai::types::{ContentBlock, DoneReason, StopReason, TextContent, ThinkingContent};
use maho_ai::utils::assistant_capture::{
    AssistantProgress, AssistantRecord, BlockMetadata, assistant_records, get_assistant_capture,
    push_assistant_progress, subscribe_assistant_capture,
};

/// `model` from internal-events-recovery-fixture.ts: a recovering model whose id is not Claude or
/// Kimi, so reasoning commits an attempt.
pub(super) fn model() -> Model {
    let mut model = crate::agent::default_model();
    model.id = "fixture".to_owned();
    model.name = "fixture".to_owned();
    model.api = "openai-completions".to_owned();
    model.provider = "fixture".to_owned();
    model.recover_text_tool_calls = Some(true);
    model
}

fn message() -> AssistantMessage {
    let mut message = crate::assistant_terminal_state::create_terminal_failure_assistant_message(
        &model(),
        maho_ai::types::StopReason::Stop,
        &crate::assistant_terminal_state::AgentStreamError::Other {
            message: String::new(),
        },
        None,
    );
    message.content.clear();
    message.error_message = None;
    message.timestamp = 1;
    message
}

/// `attempt(signature, text)` from internal-events-recovery-fixture.ts.
pub(super) fn attempt(signature: &str, text: &str) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::with_capture_blocks(true);
    let mut output = message();
    stream.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });
    output
        .content
        .push(ContentBlock::Thinking(ThinkingContent::default()));
    stream.push(AssistantMessageEvent::ThinkingStart {
        content_index: 0,
        partial: output.clone(),
    });
    push_assistant_progress(
        &stream,
        AssistantProgress::metadata(BlockMetadata {
            content_index: 0,
            thinking_signature_delta: Some(signature.to_owned()),
            ..BlockMetadata::default()
        }),
    );
    stream.push(AssistantMessageEvent::ThinkingEnd {
        content_index: 0,
        content: String::new(),
        partial: output.clone(),
    });
    if !text.is_empty() {
        output.content.push(ContentBlock::text(""));
        stream.push(AssistantMessageEvent::TextStart {
            content_index: 1,
            partial: output.clone(),
        });
        output.content[1] = ContentBlock::Text(TextContent {
            text: text.to_owned(),
            audience: None,
            text_signature: None,
        });
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 1,
            delta: text.to_owned(),
            partial: output.clone(),
        });
        stream.push(AssistantMessageEvent::TextEnd {
            content_index: 1,
            content: text.to_owned(),
            partial: output.clone(),
        });
    }
    stream.push(AssistantMessageEvent::Done {
        reason: DoneReason::Stop,
        message: output,
    });
    stream
}

async fn recovered(attempts: Vec<(&'static str, &'static str)>) -> (Vec<AssistantRecord>, usize) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let source: StreamFn = Arc::new(move |_, _, _| {
        let index = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (signature, text) = attempts[index.min(attempts.len() - 1)];
        attempt(signature, text)
    });
    let context = maho_ai::types::Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
        active_tool_names: None,
    };
    let wrapped = with_empty_assistant_recovery(&model(), source)(&model(), &context, None);
    let records = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        assistant_records(&wrapped).collect(),
    )
    .await
    .expect("bounded")
    .expect("records");
    (records, calls.load(std::sync::atomic::Ordering::SeqCst))
}

fn progress(records: &[AssistantRecord]) -> Vec<AssistantProgress> {
    records
        .iter()
        .filter_map(|record| match record {
            AssistantRecord::Progress(progress) => Some((**progress).clone()),
            AssistantRecord::Event(_) => None,
        })
        .collect()
}

/// The public events upstream's `keeps the public bytes recorded before capture` snapshot holds,
/// verbatim from `__snapshots__/internal-events-recovery.test.ts.snap`.
const PUBLIC_BYTES_SNAPSHOT: &str = r#"[{"type":"start","partial":{"role":"assistant","api":"openai-completions","provider":"fixture","model":"fixture","timestamp":0,"content":[{"type":"thinking","thinking":""},{"type":"text","text":"kept"}],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}},{"type":"thinking_start","contentIndex":0,"partial":{"role":"assistant","api":"openai-completions","provider":"fixture","model":"fixture","timestamp":0,"content":[{"type":"thinking","thinking":""},{"type":"text","text":"kept"}],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}},{"type":"thinking_end","contentIndex":0,"content":"","partial":{"role":"assistant","api":"openai-completions","provider":"fixture","model":"fixture","timestamp":0,"content":[{"type":"thinking","thinking":""},{"type":"text","text":"kept"}],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}},{"type":"text_start","contentIndex":1,"partial":{"role":"assistant","api":"openai-completions","provider":"fixture","model":"fixture","timestamp":0,"content":[{"type":"thinking","thinking":""},{"type":"text","text":"kept"}],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}},{"type":"text_delta","contentIndex":1,"delta":"kept","partial":{"role":"assistant","api":"openai-completions","provider":"fixture","model":"fixture","timestamp":0,"content":[{"type":"thinking","thinking":""},{"type":"text","text":"kept"}],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}},{"type":"text_end","contentIndex":1,"content":"kept","partial":{"role":"assistant","api":"openai-completions","provider":"fixture","model":"fixture","timestamp":0,"content":[{"type":"thinking","thinking":""},{"type":"text","text":"kept"}],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}},{"type":"done","reason":"stop","message":{"role":"assistant","api":"openai-completions","provider":"fixture","model":"fixture","timestamp":0,"content":[{"type":"thinking","thinking":""},{"type":"text","text":"kept"}],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"diagnostics":[{"type":"empty_assistant_response_recovery","timestamp":0,"details":{"retries":1}}]}}]"#;

/// `.replace(/"timestamp":\d+/g, '"timestamp":0')`, plus JS number identity: TS writes the
/// integral cost `0` where serde writes the `f64` as `0.0`.
fn mask_timestamps(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(number)
            if number.as_f64().is_some_and(|float| float.fract() == 0.0)
                && !number.is_i64()
                && !number.is_u64() =>
        {
            let integral = number.as_f64().unwrap_or_default() as i64;
            *value = serde_json::Value::from(integral);
        }
        serde_json::Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if key == "timestamp" && entry.is_i64() {
                    *entry = serde_json::Value::from(0);
                } else {
                    mask_timestamps(entry);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(mask_timestamps),
        _ => {}
    }
}

/// TS events share one mutated `partial` object, so the snapshot serializes every partial with
/// the final content; a Rust event owns the partial cloned at push time. The block content is
/// therefore compared on the terminal message only.
fn without_partial_content(events: &mut serde_json::Value) {
    for event in events.as_array_mut().expect("event array") {
        if let Some(partial) = event.get_mut("partial").and_then(|p| p.as_object_mut()) {
            partial.remove("content");
        }
    }
}

#[tokio::test]
async fn keeps_the_public_bytes_recorded_before_capture() {
    let _capture = subscribe_assistant_capture();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let source: StreamFn = Arc::new(move |_, _, _| {
        let first = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        attempt("discard-me", if first { "" } else { "kept" })
    });
    let context = maho_ai::types::Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
        active_tool_names: None,
    };
    let wrapped = with_empty_assistant_recovery(&model(), source)(&model(), &context, None);
    let events = tokio::time::timeout(std::time::Duration::from_secs(5), wrapped.collect())
        .await
        .expect("bounded")
        .expect("events");
    let mut actual = serde_json::to_value(&events).expect("serialize events");
    mask_timestamps(&mut actual);
    let mut expected: serde_json::Value =
        serde_json::from_str(PUBLIC_BYTES_SNAPSHOT).expect("snapshot json");
    assert_eq!(
        actual.as_array().and_then(|events| events.last()),
        expected.as_array().and_then(|events| events.last())
    );
    without_partial_content(&mut actual);
    without_partial_content(&mut expected);
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn forwards_the_kept_attempts_own_records_and_none_from_the_discarded_attempt() {
    let _capture = subscribe_assistant_capture();
    let (records, calls) = recovered(vec![("discard-me", ""), ("kept", "kept")]).await;
    assert_eq!(calls, 2);
    assert_eq!(
        progress(&records),
        vec![AssistantProgress::metadata(BlockMetadata {
            content_index: 0,
            thinking_signature_delta: Some("kept".into()),
            ..BlockMetadata::default()
        })]
    );
    let events: Vec<&CapturedEvent> = records
        .iter()
        .filter_map(|record| match record {
            AssistantRecord::Event(event) => Some(event),
            AssistantRecord::Progress(_) => None,
        })
        .collect();
    assert!(
        events[..events.len() - 1]
            .iter()
            .all(|event| get_assistant_capture(event).is_some())
    );
    let done = events.last().expect("terminal");
    let AssistantMessageEvent::Done { message, .. } = &done.value else {
        panic!("expected done");
    };
    let terminal = get_assistant_capture(done).expect("terminal capture");
    assert_eq!(terminal.message.as_deref(), Some(message));
    assert_eq!(
        message.diagnostics.as_ref().map(|diagnostics| diagnostics
            .iter()
            .map(|d| d.kind.as_str())
            .collect::<Vec<_>>()),
        Some(vec!["empty_assistant_response_recovery"])
    );
}

#[tokio::test]
async fn drops_both_attempts_controls_when_the_retry_is_empty_too() {
    let _capture = subscribe_assistant_capture();
    let (records, calls) = recovered(vec![("first", ""), ("second", "")]).await;
    assert_eq!(calls, 2);
    // Both controls named thinking block 0; the terminal's block 0 is the error text, so neither
    // may cross.
    assert!(progress(&records).is_empty());
    assert_eq!(records.len(), 1);
    let Some(AssistantRecord::Event(terminal)) = records.last() else {
        panic!("expected a lone error terminal");
    };
    let AssistantMessageEvent::Error { error, .. } = &terminal.value else {
        panic!("expected error terminal");
    };
    assert_eq!(error.stop_reason, StopReason::Error);
    assert!(matches!(error.content.as_slice(), [ContentBlock::Text(_)]));
}
