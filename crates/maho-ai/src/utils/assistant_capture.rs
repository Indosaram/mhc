//! Port of senpi packages/ai/src/utils/assistant-capture.ts.
//!
//! PERF-DIVERGENCE(senpi assistant capture): upstream attaches a capture to each event object
//! through a module WeakMap keyed by object identity, and readers look it up with
//! `getAssistantCapture(event)`. Rust events are values that are cloned out of the queue, so the
//! capture travels next to the event in the stream's ordered queue and readers receive it as
//! `Captured<AssistantMessageEvent>`; `get_assistant_capture` reads it from that wrapper. Frozen
//! copies are `Arc`s shared between records, so "frozen" means immutable and shared, as in TS.
//! The ordinary `EventStream::next` iterator remains the downgrade boundary: it never yields a
//! capture or a progress record.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, Usage};
use crate::utils::event_stream::{AssistantMessageEventStream, StreamError};

use super::assistant_capture_record::{build_capture, same_value, terminal_capture};
pub use super::assistant_capture_record::{freeze_capture, public_block};

fn serialize_arc<S: Serializer, T: Serialize>(
    value: &Arc<T>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    (**value).serialize(serializer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockPhase {
    Insert,
    End,
}

/// The `message_block_metadata` fields. `thinking_signature_delta` appends to the signature;
/// `thinking_signature` and `text_signature` replace it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockMetadata {
    pub content_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature_delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
    /// TS `redacted?: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
}

/// `AssistantProgress`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AssistantProgress {
    MessageBlockState {
        content_index: usize,
        phase: BlockPhase,
        #[serde(serialize_with = "serialize_arc")]
        block: Arc<ContentBlock>,
    },
    MessageBlockMetadata(BlockMetadata),
    MessageToolArgumentsReset {
        content_index: usize,
        raw_argument_prefix: String,
        parsed_through: usize,
    },
}

impl AssistantProgress {
    /// A `message_block_metadata` record.
    pub fn metadata(metadata: BlockMetadata) -> Self {
        Self::MessageBlockMetadata(metadata)
    }

    /// A `message_block_state` record of a block the producer still owns.
    pub fn block_state(content_index: usize, phase: BlockPhase, block: &ContentBlock) -> Self {
        Self::MessageBlockState {
            content_index,
            phase,
            block: Arc::new(block.clone()),
        }
    }

    pub fn content_index(&self) -> usize {
        match self {
            Self::MessageBlockState { content_index, .. }
            | Self::MessageToolArgumentsReset { content_index, .. } => *content_index,
            Self::MessageBlockMetadata(metadata) => metadata.content_index,
        }
    }

    /// The same record at another block index (a relay that remaps indices).
    pub fn with_content_index(&self, index: usize) -> Self {
        let mut next = self.clone();
        match &mut next {
            Self::MessageBlockState { content_index, .. }
            | Self::MessageToolArgumentsReset { content_index, .. } => *content_index = index,
            Self::MessageBlockMetadata(metadata) => metadata.content_index = index,
        }
        next
    }
}

/// A progress record frozen by the stream that captured it (TS `Object.isFrozen(progress)`).
pub type FrozenProgress = Arc<AssistantProgress>;

/// What `push_assistant_progress` accepts: a fresh producer record, or one already frozen by the
/// stream that captured it (a forwarded record, which every stream relays unchanged).
pub enum ProgressPush {
    Fresh(AssistantProgress),
    Frozen(FrozenProgress),
}

impl From<AssistantProgress> for ProgressPush {
    fn from(progress: AssistantProgress) -> Self {
        Self::Fresh(progress)
    }
}

impl From<FrozenProgress> for ProgressPush {
    fn from(progress: FrozenProgress) -> Self {
        Self::Frozen(progress)
    }
}

/// A value with the capture its producer attached (TS: the WeakMap entry for the event object).
#[derive(Debug, Clone)]
pub struct Captured<T> {
    pub value: T,
    pub capture: Option<Arc<AssistantCapture>>,
}

impl<T> Captured<T> {
    pub fn bare(value: T) -> Self {
        Self {
            value,
            capture: None,
        }
    }
}

pub type CapturedEvent = Captured<AssistantMessageEvent>;

