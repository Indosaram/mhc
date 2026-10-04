use std::collections::BTreeMap;
use crate::{detectors::LoopGuardDetection, policy::{IDENTICAL_BLOCK_NOTICE_THRESHOLD, IDENTICAL_HARD_STOP_BLOCK_THRESHOLD}, tracker::ToolCallRecord};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdenticalEscalationDecision { Allow, Block { tool_name: String, blocked_call_count: usize }, HardStop { tool_name: String, blocked_call_count: usize, announce: bool } }
struct IdenticalLoopEpisode { fingerprint: String, tool_name: String, admitted_notice_count: usize, activate_block_after_attempt: bool, block_active: bool, blocked_call_count: usize, hard_stop_announced: bool }
#[derive(Default)]
pub struct IdenticalLoopEscalation { episode: Option<IdenticalLoopEpisode>, attempts: BTreeMap<String, ToolCallRecord> }
impl IdenticalLoopEscalation {
    pub fn observe_attempt(&mut self, id: &str, record: ToolCallRecord) -> bool {
        let changed = self.episode.as_ref().is_some_and(|e| e.fingerprint != record.signature);
        if changed { self.reset(); }
        self.attempts.insert(id.into(), record);
        changed
    }
    pub fn observe_notice(&mut self, detection: &LoopGuardDetection) {
        match detection {
            LoopGuardDetection::Similar { .. } | LoopGuardDetection::Cycle { .. } => (),
            LoopGuardDetection::Identical { tool_name, fingerprint, .. } => {
                if self.episode.as_ref().is_none_or(|e| e.fingerprint != *fingerprint) { self.episode = Some(IdenticalLoopEpisode { fingerprint: fingerprint.clone(), tool_name: tool_name.clone(), admitted_notice_count: 0, activate_block_after_attempt: false, block_active: false, blocked_call_count: 0, hard_stop_announced: false }); }
                if let Some(episode) = self.episode.as_mut() { episode.admitted_notice_count += 1; if episode.admitted_notice_count >= IDENTICAL_BLOCK_NOTICE_THRESHOLD { episode.activate_block_after_attempt = true; } }
            }
        }
    }
    pub fn finish_turn(&mut self) { self.attempts.clear(); }
    pub fn consume_tool_call(&mut self, id: &str) -> IdenticalEscalationDecision {
        let Some(attempt) = self.attempts.remove(id) else { return IdenticalEscalationDecision::Allow; };
        let Some(episode) = self.episode.as_mut().filter(|e| e.fingerprint == attempt.signature) else { return IdenticalEscalationDecision::Allow; };
        if !episode.block_active { if episode.activate_block_after_attempt { episode.activate_block_after_attempt = false; episode.block_active = true; } return IdenticalEscalationDecision::Allow; }
        episode.blocked_call_count += 1;
        if episode.blocked_call_count >= IDENTICAL_HARD_STOP_BLOCK_THRESHOLD { let announce = !episode.hard_stop_announced; episode.hard_stop_announced = true; return IdenticalEscalationDecision::HardStop { tool_name: episode.tool_name.clone(), blocked_call_count: episode.blocked_call_count, announce }; }
        IdenticalEscalationDecision::Block { tool_name: episode.tool_name.clone(), blocked_call_count: episode.blocked_call_count }
    }
    pub fn reset(&mut self) { self.episode = None; self.attempts.clear(); }
}
#[cfg(test)] mod tests {
    use super::*;
    use crate::tracker::ToolCallTracker;
    fn armed() -> (IdenticalLoopEscalation, ToolCallRecord) { let record = ToolCallTracker::default().record("read", None); let mut escalation = IdenticalLoopEscalation::default(); let detection = LoopGuardDetection::Identical { tool_name: "read".into(), count: 3, fingerprint: record.signature.clone() }; escalation.observe_notice(&detection); escalation.observe_notice(&detection); escalation.observe_attempt("activating", record.clone()); assert_eq!(escalation.consume_tool_call("activating"), IdenticalEscalationDecision::Allow); (escalation, record) }
    #[test] fn block_begins_after_second_notice_call() { let (mut escalation, record) = armed(); escalation.observe_attempt("next", record); let result = escalation.consume_tool_call("next"); assert!(matches!(result, IdenticalEscalationDecision::Block { blocked_call_count: 1, .. })); }
    #[test] fn hard_stop_announces_only_once() { let (mut escalation, record) = armed(); let mut decisions = Vec::new(); for i in 0..4 { let id = i.to_string(); escalation.observe_attempt(&id, record.clone()); decisions.push(escalation.consume_tool_call(&id)); } assert!(matches!(decisions[2], IdenticalEscalationDecision::HardStop { announce: true, .. })); assert!(matches!(decisions[3], IdenticalEscalationDecision::HardStop { announce: false, .. })); }
    #[test] fn changed_pattern_resets_episode() { let (mut escalation, _) = armed(); let record = ToolCallTracker::default().record("bash", None); let changed = escalation.observe_attempt("new", record); let result = escalation.consume_tool_call("new"); assert!(changed); assert_eq!(result, IdenticalEscalationDecision::Allow); }
    #[test] fn turn_end_discards_pending_attempts() { let (mut escalation, record) = armed(); escalation.observe_attempt("pending", record); escalation.finish_turn(); let result = escalation.consume_tool_call("pending"); assert_eq!(result, IdenticalEscalationDecision::Allow); }
}
