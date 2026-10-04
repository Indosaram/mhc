use crate::types::TokenUsageSnapshot;
use maho_agent::types::AgentMessage;
pub fn empty_usage() -> TokenUsageSnapshot { TokenUsageSnapshot { input: 0, output: 0, cache_read: 0, cache_write: 0, total_tokens: 0 } }
fn add_usage(target: &mut TokenUsageSnapshot, source: &TokenUsageSnapshot) { target.input += source.input; target.output += source.output; target.cache_read += source.cache_read; target.cache_write += source.cache_write; target.total_tokens += source.total_tokens; }
fn message_usage(message: &AgentMessage) -> TokenUsageSnapshot { message.as_assistant().map_or_else(empty_usage, |a| TokenUsageSnapshot { input: a.usage.input, output: a.usage.output, cache_read: a.usage.cache_read, cache_write: a.usage.cache_write, total_tokens: a.usage.total_tokens }) }
pub fn collect_assistant_usage(messages: &[AgentMessage]) -> TokenUsageSnapshot { let mut usage = empty_usage(); for message in messages { add_usage(&mut usage, &message_usage(message)); } usage }
pub struct TurnUsageTracker { pending: TokenUsageSnapshot, flushed: TokenUsageSnapshot }
impl Default for TurnUsageTracker { fn default() -> Self { Self { pending: empty_usage(), flushed: empty_usage() } } }
impl TurnUsageTracker {
    pub fn reset(&mut self) { self.pending = empty_usage(); self.flushed = empty_usage(); }
    pub fn note_message_end(&mut self, message: &AgentMessage) { add_usage(&mut self.pending, &message_usage(message)); }
    pub fn take_pending(&mut self) -> TokenUsageSnapshot { let taken = std::mem::replace(&mut self.pending, empty_usage()); add_usage(&mut self.flushed, &taken); taken }
    pub fn discard_pending(&mut self) { self.take_pending(); }
    pub fn take_remaining(&mut self, messages: &[AgentMessage]) -> TokenUsageSnapshot {
        let collected = collect_assistant_usage(messages);
        let remaining = TokenUsageSnapshot { input: collected.input.saturating_sub(self.flushed.input), output: collected.output.saturating_sub(self.flushed.output), cache_read: collected.cache_read.saturating_sub(self.flushed.cache_read), cache_write: collected.cache_write.saturating_sub(self.flushed.cache_write), total_tokens: collected.total_tokens.saturating_sub(self.flushed.total_tokens) };
        self.flushed.input = self.flushed.input.max(collected.input); self.flushed.output = self.flushed.output.max(collected.output); self.flushed.cache_read = self.flushed.cache_read.max(collected.cache_read); self.flushed.cache_write = self.flushed.cache_write.max(collected.cache_write); self.flushed.total_tokens = self.flushed.total_tokens.max(collected.total_tokens);
        self.pending = empty_usage(); remaining
    }
}
#[cfg(test)] mod tests {
    use super::*;
    fn message(input: u64, output: u64) -> AgentMessage { serde_json::from_value(serde_json::json!({"role":"assistant","content":[],"api":"faux","provider":"faux","model":"faux","usage":{"input":input,"output":output,"cacheRead":7,"cacheWrite":9,"totalTokens":input + output + 16,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0})).unwrap() }
    #[test] fn checkpoint_accumulates_all_usage_fields() { let mut tracker = TurnUsageTracker::default(); tracker.note_message_end(&message(100, 50)); let result = tracker.take_pending(); assert_eq!(result, TokenUsageSnapshot { input: 100, output: 50, cache_read: 7, cache_write: 9, total_tokens: 166 }); }
    #[test] fn agent_end_does_not_double_count_checkpoint() { let mut tracker = TurnUsageTracker::default(); let first = message(100, 50); let second = message(10, 5); tracker.note_message_end(&first); tracker.take_pending(); tracker.note_message_end(&second); let result = tracker.take_remaining(&[first, second]); assert_eq!(result.total_tokens, 31); assert_eq!(tracker.take_pending().total_tokens, 0); }
    #[test] fn mid_turn_goal_discards_previous_accounting_window() { let mut tracker = TurnUsageTracker::default(); let before = message(1000, 500); let after = message(10, 5); tracker.note_message_end(&before); tracker.discard_pending(); tracker.note_message_end(&after); let result = tracker.take_remaining(&[before, after]); assert_eq!(result.total_tokens, 31); }
    #[test] fn repeated_agent_end_is_idempotent() { let mut tracker = TurnUsageTracker::default(); let messages = [message(100, 50)]; tracker.take_remaining(&messages); let result = tracker.take_remaining(&messages); assert_eq!(result, empty_usage()); }
    #[test] fn truncated_run_cannot_refund_usage() { let mut tracker = TurnUsageTracker::default(); tracker.note_message_end(&message(100, 50)); tracker.take_pending(); let result = tracker.take_remaining(&[message(10, 5)]); assert_eq!(result, empty_usage()); }
    #[test] fn reset_starts_new_run() { let mut tracker = TurnUsageTracker::default(); tracker.note_message_end(&message(100, 50)); tracker.take_pending(); tracker.reset(); let result = tracker.take_remaining(&[message(10, 5)]); assert_eq!(result.total_tokens, 31); }
}
