//! The append-only memory journal.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-journal.md`. Every change to the memory store is
//! first recorded as a journal event, so an interrupted write can be replayed to the state it was
//! meant to reach. Events are JSON lines with a monotonic sequence number; a torn last line is
//! dropped on load and never repaired into a different event.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a journal event records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Event {
    /// A memory was written.
    Remember {
        /// The memory id.
        id: String,
        /// The stored content, already redacted.
        content: String,
    },
    /// A memory was deleted.
    Forget {
        /// The memory id.
        id: String,
    },
    /// A memory's importance changed.
    Revalue {
        /// The memory id.
        id: String,
        /// The new importance.
        importance: u32,
    },
}

impl Event {
    /// The memory id this event concerns.
    pub fn id(&self) -> &str {
        match self {
            Self::Remember { id, .. } | Self::Forget { id } | Self::Revalue { id, .. } => id,
        }
    }
}

/// One numbered journal entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Monotonic, starting at 1.
    pub seq: u64,
    /// The event.
    #[serde(flatten)]
    pub event: Event,
}

/// Why an event could not be appended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    /// `seq` did not follow the last entry.
    OutOfOrder {
        /// The sequence number that was offered.
        offered: u64,
        /// The next number expected.
        expected: u64,
    },
    /// The line is not a journal entry.
    Malformed {
        /// 1-based line number.
        line: usize,
        /// The parser's message.
        reason: String,
    },
}

/// The state replay reaches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replayed {
    /// Live memories: id to content.
    pub memories: BTreeMap<String, String>,
    /// Importance per id; absent means the default.
    pub importance: BTreeMap<String, u32>,
    /// Events that could not apply, with the reason, in order.
    pub discarded: Vec<(u64, String)>,
}

/// An in-memory journal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Journal {
    entries: Vec<Entry>,
}

impl Journal {
    /// An empty journal.
    pub fn new() -> Self {
        Self::default()
    }

    /// The entries recorded so far.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The next sequence number.
    pub fn next_seq(&self) -> u64 {
        self.entries.last().map_or(1, |entry| entry.seq + 1)
    }

    /// Append an event, refusing a number that does not follow the last one.
    pub fn append(&mut self, seq: u64, event: Event) -> Result<(), JournalError> {
        let expected = self.next_seq();
        if seq != expected {
            return Err(JournalError::OutOfOrder {
                offered: seq,
                expected,
            });
        }
        self.entries.push(Entry { seq, event });
        Ok(())
    }

    /// Append with the next number.
    pub fn record(&mut self, event: Event) -> u64 {
        let seq = self.next_seq();
        self.entries.push(Entry { seq, event });
        seq
    }

