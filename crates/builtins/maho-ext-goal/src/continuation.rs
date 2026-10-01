//! Pure continuation admission and progress fingerprints from goal/continuation.ts.
use crate::types::{Goal, GoalStatus};
pub const GOAL_CONTINUATION_CAP: u64 = 8;
pub const GOAL_STALL_TOOLLESS_THRESHOLD: u64 = 3;
pub const GOAL_REPETITION_HASH_STREAK: usize = 3;
pub const GOAL_LENGTH_RECOVERY_LIMIT: u64 = 1;
pub const GOAL_UNATTENDED_CONTINUATION_LIMIT: u64 = 150;
pub const GOAL_USER_GRACE_DELAY_MS: u64 = 10_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalContinuationPath { Immediate, MonitorDelayed, UserGrace, SessionStart, SystemRecovery, ProviderRecovery }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason { Stop, Length, ToolUse, Aborted, Error }
pub struct GoalContinuationInput<'a> {
    pub goal: Option<&'a Goal>, pub is_idle: bool, pub has_pending_messages: bool,
    pub path: GoalContinuationPath, pub last_stop_reason: Option<StopReason>,
    pub last_turn_was_malformed_tool_use: bool, pub consecutive_continuations: u64,
    pub last_continuation_signature: Option<&'a str>, pub current_signature: Option<&'a str>,
    pub consecutive_length_recoveries: u64, pub recent_normalized_output_hashes: &'a [String],
    pub toolless_continuation_streak: u64, pub continuation_pending: bool,
    pub last_turn_stuck_on_context_overflow: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason { NotEligible, SingleFlight, Cap, Stale, Repetition, LengthExhausted, Unattended, ContextOverflow }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationPrompt { Full, Minimal }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalContinuationVerdict { Continue { prompt: ContinuationPrompt, stall_notice: bool }, Deny(DenyReason) }