/// One entry of a stream's single ordered queue (TS `OrderedEntry`).
#[derive(Debug, Clone)]
pub enum Record<T> {
    Event(Captured<T>),
    Progress(FrozenProgress),
}

/// `AssistantRecord = AssistantMessageEvent | AssistantProgress`.
pub type AssistantRecord = Record<AssistantMessageEvent>;

/// `AssistantCapture`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AssistantCapture {
    pub usage: Usage,
    /// Scalar message fields changed since the last non-delta capture; `Value::Null` marks a
    /// field that was removed. `None` on delta captures.
    pub metadata: Option<Arc<Map<String, Value>>>,
    pub block: Option<Arc<ContentBlock>>,
    pub message: Option<Arc<AssistantMessage>>,
    pub raw_argument_prefix: Option<String>,
    pub covered_code_units: Option<usize>,
    /// Required on block starts: zero for replay, full seed length for continuation.
    pub next_offset: Option<usize>,
    pub offset: Option<usize>,
    pub parsed_through: Option<usize>,
    /// The block's own index where the event's public `contentIndex` names another block; internal
    /// readers prefer it.
    pub content_index: Option<usize>,
    /// First block index whose earlier records no longer name the terminal's blocks (block
    /// index/identity only); readers MUST drop every block record at or above it for that message;
    /// absent = no rewrite. The terminal message always wins for CONTENT.
    pub rewritten_from: Option<usize>,
}

/// `CaptureHint`.
#[derive(Debug, Clone, Default)]
pub struct CaptureHint {
    pub raw_argument_prefix: Option<String>,
    pub parsed_through: Option<usize>,
    /// Replay deltas may begin before the bytes already covered by a deferred start. Signed so a
    /// producer frontier the seed rules reject (negative) reaches `seed_next_offset`.
    pub offset: Option<i64>,
    /// The open block's own index when the adapter's public `contentIndex` has drifted from it.
    pub content_index: Option<usize>,
    /// Set by a wrapper that re-emits a source event 1:1 at its own index while reading the source
    /// later than it was emitted: `Some(Some(_))` relays the source capture, `Some(None)` (TS
    /// `null`) records nothing, because the source captured nothing.
    pub carried: Option<Option<Arc<AssistantCapture>>>,
    /// A terminal's `AssistantCapture.rewritten_from`.
    pub rewritten_from: Option<usize>,
}

impl CaptureHint {
    pub fn content_index(index: usize) -> Self {
        Self {
            content_index: Some(index),
            ..Self::default()
        }
    }

    pub fn parsed_through(parsed_through: usize) -> Self {
        Self {
            parsed_through: Some(parsed_through),
            ..Self::default()
        }
    }

    pub fn offset(offset: i64) -> Self {
        Self {
            offset: Some(offset),
            ..Self::default()
        }
    }

