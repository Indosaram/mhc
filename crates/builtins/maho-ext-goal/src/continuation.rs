//! Pure continuation admission and progress fingerprints from goal/continuation.ts.
use crate::types::{Goal, GoalStatus};
pub const GOAL_CONTINUATION_CAP: u64 = 8;
pub const GOAL_STALL_TOOLLESS_THRESHOLD: u64 = 3;
pub const GOAL_REPETITION_HASH_STREAK: usize = 3;
pub const GOAL_LENGTH_RECOVERY_LIMIT: u64 = 1;
/// Consecutive automatic continuations without progress before the goal blocks (omo#10280).
/// Progress is a new tool effect, a changed todo state, or new human input (direct, or a
/// human-input monitor delivery); varied narration alone is not.
pub const GOAL_NO_PROGRESS_CONTINUATION_LIMIT: u64 = 60;
pub const GOAL_USER_GRACE_DELAY_MS: u64 = 10_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalContinuationPath {
    Immediate,
    MonitorDelayed,
    UserGrace,
    SessionStart,
    SystemRecovery,
    ProviderRecovery,
}
pub use maho_ai::types::StopReason;
pub struct GoalContinuationInput<'a> {
    pub goal: Option<&'a Goal>,
    pub is_idle: bool,
    pub has_pending_messages: bool,
    pub path: GoalContinuationPath,
    pub last_stop_reason: Option<StopReason>,
    pub last_turn_was_malformed_tool_use: bool,
    pub consecutive_continuations: u64,
    pub last_continuation_signature: Option<&'a str>,
    pub current_signature: Option<&'a str>,
    pub consecutive_length_recoveries: u64,
    pub recent_normalized_output_hashes: &'a [String],
    pub toolless_continuation_streak: u64,
    pub continuation_pending: bool,
    pub last_turn_stuck_on_context_overflow: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    NotEligible,
    SingleFlight,
    Cap,
    Stale,
    Repetition,
    LengthExhausted,
    NoProgress,
    ContextOverflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContinuationPrompt {
    Full,
    Minimal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalContinuationVerdict {
    Continue {
        prompt: ContinuationPrompt,
        stall_notice: bool,
    },
    Deny(DenyReason),
}
pub fn should_queue_goal_continuation_when_idle(
    goal: Option<&Goal>,
    is_idle: bool,
    has_pending_messages: bool,
) -> bool {
    goal.is_some_and(|g| g.status == GoalStatus::Active) && is_idle && !has_pending_messages
}
pub fn is_malformed_tool_use_turn(message: &maho_ai::types::AssistantMessage) -> bool {
    if message
        .content
        .iter()
        .any(|content| matches!(content, maho_ai::types::ContentBlock::ToolCall(_)))
    {
        return false;
    }
    message.stop_reason == StopReason::ToolUse
        || message.stop_reason == StopReason::Stop
            && message.diagnostics.as_ref().is_some_and(|diagnostics| {
                diagnostics
                    .iter()
                    .any(|d| d.kind == maho_agent::EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC)
            })
}
/// `MONITOR_NOTIFICATION_CUSTOM_TYPE` in builtin/monitor-state-event.ts.
pub const MONITOR_NOTIFICATION_CUSTOM_TYPE: &str = "senpi-monitor:notification";
/// `isHumanInputMonitorNotification`: a delivered monitor notification carrying a person's words.
pub fn is_human_input_monitor_notification(message: &maho_agent::types::AgentMessage) -> bool {
    let maho_agent::types::AgentMessage::Custom(maho_agent::types::CustomAgentMessage::Custom(
        custom,
    )) = message
    else {
        return false;
    };
    custom.custom_type == MONITOR_NOTIFICATION_CUSTOM_TYPE
        && custom
            .details
            .as_ref()
            .and_then(|details| details.get("monitors"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|monitors| {
                monitors.iter().any(|monitor| {
                    monitor.get("humanInput") == Some(&serde_json::Value::Bool(true))
                })
            })
}
pub fn continuation_turn_used_tools(messages: &[maho_agent::types::AgentMessage]) -> bool {
    messages.iter().any(|message| {
        message.as_tool_result().is_some()
            || message.as_assistant().is_some_and(|a| {
                a.content
                    .iter()
                    .any(|c| matches!(c, maho_ai::types::ContentBlock::ToolCall(_)))
            })
    })
}
pub fn should_queue_goal_continuation_after_agent_end(
    goal: Option<&Goal>,
    has_pending_messages: bool,
    messages: &[maho_agent::types::AgentMessage],
) -> bool {
    if !goal.is_some_and(|g| g.status == GoalStatus::Active) || has_pending_messages {
        return false;
    }
    did_agent_end_cleanly(messages)
}
pub fn did_agent_end_cleanly(messages: &[maho_agent::types::AgentMessage]) -> bool {
    let Some((index, assistant)) = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, m)| m.as_assistant().map(|a| (i, a)))
    else {
        return false;
    };
    if !matches!(assistant.stop_reason, StopReason::Stop | StopReason::Length)
        && !is_malformed_tool_use_turn(assistant)
    {
        return false;
    }
    static ABORTED: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let aborted =
        ABORTED.get_or_init(
            || match regex::Regex::new(r"(?i)(?-u:\b)abort(?:ed)?(?-u:\b)") {
                Ok(regex) => regex,
                Err(error) => panic!("invalid static abort expression: {error}"),
            },
        );
    !messages[index + 1..].iter().filter_map(maho_agent::types::AgentMessage::as_tool_result).any(|result| result.is_error && result.content.iter().any(|content| matches!(content, maho_ai::types::ContentBlock::Text(text) if aborted.is_match(&text.text))))
}
pub fn evaluate_goal_continuation(input: &GoalContinuationInput<'_>) -> GoalContinuationVerdict {
    use GoalContinuationVerdict::Deny;
    let active = input.goal.is_some_and(|g| g.status == GoalStatus::Active);
    if active && input.last_turn_stuck_on_context_overflow {
        return Deny(DenyReason::ContextOverflow);
    }
    let path_eligible = match input.path {
        GoalContinuationPath::SystemRecovery | GoalContinuationPath::ProviderRecovery => true,
        GoalContinuationPath::Immediate => {
            input.last_turn_was_malformed_tool_use
                || matches!(
                    input.last_stop_reason,
                    Some(StopReason::Stop | StopReason::Length)
                )
        }
        GoalContinuationPath::MonitorDelayed
        | GoalContinuationPath::UserGrace
        | GoalContinuationPath::SessionStart => input.is_idle,
    };
    if !active || input.has_pending_messages || !path_eligible {
        return Deny(DenyReason::NotEligible);
    }
    if input.continuation_pending {
        return Deny(DenyReason::SingleFlight);
    }
    if let Some(latest) = input.recent_normalized_output_hashes.last()
        && input
            .recent_normalized_output_hashes
            .iter()
            .rev()
            .take_while(|h| *h == latest)
            .count()
            >= GOAL_REPETITION_HASH_STREAK
    {
        return Deny(DenyReason::Repetition);
    }
    // Monitor-delayed deliveries are exempt: armed-wake waiting is rate-limited by the cache-aware timer.
    if input.path != GoalContinuationPath::MonitorDelayed
        && input
            .goal
            .and_then(|g| g.no_progress_continuations)
            .unwrap_or(0)
            >= GOAL_NO_PROGRESS_CONTINUATION_LIMIT
    {
        return Deny(DenyReason::NoProgress);
    }
    if input.consecutive_continuations >= GOAL_CONTINUATION_CAP {
        return Deny(DenyReason::Cap);
    }
    if matches!(
        input.path,
        GoalContinuationPath::Immediate | GoalContinuationPath::UserGrace
    ) && input.last_continuation_signature.is_some()
        && input.last_continuation_signature == input.current_signature
    {
        return Deny(DenyReason::Stale);
    }
    let prompt = if input.last_stop_reason == Some(StopReason::Length) {
        if input.consecutive_length_recoveries >= GOAL_LENGTH_RECOVERY_LIMIT {
            return Deny(DenyReason::LengthExhausted);
        }
        ContinuationPrompt::Minimal
    } else {
        ContinuationPrompt::Full
    };
    GoalContinuationVerdict::Continue {
        prompt,
        stall_notice: input.toolless_continuation_streak >= GOAL_STALL_TOOLLESS_THRESHOLD,
    }
}
pub fn normalize_assistant_text(text: &str) -> String {
    text.to_lowercase()
        .split(crate::validation::js_whitespace)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn hash_assistant_text(text: &str) -> String {
    let hash = normalize_assistant_text(text)
        .encode_utf16()
        .fold(0x811c9dc5_u32, |hash, unit| {
            (hash ^ u32::from(unit)).wrapping_mul(0x01000193)
        });
    format!("{hash:08x}")
}
pub fn build_goal_continuation_signature(
    goal: &Goal,
    open_todos: usize,
    total_todos: usize,
    last_assistant_text_hash: &str,
) -> String {
    format!(
        "{}:{open_todos}/{total_todos}:{last_assistant_text_hash}",
        goal.id
    )
}
pub fn has_goal_continuation_progress(input: &GoalContinuationInput<'_>) -> bool {
    input.last_continuation_signature.is_some()
        && input.current_signature.is_some()
        && input.last_continuation_signature != input.current_signature
}
/// True when the open/total todo counts in the signature changed; the narration hash is ignored.
pub fn has_goal_todo_progress(
    last_continuation_signature: Option<&str>,
    current_signature: Option<&str>,
) -> bool {
    let (Some(last), Some(current)) = (last_continuation_signature, current_signature) else {
        return false;
    };
    todo_state_of_signature(last) != todo_state_of_signature(current)
}
fn todo_state_of_signature(signature: &str) -> &str {
    signature
        .rfind(':')
        .map_or(signature, |index| &signature[..index])
}
/// How many recent tool-effect fingerprints a new one must differ from to count as progress. Far
/// above the guard's 60, so rotating through a fixed set of calls earns at most one progress turn
/// per distinct call.
pub const GOAL_TOOL_EFFECT_WINDOW: usize = 256;
// Tools whose arguments are the change itself; masking their numbers would merge distinct edits.
const MUTATING_TOOL_NAMES: [&str; 4] = ["edit", "write", "apply_patch", "todo"];
fn static_regex(
    cell: &'static std::sync::OnceLock<regex::Regex>,
    pattern: &str,
) -> &'static regex::Regex {
    cell.get_or_init(|| match regex::Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid static tool-effect expression: {error}"),
    })
}
/// Run-to-run noise in a result: ISO timestamps, clock times, durations, pids, long numbers and
/// long hex ids. Small counters ("12/200 modules") stay visible. `\b` and digits are ASCII as in
/// the JavaScript source.
fn volatile_result_token() -> &'static regex::Regex {
    static CELL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static_regex(
        &CELL,
        r"(?i)[0-9]{4}-[0-9]{2}-[0-9]{2}[T ][0-9]{2}:[0-9]{2}(?::[0-9]{2})?(?:\.[0-9]+)?Z?|(?-u:\b)[0-9]{1,2}:[0-9]{2}(?::[0-9]{2})?(?:\.[0-9]+)?(?-u:\b)|(?-u:\b)[0-9]+(?:\.[0-9]+)?\s?(?:ns|µs|us|ms|s|m|h)(?-u:\b)|(?-u:\b)pid[ =:]?[0-9]+|(?-u:\b)[0-9]{6,}(?-u:\b)|(?-u:\b)[0-9a-f]{8,}(?-u:\b)",
    )
}
/// Any number or long hex id in a non-mutating call's arguments.
fn volatile_argument_token() -> &'static regex::Regex {
    static CELL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static_regex(&CELL, r"(?i)(?-u:\b)[0-9a-f]{8,}(?-u:\b)|[0-9]+")
}
fn call_arguments(name: &str, arguments: &serde_json::Map<String, serde_json::Value>) -> String {
    let json = serde_json::to_string(arguments).unwrap_or_default();
    if MUTATING_TOOL_NAMES.contains(&name) {
        json
    } else {
        volatile_argument_token()
            .replace_all(&json, "#")
            .into_owned()
    }
}
/// Fingerprint of what a turn's tools did: every call's name and arguments (numbers masked unless
/// the tool mutates files), and every result's error flag and text with run-to-run noise masked.
/// A turn whose tool results all failed has no fingerprint: failing is never progress.
pub fn tool_effect_fingerprint(messages: &[maho_agent::types::AgentMessage]) -> Option<String> {
    use maho_ai::types::ContentBlock;
    let mut parts = Vec::new();
    let mut succeeded = false;
    for message in messages {
        if let Some(assistant) = message.as_assistant() {
            for content in &assistant.content {
                if let ContentBlock::ToolCall(call) = content {
                    parts.push(format!(
                        "call:{}:{}",
                        call.name,
                        call_arguments(&call.name, &call.arguments)
                    ));
                }
            }
        } else if let Some(result) = message.as_tool_result() {
            let text = result
                .content
                .iter()
                .map(|content| match content {
                    ContentBlock::Text(text) => text.text.as_str(),
                    other => other.type_name(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            succeeded |= !result.is_error;
            parts.push(format!(
                "result:{}:{}:{}",
                result.tool_name,
                if result.is_error { "error" } else { "ok" },
                volatile_result_token().replace_all(&text, "#")
            ));
        }
    }
    succeeded.then(|| hash_assistant_text(&parts.join("\u{0}")))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn goal() -> Goal {
        Goal {
            id: "g".into(),
            thread_id: "t".into(),
            objective: "work".into(),
            status: GoalStatus::Active,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0.0,
            consecutive_continuations: None,
            unattended_continuations: None,
            no_progress_continuations: None,
            last_continuation_signature: None,
            continuation_stopped_at: None,
            created_at: 1,
            updated_at: 1,
            last_started_at: None,
            blocked_reason: None,
            blocked_at: None,
            completed_at: None,
        }
    }
    fn input(goal: &Goal) -> GoalContinuationInput<'_> {
        GoalContinuationInput {
            goal: Some(goal),
            is_idle: true,
            has_pending_messages: false,
            path: GoalContinuationPath::Immediate,
            last_stop_reason: Some(StopReason::Stop),
            last_turn_was_malformed_tool_use: false,
            consecutive_continuations: 0,
            last_continuation_signature: None,
            current_signature: None,
            consecutive_length_recoveries: 0,
            recent_normalized_output_hashes: &[],
            toolless_continuation_streak: 0,
            continuation_pending: false,
            last_turn_stuck_on_context_overflow: false,
        }
    }
    #[test]
    fn persisted_cap_applies_to_every_path() {
        let goal = goal();
        for path in [
            GoalContinuationPath::Immediate,
            GoalContinuationPath::MonitorDelayed,
            GoalContinuationPath::UserGrace,
            GoalContinuationPath::SessionStart,
            GoalContinuationPath::SystemRecovery,
            GoalContinuationPath::ProviderRecovery,
        ] {
            let mut input = input(&goal);
            input.path = path;
            input.consecutive_continuations = 8;
            let result = evaluate_goal_continuation(&input);
            assert_eq!(result, GoalContinuationVerdict::Deny(DenyReason::Cap));
        }
    }
    #[test]
    fn only_active_idle_goal_without_pending_messages_queues() {
        let goal = goal();
        let result = [
            should_queue_goal_continuation_when_idle(Some(&goal), true, false),
            should_queue_goal_continuation_when_idle(Some(&goal), false, false),
            should_queue_goal_continuation_when_idle(Some(&goal), true, true),
            should_queue_goal_continuation_when_idle(None, true, false),
        ];
        assert_eq!(result, [true, false, false, false]);
    }
    #[test]
    fn overflow_has_priority_over_other_denials() {
        let goal = goal();
        let mut input = input(&goal);
        input.last_turn_stuck_on_context_overflow = true;
        input.has_pending_messages = true;
        let result = evaluate_goal_continuation(&input);
        assert_eq!(
            result,
            GoalContinuationVerdict::Deny(DenyReason::ContextOverflow)
        );
    }
    #[test]
    fn repetition_precedes_cap() {
        let goal = goal();
        let hashes = vec!["same".into(); 3];
        let mut input = input(&goal);
        input.recent_normalized_output_hashes = &hashes;
        input.consecutive_continuations = 8;
        let result = evaluate_goal_continuation(&input);
        assert_eq!(
            result,
            GoalContinuationVerdict::Deny(DenyReason::Repetition)
        );
    }
    #[test]
    fn exempts_the_monitor_delayed_path_from_the_no_progress_limit() {
        let mut goal = goal();
        goal.no_progress_continuations = Some(GOAL_NO_PROGRESS_CONTINUATION_LIMIT);
        let mut input = input(&goal);
        input.path = GoalContinuationPath::MonitorDelayed;
        let result = evaluate_goal_continuation(&input);
        assert!(matches!(result, GoalContinuationVerdict::Continue { .. }));
    }
    #[test]
    fn length_recovery_is_minimal_once() {
        let goal = goal();
        let mut input = input(&goal);
        input.last_stop_reason = Some(StopReason::Length);
        let result = evaluate_goal_continuation(&input);
        assert_eq!(
            result,
            GoalContinuationVerdict::Continue {
                prompt: ContinuationPrompt::Minimal,
                stall_notice: false
            }
        );
    }
    #[test]
    fn repeated_length_recovery_is_denied() {
        let goal = goal();
        let mut input = input(&goal);
        input.last_stop_reason = Some(StopReason::Length);
        input.consecutive_length_recoveries = 1;
        let result = evaluate_goal_continuation(&input);
        assert_eq!(
            result,
            GoalContinuationVerdict::Deny(DenyReason::LengthExhausted)
        );
    }
    #[test]
    fn stale_immediate_signature_is_denied() {
        let goal = goal();
        let mut input = input(&goal);
        input.last_continuation_signature = Some("same");
        input.current_signature = Some("same");
        let result = evaluate_goal_continuation(&input);
        assert_eq!(result, GoalContinuationVerdict::Deny(DenyReason::Stale));
    }
    #[test]
    fn normalized_hash_ignores_case_and_spacing() {
        let text = "  Work\n MORE  ";
        let result = hash_assistant_text(text);
        assert_eq!(result, hash_assistant_text("work more"));
    }
    #[test]
    fn single_flight_blocks_delivery() {
        let goal = goal();
        let mut input = input(&goal);
        input.continuation_pending = true;
        let result = evaluate_goal_continuation(&input);
        assert_eq!(
            result,
            GoalContinuationVerdict::Deny(DenyReason::SingleFlight)
        );
    }
    fn assistant(stop: StopReason) -> maho_ai::types::AssistantMessage {
        serde_json::from_value(serde_json::json!({"content":[],"api":"faux","provider":"faux","model":"faux","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop,"timestamp":0})).unwrap()
    }
    #[test]
    fn clean_agent_end_queues_active_goal() {
        let goal = goal();
        let messages = vec![
            maho_ai::types::Message::Assistant(std::sync::Arc::new(assistant(StopReason::Stop)))
                .into(),
        ];
        let result = should_queue_goal_continuation_after_agent_end(Some(&goal), false, &messages);
        assert!(result);
    }
    #[test]
    fn terminal_error_agent_end_does_not_queue() {
        let goal = goal();
        let messages = vec![
            maho_ai::types::Message::Assistant(std::sync::Arc::new(assistant(StopReason::Error)))
                .into(),
        ];
        let result = should_queue_goal_continuation_after_agent_end(Some(&goal), false, &messages);
        assert!(!result);
    }
    #[test]
    fn raw_empty_tool_use_is_malformed() {
        let message = assistant(StopReason::ToolUse);
        let result = is_malformed_tool_use_turn(&message);
        assert!(result);
    }
    #[test]
    fn demotion_diagnostic_preserves_malformed_admission() {
        let mut message = assistant(StopReason::Stop);
        message.diagnostics = Some(vec![
            maho_ai::utils::diagnostics::AssistantMessageDiagnostic {
                kind: maho_agent::EMPTY_TOOL_USE_DEMOTION_DIAGNOSTIC.into(),
                timestamp: 0,
                error: None,
                details: None,
            },
        ]);
        let result = is_malformed_tool_use_turn(&message);
        assert!(result);
    }
    #[test]
    fn aborted_error_tool_result_prevents_continuation() {
        let goal = goal();
        let messages = vec![
            maho_ai::types::Message::Assistant(std::sync::Arc::new(assistant(StopReason::Stop)))
                .into(),
            maho_ai::types::Message::ToolResult(maho_ai::types::ToolResultMessage {
                tool_call_id: "call".into(),
                tool_name: "wait".into(),
                content: vec![maho_ai::types::ContentBlock::text("Operation aborted")],
                details: None,
                usage: None,
                added_tool_names: None,
                nested_calls: None,
                is_error: true,
                timestamp: 0,
            })
            .into(),
        ];
        let result = should_queue_goal_continuation_after_agent_end(Some(&goal), false, &messages);
        assert!(!result);
    }
    // Mirrors upstream `goal-continuation-verdict.test.ts` cases beyond the ones above.
    #[test]
    fn denies_not_eligible_across_path_and_goal_gates() {
        let paused = Goal {
            status: GoalStatus::Paused,
            ..goal()
        };
        assert_eq!(
            evaluate_goal_continuation(&input(&paused)),
            GoalContinuationVerdict::Deny(DenyReason::NotEligible)
        );
        let active = goal();
        let mut pending = input(&active);
        pending.has_pending_messages = true;
        assert_eq!(
            evaluate_goal_continuation(&pending),
            GoalContinuationVerdict::Deny(DenyReason::NotEligible)
        );
        let mut unclean = input(&active);
        unclean.last_stop_reason = Some(StopReason::Error);
        assert_eq!(
            evaluate_goal_continuation(&unclean),
            GoalContinuationVerdict::Deny(DenyReason::NotEligible)
        );
        for path in [
            GoalContinuationPath::MonitorDelayed,
            GoalContinuationPath::UserGrace,
            GoalContinuationPath::SessionStart,
        ] {
            let mut not_idle = input(&active);
            not_idle.path = path;
            not_idle.is_idle = false;
            assert_eq!(
                evaluate_goal_continuation(&not_idle),
                GoalContinuationVerdict::Deny(DenyReason::NotEligible),
                "{path:?}"
            );
        }
    }
    #[test]
    fn denies_with_no_progress_at_the_limit_on_every_counted_path_and_admits_below_it() {
        let at_limit = Goal {
            unattended_continuations: Some(1_000),
            no_progress_continuations: Some(GOAL_NO_PROGRESS_CONTINUATION_LIMIT),
            ..goal()
        };
        for path in [
            GoalContinuationPath::Immediate,
            GoalContinuationPath::UserGrace,
            GoalContinuationPath::SessionStart,
            GoalContinuationPath::SystemRecovery,
            GoalContinuationPath::ProviderRecovery,
        ] {
            let mut input = input(&at_limit);
            input.path = path;
            assert_eq!(
                evaluate_goal_continuation(&input),
                GoalContinuationVerdict::Deny(DenyReason::NoProgress),
                "{path:?}"
            );
        }
        let below = Goal {
            no_progress_continuations: Some(GOAL_NO_PROGRESS_CONTINUATION_LIMIT - 1),
            ..goal()
        };
        assert!(matches!(
            evaluate_goal_continuation(&input(&below)),
            GoalContinuationVerdict::Continue { .. }
        ));
    }
    #[test]
    fn cap_admits_below_the_boundary_and_denies_at_it() {
        let active = goal();
        let mut input = input(&active);
        input.consecutive_continuations = GOAL_CONTINUATION_CAP - 1;
        assert!(matches!(
            evaluate_goal_continuation(&input),
            GoalContinuationVerdict::Continue { .. }
        ));
        input.consecutive_continuations = GOAL_CONTINUATION_CAP;
        assert_eq!(
            evaluate_goal_continuation(&input),
            GoalContinuationVerdict::Deny(DenyReason::Cap)
        );
    }
    #[test]
    fn per_path_eligibility_survives_an_unclean_stop() {
        let active = goal();
        for path in [
            GoalContinuationPath::MonitorDelayed,
            GoalContinuationPath::UserGrace,
            GoalContinuationPath::SessionStart,
        ] {
            let mut input = input(&active);
            input.path = path;
            input.last_stop_reason = Some(StopReason::Error);
            assert!(
                matches!(
                    evaluate_goal_continuation(&input),
                    GoalContinuationVerdict::Continue { .. }
                ),
                "{path:?}"
            );
        }
        let mut system = input(&active);
        system.path = GoalContinuationPath::SystemRecovery;
        system.is_idle = false;
        system.last_stop_reason = Some(StopReason::Error);
        assert!(matches!(
            evaluate_goal_continuation(&system),
            GoalContinuationVerdict::Continue { .. }
        ));
    }
    #[test]
    fn malformed_tool_use_admits_only_when_no_tool_call_was_produced() {
        let active = goal();
        let mut input = input(&active);
        input.last_stop_reason = Some(StopReason::ToolUse);
        input.last_turn_was_malformed_tool_use = true;
        assert!(matches!(
            evaluate_goal_continuation(&input),
            GoalContinuationVerdict::Continue { .. }
        ));
        input.last_turn_was_malformed_tool_use = false;
        assert_eq!(
            evaluate_goal_continuation(&input),
            GoalContinuationVerdict::Deny(DenyReason::NotEligible)
        );
    }
    #[test]
    fn provider_recovery_stays_eligible_while_settling() {
        let active = goal();
        let mut input = input(&active);
        input.path = GoalContinuationPath::ProviderRecovery;
        input.is_idle = false;
        input.last_stop_reason = Some(StopReason::Aborted);
        assert!(matches!(
            evaluate_goal_continuation(&input),
            GoalContinuationVerdict::Continue { .. }
        ));
    }
    #[test]
    fn stall_notice_is_set_at_the_toolless_threshold() {
        let active = goal();
        for (streak, expected) in [
            (GOAL_STALL_TOOLLESS_THRESHOLD - 1, false),
            (GOAL_STALL_TOOLLESS_THRESHOLD, true),
            (GOAL_STALL_TOOLLESS_THRESHOLD + 1, true),
        ] {
            let mut input = input(&active);
            input.toolless_continuation_streak = streak;
            assert_eq!(
                evaluate_goal_continuation(&input),
                GoalContinuationVerdict::Continue {
                    prompt: ContinuationPrompt::Full,
                    stall_notice: expected
                }
            );
        }
    }
    #[test]
    fn hash_matches_the_pinned_constant_and_signature_ignores_updated_at() {
        assert_eq!(hash_assistant_text("Hello world"), "d58b3fa7");
        let hash = hash_assistant_text("Made progress");
        assert_eq!(
            build_goal_continuation_signature(
                &Goal {
                    updated_at: 1,
                    ..goal()
                },
                2,
                5,
                &hash
            ),
            format!("g:2/5:{hash}")
        );
        assert_eq!(
            build_goal_continuation_signature(
                &Goal {
                    updated_at: 9_999,
                    ..goal()
                },
                2,
                5,
                &hash
            ),
            format!("g:2/5:{hash}")
        );
    }
    #[test]
    fn policy_constants_match_pinned() {
        assert_eq!(GOAL_CONTINUATION_CAP, 8);
        assert_eq!(GOAL_STALL_TOOLLESS_THRESHOLD, 3);
        assert_eq!(GOAL_REPETITION_HASH_STREAK, 3);
        assert_eq!(GOAL_LENGTH_RECOVERY_LIMIT, 1);
        assert_eq!(GOAL_NO_PROGRESS_CONTINUATION_LIMIT, 60);
        assert_eq!(GOAL_TOOL_EFFECT_WINDOW, 256);
        assert_eq!(GOAL_USER_GRACE_DELAY_MS, 10_000);
    }
    #[test]
    fn todo_progress_compares_only_the_todo_part_of_the_signature() {
        let goal = goal();
        let mut input = input(&goal);
        input.last_continuation_signature = Some("g:2/5:aaaa");
        input.current_signature = Some("g:2/5:bbbb");
        assert!(!has_goal_todo_progress(
            Some("g:2/5:aaaa"),
            Some("g:2/5:bbbb")
        ));
        assert!(has_goal_continuation_progress(&input));
        assert!(has_goal_todo_progress(
            Some("g:2/5:aaaa"),
            Some("g:1/5:aaaa")
        ));
        assert!(!has_goal_todo_progress(None, Some("g:1/5:aaaa")));
    }
    fn tool_turn(
        name: &str,
        arguments: serde_json::Value,
        text: &str,
        is_error: bool,
    ) -> Vec<maho_agent::types::AgentMessage> {
        use maho_ai::types::{Message, ToolResultMessage};
        let serde_json::Value::Object(arguments) = arguments else {
            panic!("arguments must be an object")
        };
        let call = maho_ai::providers::faux::faux_assistant_message(
            vec![maho_ai::providers::faux::faux_tool_call(
                name,
                arguments,
                Some("call"),
            )],
            Default::default(),
        );
        let result: ToolResultMessage = serde_json::from_value(serde_json::json!({
            "role": "toolResult",
            "toolCallId": "call",
            "toolName": name,
            "content": [{"type": "text", "text": text}],
            "isError": is_error,
            "timestamp": 0
        }))
        .unwrap();
        vec![
            maho_agent::types::AgentMessage::Llm(Message::Assistant(std::sync::Arc::new(call))),
            maho_agent::types::AgentMessage::Llm(Message::ToolResult(result)),
        ]
    }
    #[test]
    fn failing_tool_turns_have_no_fingerprint() {
        assert_eq!(
            tool_effect_fingerprint(&tool_turn(
                "bash",
                serde_json::json!({"command":"bun test"}),
                "1 fail",
                true
            )),
            None
        );
        assert_eq!(tool_effect_fingerprint(&[]), None);
    }
    #[test]
    fn fingerprint_masks_run_to_run_noise_but_keeps_small_counters() {
        let run = |turn: u32| {
            tool_turn(
                "bash",
                serde_json::json!({"command": format!("bun test --bail={turn}; true")}),
                &format!(
                    "1 fail [{}.37ms]\nstarted 2026-10-17T18:{:02}:07Z pid {} run {:x}",
                    40 + turn,
                    turn % 60,
                    4100 + turn,
                    0x9f3a_2b10_u32 + turn
                ),
                false,
            )
        };
        assert_eq!(
            tool_effect_fingerprint(&run(1)),
            tool_effect_fingerprint(&run(2))
        );
        let poll = |turn: u32| {
            tool_turn(
                "bash_output",
                serde_json::json!({"bash_id":"bash_3"}),
                &format!("building {turn}/200 modules"),
                false,
            )
        };
        assert_ne!(
            tool_effect_fingerprint(&poll(1)),
            tool_effect_fingerprint(&poll(2))
        );
    }
    #[test]
    fn mutating_tool_arguments_stay_exact() {
        let edit = |turn: u32| {
            tool_turn(
                "edit",
                serde_json::json!({"path":"src/parser.ts","oldText":format!("limit = {turn}")}),
                "Edited src/parser.ts",
                false,
            )
        };
        assert_ne!(
            tool_effect_fingerprint(&edit(1)),
            tool_effect_fingerprint(&edit(2))
        );
        let sleep = |turn: u32| {
            tool_turn(
                "bash",
                serde_json::json!({"command":format!("sleep {turn}")}),
                "",
                false,
            )
        };
        assert_eq!(
            tool_effect_fingerprint(&sleep(1)),
            tool_effect_fingerprint(&sleep(7))
        );
    }
}
