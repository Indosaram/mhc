//! Port of senpi `packages/coding-agent/src/modes/rpc/rpc-client-replies.ts`.
//!
//! Bounded tracking of unawaited client replies (`extension_ui_response`, `extension_ui_progress`)
//! and response dispatch before the next frame.

use serde_json::Value;
use std::collections::{HashMap, VecDeque};

/// The ids a client wrote without waiting for a reply (`extension_ui_response`,
/// `extension_ui_progress`), counted per id and bounded at `max` keys.
pub struct UnawaitedReplies {
    counts: HashMap<String, usize>,
    order: VecDeque<String>,
    max: usize,
}

impl Default for UnawaitedReplies {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl UnawaitedReplies {
    pub fn new(max: usize) -> Self {
        Self {
            counts: HashMap::new(),
            order: VecDeque::new(),
            max: max.max(1),
        }
    }

    pub fn note(&mut self, id: &str) {
        if let Some(count) = self.counts.get_mut(id) {
            *count += 1;
            return;
        }
        while self.counts.len() >= self.max {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.counts.remove(&oldest);
        }
        self.counts.insert(id.to_owned(), 1);
        self.order.push_back(id.to_owned());
    }

    pub fn take(&mut self, id: &str) -> bool {
        let Some(count) = self.counts.get_mut(id) else {
            return false;
        };
        if *count <= 1 {
            self.counts.remove(id);
            self.order.retain(|item| item != id);
        } else {
            *count -= 1;
        }
        true
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

pub fn is_rpc_response_frame(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("response")
}

pub fn response_id(value: &Value) -> Option<&str> {
    value.get("id").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_counts_and_bounds_unawaited_ids() {
        let mut replies = UnawaitedReplies::new(2);
        replies.note("a");
        replies.note("a");
        replies.note("b");
        replies.note("c");
        assert_eq!(replies.len(), 2);
        assert!(!replies.take("a"));
        assert!(replies.take("b"));
        assert!(!replies.take("b"));
        assert!(replies.take("c"));
    }
}
