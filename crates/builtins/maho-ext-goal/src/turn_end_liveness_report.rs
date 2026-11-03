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
    let goal = crate::store::read_goal(&crate::store_ref::context_goal_store_ref(ctx))
        .unwrap_or_else(|error| {
            maho_ext_api::diagnostic(&format!("goal lifecycle read failed: {error}"));
            None
        });
    report_with_goal(pi, ctx, input, goal.as_ref());
}

fn report_with_goal(
    pi: &ExtensionApi,
    ctx: &ExtensionContext,
    input: &TurnEndLivenessInput,
    goal: Option<&Goal>,
) {
    let verdict = evaluate_turn_end_liveness(input);
    pi.events.emit(
        TURN_END_LIVENESS_EVENT,
        &serde_json::json!({"sessionId":ctx.session_manager.session_id(),"verdict":verdict,
            "goal":goal,
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

pub fn report_turn_end_liveness_facts(
    pi: &ExtensionApi,
    ctx: &ExtensionContext,
    facts: &crate::lifecycle_facts::GoalLifecycleFacts,
) -> Option<TurnEndLivenessVerdict> {
    if let Some(input) = &facts.input {
        report_with_goal(pi, ctx, input, facts.goal.as_ref());
        Some(evaluate_turn_end_liveness(input))
    } else {
        None
    }
}

pub fn collect_and_report_restored_turn_end_liveness(
    pi: &ExtensionApi,
    ctx: &ExtensionContext,
    restored_end: &AgentEndEvent,
    goal: Option<Option<&Goal>>,
) -> Option<TurnEndLivenessVerdict> {
    let reference = crate::store_ref::context_goal_store_ref(ctx);
    let facts = crate::lifecycle_facts::collect_goal_lifecycle_facts(
        &reference,
        Some(restored_end),
        goal,
    )?;
    report_turn_end_liveness_facts(pi, ctx, &facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::providers::faux::faux_assistant_message;
    use maho_ext_api::LoadedExtension;
    use std::sync::{Arc, Mutex};

    struct TestCapture(Mutex<Vec<maho_ext_api::CustomMessage>>);
    impl maho_ext_api::ExtensionActions for TestCapture {
        fn send_message(
            &self,
            msg: maho_ext_api::CustomMessage,
            _: maho_ext_api::SendMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            self.0.lock().unwrap().push(msg);
            Ok(())
        }
        fn send_user_message(
            &self,
            _: maho_ext_api::UserMessageContent,
            _: maho_ext_api::SendUserMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            Ok(())
        }
        fn append_entry(
            &self,
            _: &str,
            _: Option<serde_json::Value>,
        ) -> Result<(), ExtensionFailure> {
            Ok(())
        }
        fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, ExtensionFailure> {
            Ok(Vec::new())
        }
    }

    fn test_api() -> ExtensionApi {
        let capture = Arc::new(TestCapture(Mutex::new(Vec::new())));
        let transport = maho_ext_api::ExtensionRuntime::default();
        transport.bind(capture);
        ExtensionApi::new(
            LoadedExtension::new("goal", "/tmp".into(), Default::default()),
            Default::default(),
            Default::default(),
            transport,
        )
    }

    fn faux_end_event(text: &str, will_retry: bool) -> AgentEndEvent {
        AgentEndEvent {
            messages: vec![maho_agent::types::AgentMessage::Llm(
                maho_ai::types::Message::Assistant(Arc::new(faux_assistant_message(
                    text,
                    Default::default(),
                ))),
            )],
            will_retry,
            aborted: false,
            abort_source: None,
        }
    }

    #[test]
    fn collect_turn_end_liveness_input_from_restored_clean_end() {
        let dir = tempfile::tempdir().expect("fixture");
        let mut ctx = crate::test_context::context();
        ctx.cwd = dir.path().into();
        ctx.agent_dir = dir.path().join("agent");
        let _session = crate::test_context::bind_session(&mut ctx);
        let event = faux_end_event("completed step", false);
        let goal: Goal = serde_json::from_value(serde_json::json!({
            "id":"g1", "threadId":"s", "objective":"work", "status":"active",
            "tokensUsed":0, "timeUsedSeconds":0, "createdAt":0, "updatedAt":0
        })).expect("goal fixture");
        let monitor = MonitorAwareGoalContinuation::default();
        let input = collect_turn_end_liveness_input(
            &ctx,
            &event,
            Some(&goal),
            Some("delivered"),
            (false, false),
            &monitor,
            1000,
        )
        .unwrap();
        assert_eq!(input.ended_by, "clean");
        assert!(!input.will_retry);
        assert!(!input.deadline_stop);
        assert_eq!(
            input.open_work,
            OpenWork::ActiveGoal {
                goal_id: "g1".into()
            }
        );
        assert_eq!(input.continuation_queued.as_deref(), Some("todo-backstop"));
    }

    #[test]
    fn collect_turn_end_liveness_input_from_restored_provider_error_end() {
        let dir = tempfile::tempdir().expect("fixture");
        let mut ctx = crate::test_context::context();
        ctx.cwd = dir.path().into();
        ctx.agent_dir = dir.path().join("agent");
        let _session = crate::test_context::bind_session(&mut ctx);
        let mut event = faux_end_event("", false);
        if let maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(
            ref mut msg,
        )) = event.messages[0]
        {
            let mut clone = (**msg).clone();
            clone.stop_reason = maho_ai::types::StopReason::Error;
            clone.error_message = Some("529 overload".into());
            *msg = Arc::new(clone);
        }
        let monitor = MonitorAwareGoalContinuation::default();
        let input = collect_turn_end_liveness_input(
            &ctx,
            &event,
            None,
            None,
            (true, false),
            &monitor,
            1000,
        )
        .unwrap();
        assert_eq!(input.ended_by, "provider-error");
        assert!(!input.will_retry);
        assert!(input.deadline_stop);
        assert_eq!(input.open_work, OpenWork::None);
    }

    #[test]
    fn report_turn_end_liveness_emits_event_and_verdict() {
        let dir = tempfile::tempdir().expect("fixture");
        let mut ctx = crate::test_context::context();
        ctx.cwd = dir.path().into();
        ctx.agent_dir = dir.path().join("agent");
        ctx.goal_store_file = Some(dir.path().join("goal.json"));
        let api = test_api();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capture = seen.clone();
        let _sub = api.events.on(
            TURN_END_LIVENESS_EVENT,
            Arc::new(move |payload| {
                capture.lock().unwrap().push(payload.clone());
            }),
        );
        let input = TurnEndLivenessInput {
            is_child_session: false,
            mode: maho_ext_api::ExtensionMode::Tui,
            ended_by: "clean".into(),
            ledger_hold: None,
            will_retry: false,
            deadline_stop: false,
            goal_stopped: false,
            open_work: OpenWork::None,
            continuation_queued: None,
            expectations: Vec::new(),
            budget_spent: false,
            provider_retry_at_ms: None,
        };
        report_turn_end_liveness(&api, &ctx, &input);
        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["sessionId"], "s");
        assert_eq!(events[0]["verdict"]["kind"], "done");
    }

    #[test]
    fn collect_and_report_restored_turn_end_liveness_with_bound_collector() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = crate::test_context::context();
        ctx.cwd = dir.path().into();
        ctx.agent_dir = dir.path().join("agent");
        ctx.goal_store_file = Some(dir.path().join("goal.json"));
        let reference = crate::store_ref::context_goal_store_ref(&ctx);
        let service = Arc::new(crate::lifecycle_facts::GoalLifecycleCollector::default());
        crate::lifecycle_facts::bind_session_lifecycle(&reference, &service);
        let restored = faux_end_event("restored message", false);
        let input = TurnEndLivenessInput {
            is_child_session: false,
            mode: maho_ext_api::ExtensionMode::Tui,
            ended_by: "clean".into(),
            ledger_hold: None,
            will_retry: false,
            deadline_stop: false,
            goal_stopped: false,
            open_work: OpenWork::None,
            continuation_queued: None,
            expectations: Vec::new(),
            budget_spent: false,
            provider_retry_at_ms: None,
        };
        let input_clone = input.clone();
        service.register_goal_lifecycle_collector(Arc::new(
            move |restored_end, _goal| {
                assert!(restored_end.is_some());
                crate::lifecycle_facts::GoalLifecycleFacts {
                    goal: None,
                    input: Some(input_clone.clone()),
                    todo_outcome: Some("delivered".into()),
                }
            },
        ));
        let api = test_api();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capture = seen.clone();
        let _sub = api.events.on(
            TURN_END_LIVENESS_EVENT,
            Arc::new(move |payload| {
                capture.lock().unwrap().push(payload.clone());
            }),
        );
        let verdict = collect_and_report_restored_turn_end_liveness(
            &api,
            &ctx,
            &restored,
            None,
        );
        assert_eq!(verdict, Some(TurnEndLivenessVerdict::Done));
        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["verdict"]["kind"], "done");
    }
}
