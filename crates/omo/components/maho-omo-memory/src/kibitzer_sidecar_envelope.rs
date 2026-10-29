//! From buffered events and candidates to the input of one envelope (latest
//! kibitzer/sidecar-envelope.ts).
//!
//! A payload is what one envelope carries; carried payloads (never read by a child) merge ahead of
//! the fresh batch, events by seq and candidates by first sight, and the result is shaped into the
//! borrowed input the prompt renderer reads. The prompt renderer (kibitzer_prompt) remains the owner
//! of redaction, cap, XML escaping, cursor ordering and the event window; this module only orders,
//! dedupes and shapes owned values, and it never recaptures a payload through the capture hooks - the
//! events producer hands it an already-captured batch.
//!
//! # Deviations (each an N/A, because the Rust crate has no direct counterpart)
//!
//! * Upstream payloadOf(batch, candidates) takes a KibitzerEventBatch and copies events, digest and
//!   cursors through. The Rust Payload (kibitzer_sidecar_core) stores the prompt-side digest and
//!   span types, so payload_of maps the events batch's KibitzerEventDigest / KibitzerEventBatchCursors
//!   onto KibitzerSidecarDigest / KibitzerCursorSpan; every value the envelope renders survives the
//!   map, and the two seq fields the prompt digest never carried are the only ones left behind.
//! * Upstream envelopeInput returns a plain object that borrows its arrays. The Rust prompt input
//!   borrows its events and candidates as slices, so the mapped candidates need owned storage:
//!   envelope_input is delivered as an owned adapter (KibitzerEnvelopeAdapter) that holds the
//!   payload's events and digest, the mapped candidates and the gated task summary, and yields the
//!   borrowed KibitzerEnvelopeInput through its input method. The adapter never returns a reference
//!   to a temporary.
//! * Upstream hands RecallCandidate through to the renderer by structural typing; the Rust
//!   renderer's KibitzerSidecarCandidate is the same shape with optional fields, so
//!   sidecar_candidates maps them losslessly.
//! * The prompt's cursor span is derived from every event cursor plus the digest range, so the
//!   payload's own cursor span is not part of the input; payload_of still fills it for the payload
//!   carrier and the adapter simply does not read it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::PoisonError;

use memory_core::recall::RecallCandidate;

use crate::kibitzer_events::{KibitzerEvent, KibitzerEventBatch, KibitzerEventBatchCursors, KibitzerEventDigest};
use crate::kibitzer_prompt::{KibitzerEnvelopeInput, KibitzerSidecarCandidate, KibitzerSidecarDigest};
use crate::kibitzer_prompt_blocks::KIBITZER_FIELD_CAPS;
use crate::kibitzer_sidecar_core::{Payload, SidecarCore};
use crate::kibitzer_sidecar_outcome::KibitzerCursorSpan;

/// payloadOf: one already-captured event batch plus the candidates this wake offers, kept together
/// as the payload one envelope carries. The optional digest and cursor span stay absent when the
/// batch has none. The batch is consumed by value; nothing is recaptured or renumbered.
pub fn payload_of(batch: KibitzerEventBatch, candidates: Vec<RecallCandidate>) -> Payload {
    Payload {
        events: batch.events,
        digest: batch.digest.as_ref().map(sidecar_digest),
        candidates,
        cursors: batch.cursors.as_ref().map(sidecar_cursor_span),
    }
}

/// merge: carried payloads oldest first, then the fresh batch. Events collapse by seq (the last
/// value wins) and sort numerically by seq, never by cursor or arrival; candidates keep their first
/// sight (the first value wins) in first-sight order; the first present digest wins; cursor spans
/// widen to the minimum first and the maximum last over every present span. No input payload and no
/// live stream is mutated.
pub fn merge(parts: &[Payload]) -> Payload {
    let mut by_seq: BTreeMap<usize, KibitzerEvent> = BTreeMap::new();
    let mut candidates: Vec<RecallCandidate> = Vec::new();
    let mut seen_paths: BTreeSet<String> = BTreeSet::new();
    let mut digest: Option<KibitzerSidecarDigest> = None;
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    for part in parts {
        for event in &part.events {
            by_seq.insert(event.seq, event.clone());
        }
        for candidate in &part.candidates {
            if seen_paths.insert(candidate.path.clone()) {
                candidates.push(candidate.clone());
            }
        }
        if digest.is_none() {
            digest = part.digest.clone();
        }
        if let Some(span) = part.cursors {
            first = Some(first.map_or(span.first, |value| value.min(span.first)));
            last = Some(last.map_or(span.last, |value| value.max(span.last)));
        }
    }
    let cursors = match (first, last) {
        (Some(first), Some(last)) => Some(KibitzerCursorSpan { first, last }),
        _ => None,
    };
    Payload { events: by_seq.into_values().collect(), digest, candidates, cursors }
}

