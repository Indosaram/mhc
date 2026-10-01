//! `tools/task/execute-single.ts`: runs one spawn (validation, spawn policy, start, foreground
//! wait with throttled progress updates, abort handling). The TS `setTimeout` trailing progress
//! emit becomes a detached timer thread parked on a condvar; `clearTimeout` bumps the timer slot.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{PlanResolutionCode, PlanResolutionError, StartResult, StartedTask};
use crate::manager::{AbortSignal, TaskManager, WaitError};
use crate::progress::{
    ChildProgress, ChildProgressTarget, ProgressActivity, ToolProgressDetails,
    create_child_progress, read_tool_progress_details,
};
use crate::shared::ManagedChildEvent;
use crate::state::{ResolvedModelRecord, TaskRecord, system_now_ms};
use crate::steering::CancelOptions;
use crate::tools::task::execute_batch::{TaskToolResult, ToolTextContent};
use crate::tools::task::execute_spec::{
    ResolvedManagerStartSpec, SingleSpawnParams, TaskToolDeps, build_start_spec,
};
use crate::tools::task::foreground_wait::{
    ForegroundWaitInput, ForegroundWaitOptions, ForegroundWaitResult, TaskToolContext,
    wait_for_foreground_task,
};
use crate::tools::task::result_details::{
    PartialTaskToolDetails, SingleSpawnParams as DetailsParams, partial_details, record_details,
    started_details,
};
use crate::tools::task::skill_result::append_missing_skills;
use crate::tools::task::spawn_policy::{SpawnPolicyDeps, SpawnPolicyVerdict, evaluate_spawn_policy};
use crate::tools::task::start_presentation::{
    StartLabels, StartedView, background_conversion_text, background_start_text,
};
use crate::tools::task::types::{SpawnTarget, TaskSkillSummary, TaskToolDetails, TaskToolMode};
use crate::tools::task::validation::{TargetInput, TaskTargetSelection, validate_task_target};

/// Minimum interval between two streamed progress updates, in milliseconds.
const PROGRESS_THROTTLE_MS: u64 = 250;

/// The task tool's dependencies: the manager, spec-building deps, and the spawn-policy seams.
#[derive(Clone, Copy)]
pub struct TaskExecuteDeps<'a> {
    pub manager: &'a TaskManager,
    pub tool: &'a TaskToolDeps,
    pub policy: &'a (dyn SpawnPolicyDeps + Sync),
}

/// `AgentToolResult<TaskToolDetails>` streamed through `onUpdate` (details carry progress).
pub struct TaskToolPartialResult {
    pub content: Vec<ToolTextContent>,
    pub details: PartialTaskToolDetails,
}

/// `AgentToolUpdateCallback<TaskToolDetails>`.
pub type TaskToolUpdate = dyn Fn(TaskToolPartialResult) + Send + Sync;

pub struct RunSpawnInput<'a> {
    pub params: SingleSpawnParams,
    pub signal: Option<&'a AbortSignal>,
    pub on_update: Option<Arc<TaskToolUpdate>>,
    pub ctx: &'a TaskToolContext,
    pub options: ForegroundWaitOptions,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn text_block(text: String) -> ToolTextContent {
    ToolTextContent {
        kind: "text".to_string(),
        text,
    }
}

fn result(text: String, details: TaskToolDetails) -> TaskToolResult {
    TaskToolResult {
        content: vec![text_block(text)],
        details,
    }
}

fn status_result(status: &str, reason: &str) -> TaskToolResult {
    result(
        reason.to_string(),
        TaskToolDetails {
            task_id: String::new(),
            status: status.to_string(),
            mode: TaskToolMode::Spawn,
            reason: Some(reason.to_string()),
            ..TaskToolDetails::default()
        },
    )
}

fn continuation_footer(task_id: &str) -> String {
    format!("\n\n[task_id: {task_id} - continue with task_send(to=\"{task_id}\", message=\"...\")]")
}

fn sync_result(
    record: &TaskRecord,
    mode: TaskToolMode,
    skills: Option<&TaskSkillSummary>,
) -> TaskToolResult {
    let body = record
        .final_response
        .clone()
        .or_else(|| record.error_message.clone())
        .unwrap_or_else(|| format!("Task {}", record.status.as_str()));
    result(
        append_missing_skills(
            &format!("{body}{}", continuation_footer(&record.task_id)),
            &[skills],
        ),
        TaskToolDetails {
            skills: skills.cloned(),
            ..record_details(record, mode)
        },
    )
}

