//! Port of senpi packages/ai/test/producer-capture-records.ts, plus the aliasing turn builder the
//! capture suites share.
//!
//! TS fixtures mutate one `output` object that every pushed event's `partial` (and the terminal)
//! points at, while capture freezes each event at push time. A wrapper reading such a buffered turn
//! later therefore sees the final message on every event, next to the push-time capture. Rust
//! events own their partials, so `AliasedTurn` replays that contract: each push is captured by a
//! probe stream at push time, and `finish` re-pushes it with the final message as its partial and
//! the probe's capture carried.

use std::time::Duration;

use serde_json::{Value, json};

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock};
use crate::utils::assistant_capture::{
    AssistantCapture, AssistantProgress, AssistantRecord, CaptureHint, Captured, CapturedEvent,
    ThreadCaptureSubscription, assistant_records, get_assistant_capture, push_assistant_progress,
    subscribe_assistant_capture_on_this_thread,
};
use crate::utils::event_stream::AssistantMessageEventStream;

pub(crate) const DEADLINE: Duration = Duration::from_secs(5);

/// Holds a capture subscription for one test, as an internal agent listener does
/// (`subscribeCaptureForEachTest`).
pub(crate) fn subscribe_capture_for_test() -> ThreadCaptureSubscription {
    subscribe_assistant_capture_on_this_thread()
}

/// Settles first: every start and delta is emitted before the reader resumes.
pub(crate) async fn drain_records(stream: &AssistantMessageEventStream) -> Vec<AssistantRecord> {
    tokio::time::timeout(DEADLINE, stream.result())
        .await
        .expect("result deadline")
        .ok();
    collect_records(stream).await
}

pub(crate) async fn collect_records(stream: &AssistantMessageEventStream) -> Vec<AssistantRecord> {
    tokio::time::timeout(DEADLINE, assistant_records(stream).collect())
        .await
        .expect("records deadline")
        .expect("records")
}

pub(crate) fn public_events(records: &[AssistantRecord]) -> Vec<CapturedEvent> {
    records
        .iter()
        .filter_map(|record| match record {
            AssistantRecord::Event(event) => Some(event.clone()),
            AssistantRecord::Progress(_) => None,
        })
        .collect()
}

/// Progress records as their JSON shape, comparable to the TS object literals.
pub(crate) fn progress_records(records: &[AssistantRecord]) -> Vec<Value> {
    records
        .iter()
        .filter_map(|record| match record {
            AssistantRecord::Progress(progress) => {
                Some(serde_json::to_value(progress.as_ref()).expect("progress json"))
            }
            AssistantRecord::Event(_) => None,
        })
        .collect()
}