/// envelopeInput's digest map: the events batch fold onto the prompt's digest shape. The line, the
/// cursor range and the folded count all survive; the batch digest's two sequence bounds are the
/// only fields the prompt digest never carried.
pub fn sidecar_digest(digest: &KibitzerEventDigest) -> KibitzerSidecarDigest {
    KibitzerSidecarDigest {
        text: digest.line.clone(),
        cursor_from: digest.first_cursor,
        cursor_to: digest.last_cursor,
        folded: digest.count,
    }
}

/// envelopeInput's cursor-span map: the batch span onto the outcome's cursor span. Kept for the
/// payload carrier; the prompt input derives its own span from the events and the digest.
pub fn sidecar_cursor_span(cursors: &KibitzerEventBatchCursors) -> KibitzerCursorSpan {
    KibitzerCursorSpan { first: cursors.first, last: cursors.last }
}

/// Shapes a payload's candidates for the prompt renderer. Upstream hands RecallCandidate through by
/// structural typing; the Rust renderer's KibitzerSidecarCandidate stores the same fields as
/// options, so every present value stays present and the mapping is lossless.
pub fn sidecar_candidates(candidates: &[RecallCandidate]) -> Vec<KibitzerSidecarCandidate> {
    candidates
        .iter()
        .map(|candidate| KibitzerSidecarCandidate {
            path: candidate.path.clone(),
            description: Some(candidate.description.clone()),
            excerpt: Some(candidate.excerpt.clone()),
            score: Some(candidate.score),
        })
        .collect()
}

/// The owned storage behind ONE envelope's borrowed input. The prompt input borrows its events and
/// candidates as slices, so the mapped candidates and the gated task summary must live somewhere
/// owned; this adapter holds them (plus the payload's events and digest) and hands out the borrowed
/// KibitzerEnvelopeInput through its input method, never a reference to a temporary.
pub struct KibitzerEnvelopeAdapter {
    session_id: String,
    max_items: usize,
    tool_budget: usize,
    events: Vec<KibitzerEvent>,
    digest: Option<KibitzerSidecarDigest>,
    candidates: Vec<KibitzerSidecarCandidate>,
    task_summary: Option<String>,
}

impl KibitzerEnvelopeAdapter {
    /// The borrowed prompt input over this owned storage: session id, max items and tool budget from
    /// the core and the call, the payload's events and digest, the mapped candidates and the gated
    /// task summary. The prompt orders the events by cursor, windows them and renders them.
    pub fn input(&self) -> KibitzerEnvelopeInput<'_> {
        KibitzerEnvelopeInput {
            session_id: &self.session_id,
            max_items: self.max_items,
            events: &self.events,
            candidates: &self.candidates,
            digest: self.digest.as_ref(),
            task_summary: self.task_summary.as_deref(),
            tool_budget: Some(self.tool_budget),
            caps: KIBITZER_FIELD_CAPS,
            event_window: None,
        }
    }
}

