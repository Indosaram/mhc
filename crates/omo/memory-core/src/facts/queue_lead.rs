//! The non-reasoning transcript row directly preceding a queue delta.
use crate::journal::entries::TranscriptEntry;
pub fn lead_of(entries: &[TranscriptEntry], start_index: usize) -> Option<TranscriptEntry> {
    entries[..start_index]
        .iter()
        .rev()
        .find(|entry| entry.kind() != "reasoning")
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(kind: &str) -> TranscriptEntry {
        TranscriptEntry::Text(crate::journal::entries::TextTranscriptEntry::new(
            kind, kind, "now", kind, kind,
        ))
    }
    #[test]
    fn skips_reasoning_but_preserves_intervening_non_reasoning_rows() {
        assert_eq!(lead_of(&[], 0), None);
        let proposal = row("assistant");
        let entries = vec![proposal.clone(), row("reasoning"), row("user")];
        assert_eq!(lead_of(&entries, 2), Some(proposal));
        let entries = vec![row("assistant"), row("user"), row("reasoning"), row("user")];
        assert_eq!(lead_of(&entries, 3), Some(row("user")));
    }
}
