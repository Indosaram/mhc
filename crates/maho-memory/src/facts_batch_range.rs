//! Choosing which slice of a transcript one extraction batch covers.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-batch-range.md`. The batch must start strictly after what has
//! already been handled and end at the newest canonical message, so consecutive runs tile the transcript
//! without gaps or repeats.

use crate::cursor::TranscriptEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRange {
    pub start_index: usize,
    pub end_index: usize,
}

impl BatchRange {
    pub fn slice<'a>(&self, entries: &'a [TranscriptEntry]) -> &'a [TranscriptEntry] {
        &entries[self.start_index..=self.end_index]
    }
}

/// `None` when there is nothing new: no canonical entry after the anchor.
///
/// A run with nothing new must not enqueue an empty batch. An empty batch would be consumed, advance nothing,
/// and re-enqueue on the next run forever — the queue head would never drain.
pub fn plan_batch_range(entries: &[TranscriptEntry], anchor: i64) -> Option<BatchRange> {
    let start_index = entries
        .iter()
        .enumerate()
        .position(|(index, entry)| index as i64 > anchor && entry.is_canonical())?;
    let end_index = entries.iter().rposition(TranscriptEntry::is_canonical)?;
    Some(BatchRange {
        start_index,
        end_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, message: &str, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            kind: kind.to_owned(),
            text: Some(text.to_owned()),
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
            source_line_id: format!("line-{message}"),
            source_message_id: message.to_owned(),
        }
    }

    fn transcript() -> Vec<TranscriptEntry> {
        vec![
            entry("user", "m1", "hi"),
            entry("assistant", "m2", "first answer"),
            entry("user", "m3", "more"),
            entry("assistant", "m4", "second answer"),
        ]
    }

    #[test]
    fn a_fresh_conversation_starts_at_the_first_canonical_message() {
        let range = plan_batch_range(&transcript(), -1).expect("a batch");
        assert_eq!(range.start_index, 1);
        assert_eq!(range.end_index, 3);
    }

    #[test]
    fn only_messages_after_the_anchor_are_included() {
        let range = plan_batch_range(&transcript(), 1).expect("a batch");
        assert_eq!(range.start_index, 3);
        assert_eq!(range.end_index, 3);
    }

    #[test]
    fn the_anchor_message_itself_is_never_reincluded() {
        assert_eq!(plan_batch_range(&transcript(), 3), None);
    }

    #[test]
    fn an_anchor_beyond_the_end_means_nothing_new() {
        assert_eq!(plan_batch_range(&transcript(), 99), None);
    }

    #[test]
    fn a_transcript_without_canonical_messages_is_not_a_batch() {
        let entries = vec![entry("user", "m1", "hi"), entry("user", "m2", "again")];
        assert_eq!(plan_batch_range(&entries, -1), None);
    }

    #[test]
    fn an_empty_transcript_is_not_a_batch() {
        assert_eq!(plan_batch_range(&[], -1), None);
    }

    #[test]
    fn a_blank_assistant_message_is_not_canonical() {
        let entries = vec![
            entry("assistant", "m1", "   "),
            entry("assistant", "m2", "real"),
        ];
        let range = plan_batch_range(&entries, -1).expect("a batch");
        assert_eq!(range.start_index, 1);
    }

    #[test]
    fn an_assistant_message_without_an_id_is_not_canonical() {
        let entries = vec![
            entry("assistant", "", "no id"),
            entry("assistant", "m2", "real"),
        ];
        let range = plan_batch_range(&entries, -1).expect("a batch");
        assert_eq!(range.start_index, 1);
    }

    #[test]
    fn the_batch_ends_at_the_newest_canonical_not_the_last_entry() {
        let mut entries = transcript();
        entries.push(entry("user", "m5", "trailing user message"));
        let range = plan_batch_range(&entries, -1).expect("a batch");
        assert_eq!(range.end_index, 3);
        assert_eq!(
            range.slice(&entries).last().expect("end").source_message_id,
            "m4"
        );
    }

    #[test]
    fn the_slice_covers_every_entry_through_the_end_inclusive() {
        let entries = transcript();
        let range = plan_batch_range(&entries, -1).expect("a batch");
        let slice = range.slice(&entries);
        assert_eq!(slice.len(), 3);
        assert_eq!(slice[0].source_message_id, "m2");
        assert_eq!(slice[2].source_message_id, "m4");
    }

    #[test]
    fn consecutive_runs_tile_the_transcript_without_gaps() {
        let entries = transcript();
        let first = plan_batch_range(&entries, -1).expect("first");
        assert_eq!(first.end_index, 3);
        assert_eq!(plan_batch_range(&entries, first.end_index as i64), None);
    }

    #[test]
    fn a_zero_anchor_skips_only_the_first_line() {
        let range = plan_batch_range(&transcript(), 0).expect("a batch");
        assert_eq!(range.start_index, 1);
    }
}
