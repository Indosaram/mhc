//! Resident sidecar event feed (latest `kibitzer/events.ts`).
//!
//! Turns prompt / `tool_call` / `tool_result` hook payloads plus the branch snapshot into bounded,
//! redacted `KibitzerEvent` values (branch cursor = branch length; the newest assistant text is
//! emitted once ahead of the hook that revealed it). Bodies are redacted BEFORE truncation to the
//! caps.
//!
//! Storage is bounded at CAPTURE time, matching the latest producer: the newest
//! `KIBITZER_EVENT_BUFFER_SIZE` events stay verbatim and every older one is folded, the moment it
//! leaves the buffer, into one digest line that always names the folded sequence and cursor range.
//! The buffered events and the accumulated fold are exposed as ONE owned `KibitzerEventBatch`
//! through `peek` (non-consuming) and `drain` (consuming, and opening a new batch). A drain clears
//! only the pending events and the fold: the lifetime sequence, the latest cursor and the assistant
//! observation state keep counting, so a wake envelope never reuses a sequence number and never
//! re-emits an assistant message a previous batch already carried.
//!
//! `render_event_batch` renders an ALREADY-CAPTURED batch without recapturing, regenerating sequence
//! numbers or folding history a second time - the seam the envelope adapter renders a stored batch
//! through.
//!
//! Honest N/A, native capture path (no scenario consumes them): the upstream `at` epoch-millisecond
//! stamp is not carried - the applied constructor takes caps only (no injectable clock), so events
//! stay clock-free and capture stays deterministic; the upstream `callId` is not carried - the
//! applied capture methods receive no tool-call id; and the upstream `logger.warn` guard is not
//! carried - `serde_json::Value` accessors cannot throw, so there is no failure path to log.
//! `truncated` IS carried because the rendered fragment reports it.

use serde_json::Value;

use crate::kibitzer_events_text::{DIGEST_TAIL_FRAGMENTS, FoldState, digest_line, escape_xml, fragment_of, truncate_head, utf16_len};
use crate::kibitzer_prompt_blocks::KIBITZER_FIELD_CAPS;

/// Re-exported so every importer reads them from the stream module (upstream `events.ts` re-exports
/// both from `events-text`).
pub use crate::kibitzer_events_text::{KIBITZER_DIGEST_MAX_CHARS, redact_kibitzer_event_text};

/// `KIBITZER_EVENT_BUFFER_SIZE`: the newest events kept verbatim; every older one folds at capture.
pub const KIBITZER_EVENT_BUFFER_SIZE: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KibitzerEventCaps { pub tool_args: usize, pub result_head: usize, pub assistant: usize, pub prompt: usize }

impl Default for KibitzerEventCaps {
    fn default() -> Self {
        Self { tool_args: KIBITZER_FIELD_CAPS.tool_args, result_head: KIBITZER_FIELD_CAPS.result_head, assistant: KIBITZER_FIELD_CAPS.assistant, prompt: KIBITZER_FIELD_CAPS.prompt }
    }
}

/// One bounded, already-redacted parent event. `is_error` is present only for a `tool_result`
/// (`None` means the host supplied no error flag, exactly as upstream leaves `isError` undefined).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KibitzerEvent { pub seq: usize, pub cursor: usize, pub kind: String, pub tool: Option<String>, pub is_error: Option<bool>, pub truncated: bool, pub body: String }

/// The one-line fold of every event that has left the buffer; the cursor range survives the fold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KibitzerEventDigest { pub count: usize, pub first_seq: usize, pub last_seq: usize, pub first_cursor: usize, pub last_cursor: usize, pub line: String }

/// `KibitzerEventBatch.cursors`: oldest known cursor through the newest buffered one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KibitzerEventBatchCursors { pub first: usize, pub last: usize }

/// One owned batch: the buffered events oldest first, the fold (if any) and the cursor span (if any).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KibitzerEventBatch { pub events: Vec<KibitzerEvent>, pub digest: Option<KibitzerEventDigest>, pub cursors: Option<KibitzerEventBatchCursors> }

