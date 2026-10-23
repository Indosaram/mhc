//! Overflow bookkeeping of the monitor notifier: events that did not fit a monitor's batch. Line
//! bodies are written to the monitor's overflow file in chunks (when a spill sink exists), and the
//! wake that carries the monitor names the file; without a file the wake says how many were omitted.
use crate::monitor_registry::MonitorEvent;
use indexmap::IndexMap;

/// Lines are written out in chunks of this many, so a burst never sits whole in memory.
pub const SPILL_CHUNK_LINES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorSpillResult {
    pub path: String,
    /// The file rotated to `<path>.1` while this chunk was written.
    pub rotated: bool,
}

pub type MonitorSpill = Box<dyn Fn(&str, &[String]) -> Option<MonitorSpillResult> + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Line,
    Summary,
}
impl EventKind {
    pub fn of(event: &MonitorEvent) -> Self {
        match event {
            MonitorEvent::Line { .. } => Self::Line,
            MonitorEvent::Summary { .. } => Self::Summary,
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Summary => "summary",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MonitorOverflow {
    pub id: String,
    pub description: String,
    pub count: usize,
    pub kinds: Vec<EventKind>,
    pub human_input: bool,
    pub pending: Vec<String>,
    pub spilled: usize,
    pub path: Option<String>,
    pub rotated: bool,
}

pub struct OverflowBook {
    spill: Option<MonitorSpill>,
    entries: IndexMap<String, MonitorOverflow>,
}

impl OverflowBook {
    pub fn new(spill: Option<MonitorSpill>) -> Self {
        Self {
            spill,
            entries: IndexMap::new(),
        }
    }
    pub fn get(&self, id: &str) -> Option<&MonitorOverflow> {
        self.entries.get(id)
    }
    pub fn count(&self, id: &str) -> usize {
        self.entries.get(id).map_or(0, |entry| entry.count)
    }
    pub fn ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// `human_input` is true when the event's line is a person's words (upstream `event.humanInput`).
    pub fn record(&mut self, event: &MonitorEvent, body: &str, human_input: bool) {
        let (id, description) = match event {
            MonitorEvent::Line {
                id, description, ..
            }
            | MonitorEvent::Summary {
                id, description, ..
            } => (id, description),
        };
        let entry = self
            .entries
            .entry(id.clone())
            .or_insert_with(|| MonitorOverflow {
                id: id.clone(),
                description: description.clone(),
                count: 0,
                kinds: vec![],
                human_input: false,
                pending: vec![],
                spilled: 0,
                path: None,
                rotated: false,
            });
        entry.count += 1;
        let kind = EventKind::of(event);
        if !entry.kinds.contains(&kind) {
            entry.kinds.push(kind);
        }
        if kind == EventKind::Line && human_input {
            entry.human_input = true;
        }
        if self.spill.is_none() {
            return;
        }
        entry.pending.push(body.to_owned());
        if entry.pending.len() >= SPILL_CHUNK_LINES {
            let id = id.clone();
            self.flush(&id);
        }
    }

    pub fn flush(&mut self, id: &str) {
        let Some(spill) = &self.spill else {
            return;
        };
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        if entry.pending.is_empty() {
            return;
        }
        if let Some(result) = spill(id, &entry.pending) {
            entry.spilled += entry.pending.len();
            entry.path = Some(result.path);
            entry.rotated |= result.rotated;
        }
        entry.pending.clear();
    }

    /// The notice a wake carries for this monitor's overflow; "" when nothing overflowed.
    pub fn notice(&self, id: &str) -> String {
        let Some(entry) = self.entries.get(id).filter(|entry| entry.count > 0) else {
            return String::new();
        };
        let mut notices = vec![];
        if let (true, Some(path)) = (entry.spilled > 0, &entry.path) {
            let rotated = if entry.rotated {
                format!("; earlier ones in {path}.1")
            } else {
                String::new()
            };
            notices.push(format!("[{} more lines in {path}{rotated}]", entry.spilled));
        }
        let unspilled = entry.count - entry.spilled;
        if unspilled > 0 {
            notices.push(format!(
                "[{unspilled} additional event lines omitted; peek bash_output for full history.]"
            ));
        }
        notices.join("\n")
    }

    pub fn delete(&mut self, id: &str) {
        self.entries.shift_remove(id);
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    fn line(text: &str) -> MonitorEvent {
        MonitorEvent::Line {
            id: "b1".to_owned(),
            description: "watch".to_owned(),
            line: text.to_owned(),
        }
    }

    #[test]
    fn without_spill_the_notice_counts_omitted_lines() {
        let mut book = OverflowBook::new(None);
        assert_eq!(book.notice("b1"), "");
        book.record(&line("a"), "a", false);
        book.record(&line("b"), "b", true);
        assert_eq!(book.count("b1"), 2);
        assert_eq!(
            book.notice("b1"),
            "[2 additional event lines omitted; peek bash_output for full history.]"
        );
        assert!(book.get("b1").unwrap().human_input);
        book.delete("b1");
        assert_eq!(book.count("b1"), 0);
    }

    #[test]
    fn spill_flushes_in_chunks_and_names_the_file() {
        let calls = Arc::new(Mutex::new(vec![]));
        let seen = calls.clone();
        let mut book = OverflowBook::new(Some(Box::new(move |id, lines| {
            seen.lock().unwrap().push((id.to_owned(), lines.len()));
            Some(MonitorSpillResult {
                path: "/tmp/o.log".to_owned(),
                rotated: true,
            })
        })));
        for index in 0..(SPILL_CHUNK_LINES + 3) {
            book.record(&line("x"), &index.to_string(), false);
        }
        assert_eq!(
            *calls.lock().unwrap(),
            vec![("b1".to_owned(), SPILL_CHUNK_LINES)]
        );
        book.flush("b1");
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert_eq!(
            book.notice("b1"),
            format!(
                "[{} more lines in /tmp/o.log; earlier ones in /tmp/o.log.1]",
                SPILL_CHUNK_LINES + 3
            )
        );
    }
}
