use crate::omo_command::run_omo_status;
use idle_injection::{IdleInjection, IdleInjectionSource};
use maho_ext_api::*;
use maho_omo_start_work_continuation::agent_end_eligibility::read_agent_end_outcome;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub const STEERING_REMINDER: &str = "<omo-senpi-ulw-loop>\nAn active ulw-loop run is present in this working directory.\nBefore continuing, inspect `mhc ulw-loop status --json` and use the existing .omo/ulw-loop ledger as the source of truth.\nContinue the current ulw-loop story with evidence-bound execution; do not start unrelated work until the active run is complete or checkpointed.\n</omo-senpi-ulw-loop>";
pub const CONTINUATION_PROMPT: &str = "Continue the active ulw-loop run.\nRun `mhc ulw-loop status --json` in this session cwd, inspect the active incomplete goals, and keep working until the run is complete or safely checkpointed.";
pub const ULW_CONTINUATION_INJECTION_KEY: &str = "omo-senpi-ulw-loop-continuation";
pub const ULW_CONTINUATION_CUSTOM_TYPE: &str = "omo-senpi:ulw-continuation";

#[derive(Default)]
struct SessionState {
    consecutive: usize,
    idle_turns: usize,
    previous_status: Option<String>,
    previous_fingerprint: Option<u64>,
    turn_had_successful_edit: bool,
}

#[derive(Default)]
struct State {
    sessions: HashMap<String, SessionState>,
}

#[derive(Clone, Default)]
struct MtimeCacheEntry {
    mtimes: (Option<SystemTime>, Option<SystemTime>),
    active: bool,
}

