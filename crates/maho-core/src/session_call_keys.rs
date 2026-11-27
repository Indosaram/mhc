//! Port of senpi packages/coding-agent/src/core/session-call-keys.ts.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// The call key a session persists and a compact peer reads (`blk:<messageId>:<contentIndex>`).
pub fn block_call_key(message_id: &str, content_index: usize) -> String {
    format!("blk:{message_id}:{content_index}")
}

/// Optional target for carrying a bridge-started execution call key onto a downstream record copy.
pub trait CallKeyCarry {
    fn set_execution_call_key(&mut self, key: String);
}

pub fn carry_execution_call_key<T: CallKeyCarry>(from_key: Option<&str>, to: &mut T) {
    if let Some(key) = from_key {
        to.set_execution_call_key(key.to_owned());
    }
}

fn tool_call_blocks(content: Option<&Value>) -> Vec<(usize, String)> {
    let Some(Value::Array(parts)) = content else {
        return Vec::new();
    };
    parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| {
            if part.get("type").and_then(Value::as_str) == Some("toolCall")
                && let Some(id) = part.get("id").and_then(Value::as_str)
            {
                Some((index, id.to_owned()))
            } else {
                None
            }
        })
        .collect()
}

/// The bridge-started calls of the open assistant message, matched to blocks the way the host's
/// calls table joins them.
#[derive(Clone, Debug, Default)]
pub struct BridgedCallKeys {
    open: bool,
    seen: HashSet<usize>,
    waiting: HashMap<String, usize>,
    bridged: HashMap<String, Vec<String>>,
    keyed: HashMap<usize, String>,
    event_keys: HashMap<String, String>,
}

impl BridgedCallKeys {
    pub fn observe_message_start(&mut self, role: Option<&str>) {
        if role == Some("assistant") {
            self.open = true;
            self.seen.clear();
            self.waiting.clear();
            self.bridged.clear();
            self.keyed.clear();
        }
    }

    pub fn observe_message_update_or_end(
        &mut self,
        is_end: bool,
        role: Option<&str>,
        content: Option<&Value>,
    ) {
        if role != Some("assistant") {
            return;
        }
        for (index, id) in tool_call_blocks(content) {
            self.block(index, &id);
        }
        if is_end {
            self.open = false;
        }
    }

    pub fn observe_tool_execution_start(
        &mut self,
        tool_call_id: &str,
        parent_tool_call_id: Option<&str>,
    ) -> Option<String> {
        if !self.open || parent_tool_call_id.is_some() {
            return None;
        }
        let waiting = self.waiting.get(tool_call_id).copied().unwrap_or(0);
        if waiting > 0 {
            self.waiting.insert(tool_call_id.to_owned(), waiting - 1);
            return None;
        }
        let key = format!("exe:{}", uuid::Uuid::new_v4());
        self.bridged
            .entry(tool_call_id.to_owned())
            .or_default()
            .push(key.clone());
        self.event_keys.insert(tool_call_id.to_owned(), key.clone());
        Some(key)
    }

    pub fn execution_call_key(&self, tool_call_id: &str) -> Option<&str> {
        self.event_keys.get(tool_call_id).map(String::as_str)
    }

    /// The ended message's bridge-started calls, content index to key; taken once at persistence.
    pub fn take(&mut self) -> HashMap<usize, String> {
        self.event_keys.clear();
        std::mem::take(&mut self.keyed)
    }

    fn block(&mut self, index: usize, id: &str) {
        if self.seen.contains(&index) || id.is_empty() {
            return;
        }
        self.seen.insert(index);
        if let Some(queue) = self.bridged.get_mut(id)
            && !queue.is_empty()
        {
            let key = queue.remove(0);
            self.keyed.insert(index, key);
            return;
        }
        *self.waiting.entry(id.to_owned()).or_insert(0) += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn joins_blocks_and_bridged_starts_in_both_orders() {
        let mut keys = BridgedCallKeys::default();
        keys.observe_message_start(Some("assistant"));
        // Start arrives before block (bridge-started)
        let exe_key = keys
            .observe_tool_execution_start("tc1", None)
            .expect("minted exe key");
        assert!(exe_key.starts_with("exe:"));
        keys.observe_message_update_or_end(
            true,
            Some("assistant"),
            Some(&json!([{"type":"toolCall","id":"tc1"}])),
        );
        let taken = keys.take();
        assert_eq!(taken.get(&0), Some(&exe_key));
    }
}
