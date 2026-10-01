//! Port of senpi packages/coding-agent/src/core/session-summary-lru.ts.

use std::collections::HashMap;

use crate::session_summary::SessionSummary;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileStamp {
    pub size: u64,
    pub mtime_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryEntry {
    pub stamp: FileStamp,
    pub summary: SessionSummary,
    pub mtime: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryCacheBudget {
    pub max_entries: usize,
    pub max_text_bytes: usize,
}

#[derive(Debug)]
struct RetainedEntry {
    entry: SummaryEntry,
    text_bytes: usize,
}

/// Least-recently-used store of session summaries bounded by BOTH entry count and retained
/// transcript bytes. Insertion order is tracked explicitly so a hit promotes an entry.
#[derive(Debug)]
pub struct SessionSummaryLru {
    entries: HashMap<String, RetainedEntry>,
    order: Vec<String>,
    budget: SummaryCacheBudget,
    retained_text_bytes: usize,
}

impl SessionSummaryLru {
    pub fn new(budget: SummaryCacheBudget) -> Self {
        Self { entries: HashMap::new(), order: Vec::new(), budget, retained_text_bytes: 0 }
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    pub fn text_bytes(&self) -> usize {
        self.retained_text_bytes
    }

    pub fn get(&mut self, key: &str) -> Option<SummaryEntry> {
        let retained = self.entries.get(key)?;
        let entry = retained.entry.clone();
        self.order.retain(|existing| existing != key);
        self.order.push(key.to_owned());
        Some(entry)
    }

    pub fn drop_key(&mut self, key: &str) {
        if let Some(retained) = self.entries.remove(key) {
            self.retained_text_bytes = self.retained_text_bytes.saturating_sub(retained.text_bytes);
            self.order.retain(|existing| existing != key);
        }
    }

    /// Retains one summary, evicting least-recently-used entries until both ceilings hold. A
    /// summary whose own transcript exceeds the byte budget is never retained.
    pub fn retain(&mut self, key: &str, entry: SummaryEntry) {
        self.drop_key(key);
        let text_bytes = entry.summary.all_messages_text.len();
        if text_bytes > self.budget.max_text_bytes {
            return;
        }
        self.entries.insert(key.to_owned(), RetainedEntry { entry, text_bytes });
        self.order.push(key.to_owned());
        self.retained_text_bytes += text_bytes;
        self.evict_until_within_budget();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.retained_text_bytes = 0;
    }

    fn evict_until_within_budget(&mut self) {
        while self.entries.len() > self.budget.max_entries || self.retained_text_bytes > self.budget.max_text_bytes {
            let Some(oldest) = self.order.first().cloned() else { break };
            self.drop_key(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(text: &str) -> SummaryEntry {
        SummaryEntry {
            stamp: FileStamp { size: 1, mtime_ms: 1.0 },
            summary: SessionSummary {
                header: json!({ "type": "session", "id": "s" }),
                name: None,
                first_user_message: String::new(),
                message_count: 0,
                last_activity_time: None,
                all_messages_text: text.to_owned(),
            },
            mtime: chrono::Utc::now(),
        }
    }

    #[test]
    fn retaining_and_getting_round_trips() {
        let mut lru = SessionSummaryLru::new(SummaryCacheBudget { max_entries: 4, max_text_bytes: 1024 });
        lru.retain("a", entry("hello"));
        assert_eq!(lru.size(), 1);
        assert_eq!(lru.get("a").expect("entry").summary.all_messages_text, "hello");
    }

    #[test]
    fn the_entry_ceiling_evicts_the_least_recently_used() {
        let mut lru = SessionSummaryLru::new(SummaryCacheBudget { max_entries: 2, max_text_bytes: 1024 });
        lru.retain("a", entry("a"));
        lru.retain("b", entry("b"));
        lru.get("a");
        lru.retain("c", entry("c"));
        assert!(lru.get("b").is_none());
        assert!(lru.get("a").is_some());
    }

    #[test]
    fn the_byte_ceiling_evicts_until_it_holds() {
        let mut lru = SessionSummaryLru::new(SummaryCacheBudget { max_entries: 100, max_text_bytes: 5 });
        lru.retain("a", entry("abc"));
        lru.retain("b", entry("de"));
        assert!(lru.text_bytes() <= 5);
    }

    #[test]
    fn an_oversized_summary_is_never_retained() {
        let mut lru = SessionSummaryLru::new(SummaryCacheBudget { max_entries: 100, max_text_bytes: 3 });
        lru.retain("big", entry("way too long"));
        assert!(lru.get("big").is_none());
        assert_eq!(lru.size(), 0);
    }

    #[test]
    fn clear_drops_everything() {
        let mut lru = SessionSummaryLru::new(SummaryCacheBudget { max_entries: 4, max_text_bytes: 1024 });
        lru.retain("a", entry("x"));
        lru.clear();
        assert_eq!(lru.size(), 0);
        assert_eq!(lru.text_bytes(), 0);
    }
}