async fn compute_worktree_fingerprint(cwd: &std::path::Path) -> Option<u64> {
    use std::hash::Hasher;
    let status_out = tokio::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-z"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;
    if !status_out.status.success() {
        return None;
    }
    let diff_out = tokio::process::Command::new("git")
        .args(["diff", "--stat"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;
    if !diff_out.status.success() {
        return None;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(&status_out.stdout);
    hasher.write(&diff_out.stdout);
    Some(hasher.finish())
}

pub type StatusResult = crate::omo_command::CommandResult;
pub type StatusFuture = std::pin::Pin<Box<dyn std::future::Future<Output = StatusResult> + Send>>;
pub type StatusReader = Arc<dyn Fn(std::path::PathBuf) -> StatusFuture + Send + Sync>;
struct PendingRun {
    payload: serde_json::Value,
    status: (String, bool),
}

#[derive(Default)]
pub struct UlwLoopComponent {
    pub read_status: Option<StatusReader>,
    pub logger: Option<Arc<dyn ComponentLogger>>,
}

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

async fn status(
    cwd: &std::path::Path,
    session_id: &str,
    reader: Option<&StatusReader>,
    logger: &Option<Arc<dyn ComponentLogger>>,
    tracker: &Mutex<crate::run_completion::RunCompletionTracker>,
    lifecycle: &LazyLifecycleApi,
) -> Option<(String, bool)> {
    if session_id.is_empty() {
        return Some((String::new(), false));
    }
    let result = if let Some(read) = reader {
        read(cwd.into()).await
    } else {
        run_omo_status(cwd, Some(session_id)).await
    };
    if result.code != 0 {
        if serde_json::from_str::<JsonValue>(&result.stdout)
            .ok()
            .is_some_and(|value| value["error"]["code"] == "ULW_LOOP_PLAN_MISSING")
        {
            tracker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .observe(None);
        }
        log_warn(
            logger,
            "omo-senpi ulw-loop status ignored",
            Some(serde_json::json!({"reason":"non-zero-exit","code":result.code})),
        );
        // Pin `index.ts:174-177`: a non-zero exit is an inactive status, not an absent one; only a
        // spawn failure yields `null`, and the in-process native reader has no spawn step.
        return Some((result.stdout, false));
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&result.stdout) else {
        log_warn(
            logger,
            "omo-senpi ulw-loop status ignored",
            Some(serde_json::json!({"reason":"malformed-json"})),
        );
        return Some((result.stdout, false));
    };
    if let Some(plan) = crate::run_completion::plan_snapshot(&value) {
        let run = tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(Some(plan));
        if let Some(run) = run
            && let Err(error) = lifecycle.mark_run_completed(&run)
        {
            log_warn(
                logger,
                "omo-senpi ulw-loop run completion failed",
                Some(serde_json::json!({"runId":run,"error":error.message})),
            );
        }
    }
    Some((result.stdout, status_has_active_incomplete_run(&value)))
}

impl Extension for UlwLoopComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let completion = Arc::new(Mutex::new(
            crate::run_completion::RunCompletionTracker::default(),
        ));
        let lifecycle = Arc::new(api.lifecycle());
        let state = Arc::new(Mutex::new(State::default()));
        let mtime_cache = Arc::new(Mutex::new(HashMap::<String, MtimeCacheEntry>::new()));
        let footer = Arc::new(Mutex::new(crate::footer_status::FooterStatus::default()));
        let reader = self.read_status.clone();
        let logger = self.logger.clone();
        let pending_run = Arc::new(Mutex::new(None::<PendingRun>));

        let start_footer = Arc::clone(&footer);
        let start_reader = reader.clone();
        let start_logger = logger.clone();
        let start_completion = Arc::clone(&completion);
        let start_lifecycle = lifecycle.clone();
        let start_mtime_cache = Arc::clone(&mtime_cache);

        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let footer = Arc::clone(&start_footer);
                let reader = start_reader.clone();
                let logger = start_logger.clone();
                let completion = Arc::clone(&start_completion);
                let lifecycle = start_lifecycle.clone();
                let mtime_cache = Arc::clone(&start_mtime_cache);
                Box::pin(async move {
                    let active = status(
                        &ctx.cwd,
                        ctx.session_manager.session_id(),
                        reader.as_ref(),
                        &ctx.logger.clone().or(logger),
                        &completion,
                        lifecycle.as_ref(),
                    )
                    .await
                    .is_some_and(|(_, active)| active);

                    let session_id = ctx.session_manager.session_id();
                    let loop_dir = ctx.cwd.join(".omo/ulw-loop").join(session_id);
                    let goals_mtime = std::fs::metadata(loop_dir.join("goals.json"))
                        .and_then(|m| m.modified())
                        .ok();
                    let revisions_mtime = std::fs::metadata(loop_dir.join("revisions"))
                        .and_then(|m| m.modified())
                        .ok();
                    {
                        let mut cache = mtime_cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        cache.insert(
                            session_id.to_string(),
                            MtimeCacheEntry {
                                mtimes: (goals_mtime, revisions_mtime),
                                active,
                            },
                        );
                    }

                    crate::footer_status::sync_shared(&footer, ctx, active);
                    Ok(EventResult::None)
                })
            }),
        );

        let tool_footer = Arc::clone(&footer);
        let tool_reader = reader.clone();
        let tool_logger = logger.clone();
        let tool_completion = Arc::clone(&completion);
        let tool_lifecycle = lifecycle.clone();
        let tool_mtime_cache = Arc::clone(&mtime_cache);
        let tool_state = Arc::clone(&state);

        api.on(
            EventKind::ToolResult,
            Arc::new(move |event, ctx| {
                let footer = Arc::clone(&tool_footer);
                let reader = tool_reader.clone();
                let logger = tool_logger.clone();
                let completion = Arc::clone(&tool_completion);
                let lifecycle = tool_lifecycle.clone();
                let mtime_cache = Arc::clone(&tool_mtime_cache);
                let state = Arc::clone(&tool_state);
                Box::pin(async move {
                    let ExtensionEvent::ToolResult(e) = event else {
                        return Ok(EventResult::None);
                    };

                    // Track successful edit|write|apply_patch result for progress
                    if matches!(e.tool_name.as_str(), "edit" | "write" | "apply_patch")
                        && !e.is_error
                    {
                        let mut guard = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        guard
                            .sessions
                            .entry(ctx.session_manager.session_id().to_string())
                            .or_default()
                            .turn_had_successful_edit = true;
                    }

                    if !matches!(
                        e.tool_name.as_str(),
                        "create_goal" | "update_goal" | "bash" | "interactive_bash"
                    ) {
                        return Ok(EventResult::None);
                    }

                    // (b) stat .omo/ulw-loop/<session>/goals.json and revisions/ dir mtime;
                    // if neither exists return early;
                    // if both mtimes equal the last observed pair reuse the cached status.
                    let session_id = ctx.session_manager.session_id();
                    let loop_dir = ctx.cwd.join(".omo/ulw-loop").join(session_id);
                    let goals_mtime = std::fs::metadata(loop_dir.join("goals.json"))
                        .and_then(|m| m.modified())
                        .ok();
                    let revisions_mtime = std::fs::metadata(loop_dir.join("revisions"))
                        .and_then(|m| m.modified())
                        .ok();

                    if goals_mtime.is_none() && revisions_mtime.is_none() {
                        return Ok(EventResult::None);
                    }

                    let current_pair = (goals_mtime, revisions_mtime);
                    {
                        let cache = mtime_cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(entry) = cache.get(session_id)
                            && entry.mtimes == current_pair
                        {
                            crate::footer_status::sync_shared(&footer, ctx, entry.active);
                            return Ok(EventResult::None);
                        }
                    }

                    let active = status(
                        &ctx.cwd,
                        session_id,
                        reader.as_ref(),
                        &ctx.logger.clone().or(logger),
                        &completion,
                        lifecycle.as_ref(),
                    )
                    .await
                    .is_some_and(|(_, active)| active);

                    {
                        let mut cache = mtime_cache
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        cache.insert(
                            session_id.to_string(),
                            MtimeCacheEntry {
                                mtimes: current_pair,
                                active,
                            },
                        );
                    }

                    crate::footer_status::sync_shared(&footer, ctx, active);
                    Ok(EventResult::None)
                })
            }),
        );

        for kind in [EventKind::SessionBeforeSwitch, EventKind::SessionShutdown] {
            let footer = Arc::clone(&footer);
            api.on(
                kind,
                Arc::new(move |_, _| {
                    let footer = Arc::clone(&footer);
                    Box::pin(async move {
                        footer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .dispose();
                        Ok(EventResult::None)
                    })
                }),
            );
        }

        let input_state = Arc::clone(&state);
        let input_footer = Arc::clone(&footer);
        let input_reader = reader.clone();
        let input_logger = logger.clone();
        let input_mtime_cache = Arc::clone(&mtime_cache);

        let input_completion = Arc::clone(&completion);
        let input_lifecycle = lifecycle.clone();
        api.on(
            EventKind::Input,
            Arc::new(move |event, ctx| {
                let state = Arc::clone(&input_state);
                let footer = Arc::clone(&input_footer);
                let reader = input_reader.clone();
                let logger = input_logger.clone();
                let completion = Arc::clone(&input_completion);
                let lifecycle = input_lifecycle.clone();
                let mtime_cache = Arc::clone(&input_mtime_cache);
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
                    mtime_cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clear();
                    if input.streaming_behavior.is_some() {
                        let active = status(
                            &ctx.cwd,
                            ctx.session_manager.session_id(),
                            reader.as_ref(),
                            &ctx.logger.clone().or(logger),
                            &completion,
                            lifecycle.as_ref(),
                        )
                        .await
                        .is_some_and(|(_, active)| active);
                        crate::footer_status::sync_shared(&footer, ctx, active);
                        if active {
                            return Ok(EventResult::Input(InputEventResult::Transform {
                                text: format!("{}\n\n{STEERING_REMINDER}", input.text),
                                images: input.images.clone(),
                            }));
                        }
                    }
                    Ok(EventResult::Input(InputEventResult::Continue))
                })
            }),
        );

        let settled_state = Arc::clone(&state);
        let settled_pending = Arc::clone(&pending_run);
        let settled_runtime = api.runtime.clone();

        let agent_end_completion = Arc::clone(&completion);
        let agent_end_lifecycle = lifecycle.clone();
        let agent_end_state = Arc::clone(&state);
        let agent_end_footer = Arc::clone(&footer);
        let agent_end_reader = reader.clone();
        let agent_end_logger = logger.clone();
        let agent_end_pending = Arc::clone(&pending_run);
        let agent_end_mtime_cache = Arc::clone(&mtime_cache);

        api.on(EventKind::AgentEnd, Arc::new(move |event, ctx| {
            let completion = Arc::clone(&agent_end_completion);
            let lifecycle = agent_end_lifecycle.clone();
            let state = Arc::clone(&agent_end_state);
            let footer = Arc::clone(&agent_end_footer);
            let reader = agent_end_reader.clone();
            let logger = ctx.logger.clone().or(agent_end_logger.clone());
            let pending_run = Arc::clone(&agent_end_pending);
            let mtime_cache = Arc::clone(&agent_end_mtime_cache);
            Box::pin(async move {
                *pending_run.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                {
                    let guard = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let session_id = ctx.session_manager.session_id();
                    if let Some(session) = guard.sessions.get(session_id)
                        && session.consecutive >= 8
                    {
                        let count = session.consecutive;
                        drop(guard);
                        log_info(&ctx.logger, "omo-senpi ulw-loop continuation skipped", Some(serde_json::json!({"reason":"continuation-cap-reached","count":count})));
                        return Ok(EventResult::None);
                    }
                }
                if !ctx.session_manager.session_id().is_empty()
                    && maho_omo_start_work_continuation::boulder_eligibility::find_continuable_boulder_work(&ctx.cwd, ctx.session_manager.session_id())
                        .map_err(|e| ExtensionFailure::new(e.to_string()))?
                        .is_some()
                {
                    log_info(&ctx.logger, "omo-senpi ulw-loop continuation skipped", Some(serde_json::json!({"reason":"boulder-continuation-active"})));
                    return Ok(EventResult::None);
                }
                let result = status(&ctx.cwd, ctx.session_manager.session_id(), reader.as_ref(), &logger, &completion, lifecycle.as_ref()).await;
                let Some((raw, active)) = result else {
                    crate::footer_status::sync_shared(&footer, ctx, false);
                    return Ok(EventResult::None);
                };
                crate::footer_status::sync_shared(&footer, ctx, active);

                let session_id = ctx.session_manager.session_id();
                let loop_dir = ctx.cwd.join(".omo/ulw-loop").join(session_id);
                let goals_mtime = std::fs::metadata(loop_dir.join("goals.json")).and_then(|m| m.modified()).ok();
                let revisions_mtime = std::fs::metadata(loop_dir.join("revisions")).and_then(|m| m.modified()).ok();
                {
                    let mut cache = mtime_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    cache.insert(session_id.to_string(), MtimeCacheEntry {
                        mtimes: (goals_mtime, revisions_mtime),
                        active,
                    });
                }

                let payload = if let ExtensionEvent::AgentEnd { messages, aborted, will_retry, .. } = event {
                    // `read_agent_end_outcome` only reads the last assistant message and what follows it.
                    let tail = messages.iter().rposition(|message| message.role() == "assistant").map_or(&[][..], |index| &messages[index..]);
                    serde_json::json!({"messages":tail,"aborted":aborted,"willRetry":will_retry})
                } else { serde_json::Value::Null };
                *pending_run.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PendingRun { payload, status: (raw, active) });
                Ok(EventResult::None)
            })
        }));

        api.on(
            EventKind::AgentSettled,
            Arc::new(move |_, ctx| {
                let state = Arc::clone(&settled_state);
                let pending_run = Arc::clone(&settled_pending);
                let runtime = settled_runtime.clone();
                Box::pin(async move {
                    let Some(run) = pending_run
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    else {
                        return Ok(EventResult::None);
                    };
                    let outcome = read_agent_end_outcome(&run.payload);
                    if outcome.blocked_by.is_some() {
                        log_info(
                            &ctx.logger,
                            "omo-senpi ulw-loop continuation skipped",
                            Some(serde_json::json!({"reason":"terminal-outcome"})),
                        );
                        return Ok(EventResult::None);
                    }
                    // Taken before the state lock so no guard is held across the await.
                    let current_fingerprint = if run.status.1 {
                        compute_worktree_fingerprint(&ctx.cwd).await
                    } else {
                        None
                    };
                    let session_id = ctx.session_manager.session_id().to_string();
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let session = state.sessions.entry(session_id).or_default();
                    if !run.status.1 {
                        session.previous_status = None;
                        session.idle_turns = 0;
                        drop(state);
                        log_info(
                            &ctx.logger,
                            "omo-senpi ulw-loop continuation skipped",
                            Some(serde_json::json!({"reason":"inactive"})),
                        );
                        return Ok(EventResult::None);
                    }

                    // SAFETY-DIVERGENCE(packages/omo-senpi/components/ulw-loop/index.ts): upstream stops after 1 turn with unchanged status; mhc allows progress via file edits or worktree fingerprint changes and requires 2 idle turns before stopping.
                    let status_changed = session.previous_status.as_ref() != Some(&run.status.0);
                    let edit_succeeded = session.turn_had_successful_edit;
                    let fingerprint_changed = current_fingerprint.is_some()
                        && session.previous_fingerprint.is_some()
                        && current_fingerprint != session.previous_fingerprint;

                    let progress = status_changed || edit_succeeded || fingerprint_changed;

                    session.turn_had_successful_edit = false;
                    session.previous_status = Some(run.status.0);
                    if current_fingerprint.is_some() {
                        session.previous_fingerprint = current_fingerprint;
                    }

                    if progress {
                        session.idle_turns = 0;
                    } else {
                        session.idle_turns += 1;
                    }

                    if session.idle_turns >= 2 {
                        drop(state);
                        log_info(
                            &ctx.logger,
                            "omo-senpi ulw-loop continuation skipped",
                            Some(serde_json::json!({"reason":"stale-status"})),
                        );
                        return Ok(EventResult::None);
                    }

                    session.consecutive += 1;
                    drop(state);

                    if let Some(coordinator) = idle_injection::from_slot(&ctx.idle_coordinator) {
                        // A retired coordinator refuses the injection and the caller still owns it:
                        // dropping that answer would end the loop's turn with no continuation queued
                        // and nothing recorded. Fall back to the direct send below and say so.
                        let accepted = coordinator.enqueue(IdleInjection {
                            key: ULW_CONTINUATION_INJECTION_KEY.into(),
                            source: IdleInjectionSource::UlwContinuation,
                            custom_type: Some(ULW_CONTINUATION_CUSTOM_TYPE.into()),
                            content: CONTINUATION_PROMPT.into(),
                            display: Some(false),
                            details: None,
                            passive: Some(false),
                            on_flushed: None,
                            on_delivery_failed: None,
                        });
                        if accepted {
                            coordinator.schedule_flush();
                            return Ok(EventResult::None);
                        }
                        log_warn(
                            &ctx.logger,
                            "omo-senpi ulw-loop continuation injection refused; sending directly",
                            Some(serde_json::json!({"reason":"coordinator-retired"})),
                        );
                    }

                    let api = ExtensionApi::new(
                        LoadedExtension::new("ulw-loop", ctx.cwd.clone(), SourceInfo::default()),
                        ExtensionSessionProfile::default(),
                        EventBus::default(),
                        runtime,
                    );
                    api.send_message(
                        CustomMessage {
                            custom_type: ULW_CONTINUATION_CUSTOM_TYPE.into(),
                            content: vec![ToolContent::text(CONTINUATION_PROMPT)],
                            display: false,
                            details: None,
                        },
                        SendMessageOptions {
                            trigger_turn: true,
                            deliver_as: Some(DeliverAs::FollowUp),
                        },
                    )?;
                    Ok(EventResult::None)
                })
            }),
        );
    }
}

pub fn status_has_active_incomplete_run(value: &serde_json::Value) -> bool {
    if value["ok"] != true
        || !value["plan"].is_object()
        || value["plan"]["aggregateCompletion"]["status"] == "complete"
    {
        return false;
    }
    value["plan"]["goals"].as_array().is_some_and(|goals| {
        goals.iter().any(|goal| {
            if !goal.is_object()
                || matches!(
                    goal["steeringStatus"].as_str(),
                    Some("superseded" | "blocked")
                )
                || !matches!(goal["status"].as_str(), Some("pending" | "in_progress"))
            {
                return false;
            }
            goal["successCriteria"].as_array().is_none_or(|criteria| {
                criteria.is_empty() || criteria.iter().any(|c| c["status"] != "pass")
            })
        })
    })
}