fn details_params(params: &SingleSpawnParams) -> DetailsParams {
    DetailsParams {
        prompt: params.prompt.clone(),
        task_summary: params.task_summary.clone(),
        description: params.description.clone(),
        category: params.category.clone(),
        subagent_type: params.subagent_type.clone(),
        run_in_background: params.run_in_background,
        name: params.name.clone(),
        model: params.model.clone(),
        load_skills: params.load_skills.clone(),
    }
}

fn plan_suffixes(error: &PlanResolutionError) -> String {
    let agent_suffix = error
        .available_agents
        .as_ref()
        .filter(|agents| !agents.is_empty())
        .map(|agents| format!(" Available agents: {}.", agents.join(", ")))
        .unwrap_or_default();
    let category_suffix = error
        .available_categories
        .as_ref()
        .filter(|categories| !categories.is_empty())
        .map(|categories| {
            if matches!(error.code, PlanResolutionCode::ModelUnavailable) {
                format!(
                    " Valid category names: {}. Retry one of these, or configure categories.<name>.models in omo.json — model overrides cannot be combined with category.",
                    categories.join(", ")
                )
            } else {
                format!(" Available categories: {}.", categories.join(", "))
            }
        })
        .unwrap_or_default();
    format!("{agent_suffix}{category_suffix}")
}

/// An owned view of the started task, shared with the progress listener and timer threads.
struct StartedSnapshot {
    task_id: String,
    status: String,
    name: String,
    queue_position: Option<u64>,
    resolved_model: Option<ResolvedModelRecord>,
}

impl StartedSnapshot {
    fn of(started: &StartedTask) -> Self {
        Self {
            task_id: started.task_id().to_string(),
            status: started.status().to_string(),
            name: started.name().to_string(),
            queue_position: started.queue_position(),
            resolved_model: started.resolved_model().cloned(),
        }
    }
}

impl StartedView for StartedSnapshot {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn status(&self) -> &str {
        &self.status
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn queue_position(&self) -> Option<u64> {
        self.queue_position
    }

    fn resolved_model(&self) -> Option<&ResolvedModelRecord> {
        self.resolved_model.as_ref()
    }
}

type Clock = fn() -> u64;

#[derive(Default)]
struct ThrottleState {
    emitted_at: u64,
    received_child_event: bool,
    closed: bool,
    /// The armed timer's id; `None` when no trailing emit is pending.
    timer: Option<u64>,
    next_timer: u64,
}

struct ProgressEmitter {
    progress: Mutex<ChildProgress<Clock>>,
    started: StartedSnapshot,
    params: DetailsParams,
    execution_mode: ExecutionMode,
    skills: Option<TaskSkillSummary>,
    on_update: Option<Arc<TaskToolUpdate>>,
    state: Mutex<ThrottleState>,
    wake: Condvar,
}

impl ProgressEmitter {
    fn send(&self, text: String, progress: ToolProgressDetails) {
        if let Some(on_update) = &self.on_update {
            on_update(TaskToolPartialResult {
                content: vec![text_block(text)],
                details: partial_details(
                    &self.started,
                    &self.params,
                    self.execution_mode,
                    progress,
                    self.skills.clone(),
                ),
            });
        }
    }

    fn emit(&self) {
        if self.on_update.is_none() {
            return;
        }
        {
            let mut state = lock(&self.state);
            if state.closed {
                return;
            }
            state.emitted_at = system_now_ms();
        }
        let (text, details) = {
            let progress = lock(&self.progress);
            (progress.content_text(), progress.details())
        };
        self.send(text, details);
    }

    fn schedule(self: &Arc<Self>) {
        if self.on_update.is_none() {
            return;
        }
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        if !state.received_child_event {
            state.received_child_event = true;
            drop(state);
            self.emit();
            return;
        }
        let elapsed = system_now_ms().saturating_sub(state.emitted_at);
        if elapsed >= PROGRESS_THROTTLE_MS {
            drop(state);
            self.emit();
        } else if state.timer.is_none() {
            let id = state.next_timer;
            state.next_timer += 1;
            state.timer = Some(id);
            drop(state);
            let delay = Duration::from_millis(PROGRESS_THROTTLE_MS - elapsed);
            let emitter = Arc::clone(self);
            std::thread::spawn(move || emitter.run_timer(id, delay));
        }
    }