    pub fn rewritten_from(rewritten_from: Option<usize>) -> Self {
        Self {
            rewritten_from,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapturePosition {
    pub offset: usize,
    pub parsed_through: usize,
}

#[derive(Debug, Default)]
pub struct CaptureState {
    pub positions: HashMap<usize, CapturePosition>,
    pub metadata: Map<String, Value>,
    pub usage: Option<Usage>,
    pub insert_copies: HashMap<usize, Arc<ContentBlock>>,
    pub failed_blocks: HashSet<usize>,
}

/// Per-stream capture bookkeeping (TS: the `states`, `demanded` and `warned` WeakMaps).
#[derive(Debug, Default)]
pub struct CaptureSlot {
    enabled: bool,
    /// `None` until a capture-enabled stream's first push decides it, `Some(None)` once decided off.
    decided: Option<Option<CaptureState>>,
    demanded: bool,
    warned: bool,
    relay: Option<RelaySource>,
}

/// The source event a `relaying` step copies 1:1 (TS `relaySource` / `relayed`).
#[derive(Debug)]
struct RelaySource {
    kind: std::mem::Discriminant<AssistantMessageEvent>,
    capture: Option<Arc<AssistantCapture>>,
    relayed: bool,
}

static SUBSCRIBERS: AtomicUsize = AtomicUsize::new(0);

/// The handle `subscribe_assistant_capture` returns; `release` (or drop) ends the subscription once.
#[derive(Debug)]
pub struct AssistantCaptureSubscription {
    counter: &'static AtomicUsize,
    held: bool,
}

impl AssistantCaptureSubscription {
    /// A subscription counted on `counter`; tests count on their own counter so they never turn
    /// capture on for streams other tests drive in parallel.
    fn on(counter: &'static AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self {
            counter,
            held: true,
        }
    }

    pub fn release(&mut self) {
        if self.held {
            self.counter.fetch_sub(1, Ordering::SeqCst);
        }
        self.held = false;
    }
}

impl Drop for AssistantCaptureSubscription {
    fn drop(&mut self) {
        self.release();
    }
}

/// While held, every capture-enabled stream whose first push comes after it builds capture records.
pub fn subscribe_assistant_capture() -> AssistantCaptureSubscription {
    AssistantCaptureSubscription::on(&SUBSCRIBERS)
}

#[cfg(test)]
thread_local! {
    static THREAD_SUBSCRIBERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A subscription that only streams first pushed on this thread see: the capture suites hold one
/// per test (TS `subscribeCaptureForEachTest`) without turning capture on for tests other threads
/// run in parallel. `#[tokio::test]` runs every spawned task on the test's own thread.
#[cfg(test)]
pub(crate) struct ThreadCaptureSubscription(());

#[cfg(test)]
impl Drop for ThreadCaptureSubscription {
    fn drop(&mut self) {
        THREAD_SUBSCRIBERS.with(|count| count.set(count.get() - 1));
    }
}

#[cfg(test)]
pub(crate) fn subscribe_assistant_capture_on_this_thread() -> ThreadCaptureSubscription {
    THREAD_SUBSCRIBERS.with(|count| count.set(count.get() + 1));
    ThreadCaptureSubscription(())
}

#[cfg(test)]
fn thread_subscribed() -> bool {
    THREAD_SUBSCRIBERS.with(|count| count.get() > 0)
}

#[cfg(not(test))]
fn thread_subscribed() -> bool {
    false
}

impl CaptureSlot {
    pub(crate) fn enable(&mut self) {
        self.enabled = true;
    }

    pub(crate) fn demand(&mut self) {
        self.demanded = true;
    }

    pub(crate) fn begin_relay(
        &mut self,
        source: std::mem::Discriminant<AssistantMessageEvent>,
        capture: Option<Arc<AssistantCapture>>,
    ) {
        self.relay = Some(RelaySource {
            kind: source,
            capture,
            relayed: false,
        });
    }

    pub(crate) fn end_relay(&mut self) {
        self.relay = None;
    }

    /// Only a relaying step's first push of the source's type is the copy; it relays the source's
    /// emission-time record, and any later one records nothing.
    pub(crate) fn relay_hint(
        &mut self,
        event: &AssistantMessageEvent,
        hint: &CaptureHint,
    ) -> Option<CaptureHint> {
        let relay = self.relay.as_mut()?;
        if relay.kind != std::mem::discriminant(event) {
            return None;
        }
        let carried = if relay.relayed {
            None
        } else {
            relay.capture.clone()
        };
        relay.relayed = true;
        Some(CaptureHint {
            carried: Some(carried),
            ..hint.clone()
        })
    }

    /// Decide capture at this push without recording anything (a relayed event that already
    /// carries its source capture).
    pub(crate) fn decide(&mut self) {
        let _ = self.active_state();
    }

    /// Decided once, at the first push, so a stream never starts capturing midway with partial
    /// offsets.
    fn active_state(&mut self) -> Option<&mut CaptureState> {
        if !self.enabled {
            return None;
        }
        if self.decided.is_none() {
            let on = SUBSCRIBERS.load(Ordering::SeqCst) > 0 || self.demanded || thread_subscribed();
            self.decided = Some(on.then(CaptureState::default));
        }
        self.decided.as_mut().and_then(Option::as_mut)
    }

    /// A capture failure never reaches the provider stream: the block loses its records and one
    /// warning is logged.
    fn drop_capture(&mut self, error: &dyn std::fmt::Display, content_index: Option<usize>) {
        if let (Some(index), Some(state)) = (
            content_index,
            self.decided.as_mut().and_then(Option::as_mut),
        ) {
            state.failed_blocks.insert(index);
        }
        if self.warned {
            return;
        }
        self.warned = true;
        eprintln!(
            "[assistant-capture] could not build an internal capture record; that block's records are dropped {error}"
        );
    }

    /// `freezeProgress` behind `pushAssistantProgress`; `None` records nothing.
    pub(crate) fn freeze_progress(&mut self, progress: ProgressPush) -> Option<FrozenProgress> {
        let progress = match progress {
            // A forwarded record is already a frozen public copy, made by the stream that captured it.
            ProgressPush::Frozen(frozen) => return Some(frozen),
            ProgressPush::Fresh(progress) => progress,
        };
        let state = self.active_state()?;
        if state.failed_blocks.contains(&progress.content_index()) {
            return None;
        }
        if let AssistantProgress::MessageToolArgumentsReset {
            content_index,
            raw_argument_prefix,
            parsed_through,
        } = &progress
        {
            state.positions.insert(
                *content_index,
                CapturePosition {
                    offset: utf16_len(raw_argument_prefix),
                    parsed_through: *parsed_through,
                },
            );
        }
        match progress {
            reset @ AssistantProgress::MessageToolArgumentsReset { .. } => Some(Arc::new(reset)),
            AssistantProgress::MessageBlockState {
                content_index,
                phase,
                block,
            } => {
                let current = public_block(&block);
                let block = match state.insert_copies.get(&content_index) {
                    Some(previous) if same_value(&current, previous.as_ref()) => previous.clone(),
                    _ => Arc::new(current),
                };
                match phase {
                    BlockPhase::Insert => {
                        state.insert_copies.insert(content_index, block.clone());
                    }
                    BlockPhase::End => {
                        state.insert_copies.remove(&content_index);
                    }
                }
                Some(Arc::new(AssistantProgress::MessageBlockState {
                    content_index,
                    phase,
                    block,
                }))
            }
            metadata @ AssistantProgress::MessageBlockMetadata(_) => Some(Arc::new(metadata)),
        }
    }

    /// `captureAssistantEvent`: never parses or traverses a block on a delta, including a
    /// no-growth delta. `existing` is a capture the event already carries (a relayed event).
    pub(crate) fn capture_event(
        &mut self,
        event: &AssistantMessageEvent,
        hint: &CaptureHint,
    ) -> Option<Arc<AssistantCapture>> {
        // The first push decides capture for the stream, whatever this event records.
        self.active_state()?;
        // A relay of a source that captured nothing, or captured a drifted index, gets no record:
        // lack, never wrong.
        match &hint.carried {
            Some(None) => return None,
            Some(Some(carried)) if carried.content_index.is_some() => return None,
            _ => {}
        }
        let content_index =
            event_content_index(event).map(|index| hint.content_index.unwrap_or(index));
        let built = {
            let state = self.active_state()?;
            if content_index.is_some_and(|index| state.failed_blocks.contains(&index)) {
                return None;
            }
            build_capture(state, event, hint)
        };
        match built {
            Ok(capture) => capture.map(Arc::new),
            Err(error) => {
                self.drop_capture(&error, content_index);
                None
            }
        }
    }
}

/// UTF-16 code units, the unit TS string offsets count.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

pub(crate) fn event_content_index(event: &AssistantMessageEvent) -> Option<usize> {
    match event {
        AssistantMessageEvent::Start { .. }
        | AssistantMessageEvent::Done { .. }
        | AssistantMessageEvent::Error { .. } => None,
        AssistantMessageEvent::TextStart { content_index, .. }
        | AssistantMessageEvent::TextDelta { content_index, .. }
        | AssistantMessageEvent::TextEnd { content_index, .. }
        | AssistantMessageEvent::ThinkingStart { content_index, .. }
        | AssistantMessageEvent::ThinkingDelta { content_index, .. }
        | AssistantMessageEvent::ThinkingEnd { content_index, .. }
        | AssistantMessageEvent::ToolcallStart { content_index, .. }
        | AssistantMessageEvent::ToolcallDelta { content_index, .. }
        | AssistantMessageEvent::ToolcallEnd { content_index, .. } => Some(*content_index),
    }
}

pub fn enable_assistant_capture(owner: &AssistantMessageEventStream) {
    owner.with_capture_slot(CaptureSlot::enable);
}

pub fn push_assistant_progress(
    owner: &AssistantMessageEventStream,
    progress: impl Into<ProgressPush>,
) {
    owner.push_progress(progress.into());
}

/// A text or thinking signature replaced without a public event; an unchanged or empty one
/// records nothing.
pub fn push_assistant_signature(
    owner: &AssistantMessageEventStream,
    content_index: usize,
    block: &ContentBlock,
    signature: Option<&str>,
) {
    let Some(signature) = signature.filter(|signature| !signature.is_empty()) else {
        return;
    };
    let metadata = match block {
        ContentBlock::Text(text) => {
            if text.text_signature.as_deref() == Some(signature) {
                return;
            }
            BlockMetadata {
                content_index,
                text_signature: Some(signature.to_owned()),
                ..BlockMetadata::default()
            }
        }
        ContentBlock::Thinking(thinking) => {
            if thinking.thinking_signature.as_deref() == Some(signature) {
                return;
            }
            BlockMetadata {
                content_index,
                thinking_signature: Some(signature.to_owned()),
                ..BlockMetadata::default()
            }
        }
        _ => return,
    };
    push_assistant_progress(owner, AssistantProgress::metadata(metadata));
}

/// A block appended complete with no public event: its unchanged end reuses the insert's frozen
/// copy.
pub fn push_assistant_complete_block(
    owner: &AssistantMessageEventStream,
    content_index: usize,
    block: &ContentBlock,
) {
    for phase in [BlockPhase::Insert, BlockPhase::End] {
        push_assistant_progress(
            owner,
            AssistantProgress::block_state(content_index, phase, block),
        );
    }
}

/// Internal consumption is explicit; ordinary iterators are the downgrade boundary. Attached
/// before the first push, it turns capture on.
pub fn assistant_records(source: &AssistantMessageEventStream) -> AssistantRecords {
    source.with_capture_slot(CaptureSlot::demand);
    AssistantRecords {
        source: source.clone(),
    }
}

pub struct AssistantRecords {
    source: AssistantMessageEventStream,
}

impl AssistantRecords {
    pub async fn next(&self) -> Result<Option<AssistantRecord>, StreamError> {
        self.source.next_record().await
    }

    pub async fn collect(&self) -> Result<Vec<AssistantRecord>, StreamError> {
        let mut records = Vec::new();
        while let Some(record) = self.next().await? {
            records.push(record);
        }
        Ok(records)
    }
}

/// Events only; each progress record goes to `on_progress` as it is read, so a forwarder spends no
/// turn on it.
pub fn assistant_events<F: FnMut(FrozenProgress) + Send>(
    source: &AssistantMessageEventStream,
    on_progress: F,
) -> AssistantEvents<F> {
    AssistantEvents {
        source: source.clone(),
        on_progress,
    }
}

pub struct AssistantEvents<F> {
    source: AssistantMessageEventStream,
    on_progress: F,
}

impl<F: FnMut(FrozenProgress) + Send> AssistantEvents<F> {
    pub async fn next(&mut self) -> Result<Option<CapturedEvent>, StreamError> {
        loop {
            match self.source.next_record().await? {
                None => return Ok(None),
                Some(AssistantRecord::Event(event)) => return Ok(Some(event)),
                Some(AssistantRecord::Progress(progress)) => (self.on_progress)(progress),
            }
        }
    }

    pub fn source(&self) -> &AssistantMessageEventStream {
        &self.source
    }
}

/// Internal readers address a block by `capture.content_index ?? event.content_index`; the public
/// index can drift (Google/Vertex).
pub fn get_assistant_capture<T>(event: &Captured<T>) -> Option<&AssistantCapture> {
    event.capture.as_deref()
}

pub fn is_assistant_progress(record: &AssistantRecord) -> bool {
    matches!(record, AssistantRecord::Progress(_))
}

/// `dropAssistantCapture`: the event without its record.
pub fn drop_assistant_capture<T>(event: &mut Captured<T>) {
    event.capture = None;
}

/// The source's capture on a new value; with `terminal`, a capture rewritten for the replaced
/// terminal message.
pub fn carry_assistant_capture<S, T>(
    source: &Captured<S>,
    target: T,
    terminal: Option<&AssistantMessage>,
) -> Captured<T> {
    let capture = source.capture.as_ref().map(|capture| match terminal {
        Some(terminal) => Arc::new(terminal_capture(capture, terminal)),
        None => capture.clone(),
    });
    Captured {
        value: target,
        capture,
    }
}

#[cfg(test)]
#[path = "assistant_capture_tests.rs"]
mod tests;