pub struct KibitzerEventStream { caps: KibitzerEventCaps, events: Vec<KibitzerEvent>, fold: Option<FoldState>, seq: usize, latest_cursor: Option<usize>, last_assistant: Option<String> }

impl KibitzerEventStream {
    pub fn new(caps: KibitzerEventCaps) -> Self { Self { caps, events: Vec::new(), fold: None, seq: 0, latest_cursor: None, last_assistant: None } }

    /// Buffered (unfolded) events, at most `KIBITZER_EVENT_BUFFER_SIZE`.
    pub fn size(&self) -> usize { self.events.len() }

    /// Cursor of the newest recorded event across drains; `None` before any event was recorded.
    pub fn last_cursor(&self) -> Option<usize> { self.latest_cursor }

    pub fn on_prompt(&mut self, prompt: &str, cursor: usize) -> bool {
        if prompt.is_empty() { return false; }
        self.latest_cursor = Some(cursor);
        let (body, truncated) = self.bounded(prompt, self.caps.prompt);
        self.push("prompt", None, None, body, truncated);
        true
    }

    pub fn on_tool_call(&mut self, tool_name: &str, args: &Value, cursor: usize) -> bool {
        if tool_name.is_empty() { return false; }
        self.latest_cursor = Some(cursor);
        let (body, truncated) = self.bounded(&tool_args_text(args), self.caps.tool_args);
        self.push("tool_call", Some(tool_name.to_string()), None, body, truncated);
        true
    }

    pub fn on_tool_result(&mut self, tool_name: &str, result: &Value, is_error: bool, cursor: usize) -> bool {
        if tool_name.is_empty() { return false; }
        self.latest_cursor = Some(cursor);
        let (body, truncated) = self.bounded(&result_text(result), self.caps.result_head);
        self.push("tool_result", Some(tool_name.to_string()), Some(is_error), body, truncated);
        true
    }

    pub fn newest_assistant(&mut self, entries: &[Value], cursor: usize) -> bool {
        let Some(text) = newest_assistant_text(entries) else { return false; };
        if self.last_assistant.as_deref() == Some(text.as_str()) { return false; }
        self.latest_cursor = Some(cursor);
        let (body, truncated) = self.bounded(&text, self.caps.assistant);
        self.push("assistant", None, None, body, truncated);
        self.last_assistant = Some(text);
        true
    }

    /// `redactKibitzerEventText` then the cap; `truncated` reports whether the cap cut the body, so
    /// redaction always precedes truncation and storage.
    fn bounded(&self, text: &str, cap: usize) -> (String, bool) {
        let clean = redact_kibitzer_event_text(text);
        let truncated = utf16_len(&clean) > cap;
        (truncate_head(&clean, cap), truncated)
    }

    /// Records one event with the lifetime sequence, then folds the oldest event the moment the
    /// buffer exceeds `KIBITZER_EVENT_BUFFER_SIZE`, so retained storage is bounded.
    fn push(&mut self, kind: &str, tool: Option<String>, is_error: Option<bool>, body: String, truncated: bool) {
        self.seq += 1;
        let cursor = self.latest_cursor.unwrap_or(0);
        self.events.push(KibitzerEvent { seq: self.seq, cursor, kind: kind.to_string(), tool, is_error, truncated, body });
        if self.events.len() <= KIBITZER_EVENT_BUFFER_SIZE { return; }
        let oldest = self.events.remove(0);
        self.fold_into(oldest);
    }

    /// `foldInto(event)`: the first folded event becomes the head; later ones append to the bounded
    /// tail, and the fold's count, sequence and cursor ranges keep widening.
    fn fold_into(&mut self, event: KibitzerEvent) {
        let fragment = fragment_of(&event.kind, event.tool.as_deref(), event.is_error.unwrap_or(false), &event.body);
        match &mut self.fold {
            None => self.fold = Some(FoldState { count: 1, first_seq: event.seq, last_seq: event.seq, first_cursor: event.cursor, last_cursor: event.cursor, head: fragment, tail: Vec::new() }),
            Some(fold) => {
                fold.count += 1;
                fold.last_seq = event.seq;
                fold.last_cursor = event.cursor;
                fold.tail.push(fragment);
                if fold.tail.len() > DIGEST_TAIL_FRAGMENTS { fold.tail.remove(0); }
            }
        }
    }