pub(crate) fn event_type(event: &AssistantMessageEvent) -> String {
    serde_json::to_value(event).expect("event json")["type"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn event_index(event: &AssistantMessageEvent) -> Option<usize> {
    crate::utils::assistant_capture::event_content_index(event)
}

pub(crate) fn record_kinds(records: &[AssistantRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| match record {
            AssistantRecord::Progress(progress) => {
                let value = serde_json::to_value(progress.as_ref()).expect("progress json");
                let kind = value["type"].as_str().unwrap_or_default();
                match progress.as_ref() {
                    AssistantProgress::MessageBlockState { phase, .. } => format!(
                        "{kind}:{}:{}",
                        progress.content_index(),
                        serde_json::to_value(phase)
                            .expect("phase")
                            .as_str()
                            .unwrap_or_default()
                    ),
                    _ => format!("{kind}:{}", progress.content_index()),
                }
            }
            AssistantRecord::Event(event) => match event_index(&event.value) {
                Some(index) => format!("{}:{index}", event_type(&event.value)),
                None => event_type(&event.value),
            },
        })
        .collect()
}

/// Zeroes every numeric `timestamp`: snapshots pin the clock, Rust producers stamp it.
pub(crate) fn zero_timestamps(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if key == "timestamp" && entry.is_number() {
                    *entry = json!(0);
                } else {
                    zero_timestamps(entry);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(zero_timestamps),
        // JS has one number type, so the TS snapshot cannot tell `0` from `0.0`. serde_json's
        // `float_roundtrip` (unified on in workspace builds) keeps an `f64` zero a float; fold
        // integral floats back to integers so the comparison sees the bytes TS would.
        Value::Number(number) => {
            if let Some(float) = number.as_f64()
                && number.is_f64()
                && float.fract() == 0.0
                && float.abs() < 9_007_199_254_740_992.0
            {
                *value = json!(float as i64);
            }
        }
        _ => {}
    }
}

/// The public bytes of the events, with every timestamp zeroed.
pub(crate) fn public_bytes(events: &[CapturedEvent]) -> Value {
    let mut value = Value::Array(
        events
            .iter()
            .map(|event| {
                let mut v = serde_json::to_value(&event.value).expect("event json");
                if let Some(partial) = v.get("partial").cloned()
                    && partial.get("api").and_then(|a| a.as_str()) == Some("pi-messages")
                {
                    if v.get("type").and_then(|t| t.as_str()) == Some("text_end")
                        && let Some(idx) = v.get("contentIndex").and_then(|i| i.as_u64())
                        && let Some(block) = partial.get("content").and_then(|c| c.get(idx as usize))
                        && let Some(sig) = block.get("textSignature").cloned()
                        && let Value::Object(map) = &mut v
                    {
                        map.insert("contentSignature".into(), sig);
                    } else if v.get("type").and_then(|t| t.as_str()) == Some("toolcall_start")
                        && let Some(idx) = v.get("contentIndex").and_then(|i| i.as_u64())
                        && let Some(block) = partial.get("content").and_then(|c| c.get(idx as usize))
                        && let (Some(id), Some(name)) =
                            (block.get("id").cloned(), block.get("name").cloned())
                        && let Value::Object(map) = &mut v
                    {
                        map.insert("id".into(), id);
                        map.insert("toolName".into(), name);
                    }
                }
                v
            })
            .collect(),
    );
    zero_timestamps(&mut value);
    value
}

/// The fields a capture sets, as the TS object `toMatchObject` reads.
pub(crate) fn capture_json(capture: Option<&AssistantCapture>) -> Value {
    let Some(capture) = capture else {
        return Value::Null;
    };
    let mut map = serde_json::Map::new();
    if let Some(block) = &capture.block {
        map.insert(
            "block".into(),
            serde_json::to_value(block.as_ref()).expect("block"),
        );
    }
    if let Some(prefix) = &capture.raw_argument_prefix {
        map.insert("rawArgumentPrefix".into(), json!(prefix));
    }
    for (key, value) in [
        ("coveredCodeUnits", capture.covered_code_units),
        ("nextOffset", capture.next_offset),
        ("offset", capture.offset),
        ("parsedThrough", capture.parsed_through),
        ("contentIndex", capture.content_index),
        ("rewrittenFrom", capture.rewritten_from),
    ] {
        if let Some(value) = value {
            map.insert(key.into(), json!(value));
        }
    }
    Value::Object(map)
}

pub(crate) fn capture(event: &CapturedEvent) -> Option<&AssistantCapture> {
    get_assistant_capture(event)
}

/// `{ delta, offset, parsedThrough }` of each delta of one kind.
pub(crate) fn delta_captures(
    events: &[CapturedEvent],
    kind: &str,
) -> Vec<(String, Option<usize>, Option<usize>)> {
    events
        .iter()
        .filter(|event| event_type(&event.value) == kind)
        .filter_map(|event| {
            let delta = match &event.value {
                AssistantMessageEvent::TextDelta { delta, .. }
                | AssistantMessageEvent::ThinkingDelta { delta, .. }
                | AssistantMessageEvent::ToolcallDelta { delta, .. } => delta.clone(),
                _ => return None,
            };
            let captured = capture(event);
            Some((
                delta,
                captured.and_then(|capture| capture.offset),
                captured.and_then(|capture| capture.parsed_through),
            ))
        })
        .collect()
}

pub(crate) fn first_event<'a>(
    events: &'a [CapturedEvent],
    kind: &str,
    content_index: Option<usize>,
) -> &'a CapturedEvent {
    events
        .iter()
        .find(|event| {
            event_type(&event.value) == kind
                && (content_index.is_none() || event_index(&event.value) == content_index)
        })
        .unwrap_or_else(|| panic!("no {kind} event at {content_index:?}"))
}

pub(crate) fn terminal_message(event: &AssistantMessageEvent) -> &AssistantMessage {
    match event {
        AssistantMessageEvent::Done { message, .. } => message,
        AssistantMessageEvent::Error { error, .. } => error,
        _ => panic!("not a terminal event"),
    }
}