/// envelopeInput: shapes a payload for the prompt renderer, owning the mapped candidates and the
/// task summary so the borrowed input outlives the mapping temporaries. The core supplies the
/// session id, the tool budget and the task summary; with_task gates the summary (the seed carries
/// it, a wake usually does not). The payload's own cursor span is not read: the prompt derives its
/// span from the events and the digest.
pub fn envelope_input(core: &SidecarCore, payload: Payload, max_items: usize, with_task: bool) -> KibitzerEnvelopeAdapter {
    let Payload { events, digest, candidates, .. } = payload;
    let task_summary = if with_task {
        core.record.lock().unwrap_or_else(PoisonError::into_inner).task_summary.clone()
    } else {
        None
    };
    KibitzerEnvelopeAdapter {
        session_id: core.session_id.clone(),
        max_items,
        tool_budget: core.tool_budget,
        events,
        digest,
        candidates: sidecar_candidates(&candidates),
        task_summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(seq: usize, cursor: usize, kind: &str, body: &str) -> KibitzerEvent {
        KibitzerEvent {
            seq,
            cursor,
            kind: kind.to_string(),
            tool: None,
            is_error: None,
            truncated: false,
            body: body.to_string(),
        }
    }

    fn batch(events: Vec<KibitzerEvent>, digest: Option<KibitzerEventDigest>, cursors: Option<KibitzerEventBatchCursors>) -> KibitzerEventBatch {
        KibitzerEventBatch { events, digest, cursors }
    }

    fn event_digest(line: &str, first_cursor: usize, last_cursor: usize, count: usize) -> KibitzerEventDigest {
        KibitzerEventDigest { count, first_seq: 1, last_seq: count, first_cursor, last_cursor, line: line.to_string() }
    }

    fn candidate(path: &str, score: f64) -> RecallCandidate {
        RecallCandidate {
            path: path.to_string(),
            description: format!("{path} description"),
            excerpt: format!("{path} excerpt"),
            score,
        }
    }

    #[test]
    fn given_duplicate_and_out_of_order_seqs_when_merged_then_last_wins_and_events_sort_by_seq() {
        let carried = payload_of(
            batch(vec![event(5, 50, "assistant", "carried-5"), event(2, 20, "prompt", "carried-2")], None, None),
            vec![],
        );
        let fresh = payload_of(
            batch(vec![event(2, 22, "prompt", "fresh-2"), event(9, 90, "assistant", "fresh-9")], None, None),
            vec![],
        );
        let merged = merge(&[carried, fresh]);
        let seqs: Vec<usize> = merged.events.iter().map(|event| event.seq).collect();
        assert_eq!(seqs, vec![2, 5, 9], "events sort numerically by seq");
        let duplicate = merged.events.iter().find(|event| event.seq == 2).expect("seq 2 present");
        assert_eq!(duplicate.body, "fresh-2", "the last value for a duplicate seq wins");
        assert_eq!(duplicate.cursor, 22);
    }

    #[test]
    fn given_duplicate_candidate_paths_when_merged_then_first_value_wins_and_order_is_kept() {
        let carried = payload_of(batch(vec![], None, None), vec![candidate("a.md", 0.1), candidate("b.md", 0.2)]);
        let fresh = payload_of(batch(vec![], None, None), vec![candidate("a.md", 0.9), candidate("c.md", 0.3)]);
        let merged = merge(&[carried, fresh]);
        let paths: Vec<&str> = merged.candidates.iter().map(|candidate| candidate.path.as_str()).collect();
        assert_eq!(paths, vec!["a.md", "b.md", "c.md"], "first-sight order, no duplicate path");
        assert_eq!(merged.candidates[0].score, 0.1, "the first candidate for a path wins");
        assert_eq!(merged.candidates[0].description, "a.md description");
    }

    #[test]
    fn given_multiple_digests_and_spans_when_merged_then_first_digest_wins_and_span_widens() {
        let one = payload_of(batch(vec![], Some(event_digest("first", 1, 5, 4)), Some(KibitzerEventBatchCursors { first: 4, last: 10 })), vec![]);
        let two = payload_of(batch(vec![], Some(event_digest("second", 2, 9, 7)), Some(KibitzerEventBatchCursors { first: 1, last: 7 })), vec![]);
        let merged = merge(&[one, two]);
        assert_eq!(merged.digest.as_ref().map(|digest| digest.text.as_str()), Some("first"));
        let span = merged.cursors.expect("a widened span");
        assert_eq!(span.first, 1, "the minimum first");
        assert_eq!(span.last, 10, "the maximum last");
    }

    #[test]
    fn given_no_parts_when_merged_then_every_field_is_empty_or_absent() {
        let merged = merge(&[]);
        assert!(merged.events.is_empty());
        assert!(merged.candidates.is_empty());
        assert!(merged.digest.is_none());
        assert!(merged.cursors.is_none());
    }

    #[test]
    fn given_a_batch_when_payload_of_then_absent_options_stay_absent_and_present_ones_map_through() {
        let absent = payload_of(batch(vec![event(1, 3, "prompt", "hello")], None, None), vec![candidate("notes/a.md", 0.5)]);
        assert_eq!(absent.events.len(), 1);
        assert_eq!(absent.candidates.len(), 1);
        assert!(absent.digest.is_none());
        assert!(absent.cursors.is_none());

        let present = payload_of(
            batch(vec![event(2, 8, "assistant", "body")], Some(event_digest("fold line", 4, 30, 7)), Some(KibitzerEventBatchCursors { first: 4, last: 30 })),
            vec![],
        );
        let digest = present.digest.expect("a mapped digest");
        assert_eq!(digest.text, "fold line");
        assert_eq!(digest.cursor_from, 4);
        assert_eq!(digest.cursor_to, 30);
        assert_eq!(digest.folded, 7);
        let span = present.cursors.expect("a mapped span");
        assert_eq!(span.first, 4);
        assert_eq!(span.last, 30);
    }

    #[test]
    fn given_carried_and_fresh_payloads_when_merged_then_the_passed_inputs_are_unchanged_and_outcomes_are_ordered() {
        let parts = vec![
            payload_of(batch(vec![event(1, 10, "prompt", "carried")], None, None), vec![candidate("a.md", 0.1)]),
            payload_of(batch(vec![event(2, 20, "assistant", "fresh")], None, None), vec![candidate("b.md", 0.2)]),
        ];
        let before = parts.clone();
        let merged = merge(&parts);
        assert_eq!(parts, before, "merge must not mutate the payloads it was passed");
        let seqs: Vec<usize> = merged.events.iter().map(|event| event.seq).collect();
        assert_eq!(seqs, vec![1, 2], "merged events keep ascending seq order");
        let paths: Vec<&str> = merged.candidates.iter().map(|candidate| candidate.path.as_str()).collect();
        assert_eq!(paths, vec!["a.md", "b.md"], "merged candidates keep first-sight order");
        assert_eq!(merged.candidates[0].score, 0.1, "the carried candidate's value is kept");
    }
}
