//! Port of senpi `packages/tui/src/kill-ring.ts`.
//!
//! Ring buffer for Emacs-style kill/yank operations. Tracks killed (deleted) text entries.
//! Consecutive kills can accumulate into a single entry. Supports yank (paste most recent)
//! and yank-pop (cycle through older entries).

/// Ring buffer of killed text.
#[derive(Debug, Default, Clone)]
pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    pub fn new() -> Self {
        Self { ring: Vec::new() }
    }

    /// Add text to the kill ring.
    ///
    /// `prepend` selects, when accumulating, whether the new text goes before (backward
    /// deletion) or after (forward deletion) the most recent entry. `accumulate` merges with
    /// the most recent entry instead of creating a new one.
    pub fn push(&mut self, text: &str, prepend: bool, accumulate: bool) {
        if text.is_empty() {
            return;
        }

        if accumulate && !self.ring.is_empty() {
            let last = self.ring.pop().expect("ring is non-empty");
            self.ring.push(if prepend {
                format!("{text}{last}")
            } else {
                format!("{last}{text}")
            });
        } else {
            self.ring.push(text.to_string());
        }
    }

    /// Get the most recent entry without modifying the ring.
    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    /// Move the last entry to the front (for yank-pop cycling).
    pub fn rotate(&mut self) {
        if self.ring.len() > 1 {
            let last = self.ring.pop().expect("ring has more than one entry");
            self.ring.insert(0, last);
        }
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}