pub(crate) fn block_type(block: &ContentBlock) -> &'static str {
    match block {
        ContentBlock::Text(_) => "text",
        ContentBlock::Thinking(_) => "thinking",
        ContentBlock::Image(_) => "image",
        ContentBlock::ToolCall(_) => "toolCall",
        ContentBlock::ProviderNative(_) => "providerNative",
    }
}

pub(crate) fn block_types(message: &AssistantMessage) -> Vec<&'static str> {
    message.content.iter().map(block_type).collect()
}

pub(crate) fn block_json(capture: Option<&AssistantCapture>) -> Value {
    capture
        .and_then(|capture| capture.block.as_deref())
        .map(|block| serde_json::to_value(block).expect("block json"))
        .unwrap_or(Value::Null)
}

/// Records whose block kind differs from the block the terminal holds at their index.
pub(crate) fn records_on_foreign_blocks(
    records: &[AssistantRecord],
    terminal: &AssistantMessage,
) -> Vec<String> {
    records
        .iter()
        .filter_map(|record| {
            let (kind_name, index, kind) = match record {
                AssistantRecord::Progress(progress) => {
                    let value = serde_json::to_value(progress.as_ref()).expect("progress json");
                    let kind = match progress.as_ref() {
                        AssistantProgress::MessageBlockState { block, .. } => {
                            Some(block_type(block))
                        }
                        AssistantProgress::MessageBlockMetadata(metadata) => {
                            Some(if metadata.text_signature.is_some() {
                                "text"
                            } else if metadata.name.is_some() {
                                "toolCall"
                            } else {
                                "thinking"
                            })
                        }
                        AssistantProgress::MessageToolArgumentsReset { .. } => Some("toolCall"),
                    };
                    (
                        value["type"].as_str().unwrap_or_default().to_owned(),
                        progress.content_index(),
                        kind,
                    )
                }
                AssistantRecord::Event(event) => {
                    let public = event_index(&event.value)?;
                    let index = capture(event)
                        .and_then(|capture| capture.content_index)
                        .unwrap_or(public);
                    let name = event_type(&event.value);
                    let kind = if name.starts_with("toolcall_") {
                        Some("toolCall")
                    } else if name.starts_with("text_") {
                        Some("text")
                    } else if name.starts_with("thinking_") {
                        Some("thinking")
                    } else {
                        None
                    };
                    (name, index, kind)
                }
            };
            let actual = terminal.content.get(index).map(block_type);
            match kind {
                Some(kind) if Some(kind) != actual => Some(format!(
                    "{kind_name}:{index} names {}",
                    actual.unwrap_or("nothing")
                )),
                _ => None,
            }
        })
        .collect()
}

/// Records that carry a capture: progress, or an event with one.
pub(crate) fn carrying_records(records: &[AssistantRecord]) -> Vec<AssistantRecord> {
    records
        .iter()
        .filter(|record| match record {
            AssistantRecord::Progress(_) => true,
            AssistantRecord::Event(event) => capture(event).is_some(),
        })
        .cloned()
        .collect()
}

/// `expect(actual).toMatchObject(expected)`: every key `expected` names matches, recursively.
pub(crate) fn assert_matches_object(actual: &Value, expected: &Value) {
    fn matches(actual: &Value, expected: &Value) -> bool {
        match (actual, expected) {
            (Value::Object(actual), Value::Object(expected)) => expected
                .iter()
                .all(|(key, value)| actual.get(key).is_some_and(|actual| matches(actual, value))),
            (Value::Array(actual), Value::Array(expected)) => {
                actual.len() == expected.len()
                    && actual.iter().zip(expected).all(|(a, e)| matches(a, e))
            }
            _ => actual == expected,
        }
    }
    assert!(
        matches(actual, expected),
        "{actual:#} does not match {expected:#}"
    );
}

/// A fixture message from JSON (camelCase fields).
pub(crate) fn assistant_message(
    api: &str,
    provider: &str,
    model: &str,
    content: Value,
    stop_reason: &str,
) -> AssistantMessage {
    serde_json::from_value(json!({
        "role": "assistant", "api": api, "provider": provider, "model": model,
        "content": content,
        "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 15,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
        "stopReason": stop_reason, "timestamp": 123
    }))
    .expect("assistant message fixture")
}

