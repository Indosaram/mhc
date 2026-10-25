use crate::{
    monitor_continuation::MonitorAwareGoalContinuation,
    turn_end_liveness::*,
    types::{Goal, GoalStatus},
};
use maho_core::agent_abort_provenance::AgentEndEvent;
use maho_ext_api::{ExtensionApi, ExtensionContext, ExtensionFailure};
pub fn collect_turn_end_liveness_input(
    ctx: &ExtensionContext,
    event: &AgentEndEvent,
    goal: Option<&Goal>,
    todo_outcome: Option<&str>,
    deadline_stop: bool,
    pending: bool,
    monitor: &MonitorAwareGoalContinuation,
    now: i64,
) -> Result<TurnEndLivenessInput, ExtensionFailure> {
    let count = crate::todo_gate::open_todo_task_contents(&ctx.session_manager.get_branch()).len();
    let open_work = if let Some(goal) = goal.filter(|goal| goal.status == GoalStatus::Active) {
        OpenWork::ActiveGoal {
            goal_id: goal.id.clone(),
        }
    } else if count > 0 {
        OpenWork::OpenTodos { count }
    } else {
        OpenWork::None
    };
    let mut expectations = monitor
        .live_wake_sources()
        .into_iter()
        .map(|source| WakeExpectation::WakeSource { source })
        .collect::<Vec<_>>();
    if let Some(timer) = monitor.scheduled_continuation() {
        expectations.push(WakeExpectation::ArmedTimer {
            timer: match timer {
                crate::wait_progress::GoalWaitKind::Monitor => "monitor",
                crate::wait_progress::GoalWaitKind::UserGrace => "user-grace",
            }
            .into(),
        });
    }
    if monitor.has_staged_recovery() {
        expectations.push(WakeExpectation::RecoveryStaged);
    }
    if ctx.has_pending_messages()? {
        expectations.push(WakeExpectation::PendingMessages);
    }
    if crate::todo_owed_backstop::called_ask_user(&event.messages)
        || crate::todo_owed_backstop::ends_with_question(
            &crate::lifecycle_helpers::last_assistant_text(&event.messages),
        )
    {
        expectations.push(WakeExpectation::QuestionToUser);
    }
    let engine_held = ctx.engine_turns_held()?;
    let ledger_hold = crate::stop_pause::live_hold_kinds(&ctx.session_manager.get_entries())
        .into_iter()
        .next()
        .or_else(|| engine_held.then(|| "unattended".into()));
    Ok(TurnEndLivenessInput {
        is_child_session: false,
        mode: ctx.mode,
        ended_by: classify_turn_end(event),
        ledger_hold,
        will_retry: event.will_retry,
        deadline_stop,
        goal_stopped: goal
            .is_some_and(|goal| matches!(goal.status, GoalStatus::Paused | GoalStatus::Blocked)),
        open_work,
        continuation_queued: if pending {
            Some("goal".into())
        } else if todo_outcome == Some("delivered") {
            Some("todo-backstop".into())
        } else {
            None
        },
        expectations,
        budget_spent: goal.is_some_and(|goal| {
            goal.status == GoalStatus::Active && goal.continuation_stopped_at.is_some()
        }) || matches!(todo_outcome, Some("cap" | "capped")),
        provider_retry_at_ms: crate::provider_retry::provider_retry_at(&event.messages, now)
            .map(|time| time as f64),
    })
}
pub fn report_turn_end_liveness(
    pi: &ExtensionApi,
    ctx: &ExtensionContext,
    input: &TurnEndLivenessInput,
) {
    let verdict = evaluate_turn_end_liveness(input);
    pi.events.emit(
        TURN_END_LIVENESS_EVENT,
        &serde_json::json!({"sessionId":ctx.session_manager.session_id(),"verdict":verdict}),
    );
    if matches!(verdict, TurnEndLivenessVerdict::Stalled { .. })
        && !repeats_last_stall(&ctx.session_manager.get_branch(), &verdict)
        && let Err(error) = pi.append_entry(
            TURN_END_LIVENESS_ENTRY_TYPE,
            Some(serde_json::json!({"verdict":verdict,"openWork":input.open_work})),
        )
    {
        maho_ext_api::diagnostic(&format!("goal_continuation_record_write_failed: {error}"));
    }
}
