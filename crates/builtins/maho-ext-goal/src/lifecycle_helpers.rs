use crate::{
    continuation::DenyReason,
    continuation_recovery::*,
    types::{Goal, GoalStatus},
};
use maho_agent::types::AgentMessage;
use maho_ai::types::ContentBlock;
use maho_ext_api::{ExtensionContext, ExtensionFailure};

pub fn queue_hidden_goal_prompt(
    api: &maho_ext_api::ExtensionApi,
    content: String,
) -> Result<(), ExtensionFailure> {
    api.send_message(
        maho_ext_api::CustomMessage {
            custom_type: "goal-continuation".into(),
            content: vec![maho_ext_api::ToolContent::text(content)],
            display: false,
            details: None,
        },
        maho_ext_api::SendMessageOptions {
            trigger_turn: true,
            deliver_as: Some(maho_ext_api::DeliverAs::FollowUp),
        },
    )
}
pub const GOAL_STALE_STOP_NOTICE: &str = "Goal auto-continuation stopped because progress is stale. Send a message or run /goal resume to continue.";
/// Persisted outcome of one admission: the current goal, the verdict and, for a denial, whether this
/// call durably recorded the stop (`recordGoalContinuationStopped(...).recorded`).
pub struct GoalContinuationAdmission {
    pub goal: Option<Goal>,
    pub verdict: crate::continuation::GoalContinuationVerdict,
    pub stop_recorded: bool,
}
/// `now` is in seconds, matching the store's other timestamps; the stop records `now * 1000` as `at`.
pub async fn admit_and_record_goal_continuation(
    reference: &crate::types::GoalStoreRef,
    input: &crate::continuation::GoalContinuationInput<'_>,
    now: u64,
) -> Result<GoalContinuationAdmission, ExtensionFailure> {
    use crate::continuation::{
        GoalContinuationPath, GoalContinuationVerdict, evaluate_goal_continuation,
    };
    let verdict = evaluate_goal_continuation(input);
    let Some(goal) = input.goal else {
        return Ok(GoalContinuationAdmission {
            goal: None,
            verdict,
            stop_recorded: false,
        });
    };
    match verdict {
        GoalContinuationVerdict::Deny(reason) => {
            if goal.status != GoalStatus::Active
                || matches!(reason, DenyReason::NotEligible | DenyReason::SingleFlight)
            {
                return Ok(GoalContinuationAdmission {
                    goal: Some(goal.clone()),
                    verdict,
                    stop_recorded: false,
                });
            }
            let (current, stop_recorded) = crate::store::record_goal_continuation_stopped(
                reference,
                goal,
                now.saturating_mul(1000),
                blocked_reason_for_continuation_guard(reason),
            )
            .await
            .map_err(|error| ExtensionFailure::new(error.to_string()))?;
            Ok(GoalContinuationAdmission {
                goal: current,
                verdict,
                stop_recorded,
            })
        }
        GoalContinuationVerdict::Continue { .. } => {
            let signature = input.current_signature.ok_or_else(|| {
                ExtensionFailure::new(
                    "Cannot queue a goal continuation without a progress signature",
                )
            })?;
            let recorded = crate::store::record_continuation_delivered(
                reference,
                signature,
                Some(&goal.id),
                input.path != GoalContinuationPath::MonitorDelayed,
            )
            .await
            .map_err(|error| ExtensionFailure::new(error.to_string()))?;
            Ok(GoalContinuationAdmission {
                goal: recorded,
                verdict,
                stop_recorded: false,
            })
        }
    }
}
pub async fn admit_and_queue_goal_continuation(
    api: &maho_ext_api::ExtensionApi,
    reference: &crate::types::GoalStoreRef,
    input: &crate::continuation::GoalContinuationInput<'_>,
    now: u64,
    mark_pending: impl FnOnce(),
    content: impl FnOnce(crate::continuation::GoalContinuationVerdict) -> String,
) -> Result<Option<Goal>, ExtensionFailure> {
    let GoalContinuationAdmission {
        goal: recorded,
        verdict,
        ..
    } = admit_and_record_goal_continuation(reference, input, now).await?;
    if recorded.is_some()
        && matches!(
            verdict,
            crate::continuation::GoalContinuationVerdict::Continue { .. }
        )
    {
        mark_pending();
        queue_hidden_goal_prompt(api, content(verdict))?;
    }
    Ok(recorded)
}
pub async fn admit_and_report_goal_continuation(
    api: &maho_ext_api::ExtensionApi,
    ctx: &ExtensionContext,
    reference: &crate::types::GoalStoreRef,
    input: &crate::continuation::GoalContinuationInput<'_>,
    now: u64,
) -> Result<(Option<Goal>, crate::continuation::GoalContinuationVerdict), ExtensionFailure> {
    let admission = admit_and_record_goal_continuation(reference, input, now).await?;
    if admission.stop_recorded
        && let crate::continuation::GoalContinuationVerdict::Deny(reason) = admission.verdict
        && let Some(goal) = input.goal
    {
        report_denied_continuation(api, ctx, goal, input, reason, now.saturating_mul(1000));
    }
    Ok((admission.goal, admission.verdict))
}

