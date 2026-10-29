use crate::agent_end_eligibility::read_agent_end_outcome;
use crate::boulder_eligibility::{ContinuableWork, find_continuable_boulder_work_in};
use idle_injection::{IdleInjection, IdleInjectionSource};
use maho_ext_api::*;
use std::sync::{Arc, Mutex};
pub const ULW_EXECUTE_STEERING_REMINDER: &str = "<omo-senpi-ulw-execute>\nAn active ulw-execute work plan is present in this working directory.\nBefore continuing, read `.omo/boulder.json` and the active plan file to determine what remains; use the ledger and plan as the source of truth.\nContinue the current work with evidence-bound execution; do not start unrelated work until every top-level checkbox is `- [x]`.\n</omo-senpi-ulw-execute>";
const CONTINUATION_INJECTION_KEY: &str = "omo-senpi-ulw-execute-continuation";
const CONTINUATION_CUSTOM_TYPE: &str = "omo-senpi:ulw-execute-continuation";
/// Pin `77f3067f1` registers this component as `start-work-continuation` (`index.ts:38`), so
/// `compose.ts` gives it the flag `omo-senpi-start-work-continuation-disabled`. The latest rename keeps
/// the pinned flag working as an alias of `omo-senpi-ulw-execute-continuation-disabled`.
pub const START_WORK_CONTINUATION_DISABLED_FLAG: &str =
    "omo-senpi-start-work-continuation-disabled";
