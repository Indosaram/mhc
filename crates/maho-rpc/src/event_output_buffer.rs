//! Per-connection event batching. An immediate response flushes earlier events first.
use serde::Serialize;
use crate::jsonl::serialize_json_line;

#[derive(Default)]
pub struct RpcEventOutputBuffer { pending_event_lines: Vec<String>, flush_scheduled: bool }
impl RpcEventOutputBuffer {
    /// Returns true exactly once per pending batch; the host schedules the flush.
    pub fn enqueue_event(&mut self, event: &impl Serialize) -> Result<bool,serde_json::Error> {
        self.pending_event_lines.push(serialize_json_line(event)?);
        let schedule = !self.flush_scheduled;
        self.flush_scheduled = true;
        Ok(schedule)
    }
    pub fn flush_events(&mut self) -> Option<String> {
        self.flush_scheduled = false;
        if self.pending_event_lines.is_empty() { return None; }
        let batch = self.pending_event_lines.concat();
        self.pending_event_lines.clear();
        Some(batch)
    }
    pub fn write_immediate(&mut self, value: &impl Serialize) -> Result<Vec<String>,serde_json::Error> {
        let mut writes = Vec::new();
        if let Some(batch) = self.flush_events() { writes.push(batch); }
        writes.push(serialize_json_line(value)?);
        Ok(writes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn one_schedule_coalesces_events() { let mut buffer = RpcEventOutputBuffer::default(); assert!(buffer.enqueue_event(&json!({"a":1})).unwrap()); assert!(!buffer.enqueue_event(&json!({"b":2})).unwrap()); assert_eq!(buffer.flush_events(),Some("{\"a\":1}\n{\"b\":2}\n".into())); }
    #[test] fn response_cannot_overtake_events() { let mut buffer = RpcEventOutputBuffer::default(); buffer.enqueue_event(&json!({"event":1})).unwrap(); assert_eq!(buffer.write_immediate(&json!({"response":2})).unwrap(),vec!["{\"event\":1}\n","{\"response\":2}\n"]); }
    #[test] fn empty_flush_writes_nothing_and_new_batch_schedules() { let mut buffer = RpcEventOutputBuffer::default(); assert_eq!(buffer.flush_events(),None); assert!(buffer.enqueue_event(&json!({})).unwrap()); buffer.flush_events(); assert!(buffer.enqueue_event(&json!({})).unwrap()); }
}