pub fn is_resume_of_stopped_goal(
    ctx: &ExtensionContext,
    reason: &str,
    goal: Option<&Goal>,
) -> Result<bool, ExtensionFailure> {
    if reason != "resume"
        || !goal.is_some_and(|goal| matches!(goal.status, GoalStatus::Paused | GoalStatus::Blocked))
        || !ctx.has_ui
        || !ctx.is_idle()
    {
        return Ok(false);
    }
    Ok(!ctx.has_pending_messages()?)
}
pub fn last_assistant_text(messages: &[AgentMessage]) -> String {
    crate::last_assistant_message::last_assistant_message(messages).map_or_else(
        String::new,
        |message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        },
    )
}
pub fn last_assistant_from_entries(
    entries: &[maho_ext_api::SessionEntry],
) -> Option<maho_ai::types::AssistantMessage> {
    entries
        .iter()
        .rev()
        .filter(|entry| entry.kind == "message")
        .find_map(|entry| {
            let message = entry.data.get("message")?;
            if message.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
                return None;
            }
            serde_json::from_value(message.clone()).ok()
        })
}
pub fn last_assistant_text_from_entries(entries: &[maho_ext_api::SessionEntry]) -> String {
    last_assistant_from_entries(entries).map_or_else(String::new, |message| {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}
pub fn is_last_turn_stuck_on_context_overflow(
    context: &ExtensionContext,
    last_assistant: Option<&maho_ai::types::AssistantMessage>,
) -> bool {
    last_assistant.is_some_and(|message| {
        maho_core::compaction::is_turn_stuck_on_context_overflow(
            message,
            context
                .model
                .as_ref()
                .map_or(0, |model| model.context_window),
        )
    })
}
pub fn blocked_reason_for_continuation_guard(reason: DenyReason) -> Option<&'static str> {
    match reason {
        DenyReason::Cap => Some(CONTINUATION_CAP_BLOCKED_REASON),
        DenyReason::NoProgress => Some(NO_PROGRESS_BLOCKED_REASON),
        DenyReason::Repetition => Some(REPETITION_BLOCKED_REASON),
        DenyReason::LengthExhausted => Some(LENGTH_EXHAUSTED_BLOCKED_REASON),
        DenyReason::ContextOverflow => Some(CONTEXT_OVERFLOW_BLOCKED_REASON),
        DenyReason::NotEligible | DenyReason::SingleFlight | DenyReason::Stale => None,
    }
}
fn deny_reason_name(reason: DenyReason) -> &'static str {
    match reason {
        DenyReason::NotEligible => "not-eligible",
        DenyReason::SingleFlight => "single-flight",
        DenyReason::Cap => "cap",
        DenyReason::Stale => "stale",
        DenyReason::NoProgress => "no-progress",
        DenyReason::Repetition => "repetition",
        DenyReason::LengthExhausted => "length-exhausted",
        DenyReason::ContextOverflow => "context-overflow",
    }
}
/// Side effects of a durably recorded stop (`handleDeniedContinuation` after `recorded`): the stale
/// notice, the guard warning and event, the `goal-continuation-stopped` entry and, for stale or
/// repeated output, one `engine-paused` entry per turn. Entry write failures are logged, not raised.
/// `goal` is the pre-stop snapshot, so the reported counters are the ones that tripped the guard.
pub fn report_denied_continuation(
    api: &maho_ext_api::ExtensionApi,
    ctx: &ExtensionContext,
    goal: &Goal,
    input: &crate::continuation::GoalContinuationInput<'_>,
    reason: DenyReason,
    at: u64,
) {
    let name = deny_reason_name(reason);
    if reason == DenyReason::Stale && ctx.has_ui {
        ctx.ui
            .notify(GOAL_STALE_STOP_NOTICE, maho_ext_api::NotificationType::Info);
    }
    let unattended = goal.unattended_continuations.unwrap_or(0);
    let no_progress = goal.no_progress_continuations.unwrap_or(0);
    if let Some(blocked_reason) = blocked_reason_for_continuation_guard(reason) {
        if ctx.has_ui {
            ctx.ui.notify(
                &continuation_cap_recovery_hint(blocked_reason),
                maho_ext_api::NotificationType::Warning,
            );
        }
        api.events.emit("goal_continuation_guard_tripped",&serde_json::json!({"goalId":goal.id,"reason":name,"count":input.consecutive_continuations,"unattendedContinuations":unattended,"noProgressContinuations":no_progress}));
    }
    let warn = |kind: &str, error: ExtensionFailure| {
        if let Some(logger) = &ctx.logger {
            logger.warn(
                "goal_continuation_record_write_failed",
                Some(&serde_json::json!({"kind":kind,"error":error.to_string()})),
            );
        }
    };
    if let Err(error)=api.append_entry("goal-continuation-stopped",Some(serde_json::json!({"goalId":goal.id,"reason":name,"consecutiveContinuations":input.consecutive_continuations,"unattendedContinuations":unattended,"noProgressContinuations":no_progress,"at":at}))) { warn("goal-continuation-stopped",error); }
    if matches!(reason, DenyReason::Stale | DenyReason::Repetition) {
        let branch: Vec<serde_json::Value> = ctx
            .session_manager
            .get_branch()
            .into_iter()
            .map(|entry| {
                let mut value = entry.data;
                value["type"] = entry.kind.into();
                value
            })
            .collect();
        if !maho_core::engine_paused::engine_pause_since_last_turn(&branch)
            && let Err(error)=api.append_entry(maho_core::engine_paused::ENGINE_PAUSED_ENTRY_TYPE,Some(serde_json::json!({"reason":if reason==DenyReason::Stale {"goal-stale"} else {"goal-repeat"},"customType":"goal-continuation","count":input.consecutive_continuations,"at":at}))) { warn("engine-paused",error); }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn branch_assistant_extraction_ignores_user_and_notice_entries_and_preserves_text_order() {
        let message = maho_ai::providers::faux::faux_assistant_message("first", Default::default());
        let mut value = serde_json::to_value(message).unwrap();
        value["role"] = "assistant".into();
        value["content"] =
            serde_json::json!([{"type":"text","text":"first"},{"type":"text","text":"second"}]);
        let entry = |kind: &str, data| maho_ext_api::SessionEntry {
            id: String::new(),
            parent_id: None,
            timestamp: String::new(),
            kind: kind.into(),
            data,
        };
        let entries = vec![
            entry("message", serde_json::json!({"message":value})),
            entry(
                "message",
                serde_json::json!({"message":{"role":"user","content":"question"}}),
            ),
            entry("custom_message", serde_json::json!({"customType":"notice"})),
        ];
        assert_eq!(last_assistant_text_from_entries(&entries), "first\nsecond");
        assert!(last_assistant_from_entries(&[]).is_none());
        assert!(!is_last_turn_stuck_on_context_overflow(
            &crate::test_context::context(),
            last_assistant_from_entries(&entries).as_ref()
        ));
    }
    #[tokio::test]
    async fn guard_notification_event_observes_persisted_block_and_original_counters() {
        use maho_ext_api::*;
        let dir = tempfile::tempdir().unwrap();
        let reference = crate::types::GoalStoreRef {
            base_dir: dir.path().into(),
            thread_id: "s".into(),
        };
        let goal = crate::store::create_goal(&reference, "work", None, 0)
            .await
            .unwrap();
        let api = ExtensionApi::new(
            LoadedExtension::new("goal", "/tmp".into(), SourceInfo::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let capture = seen.clone();
        let stored = reference.clone();
        let _subscription = api.events.on(
            "goal_continuation_guard_tripped",
            std::sync::Arc::new(move |data| {
                assert_eq!(
                    crate::store::read_goal(&stored).unwrap().unwrap().status,
                    GoalStatus::Blocked
                );
                capture.lock().unwrap().push(data.clone());
            }),
        );
        let mut input = admission(&goal);
        input.consecutive_continuations = crate::continuation::GOAL_CONTINUATION_CAP;
        let (blocked, verdict) = admit_and_report_goal_continuation(
            &api,
            &crate::test_context::context(),
            &reference,
            &input,
            1,
        )
        .await
        .unwrap();
        assert_eq!(
            verdict,
            crate::continuation::GoalContinuationVerdict::Deny(DenyReason::Cap)
        );
        assert_eq!(blocked.unwrap().status, GoalStatus::Blocked);
        assert_eq!(seen.lock().unwrap()[0]["count"], 8);
    }
    #[test]
    fn guard_trip_reports_machine_reason_and_counts_only_for_guardrail_denial() {
        use maho_ext_api::*;
        let goal:Goal=serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":"blocked","tokensUsed":0,"timeUsedSeconds":0,"createdAt":0,"updatedAt":0})).unwrap();
        let api = ExtensionApi::new(
            LoadedExtension::new("goal", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = seen.clone();
        let _subscription = api.events.on(
            "goal_continuation_guard_tripped",
            std::sync::Arc::new(move |data| captured.lock().unwrap().push(data.clone())),
        );
        let input = admission(&goal);
        let ctx = crate::test_context::context();
        report_denied_continuation(&api, &ctx, &goal, &input, DenyReason::Stale, 1);
        assert!(seen.lock().unwrap().is_empty());
        report_denied_continuation(&api, &ctx, &goal, &input, DenyReason::Cap, 1);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                serde_json::json!({"goalId":"g","reason":"cap","count":0,"unattendedContinuations":0,"noProgressContinuations":0})
            ]
        );
    }
    fn admission(goal: &Goal) -> crate::continuation::GoalContinuationInput<'_> {
        crate::continuation::GoalContinuationInput {
            goal: Some(goal),
            is_idle: true,
            has_pending_messages: false,
            path: crate::continuation::GoalContinuationPath::SessionStart,
            last_stop_reason: None,
            last_turn_was_malformed_tool_use: false,
            consecutive_continuations: 0,
            last_continuation_signature: None,
            current_signature: Some("sig"),
            consecutive_length_recoveries: 0,
            recent_normalized_output_hashes: &[],
            toolless_continuation_streak: 0,
            continuation_pending: false,
            last_turn_stuck_on_context_overflow: false,
        }
    }
    #[tokio::test]
    async fn admitted_transport_observes_persisted_counter_and_pending_flag() {
        use maho_ext_api::*;
        struct Capture {
            reference: crate::types::GoalStoreRef,
            pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
            seen: std::sync::Mutex<bool>,
        }
        impl ExtensionActions for Capture {
            fn send_message(
                &self,
                _: CustomMessage,
                _: SendMessageOptions,
            ) -> Result<(), ExtensionFailure> {
                assert!(self.pending.load(std::sync::atomic::Ordering::SeqCst));
                assert_eq!(
                    crate::store::read_goal(&self.reference)
                        .unwrap()
                        .unwrap()
                        .consecutive_continuations,
                    Some(1)
                );
                *self.seen.lock().unwrap() = true;
                Ok(())
            }
            fn send_user_message(
                &self,
                _: UserMessageContent,
                _: SendUserMessageOptions,
            ) -> Result<(), ExtensionFailure> {
                Err("unexpected user message".into())
            }
            fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
                Err("unexpected entry".into())
            }
            fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
                Ok(Vec::new())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let reference = crate::types::GoalStoreRef {
            base_dir: dir.path().into(),
            thread_id: "s".into(),
        };
        let goal = crate::store::create_goal(&reference, "work", None, 0)
            .await
            .unwrap();
        let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let capture = std::sync::Arc::new(Capture {
            reference: reference.clone(),
            pending: pending.clone(),
            seen: std::sync::Mutex::new(false),
        });
        let runtime = ExtensionRuntime::default();
        runtime.bind(capture.clone());
        let api = ExtensionApi::new(
            LoadedExtension::new("goal", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        );
        let recorded = admit_and_queue_goal_continuation(
            &api,
            &reference,
            &admission(&goal),
            1,
            || pending.store(true, std::sync::atomic::Ordering::SeqCst),
            |_| "payload".into(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(recorded.id, goal.id);
        assert!(*capture.seen.lock().unwrap());
    }
    #[tokio::test]
    async fn admitted_delivery_records_before_transport_and_exempts_monitor_waits() {
        let dir = tempfile::tempdir().unwrap();
        let reference = crate::types::GoalStoreRef {
            base_dir: dir.path().into(),
            thread_id: "s".into(),
        };
        let goal = crate::store::create_goal(&reference, "work", None, 0)
            .await
            .unwrap();
        let mut input = admission(&goal);
        input.path = crate::continuation::GoalContinuationPath::MonitorDelayed;
        let GoalContinuationAdmission {
            goal: recorded,
            verdict,
            ..
        } = admit_and_record_goal_continuation(&reference, &input, 1)
            .await
            .unwrap();
        assert!(matches!(
            verdict,
            crate::continuation::GoalContinuationVerdict::Continue { .. }
        ));
        let recorded = recorded.unwrap();
        assert_eq!(recorded.consecutive_continuations, Some(1));
        assert_eq!(recorded.unattended_continuations, Some(0));
        assert_eq!(recorded.last_continuation_signature.as_deref(), Some("sig"));
        assert_eq!(crate::store::read_goal(&reference).unwrap(), Some(recorded));
    }
    #[tokio::test]
    async fn guard_denial_persists_block_but_single_flight_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let reference = crate::types::GoalStoreRef {
            base_dir: dir.path().into(),
            thread_id: "s".into(),
        };
        let goal = crate::store::create_goal(&reference, "work", None, 0)
            .await
            .unwrap();
        let mut input = admission(&goal);
        input.continuation_pending = true;
        let same = admit_and_record_goal_continuation(&reference, &input, 1)
            .await
            .unwrap();
        assert_eq!(same.goal, Some(goal.clone()));
        assert!(!same.stop_recorded);
        input.continuation_pending = false;
        input.consecutive_continuations = crate::continuation::GOAL_CONTINUATION_CAP;
        let blocked = admit_and_record_goal_continuation(&reference, &input, 2)
            .await
            .unwrap();
        assert!(blocked.stop_recorded);
        assert_eq!(blocked.goal.unwrap().status, GoalStatus::Blocked);
    }
    #[test]
    fn hidden_prompt_uses_bound_followup_transport() {
        use maho_ext_api::*;
        struct Capture(std::sync::Mutex<Option<(CustomMessage, SendMessageOptions)>>);
        impl ExtensionActions for Capture {
            fn send_message(
                &self,
                message: CustomMessage,
                options: SendMessageOptions,
            ) -> Result<(), ExtensionFailure> {
                *self.0.lock().unwrap() = Some((message, options));
                Ok(())
            }
            fn send_user_message(
                &self,
                _: UserMessageContent,
                _: SendUserMessageOptions,
            ) -> Result<(), ExtensionFailure> {
                Err("unexpected user message".into())
            }
            fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
                Err("unexpected entry".into())
            }
            fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
                Ok(Vec::new())
            }
        }
        let capture = std::sync::Arc::new(Capture(std::sync::Mutex::new(None)));
        let runtime = ExtensionRuntime::default();
        runtime.bind(capture.clone());
        let api = ExtensionApi::new(
            LoadedExtension::new("goal", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        );
        queue_hidden_goal_prompt(&api, "payload".into()).unwrap();
        let (message, options) = capture.0.lock().unwrap().take().unwrap();
        assert_eq!(message.custom_type, "goal-continuation");
        assert!(!message.display);
        assert!(options.trigger_turn);
        assert_eq!(options.deliver_as, Some(DeliverAs::FollowUp));
        assert_eq!(message.content, vec![ToolContent::text("payload")]);
    }
    #[test]
    fn only_guardrail_denials_block_persisted_goal() {
        for reason in [
            DenyReason::Cap,
            DenyReason::NoProgress,
            DenyReason::Repetition,
            DenyReason::LengthExhausted,
            DenyReason::ContextOverflow,
        ] {
            assert!(is_mechanical_continuation_block(
                blocked_reason_for_continuation_guard(reason)
            ));
        }
        for reason in [
            DenyReason::NotEligible,
            DenyReason::SingleFlight,
            DenyReason::Stale,
        ] {
            assert_eq!(blocked_reason_for_continuation_guard(reason), None);
        }
    }
    #[test]
    fn absent_assistant_has_no_text() {
        assert!(last_assistant_text(&[]).is_empty());
    }
    fn stopped_goal(status: GoalStatus) -> Goal {
        serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":status,"tokensUsed":0,"timeUsedSeconds":0,"createdAt":0,"updatedAt":0})).unwrap()
    }
    // The predicate reads idle/pending state from the bound session (pinned ctx.isIdle() and
    // !ctx.hasPendingMessages()), so every case binds a real session.
    #[tokio::test]
    async fn resume_prompt_fires_only_for_stopped_unfinished_statuses() {
        let mut ctx = crate::test_context::context();
        let session = crate::test_context::bind_session(&mut ctx);
        for status in [GoalStatus::Paused, GoalStatus::Blocked] {
            assert!(
                is_resume_of_stopped_goal(&ctx, "resume", Some(&stopped_goal(status))).unwrap(),
                "{status:?}"
            );
        }
        for status in [GoalStatus::Active, GoalStatus::Complete] {
            assert!(
                !is_resume_of_stopped_goal(&ctx, "resume", Some(&stopped_goal(status))).unwrap(),
                "{status:?}"
            );
        }
        assert!(!is_resume_of_stopped_goal(&ctx, "resume", None).unwrap());
        session.dispose().await;
    }
    #[tokio::test]
    async fn resume_prompt_requires_the_resume_reason() {
        let mut ctx = crate::test_context::context();
        let session = crate::test_context::bind_session(&mut ctx);
        let goal = stopped_goal(GoalStatus::Paused);
        for reason in ["startup", "reload", "new"] {
            assert!(
                !is_resume_of_stopped_goal(&ctx, reason, Some(&goal)).unwrap(),
                "{reason}"
            );
        }
        assert!(is_resume_of_stopped_goal(&ctx, "resume", Some(&goal)).unwrap());
        session.dispose().await;
    }
    #[tokio::test]
    async fn resume_prompt_requires_ui() {
        let goal = stopped_goal(GoalStatus::Paused);
        let mut bound = crate::test_context::context();
        let session = crate::test_context::bind_session(&mut bound);
        assert!(is_resume_of_stopped_goal(&bound, "resume", Some(&goal)).unwrap());
        bound.has_ui = false;
        assert!(!is_resume_of_stopped_goal(&bound, "resume", Some(&goal)).unwrap());
        session.dispose().await;
    }
    // Port of regressions/3007-goal-continuation-stopped.test.ts (senpi 903b8336).
    struct Entries(std::sync::Mutex<Vec<(String, serde_json::Value)>>);
    impl maho_ext_api::ExtensionActions for Entries {
        fn send_message(
            &self,
            _: maho_ext_api::CustomMessage,
            _: maho_ext_api::SendMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            Err("unexpected continuation".into())
        }
        fn send_user_message(
            &self,
            _: maho_ext_api::UserMessageContent,
            _: maho_ext_api::SendUserMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            Err("unexpected user message".into())
        }
        fn append_entry(
            &self,
            custom_type: &str,
            data: Option<maho_ext_api::JsonValue>,
        ) -> Result<(), ExtensionFailure> {
            self.0
                .lock()
                .unwrap()
                .push((custom_type.into(), data.unwrap_or_default()));
            Ok(())
        }
        fn get_all_tools(&self) -> Result<Vec<maho_ext_api::ToolInfo>, ExtensionFailure> {
            Ok(Vec::new())
        }
    }
    async fn stop_fixture(
        dir: &tempfile::TempDir,
    ) -> (
        crate::types::GoalStoreRef,
        Goal,
        maho_ext_api::ExtensionApi,
        std::sync::Arc<Entries>,
    ) {
        use maho_ext_api::*;
        let reference = crate::types::GoalStoreRef {
            base_dir: dir.path().into(),
            thread_id: "s".into(),
        };
        let mut goal = crate::store::create_goal(&reference, "Implement the stop signal", None, 0)
            .await
            .unwrap();
        goal.last_started_at = Some(100);
        goal.time_used_seconds = 7.0;
        goal.consecutive_continuations = Some(2);
        goal.unattended_continuations = Some(4);
        goal.last_continuation_signature = Some("same".into());
        crate::store::write_goal(&reference, Some(&goal))
            .await
            .unwrap();
        let entries = std::sync::Arc::new(Entries(std::sync::Mutex::new(Vec::new())));
        let runtime = ExtensionRuntime::default();
        runtime.bind(entries.clone());
        let api = ExtensionApi::new(
            LoadedExtension::new("goal", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        );
        (reference, goal, api, entries)
    }
    fn stop_input(goal: &Goal) -> crate::continuation::GoalContinuationInput<'_> {
        crate::continuation::GoalContinuationInput {
            goal: Some(goal),
            is_idle: true,
            has_pending_messages: false,
            path: crate::continuation::GoalContinuationPath::Immediate,
            last_stop_reason: Some(maho_ai::types::StopReason::Stop),
            last_turn_was_malformed_tool_use: false,
            consecutive_continuations: 2,
            last_continuation_signature: Some("same"),
            current_signature: Some("same"),
            consecutive_length_recoveries: 0,
            recent_normalized_output_hashes: &[],
            toolless_continuation_streak: 0,
            continuation_pending: false,
            last_turn_stuck_on_context_overflow: false,
        }
    }
    #[tokio::test]
    async fn stale_stop_is_recorded_once_closes_elapsed_accounting_and_ignores_probes() {
        let dir = tempfile::tempdir().unwrap();
        let (reference, goal, api, entries) = stop_fixture(&dir).await;
        let ctx = crate::test_context::context();
        let mut probe = stop_input(&goal);
        probe.has_pending_messages = true;
        admit_and_report_goal_continuation(&api, &ctx, &reference, &probe, 110)
            .await
            .unwrap();
        probe.has_pending_messages = false;
        probe.continuation_pending = true;
        admit_and_report_goal_continuation(&api, &ctx, &reference, &probe, 110)
            .await
            .unwrap();
        assert!(entries.0.lock().unwrap().is_empty());
        let (stopped, _) =
            admit_and_report_goal_continuation(&api, &ctx, &reference, &stop_input(&goal), 110)
                .await
                .unwrap();
        admit_and_report_goal_continuation(&api, &ctx, &reference, &stop_input(&goal), 110)
            .await
            .unwrap();
        assert_eq!(
            *entries.0.lock().unwrap(),
            vec![
                (
                    "goal-continuation-stopped".to_owned(),
                    serde_json::json!({"goalId":goal.id,"reason":"stale","consecutiveContinuations":2,"unattendedContinuations":4,"noProgressContinuations":0,"at":110_000})
                ),
                (
                    "engine-paused".to_owned(),
                    serde_json::json!({"reason":"goal-stale","customType":"goal-continuation","count":2,"at":110_000})
                ),
            ]
        );
        let stopped = stopped.unwrap();
        assert_eq!(
            (
                stopped.status,
                stopped.time_used_seconds,
                stopped.last_started_at
            ),
            (GoalStatus::Active, 17.0, None)
        );
        assert_eq!(crate::store::read_goal(&reference).unwrap(), Some(stopped));
    }
    #[tokio::test]
    async fn guard_stops_are_recorded_once_for_a_retried_active_snapshot() {
        let hashes = ["a".to_owned(), "a".to_owned(), "a".to_owned()];
        for reason in ["cap", "repetition", "length-exhausted", "context-overflow"] {
            let dir = tempfile::tempdir().unwrap();
            let (reference, goal, api, entries) = stop_fixture(&dir).await;
            let ctx = crate::test_context::context();
            let mut input = stop_input(&goal);
            match reason {
                "cap" => {
                    input.consecutive_continuations = crate::continuation::GOAL_CONTINUATION_CAP
                }
                "repetition" => input.recent_normalized_output_hashes = &hashes,
                "length-exhausted" => {
                    input.current_signature = Some("new");
                    input.last_stop_reason = Some(maho_ai::types::StopReason::Length);
                    input.consecutive_length_recoveries =
                        crate::continuation::GOAL_LENGTH_RECOVERY_LIMIT;
                }
                _ => input.last_turn_stuck_on_context_overflow = true,
            }
            admit_and_report_goal_continuation(&api, &ctx, &reference, &input, 5)
                .await
                .unwrap();
            admit_and_report_goal_continuation(&api, &ctx, &reference, &input, 5)
                .await
                .unwrap();
            let recorded = entries.0.lock().unwrap().clone();
            let stops: Vec<_> = recorded
                .iter()
                .filter(|(kind, _)| kind == "goal-continuation-stopped")
                .collect();
            assert_eq!(stops.len(), 1, "{reason}");
            assert_eq!(stops[0].1["reason"], reason);
            assert_eq!(stops[0].1["unattendedContinuations"], 4);
            assert_eq!(
                stops[0].1["consecutiveContinuations"],
                if reason == "cap" {
                    crate::continuation::GOAL_CONTINUATION_CAP
                } else {
                    2
                }
            );
            let blocked = crate::store::read_goal(&reference).unwrap().unwrap();
            assert_eq!(blocked.status, GoalStatus::Blocked, "{reason}");
            assert_eq!(
                (
                    blocked.consecutive_continuations,
                    blocked.unattended_continuations,
                    blocked.last_continuation_signature
                ),
                (Some(0), Some(0), None)
            );
            let pauses: Vec<_> = recorded
                .iter()
                .filter(|(kind, _)| kind == "engine-paused")
                .map(|(_, data)| data["reason"].clone())
                .collect();
            assert_eq!(
                pauses,
                if reason == "repetition" {
                    vec![serde_json::json!("goal-repeat")]
                } else {
                    vec![]
                },
                "{reason}"
            );
        }
    }
}