fn log_info(logger: &Option<Arc<dyn ComponentLogger>>, message: &str, details: Option<JsonValue>) {
    if let Some(logger) = logger {
        logger.info(message, details.as_ref());
    }
}
fn log_warn(logger: &Option<Arc<dyn ComponentLogger>>, message: &str, details: Option<JsonValue>) {
    if let Some(logger) = logger {
        logger.warn(message, details.as_ref());
    }
}
struct PendingRun {
    payload: JsonValue,
    session_id: Option<String>,
    cwd: std::path::PathBuf,
}
#[derive(Default)]
struct State {
    consecutive: usize,
    last_signature: Option<String>,
    same_signature_retries: usize,
    pending_run: Option<PendingRun>,
}
/// Pin `index.ts:14` `DEFAULT_MAX_SAME_SIGNATURE_RETRIES`.
pub const DEFAULT_MAX_SAME_SIGNATURE_RETRIES: usize = 1;
pub struct StartWorkContinuationComponent {
    pub continuation_limit: usize,
    /// Pin `index.ts:8` `maxSameSignatureRetries`: retries for an unchanged plan/checklist signature.
    pub max_same_signature_retries: usize,
}
impl Default for StartWorkContinuationComponent {
    fn default() -> Self {
        Self {
            continuation_limit: 8,
            max_same_signature_retries: DEFAULT_MAX_SAME_SIGNATURE_RETRIES,
        }
    }
}
impl Extension for StartWorkContinuationComponent {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag(
            START_WORK_CONTINUATION_DISABLED_FLAG,
            FlagType::Boolean {
                default: Some(false),
            },
            Some("Disable the omo-senpi start-work-continuation component.".to_owned()),
        );
        if api.get_flag(START_WORK_CONTINUATION_DISABLED_FLAG) == Some(FlagValue::Boolean(true)) {
            return;
        }
        let completion = Arc::new(Mutex::new(
            crate::run_completion::RunCompletionTracker::default(),
        ));
        let input_completion = Arc::clone(&completion);
        let input_lifecycle = Arc::new(api.lifecycle());
        let state = Arc::new(Mutex::new(State::default()));
        let input_state = Arc::clone(&state);
        api.on(
            EventKind::Input,
            Arc::new(move |event, ctx| {
                let state = Arc::clone(&input_state);
                let completion = Arc::clone(&input_completion);
                let lifecycle = input_lifecycle.clone();
                Box::pin(async move {
                    let ExtensionEvent::Input(input) = event else {
                        return Ok(EventResult::None);
                    };
                    if input.source == InputSource::Extension {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }
                    *state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = State::default();
                    observe_completion(&completion, ctx, lifecycle.as_ref());
                    if input.streaming_behavior.is_some()
                        && !ctx.session_manager.session_id().is_empty()
                        && find_continuable_boulder_work_in(
                            &ctx.cwd,
                            ctx.session_manager.session_id(),
                            &ctx.agent_dir.join("sessions"),
                        )
                        .map_err(|e| ExtensionFailure::new(e.to_string()))?
                        .is_some()
                    {
                        return Ok(EventResult::Input(InputEventResult::Transform {
                            text: format!("{}\n\n{ULW_EXECUTE_STEERING_REMINDER}", input.text),
                            images: input.images.clone(),
                        }));
                    }
                    Ok(EventResult::Input(InputEventResult::Continue))
                })
            }),
        );
        // Record only: the outcome is decided on agent_settled, the host edge that guarantees no
        // automatic retry, compaction or queued continuation will run.
        let record_state = Arc::clone(&state);
        api.on(EventKind::AgentEnd,Arc::new(move |event,ctx| { let state=Arc::clone(&record_state);Box::pin(async move {
            let ExtensionEvent::AgentEnd{messages,aborted,will_retry,..}=event else { return Ok(EventResult::None); };
            let payload=serde_json::json!({"messages":messages,"aborted":aborted.unwrap_or(false),"willRetry":will_retry.unwrap_or(false)});
            state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending_run=Some(PendingRun{payload,session_id:Some(ctx.session_manager.session_id().to_owned()).filter(|id|!id.is_empty()),cwd:ctx.cwd.clone()});
            Ok(EventResult::None)
        }) }));
        let lifecycle = Arc::new(api.lifecycle());
        let limit = self.continuation_limit;
        let max_retries = self.max_same_signature_retries;
        let runtime = api.runtime.clone();
        api.on(EventKind::AgentSettled,Arc::new(move |_,ctx| { let state=Arc::clone(&state);let runtime=runtime.clone(); let completion=Arc::clone(&completion); let lifecycle=lifecycle.clone(); Box::pin(async move {
            observe_completion(&completion, ctx, lifecycle.as_ref());
            let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(run)=state.pending_run.take() else { return Ok(EventResult::None); };
            let outcome=read_agent_end_outcome(&run.payload);
            if let Some(blocked_by)=outcome.blocked_by {
                log_info(&ctx.logger,"omo-senpi ulw-execute-continuation skipped",Some(serde_json::json!({"reason":"terminal-outcome","blockedBy":blocked_by.as_str(),"stopReason":outcome.stop_reason,"aborted":outcome.aborted,"willRetry":outcome.will_retry})));
                return Ok(EventResult::None);
            }
            if state.consecutive>=limit { let count=state.consecutive; drop(state); log_info(&ctx.logger,"omo-senpi ulw-execute-continuation skipped",Some(serde_json::json!({"reason":"continuation-cap-reached","count":count}))); return Ok(EventResult::None); }
            let Some(session_id)=run.session_id else { log_info(&ctx.logger,"omo-senpi ulw-execute-continuation skipped",Some(serde_json::json!({"reason":"missing-context"}))); return Ok(EventResult::None); };
            let Some(work)=find_continuable_boulder_work_in(&run.cwd,&session_id,&ctx.agent_dir.join("sessions")).map_err(|e|ExtensionFailure::new(e.to_string()))? else { state.last_signature=None;state.same_signature_retries=0;log_info(&ctx.logger,"omo-senpi ulw-execute-continuation skipped",Some(serde_json::json!({"reason":"not-continuable"})));return Ok(EventResult::None); };
            let signature=format!("{}:{}:{}/{}",work.work.work_id().unwrap_or_default(),work.work.updated_at().or_else(||work.work.started_at()).unwrap_or_default(),work.checklist.completed,work.checklist.total);
            // Pin `index.ts:101-118`: an unchanged signature gets a bounded recovery retry budget; once
            // spent, the edge is skipped with `retry-budget-exhausted` until progress or user input.
            if state.last_signature.as_ref()==Some(&signature) {
                if state.same_signature_retries>=max_retries {
                    let retries=state.same_signature_retries; drop(state);
                    log_info(&ctx.logger,"omo-senpi ulw-execute-continuation skipped",Some(serde_json::json!({"reason":"retry-budget-exhausted","signature":signature,"retries":retries,"maxRetries":max_retries})));
                    return Ok(EventResult::None);
                }
                state.same_signature_retries+=1;
            } else {
                state.last_signature=Some(signature);
                state.same_signature_retries=0;
            }
            state.consecutive+=1;
            let content=render_directive(&work,&run.cwd,&session_id);
            deliver(ctx,&runtime,content)?;
            Ok(EventResult::None)
        }) }));
    }
}
fn deliver(
    ctx: &ExtensionContext,
    runtime: &ExtensionRuntime,
    content: String,
) -> Result<(), ExtensionFailure> {
    if let Some(coordinator) = idle_injection::from_slot(&ctx.idle_coordinator) {
        let accepted = coordinator.enqueue(IdleInjection {
            key: CONTINUATION_INJECTION_KEY.into(),
            source: IdleInjectionSource::BoulderContinuation,
            custom_type: Some(CONTINUATION_CUSTOM_TYPE.into()),
            content,
            display: Some(false),
            details: None,
            passive: Some(false),
            on_flushed: None,
            on_delivery_failed: None,
        });
        // Refused = the coordinator retired with the session. The continuation is derived state, not a
        // durable notification: the next boulder edge re-derives it. Log rather than drop in silence.
        if !accepted {
            log_warn(
                &ctx.logger,
                "omo-senpi ulw execute continuation skipped: idle-injection coordinator retired",
                None,
            );
            return Ok(());
        }
        coordinator.schedule_flush();
        return Ok(());
    }
    let api = ExtensionApi::new(
        LoadedExtension::new(
            "ulw-execute-continuation",
            ctx.cwd.clone(),
            SourceInfo::default(),
        ),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        runtime.clone(),
    );
    api.send_message(
        CustomMessage {
            custom_type: CONTINUATION_CUSTOM_TYPE.into(),
            content: vec![ToolContent::text(content)],
            display: false,
            details: None,
        },
        SendMessageOptions {
            trigger_turn: true,
            deliver_as: Some(DeliverAs::FollowUp),
        },
    )
}
pub fn render_directive(work: &ContinuableWork, cwd: &std::path::Path, session_id: &str) -> String {
    let worktree = work
        .work
        .worktree_path()
        .map(|p| {
            format!(
                "\n- Worktree: `{p}` (all edits, tests, and commands run inside this directory)"
            )
        })
        .unwrap_or_default();
    let final_hint = if work.checklist.remaining == 0 {
        "\nAll top-level checkboxes are complete. Run the Final Verification Wave and mark the boulder work completed."
    } else {
        ""
    };
    vec![
        "<omo-senpi-ulw-execute-continuation>".into(),
        "You are mid-flight on a ulw-execute work plan; this turn is an automatic continuation. Do NOT ask whether to continue — the contract is auto-continue until every top-level checkbox is `- [x]`.".into(),"".into(),"# State".into(),"".into(),
        format!("- Plan: `{}`",work.work.plan_name().unwrap_or_default()),format!("- Plan file: `{}`",work.plan_path.display()),format!("- Boulder state: `{}`",cwd.join(".omo/boulder.json").display()),
        format!("- Remaining top-level checkboxes: {} of {}",work.checklist.remaining,work.checklist.total),format!("- [Status: {}/{}, next: {}]",work.checklist.completed,work.checklist.total,work.checklist.next_task_label.as_deref().unwrap_or("none (final gate pending)")),format!("- Ledger: `{}`",cwd.join(".omo/ulw-execute/ledger.jsonl").display()),format!("- Your session id in boulder.json: senpi:{session_id}"),worktree,"".into(),"# What to do this turn".into(),"".into(),
        "1. Read the plan file AND the ledger first — they are the only sources of truth for what remains and what evidence exists; do not trust your memory of prior turns.".into(),
        format!("2. When the remaining count is `0`, skip checkbox execution and perform the Final gate now. Otherwise, pick the FIRST unchecked top-level checkbox in `## TODOs` or `## Final Verification Wave`.{final_hint}"),
        "3. Apply the checkbox's tier and verify with real-surface evidence. Decompose and dispatch sub-tasks in parallel via Senpi's `task` tool when safe.".into(),
        "4. Honor the delivery mode recorded in the goal/ledger at session start: `--make-pr` finishes through the task-owned worktree and an opened PR, then hands off with the PR URL; `--ship` keeps working until that PR is MERGED, then removes the worktree and syncs `.omo/` state back.".into(),
        "5. After verification, apply the checkbox, append a durable evidence record to the ledger, and continue.".into(),"</omo-senpi-ulw-execute-continuation>".into(),
    ].join("\n")
}

fn observe_completion(
    tracker: &Mutex<crate::run_completion::RunCompletionTracker>,
    ctx: &ExtensionContext,
    lifecycle: &LazyLifecycleApi,
) {
    let id = ctx.session_manager.session_id();
    if id.is_empty() {
        return;
    }
    let run = tracker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .observe(&ctx.cwd, id);
    if let Some(run) = run
        && let Err(error) = lifecycle.mark_run_completed(&run) {
            log_warn(
                &ctx.logger,
                "omo-senpi ulw-execute run completion failed",
                Some(serde_json::json!({"runId":run,"error":error.message})),
            );
        }
}