    /// The pending batch WITHOUT consuming it: buffered events, the fold and the cursor span.
    pub fn peek(&self) -> KibitzerEventBatch {
        let digest = self.fold.as_ref().map(|fold| KibitzerEventDigest {
            count: fold.count,
            first_seq: fold.first_seq,
            last_seq: fold.last_seq,
            first_cursor: fold.first_cursor,
            last_cursor: fold.last_cursor,
            line: digest_line(fold),
        });
        let first = digest.as_ref().map(|digest| digest.first_cursor).or_else(|| self.events.first().map(|event| event.cursor));
        let last = self.events.last().map(|event| event.cursor).or_else(|| digest.as_ref().map(|digest| digest.last_cursor));
        let cursors = match (first, last) {
            (Some(first), Some(last)) => Some(KibitzerEventBatchCursors { first, last }),
            _ => None,
        };
        KibitzerEventBatch { events: self.events.clone(), digest, cursors }
    }

    /// Returns the pending batch and starts a new one; the lifetime `seq`, the latest cursor and the
    /// assistant observation state keep counting across the drain.
    pub fn drain(&mut self) -> KibitzerEventBatch {
        let pending = self.peek();
        self.events.clear();
        self.fold = None;
        pending
    }

    /// `renderKibitzerEventBatch(peek())`: the fragment for the current pending batch. Kept for the
    /// existing prompt call site; the envelope adapter renders an ALREADY-CAPTURED batch through
    /// [`render_event_batch`], so a drained batch is never recaptured.
    pub fn render_batch(&self) -> String { render_event_batch(&self.peek()) }
}

pub fn create_kibitzer_event_stream(caps: KibitzerEventCaps) -> KibitzerEventStream { KibitzerEventStream::new(caps) }

/// `renderKibitzerEventBatch(batch)`: one `<digest>` line, then one `<event>` per buffered event.
/// Renders an already-captured batch losslessly - no recapture, no sequence regeneration, no second
/// fold - escaping the tool attribute and the body without changing the stored values.
pub fn render_event_batch(batch: &KibitzerEventBatch) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(digest) = &batch.digest {
        lines.push(format!("<digest folded=\"{}\" cursor=\"{}..{}\">{}</digest>", digest.count, digest.first_cursor, digest.last_cursor, escape_xml(&digest.line)));
    }
    for event in &batch.events {
        lines.push(render_event(event));
    }
    lines.join("\n")
}

fn render_event(event: &KibitzerEvent) -> String {
    let mut attributes = vec![format!("seq=\"{}\"", event.seq), format!("cursor=\"{}\"", event.cursor), format!("kind=\"{}\"", event.kind)];
    if let Some(tool) = &event.tool { attributes.push(format!("tool=\"{}\"", escape_xml(tool))); }
    if let Some(is_error) = event.is_error { attributes.push(format!("error=\"{is_error}\"")); }
    if event.truncated { attributes.push("truncated=\"true\"".to_string()); }
    format!("<event {}>{}</event>", attributes.join(" "), escape_xml(&event.body))
}

fn tool_args_text(args: &Value) -> String {
    if let Some(summary) = args.get("summary").and_then(Value::as_str) { return summary.to_string(); }
    if let Some(code) = args.get("code").and_then(Value::as_str) { return code.to_string(); }
    serde_json::to_string(args).unwrap_or_default()
}

fn result_text(result: &Value) -> String {
    match result { Value::String(text) => text.clone(), Value::Null => String::new(), other => serde_json::to_string(other).unwrap_or_default() }
}

fn newest_assistant_text(entries: &[Value]) -> Option<String> {
    for entry in entries.iter().rev() {
        if entry.get("type").and_then(Value::as_str) != Some("message") { continue; }
        let message = entry.get("message")?;
        if message.get("role").and_then(Value::as_str) != Some("assistant") { continue; }
        let text = crate::recall_session_read::text_of(message.get("content"));
        if !text.trim().is_empty() { return Some(text); }
    }
    None
}