enum Pushed {
    Aliased,
    Literal,
}

/// A turn whose events alias one mutable message, as the TS fixtures' do (module docs).
pub(crate) struct AliasedTurn {
    probe: AssistantMessageEventStream,
    pushed: Vec<Pushed>,
    capture_blocks: bool,
}

impl AliasedTurn {
    pub(crate) fn new(capture_blocks: bool) -> Self {
        let probe = AssistantMessageEventStream::with_capture_blocks(capture_blocks);
        Self {
            probe,
            pushed: Vec::new(),
            capture_blocks,
        }
    }

    /// `stream.push(event)` whose partial (or terminal message) is the shared `output`.
    pub(crate) fn push(&mut self, event: AssistantMessageEvent) {
        self.push_with_hint(event, CaptureHint::default());
    }

    pub(crate) fn push_with_hint(&mut self, event: AssistantMessageEvent, hint: CaptureHint) {
        self.pushed.push(Pushed::Aliased);
        self.probe.push_with_hint(event, hint);
    }

    /// A push whose message is its own object, never mutated afterwards.
    pub(crate) fn push_literal(&mut self, event: AssistantMessageEvent) {
        self.pushed.push(Pushed::Literal);
        self.probe.push(event);
    }

    pub(crate) fn progress(&mut self, progress: AssistantProgress) {
        push_assistant_progress(&self.probe, progress);
    }

    /// The source stream and the events it was given, each aliased event carrying `output`.
    pub(crate) async fn finish(
        self,
        output: &AssistantMessage,
        result: Option<AssistantMessage>,
    ) -> (AssistantMessageEventStream, Vec<AssistantMessageEvent>) {
        self.finish_open(output, result, None).await
    }

    /// As `finish`, but only through the first event of `kind`, and the stream stays open: a
    /// provider still streaming when its consumer stops.
    pub(crate) async fn finish_through(
        self,
        output: &AssistantMessage,
        kind: &str,
    ) -> (AssistantMessageEventStream, Vec<AssistantMessageEvent>) {
        self.finish_open(output, None, Some(kind)).await
    }

    async fn finish_open(
        self,
        output: &AssistantMessage,
        result: Option<AssistantMessage>,
        through: Option<&str>,
    ) -> (AssistantMessageEventStream, Vec<AssistantMessageEvent>) {
        self.probe.end(result.clone());
        let mut records = collect_records(&self.probe).await;
        if let Some(kind) = through {
            let last = records
                .iter()
                .position(|record| {
                    matches!(record, AssistantRecord::Event(event) if event_type(&event.value) == kind)
                })
                .expect("cutoff event");
            records.truncate(last + 1);
        }
        let end = through.is_none();
        let stream = AssistantMessageEventStream::with_capture_blocks(self.capture_blocks);
        let mut kinds = self.pushed.into_iter();
        let mut events = Vec::new();
        for record in records {
            match record {
                AssistantRecord::Progress(progress) => stream.push_frozen_progress(progress),
                AssistantRecord::Event(Captured { value, capture }) => {
                    let value = match kinds.next() {
                        Some(Pushed::Aliased) => alias(value, output),
                        _ => value,
                    };
                    events.push(value.clone());
                    match capture {
                        Some(capture) => stream.push_captured(
                            Captured {
                                value,
                                capture: Some(capture),
                            },
                            CaptureHint::default(),
                        ),
                        None => stream.push_with_hint(
                            value,
                            CaptureHint {
                                carried: Some(None),
                                ..CaptureHint::default()
                            },
                        ),
                    }
                }
            }
        }
        if end {
            stream.end(result);
        }
        (stream, events)
    }
}

/// Points a non-terminal event's partial at `output` (TS events alias one mutable message).
pub(crate) fn alias_partial(event: &mut AssistantMessageEvent, output: &AssistantMessage) {
    if !matches!(
        event,
        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
    ) {
        *event = alias(event.clone(), output);
    }
}

fn alias(event: AssistantMessageEvent, output: &AssistantMessage) -> AssistantMessageEvent {
    let mut event = event;
    match &mut event {
        AssistantMessageEvent::Done { message, .. } => *message = output.clone(),
        AssistantMessageEvent::Error { error, .. } => *error = output.clone(),
        AssistantMessageEvent::Start { partial }
        | AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => *partial = output.clone(),
    }
    event
}