pub fn should_queue_goal_continuation_when_idle(goal: Option<&Goal>, is_idle: bool, has_pending_messages: bool) -> bool {
    goal.is_some_and(|g| g.status == GoalStatus::Active) && is_idle && !has_pending_messages
}
pub fn evaluate_goal_continuation(input: &GoalContinuationInput<'_>) -> GoalContinuationVerdict {
    use GoalContinuationVerdict::Deny;
    let active = input.goal.is_some_and(|g| g.status == GoalStatus::Active);
    if active && input.last_turn_stuck_on_context_overflow { return Deny(DenyReason::ContextOverflow); }
    let path_eligible = match input.path {
        GoalContinuationPath::SystemRecovery | GoalContinuationPath::ProviderRecovery => true,
        GoalContinuationPath::Immediate => input.last_turn_was_malformed_tool_use || matches!(input.last_stop_reason, Some(StopReason::Stop | StopReason::Length)),
        GoalContinuationPath::MonitorDelayed | GoalContinuationPath::UserGrace | GoalContinuationPath::SessionStart => input.is_idle,
    };
    if !active || input.has_pending_messages || !path_eligible { return Deny(DenyReason::NotEligible); }
    if input.continuation_pending { return Deny(DenyReason::SingleFlight); }
    if let Some(latest) = input.recent_normalized_output_hashes.last()
        && input.recent_normalized_output_hashes.iter().rev().take_while(|h| *h == latest).count() >= GOAL_REPETITION_HASH_STREAK { return Deny(DenyReason::Repetition); }
    if input.path != GoalContinuationPath::MonitorDelayed && input.goal.and_then(|g| g.unattended_continuations).unwrap_or(0) >= GOAL_UNATTENDED_CONTINUATION_LIMIT { return Deny(DenyReason::Unattended); }
    if input.consecutive_continuations >= GOAL_CONTINUATION_CAP { return Deny(DenyReason::Cap); }
    if matches!(input.path, GoalContinuationPath::Immediate | GoalContinuationPath::UserGrace) && input.last_continuation_signature.is_some() && input.last_continuation_signature == input.current_signature { return Deny(DenyReason::Stale); }
    let prompt = if input.last_stop_reason == Some(StopReason::Length) {
        if input.consecutive_length_recoveries >= GOAL_LENGTH_RECOVERY_LIMIT { return Deny(DenyReason::LengthExhausted); }
        ContinuationPrompt::Minimal
    } else { ContinuationPrompt::Full };
    GoalContinuationVerdict::Continue { prompt, stall_notice: input.toolless_continuation_streak >= GOAL_STALL_TOOLLESS_THRESHOLD }
}
pub fn normalize_assistant_text(text: &str) -> String { text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ") }
pub fn hash_assistant_text(text: &str) -> String {
    let hash = normalize_assistant_text(text).encode_utf16().fold(0x811c9dc5_u32, |hash, unit| (hash ^ u32::from(unit)).wrapping_mul(0x01000193));
    format!("{hash:08x}")
}
pub fn build_goal_continuation_signature(goal: &Goal, open_todos: usize, total_todos: usize, last_assistant_text_hash: &str) -> String { format!("{}:{open_todos}/{total_todos}:{last_assistant_text_hash}", goal.id) }
pub fn has_goal_continuation_progress(input: &GoalContinuationInput<'_>) -> bool { input.last_continuation_signature.is_some() && input.current_signature.is_some() && input.last_continuation_signature != input.current_signature }
#[cfg(test)] mod tests {
    use super::*;
    fn goal() -> Goal { Goal { id: "g".into(), thread_id: "t".into(), objective: "work".into(), status: GoalStatus::Active, token_budget: None, tokens_used: 0, time_used_seconds: 0.0, consecutive_continuations: None, unattended_continuations: None, last_continuation_signature: None, created_at: 1, updated_at: 1, last_started_at: None, blocked_reason: None, blocked_at: None, completed_at: None } }
    fn input(goal: &Goal) -> GoalContinuationInput<'_> { GoalContinuationInput { goal: Some(goal), is_idle: true, has_pending_messages: false, path: GoalContinuationPath::Immediate, last_stop_reason: Some(StopReason::Stop), last_turn_was_malformed_tool_use: false, consecutive_continuations: 0, last_continuation_signature: None, current_signature: None, consecutive_length_recoveries: 0, recent_normalized_output_hashes: &[], toolless_continuation_streak: 0, continuation_pending: false, last_turn_stuck_on_context_overflow: false } }
    #[test] fn persisted_cap_applies_to_every_path() { let goal = goal(); for path in [GoalContinuationPath::Immediate, GoalContinuationPath::MonitorDelayed, GoalContinuationPath::UserGrace, GoalContinuationPath::SessionStart, GoalContinuationPath::SystemRecovery, GoalContinuationPath::ProviderRecovery] { let mut input = input(&goal); input.path = path; input.consecutive_continuations = 8; let result = evaluate_goal_continuation(&input); assert_eq!(result, GoalContinuationVerdict::Deny(DenyReason::Cap)); } }
    #[test] fn only_active_idle_goal_without_pending_messages_queues() { let goal = goal(); let result = [should_queue_goal_continuation_when_idle(Some(&goal), true, false), should_queue_goal_continuation_when_idle(Some(&goal), false, false), should_queue_goal_continuation_when_idle(Some(&goal), true, true), should_queue_goal_continuation_when_idle(None, true, false)]; assert_eq!(result, [true, false, false, false]); }
    #[test] fn overflow_has_priority_over_other_denials() { let goal = goal(); let mut input = input(&goal); input.last_turn_stuck_on_context_overflow = true; input.has_pending_messages = true; let result = evaluate_goal_continuation(&input); assert_eq!(result, GoalContinuationVerdict::Deny(DenyReason::ContextOverflow)); }
    #[test] fn repetition_precedes_cap() { let goal = goal(); let hashes = vec!["same".into(); 3]; let mut input = input(&goal); input.recent_normalized_output_hashes = &hashes; input.consecutive_continuations = 8; let result = evaluate_goal_continuation(&input); assert_eq!(result, GoalContinuationVerdict::Deny(DenyReason::Repetition)); }
    #[test] fn monitor_wait_is_exempt_from_unattended_limit() { let mut goal = goal(); goal.unattended_continuations = Some(150); let mut input = input(&goal); input.path = GoalContinuationPath::MonitorDelayed; let result = evaluate_goal_continuation(&input); assert!(matches!(result, GoalContinuationVerdict::Continue { .. })); }
    #[test] fn length_recovery_is_minimal_once() { let goal = goal(); let mut input = input(&goal); input.last_stop_reason = Some(StopReason::Length); let result = evaluate_goal_continuation(&input); assert_eq!(result, GoalContinuationVerdict::Continue { prompt: ContinuationPrompt::Minimal, stall_notice: false }); }
    #[test] fn repeated_length_recovery_is_denied() { let goal = goal(); let mut input = input(&goal); input.last_stop_reason = Some(StopReason::Length); input.consecutive_length_recoveries = 1; let result = evaluate_goal_continuation(&input); assert_eq!(result, GoalContinuationVerdict::Deny(DenyReason::LengthExhausted)); }
    #[test] fn stale_immediate_signature_is_denied() { let goal = goal(); let mut input = input(&goal); input.last_continuation_signature = Some("same"); input.current_signature = Some("same"); let result = evaluate_goal_continuation(&input); assert_eq!(result, GoalContinuationVerdict::Deny(DenyReason::Stale)); }
    #[test] fn normalized_hash_ignores_case_and_spacing() { let text = "  Work\n MORE  "; let result = hash_assistant_text(text); assert_eq!(result, hash_assistant_text("work more")); }
    #[test] fn single_flight_blocks_delivery() { let goal = goal(); let mut input = input(&goal); input.continuation_pending = true; let result = evaluate_goal_continuation(&input); assert_eq!(result, GoalContinuationVerdict::Deny(DenyReason::SingleFlight)); }
}