    /// Render the journal as JSON lines; every line ends with a newline.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            out.push_str(&serde_json::to_string(entry).expect("journal entries serialize"));
            out.push('\n');
        }
        out
    }

    /// Read a journal, dropping a torn final line.
    ///
    /// A damaged line anywhere but the end is a real corruption and is reported; a damaged last
    /// line is what a crash mid-write leaves behind, so it is discarded and the rest is kept.
    pub fn parse(source: &str) -> Result<Self, JournalError> {
        let mut entries: Vec<Entry> = Vec::new();
        let lines: Vec<&str> = source.lines().collect();
        let last = lines.len().saturating_sub(1);
        for (index, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(line) {
                Ok(entry) => entries.push(entry),
                Err(_) if index == last => break,
                Err(error) => {
                    return Err(JournalError::Malformed {
                        line: index + 1,
                        reason: error.to_string(),
                    });
                }
            }
        }
        entries.sort_by_key(|entry| entry.seq);
        Ok(Self { entries })
    }

    /// Replay every event into the state it describes.
    ///
    /// An event that cannot apply — forgetting an id that is absent, revaluing an id that was never
    /// remembered — is discarded with a reason instead of aborting the replay, because a lost tail
    /// event must not hide the events that came before it.
    pub fn replay(&self) -> Replayed {
        let mut state = Replayed::default();
        for entry in &self.entries {
            match &entry.event {
                Event::Remember { id, content } => {
                    state.memories.insert(id.clone(), content.clone());
                }
                Event::Forget { id } => {
                    if state.memories.remove(id).is_none() {
                        state
                            .discarded
                            .push((entry.seq, format!("forget of unknown memory {id}")));
                    }
                    state.importance.remove(id);
                }
                Event::Revalue { id, importance } => {
                    if !state.memories.contains_key(id) {
                        state
                            .discarded
                            .push((entry.seq, format!("revalue of unknown memory {id}")));
                        continue;
                    }
                    state.importance.insert(id.clone(), *importance);
                }
            }
        }
        state
    }

    /// Drop everything up to and including `seq`, keeping the tail.
    ///
    /// Compaction is only safe after the state has been persisted elsewhere; the journal does not
    /// decide that for the caller.
    pub fn compact_through(&mut self, seq: u64) {
        self.entries.retain(|entry| entry.seq > seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remember(id: &str, content: &str) -> Event {
        Event::Remember {
            id: id.to_owned(),
            content: content.to_owned(),
        }
    }

    #[test]
    fn numbers_start_at_one_and_advance() {
        let mut journal = Journal::new();
        assert_eq!(journal.next_seq(), 1);
        assert_eq!(journal.record(remember("a", "one")), 1);
        assert_eq!(journal.next_seq(), 2);
    }

    #[test]
    fn an_out_of_order_number_is_refused() {
        let mut journal = Journal::new();
        journal.record(remember("a", "one"));
        assert_eq!(
            journal.append(5, remember("b", "two")),
            Err(JournalError::OutOfOrder {
                offered: 5,
                expected: 2
            })
        );
        assert_eq!(journal.entries().len(), 1);
    }

    #[test]
    fn a_journal_round_trips_through_json_lines() {
        let mut journal = Journal::new();
        journal.record(remember("a", "one"));
        journal.record(Event::Forget { id: "a".to_owned() });
        let parsed = Journal::parse(&journal.render()).expect("parse");
        assert_eq!(parsed, journal);
    }

    #[test]
    fn a_torn_final_line_is_dropped_and_the_rest_kept() {
        let mut journal = Journal::new();
        journal.record(remember("a", "one"));
        let mut text = journal.render();
        text.push_str("{\"seq\":2,\"op\":\"remem");
        let parsed = Journal::parse(&text).expect("parse");
        assert_eq!(parsed.entries().len(), 1);
    }

    #[test]
    fn a_damaged_line_before_the_end_is_an_error() {
        let text = "{\"seq\":1,\"op\":\"forget\",\"id\":\"a\"}\nnot json\n{\"seq\":3,\"op\":\"forget\",\"id\":\"b\"}\n";
        assert!(matches!(
            Journal::parse(text),
            Err(JournalError::Malformed { line: 2, .. })
        ));
    }

    #[test]
    fn replay_reaches_the_state_the_events_describe() {
        let mut journal = Journal::new();
        journal.record(remember("a", "first"));
        journal.record(remember("b", "second"));
        journal.record(Event::Forget { id: "a".to_owned() });
        let state = journal.replay();
        assert_eq!(state.memories.len(), 1);
        assert_eq!(state.memories.get("b").map(String::as_str), Some("second"));
        assert!(state.discarded.is_empty());
    }

    #[test]
    fn revaluing_a_known_memory_records_its_importance() {
        let mut journal = Journal::new();
        journal.record(remember("a", "first"));
        journal.record(Event::Revalue {
            id: "a".to_owned(),
            importance: 7,
        });
        assert_eq!(journal.replay().importance.get("a").copied(), Some(7));
    }

    #[test]
    fn an_event_that_cannot_apply_is_discarded_not_fatal() {
        let mut journal = Journal::new();
        journal.record(Event::Forget {
            id: "missing".to_owned(),
        });
        journal.record(remember("a", "first"));
        let state = journal.replay();
        assert_eq!(state.memories.len(), 1);
        assert_eq!(state.discarded.len(), 1);
        assert_eq!(state.discarded[0].0, 1);
    }

    #[test]
    fn revaluing_an_unknown_memory_leaves_no_importance() {
        let mut journal = Journal::new();
        journal.record(Event::Revalue {
            id: "missing".to_owned(),
            importance: 3,
        });
        let state = journal.replay();
        assert!(state.importance.is_empty());
        assert_eq!(state.discarded.len(), 1);
    }

    #[test]
    fn forgetting_also_drops_the_importance() {
        let mut journal = Journal::new();
        journal.record(remember("a", "first"));
        journal.record(Event::Revalue {
            id: "a".to_owned(),
            importance: 9,
        });
        journal.record(Event::Forget { id: "a".to_owned() });
        let state = journal.replay();
        assert!(state.importance.is_empty());
    }

    #[test]
    fn re_remembering_an_id_replaces_its_content() {
        let mut journal = Journal::new();
        journal.record(remember("a", "old"));
        journal.record(remember("a", "new"));
        assert_eq!(
            journal.replay().memories.get("a").map(String::as_str),
            Some("new")
        );
    }

    #[test]
    fn compaction_keeps_the_tail_and_continues_the_numbering() {
        let mut journal = Journal::new();
        for index in 0..5 {
            journal.record(remember(&format!("m{index}"), "content"));
        }
        journal.compact_through(3);
        assert_eq!(journal.entries().len(), 2);
        assert_eq!(journal.entries()[0].seq, 4);
        assert_eq!(journal.next_seq(), 6);
    }

    #[test]
    fn an_empty_source_parses_to_an_empty_journal() {
        let journal = Journal::parse("").expect("parse");
        assert!(journal.entries().is_empty());
        assert_eq!(journal.next_seq(), 1);
    }

    #[test]
    fn every_event_reports_its_memory_id() {
        assert_eq!(remember("a", "x").id(), "a");
        assert_eq!(Event::Forget { id: "b".to_owned() }.id(), "b");
        assert_eq!(
            Event::Revalue {
                id: "c".to_owned(),
                importance: 1
            }
            .id(),
            "c"
        );
    }

    #[test]
    fn a_rendered_journal_has_one_line_per_entry() {
        let mut journal = Journal::new();
        journal.record(remember("a", "one"));
        journal.record(remember("b", "two"));
        assert_eq!(journal.render().lines().count(), 2);
        assert!(journal.render().ends_with('\n'));
    }
}
