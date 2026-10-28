use crate::{
    monitor_continuation::MonitorAwareGoalContinuation,
    turn_end_liveness::*,
    types::{Goal, GoalStatus},
};
use maho_core::agent_abort_provenance::AgentEndEvent;
use maho_ext_api::{ExtensionApi, ExtensionContext, ExtensionFailure};
pub fn deadline_stop_pending(ctx: &ExtensionContext) -> bool {
    for entry in ctx.session_manager.get_branch().iter().rev() {
        if entry.kind == "message" && entry.data["message"]["role"] == "user" {
            return false;
        }
        if entry.kind == "custom" && entry.data["customType"] == "deadline-stop" {
            let data = &entry.data["data"];
            if matches!(data["kind"].as_str(), Some("inactivity" | "max_runtime"))
                && ["limitMs", "elapsedMs", "at"]
                    .iter()
                    .all(|key| data[*key].as_f64().is_some_and(f64::is_finite))
            {
                return true;
            }
        }
    }
    false
}
pub fn collect_turn_end_liveness_input(
    ctx: &ExtensionContext,
    event: &AgentEndEvent,
    goal: Option<&Goal>,
    todo_outcome: Option<&str>,
    stop_and_pending: (bool, bool),
    monitor: &MonitorAwareGoalContinuation,
    now: i64,
) -> Result<TurnEndLivenessInput, ExtensionFailure> {
    let (deadline_stop, pending) = stop_and_pending;
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
        expectations.push(if ctx.has_queued_person_input()? {
            WakeExpectation::UserInputQueued
        } else {
            WakeExpectation::PendingMessages
        });
    }
    if crate::todo_owed_backstop::called_ask_user(&event.messages)
        || crate::todo_owed_backstop::ends_with_question(
            &crate::lifecycle_helpers::last_assistant_text(&event.messages),
        )
    {
        expectations.push(WakeExpectation::QuestionToUser);
    }
    let pause_state = ctx.get_pause_state()?;
    let ledger_hold = pause_state["holds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|hold| hold["kind"].as_str())
        .next()
        .map(str::to_owned);
    Ok(TurnEndLivenessInput {
        is_child_session: ctx.session_manager.is_child_session(),
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
        &serde_json::json!({"sessionId":ctx.session_manager.session_id(),"verdict":verdict,
            "goal":crate::lifecycle_facts::session_lifecycle(&crate::store_ref::context_goal_store_ref(ctx)).and_then(|service|service.collect_goal_lifecycle_facts()).and_then(|facts|facts.goal),
            "input":{"endedBy":input.ended_by,"willRetry":input.will_retry,"deadlineStop":input.deadline_stop,
            "goalStopped":input.goal_stopped,"openWork":input.open_work,"continuationQueued":input.continuation_queued,
            "expectations":effective_wakes(input),"budgetSpent":input.budget_spent,"providerRetryAtMs":input.provider_retry_at_ms}}),
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
