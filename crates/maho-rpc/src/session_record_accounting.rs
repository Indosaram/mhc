//! The shared writer's queue nodes and their running totals (senpi `session-record-accounting.ts`).
//!
//! Every queued record carries its serialized JSONL line and that line's byte length, measured once when the record
//! is appended or replaced. The linked-queue mutations own the retained totals, so `bufferedRecordCount` and
//! `bufferedByteLength` are O(1) and a burst of records costs a linear number of serializations. In-flight records
//! are counted by the writer, not here.
use serde_json::Value;
use std::collections::VecDeque;

/// One queued record with its frozen line (senpi `QueueNode`). The queue order is the `VecDeque` order, so the
/// upstream `previous`/`next` links are implicit.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueNode {
    pub value: Value,
    pub line: String,
    pub byte_length: usize,
    pub key: Option<String>,
}

/// The retained totals of every linked queue (senpi `SessionRecordAccounting`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SessionRecordAccounting {
    pub count: usize,
    pub bytes: usize,
}

impl SessionRecordAccounting {
    /// Appends `value` at the tail of `queue`, serializing it once.
    pub fn append(
        &mut self,
        queue: &mut VecDeque<QueueNode>,
        value: Value,
        key: Option<String>,
    ) -> Result<(), serde_json::Error> {
        let line = crate::jsonl::serialize_json_line(&value)?;
        self.append_line(queue, value, line, key);
        Ok(())
    }

    /// [`Self::append`] with the line the caller already serialized for `value`.
    pub fn append_line(
        &mut self,
        queue: &mut VecDeque<QueueNode>,
        value: Value,
        line: String,
        key: Option<String>,
    ) {
        let byte_length = line.len();
        queue.push_back(QueueNode {
            value,
            line,
            byte_length,
            key,
        });
        self.count += 1;
        self.bytes += byte_length;
    }

    /// Re-serializes a node whose value changed in place, keeping the byte total exact.
    pub fn replace(&mut self, node: &mut QueueNode) -> Result<(), serde_json::Error> {
        let line = crate::jsonl::serialize_json_line(&node.value)?;
        self.bytes = self.bytes + line.len() - node.byte_length;
        node.byte_length = line.len();
        node.line = line;
        Ok(())
    }

    /// Removes the node at `index` from `queue` and from the totals.
    pub fn unlink(&mut self, queue: &mut VecDeque<QueueNode>, index: usize) -> Option<QueueNode> {
        let node = queue.remove(index)?;
        self.count -= 1;
        self.bytes -= node.byte_length;
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn totals_follow_append_replace_and_unlink_exactly() {
        let mut accounting = SessionRecordAccounting::default();
        let mut queue = VecDeque::new();
        accounting
            .append(&mut queue, json!({"text":"é"}), None)
            .unwrap();
        accounting
            .append(&mut queue, json!({"n":1}), Some("k".into()))
            .unwrap();
        let sum = |queue: &VecDeque<QueueNode>| -> usize {
            queue.iter().map(|node| node.line.len()).sum()
        };
        assert_eq!((accounting.count, accounting.bytes), (2, sum(&queue)));
        queue[0].value = json!({"text":"éééé"});
        accounting.replace(&mut queue[0]).unwrap();
        assert_eq!(queue[0].line, "{\"text\":\"éééé\"}\n");
        assert_eq!(accounting.bytes, sum(&queue));
        let removed = accounting.unlink(&mut queue, 1).unwrap();
        assert_eq!(removed.key.as_deref(), Some("k"));
        assert_eq!((accounting.count, accounting.bytes), (1, sum(&queue)));
        assert!(accounting.unlink(&mut queue, 5).is_none());
        accounting.unlink(&mut queue, 0).unwrap();
        assert_eq!(accounting, SessionRecordAccounting::default());
    }
}
