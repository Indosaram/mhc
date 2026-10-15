//! Port of senpi packages/coding-agent/src/core/session-summary-lru.ts.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::session_summary::SessionSummary;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileStamp {
    pub size: u64,
    pub mtime_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryEntry {
    pub stamp: FileStamp,
    pub summary: Arc<SessionSummary>,
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
/// transcript bytes. The map's insertion order is the recency order, so a hit promotes an entry by
/// moving it to the end.
#[derive(Debug)]
pub struct SessionSummaryLru {
    entries: IndexMap<String, RetainedEntry>,
    budget: SummaryCacheBudget,
    retained_text_bytes: usize,
}

impl SessionSummaryLru {
    pub fn new(budget: SummaryCacheBudget) -> Self {
        Self { entries: IndexMap::new(), budget, retained_text_bytes: 0 }
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    pub fn text_bytes(&self) -> usize {
        self.retained_text_bytes
    }

    pub fn get(&mut self, key: &str) -> Option<SummaryEntry> {
        let index = self.entries.get_index_of(key)?;
        let last = self.entries.len() - 1;
        self.entries.move_index(index, last);
        self.entries.get_index(last).map(|(_, retained)| retained.entry.clone())
    }

    pub fn drop_key(&mut self, key: &str) {
        if let Some(retained) = self.entries.shift_remove(key) {
            self.retained_text_bytes = self.retained_text_bytes.saturating_sub(retained.text_bytes);
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
        self.retained_text_bytes += text_bytes;
        self.evict_until_within_budget();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.retained_text_bytes = 0;
    }

    fn evict_until_within_budget(&mut self) {
        while self.entries.len() > self.budget.max_entries || self.retained_text_bytes > self.budget.max_text_bytes {
            let Some((_, oldest)) = self.entries.shift_remove_index(0) else { break };
            self.retained_text_bytes = self.retained_text_bytes.saturating_sub(oldest.text_bytes);
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
            summary: Arc::new(SessionSummary {
                header: json!({ "type": "session", "id": "s" }),
                name: None,
                first_user_message: String::new(),
                message_count: 0,
                last_activity_time: None,
                all_messages_text: text.to_owned(),
            }),
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
    fn a_hit_promotes_and_re_retaining_moves_to_the_end() {
        let mut lru = SessionSummaryLru::new(SummaryCacheBudget { max_entries: 3, max_text_bytes: 1024 });
        lru.retain("a", entry("a"));
        lru.retain("b", entry("b"));
        lru.retain("c", entry("c"));
        lru.get("a");
        lru.retain("b", entry("bb"));
        lru.retain("d", entry("d"));
        assert!(lru.get("c").is_none());
        assert_eq!(lru.text_bytes(), 4);
        lru.retain("e", entry("e"));
        assert!(lru.get("a").is_none());
        assert_eq!(lru.get("b").expect("b").summary.all_messages_text, "bb");
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