    fn run_timer(&self, id: u64, delay: Duration) {
        let deadline = Instant::now() + delay;
        let mut state = lock(&self.state);
        loop {
            if state.closed || state.timer != Some(id) {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self
                .wake
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.timer = None;
        drop(state);
        self.emit();
    }

    /// `clearTimeout(timer); emit()` when a trailing emit is pending.
    fn flush_pending(&self) {
        let pending = lock(&self.state).timer.take().is_some();
        self.wake.notify_all();
        if pending {
            self.emit();
        }
    }

    fn close(&self) {
        {
            let mut state = lock(&self.state);
            state.closed = true;
            state.timer = None;
        }
        self.wake.notify_all();
    }

    /// The `{ progress, childId, turns: 0 }` details sent while the task waits for a slot.
    fn queued_details(&self, child_id: &str, started_at: u64) -> ToolProgressDetails {
        let activity = "queued · waiting for slot";
        let value = serde_json::json!({
            "progress": { "activity": activity, "startedAt": started_at },
            "childId": child_id,
            "turns": 0,
        });
        read_tool_progress_details(&value).unwrap_or_else(|| ToolProgressDetails {
            progress: ProgressActivity {
                activity: activity.to_string(),
                started_at: started_at as f64,
            },
            child_id: child_id.to_string(),
            turns: 0.0,
            ..lock(&self.progress).details()
        })
    }
}

pub fn run_spawn(
    deps: TaskExecuteDeps<'_>,
    input: RunSpawnInput<'_>,
) -> Result<TaskToolResult, WaitError> {
    let RunSpawnInput {
        params,
        signal,
        on_update,
        ctx,
        options,
    } = input;
    if signal.is_some_and(AbortSignal::aborted) {
        return Ok(status_result("cancelled", "Parent aborted before spawn"));
    }
    let selection = validate_task_target(TargetInput {
        category: params.category.as_deref(),
        subagent_type: params.subagent_type.as_deref(),
        model: params.model.as_deref(),
    });
    let target = match selection {
        TaskTargetSelection::Error(error) => {
            return Ok(status_result("invalid_arguments", &error.message));
        }
        TaskTargetSelection::Category(category) => SpawnTarget::Category(category),
        TaskTargetSelection::SubagentType(agent) => SpawnTarget::SubagentType(agent),
    };
    let policy = match &target {
        SpawnTarget::SubagentType(agent) => Some(evaluate_spawn_policy(
            deps.policy,
            agent,
            &params.prompt,
            &ctx.session_id,
        )),
        SpawnTarget::Category(_) => None,
    };
    let effective_params = match policy {
        Some(SpawnPolicyVerdict::Deny { message }) => {
            return Ok(status_result("denied", &message));
        }
        Some(SpawnPolicyVerdict::Force { prompt }) => SingleSpawnParams {
            prompt,
            load_skills: Some(Vec::new()),
            ..params.clone()
        },
        Some(SpawnPolicyVerdict::Allow) | None => params.clone(),
    };
    let ResolvedManagerStartSpec {
        spec,
        execution_mode,
        skills,
    } = build_start_spec(
        &effective_params,
        &target,
        &ctx.session_id,
        deps.tool,
        &ctx.cwd,
    );
    let started = match deps.manager.start(&spec) {
        StartResult::Started(started) => started,
        StartResult::PlanUnresolved(error) => {
            return Ok(result(
                format!("{}{}", error.message, plan_suffixes(&error)),
                TaskToolDetails {
                    task_id: String::new(),
                    status: "plan_error".to_string(),
                    mode: TaskToolMode::Spawn,
                    reason: Some(error.message.clone()),
                    ..TaskToolDetails::default()
                },
            ));
        }
        StartResult::DepthDenied { reason, .. } => {
            return Ok(status_result("denied", &reason));
        }
        StartResult::StartFailed(failure) => {
            return Ok(result(
                append_missing_skills(&failure.error_message, &[skills.as_ref()]),
                TaskToolDetails {
                    task_id: failure.task_id.clone(),
                    status: "error".to_string(),
                    mode: TaskToolMode::Spawn,
                    name: Some(failure.name.clone()),
                    category: failure.category.clone(),
                    subagent_type: failure.subagent_type.clone(),
                    execution_mode: Some(failure.execution_mode.as_str().to_string()),
                    model: Some(failure.model.clone()),
                    resolved_model: failure.resolved_model.clone(),
                    run_in_background: Some(failure.run_in_background),
                    reason: Some(failure.error_message.clone()),
                    skills: skills.clone(),
                    ..TaskToolDetails::default()
                },
            ));
        }
        StartResult::ResidencyDenied { reason, .. } => {
            return Ok(status_result("residency_denied", &reason));
        }
    };

    let detail_params = details_params(&params);
    let labels = StartLabels {
        task_summary: params.task_summary.clone(),
        description: params.description.clone(),
    };
    if params.run_in_background == Some(true) {
        return Ok(result(
            append_missing_skills(
                &background_start_text(&started, &labels),
                &[skills.as_ref()],
            ),
            started_details(&started, &detail_params, execution_mode, skills.clone()),
        ));
    }

    let started_at = system_now_ms();
    let clock: Clock = system_now_ms;
    let progress = create_child_progress(
        started.task_id(),
        ChildProgressTarget {
            category: params.category.clone(),
            agent_type: params.subagent_type.clone(),
            resolved_model: started.resolved_model().cloned(),
            model: params.model.clone(),
            name: Some(started.name().to_string()),
            task_summary: params.task_summary.clone(),
            description: params.description.clone(),
        },
        started_at,
        clock,
    );
    let emitter = Arc::new(ProgressEmitter {
        progress: Mutex::new(progress),
        started: StartedSnapshot::of(&started),
        params: detail_params.clone(),
        execution_mode,
        skills: skills.clone(),
        on_update,
        state: Mutex::new(ThrottleState::default()),
        wake: Condvar::new(),
    });
    let listener_emitter = Arc::clone(&emitter);
    let listener: Box<dyn Fn(&ManagedChildEvent) + Send + Sync> =
        Box::new(move |event: &ManagedChildEvent| {
            let accepted = lock(&listener_emitter.progress).accept(event);
            if accepted {
                listener_emitter.schedule();
            }
        });
    let unsubscribe = deps
        .manager
        .subscribe_child(started.task_id(), listener.into());
    if started.status() == "pending" {
        if emitter.on_update.is_some() {
            let details = emitter.queued_details(started.task_id(), started_at);
            emitter.send(String::new(), details);
        }
    } else {
        emitter.emit();
    }

    let waited = wait_for_foreground_task(&ForegroundWaitInput {
        manager: deps.manager,
        task_id: started.task_id(),
        signal,
        ctx,
        options,
    });
    let outcome = match waited {
        Ok(ForegroundWaitResult::Promoted { budget_seconds }) => Ok(result(
            append_missing_skills(
                &background_conversion_text(&started, &labels, budget_seconds),
                &[skills.as_ref()],
            ),
            TaskToolDetails {
                run_in_background: Some(true),
                ..started_details(&started, &detail_params, execution_mode, skills.clone())
            },
        )),
        Ok(ForegroundWaitResult::Completed { record }) => {
            emitter.flush_pending();
            Ok(sync_result(&record, TaskToolMode::Spawn, skills.as_ref()))
        }
        Err(error) => {
            if signal.is_some_and(AbortSignal::aborted) {
                let reason = "parent turn aborted";
                let task_id = started.task_id();
                let _ = deps
                    .manager
                    .cancel_task(task_id, reason.into(), CancelOptions::default());
                Ok(result(
                    format!(
                        "Task {task_id} cancelled: {reason}.{}",
                        continuation_footer(task_id)
                    ),
                    TaskToolDetails {
                        status: "cancelled".to_string(),
                        reason: Some(reason.to_string()),
                        ..started_details(
                            &started,
                            &detail_params,
                            execution_mode,
                            skills.clone(),
                        )
                    },
                ))
            } else {
                Err(error)
            }
        }
    };
    emitter.close();
    unsubscribe();
    outcome
}
