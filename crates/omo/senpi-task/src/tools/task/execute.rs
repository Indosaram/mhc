//! `tools/task/execute.ts`: the task tool's execute entry point. Validates the batch shape and
//! targets, then dispatches a single spawn to `run_spawn` or a batch to `execute_batch`.
//! The TS `WeakMap` keyed by item identity becomes a map keyed by the item's address.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::manager::types::{PlanResolutionCode, PlanResolutionError, StartResult};
use crate::manager::{AbortSignal, WaitError};
use crate::tools::task::execute_batch::{
    ExecuteBatchInput, TaskToolResult, ToolTextContent, execute_batch,
};
use crate::tools::task::execute_single::{
    RunSpawnInput, TaskExecuteDeps, TaskToolUpdate, run_spawn,
};
use crate::tools::task::execute_spec::{
    ResolvedManagerStartSpec, SingleSpawnParams, build_start_spec, single_spawn_params,
};
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::spawn_policy::{SpawnPolicyVerdict, evaluate_spawn_policy};
use crate::tools::task::types::{
    ResolvedSpawnItem, SpawnTarget, TaskSkillSummary, TaskToolDetails, TaskToolMode,
};
use crate::tools::task::validation::{
    BatchShape, SpawnParamsInput, TargetInput, TaskTargetSelection, resolve_spawn_items,
    validate_batch_shape, validate_task_target,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn invalid_arguments(message: &str) -> TaskToolResult {
    TaskToolResult {
        content: vec![ToolTextContent {
            kind: "text".to_string(),
            text: message.to_string(),
        }],
        details: TaskToolDetails {
            task_id: String::new(),
            status: "invalid_arguments".to_string(),
            mode: TaskToolMode::Spawn,
            reason: Some(message.to_string()),
            ..TaskToolDetails::default()
        },
    }
}

/// Identity key for a resolved item (the TS `WeakMap` key).
fn item_key(item: &ResolvedSpawnItem) -> usize {
    std::ptr::from_ref(item).addr()
}

/// The built task tool `execute` function.
pub struct TaskExecute<'a> {
    deps: TaskExecuteDeps<'a>,
    options: ForegroundWaitOptions,
}

pub fn build_task_execute(
    deps: TaskExecuteDeps<'_>,
    options: ForegroundWaitOptions,
) -> TaskExecute<'_> {
    TaskExecute { deps, options }
}

impl TaskExecute<'_> {
    pub fn execute(
        &self,
        _tool_call_id: &str,
        params: &SpawnParamsInput,
        signal: Option<&AbortSignal>,
        on_update: Option<Arc<TaskToolUpdate>>,
        ctx: &TaskToolContext,
    ) -> Result<TaskToolResult, WaitError> {
        let shape = validate_batch_shape(params);
        if let BatchShape::Error(error) = &shape {
            return Ok(invalid_arguments(&error.message));
        }

        let items = match resolve_spawn_items(params) {
            Ok(items) => items,
            Err(error) => {
                if shape == BatchShape::Single && error.code() == "item_target" {
                    let target = validate_task_target(TargetInput {
                        category: params.category.as_deref(),
                        subagent_type: params.subagent_type.as_deref(),
                        model: params.model.as_deref(),
                    });
                    if let TaskTargetSelection::Error(target_error) = target {
                        return Ok(invalid_arguments(&target_error.message));
                    }
                }
                return Ok(invalid_arguments(error.message()));
            }
        };

        let Some(first) = items.first() else {
            return Ok(invalid_arguments("Provide at least one task item."));
        };
        // `apply`/`merge` are isolation-only: asking to merge a child that never ran in a clone is a
        // caller mistake, refused BEFORE any spawn so zero child sessions are created.
        for item in &items {
            let isolated = item
                .isolated
                .or_else(|| {
                    self.deps.tool
                        .omo_config
                        .task
                        .as_ref()
                        .and_then(|task| task.isolation.as_ref())
                        .and_then(|isolation| isolation.enabled)
                })
                .unwrap_or(false);
            if !isolated && (item.apply.is_some() || item.merge.is_some()) {
                return Ok(invalid_arguments(
                    "apply and merge require isolated: true or task.isolation.enabled.",
                ));
            }
        }

        if items.len() == 1 {
            return run_spawn(
                self.deps,
                RunSpawnInput {
                    params: single_spawn_params(first, params.run_in_background),
                    signal,
                    on_update,
                    ctx,
                    options: self.options.clone(),
                },
            );
        }

        let deps = self.deps;
        let parent_session_id = ctx.session_id.clone();
        let run_in_background = params.run_in_background;
        let skill_summaries: Mutex<HashMap<usize, TaskSkillSummary>> = Mutex::default();
        let start_item = |item: &ResolvedSpawnItem| -> Result<StartResult, String> {
            let mut item_params: SingleSpawnParams = single_spawn_params(item, run_in_background);
            if let SpawnTarget::SubagentType(agent) = &item.target {
                match evaluate_spawn_policy(
                    deps.policy,
                    agent,
                    &item_params.prompt,
                    &parent_session_id,
                ) {
                    SpawnPolicyVerdict::Deny { message } => {
                        return Ok(StartResult::PlanUnresolved(PlanResolutionError {
                            code: PlanResolutionCode::InvalidTarget,
                            message,
                            available_categories: None,
                            available_agents: None,
                            attempted_chain: None,
                            category: None,
                            missing_providers: None,
                        }));
                    }
                    SpawnPolicyVerdict::Force { prompt } => {
                        item_params = SingleSpawnParams {
                            prompt,
                            load_skills: Some(Vec::new()),
                            ..item_params
                        };
                    }
                    SpawnPolicyVerdict::Allow => {}
                }
            }
            let ResolvedManagerStartSpec { spec, skills, .. } = build_start_spec(
                &item_params,
                &item.target,
                &parent_session_id,
                deps.tool,
                &ctx.cwd,
            );
            if let Some(skills) = skills {
                lock(&skill_summaries).insert(item_key(item), skills);
            }
            Ok(deps.manager.start(&spec))
        };
        let skill_summary_for = |item: &ResolvedSpawnItem| -> Option<TaskSkillSummary> {
            lock(&skill_summaries).get(&item_key(item)).cloned()
        };
        let start_item_ref: &dyn Fn(&ResolvedSpawnItem) -> Result<StartResult, String> =
            &start_item;
        let skill_summary_ref: &dyn Fn(&ResolvedSpawnItem) -> Option<TaskSkillSummary> =
            &skill_summary_for;
        // SAFETY: `ExecuteBatchInput` types its callbacks with a `'static` trait-object bound, but
        // `execute_batch` runs synchronously and only borrows the input for the duration of the
        // call; both closures (and everything they capture) outlive that call, so extending the
        // trait-object lifetime here never lets them be invoked after their captures are dropped.
        // Only the lifetime changes; the fat-pointer layout (data + vtable) is identical.
        let (start_item_static, skill_summary_static) = unsafe {
            (
                std::mem::transmute::<
                    &dyn Fn(&ResolvedSpawnItem) -> Result<StartResult, String>,
                    &(dyn Fn(&ResolvedSpawnItem) -> Result<StartResult, String> + Send + Sync),
                >(start_item_ref),
                std::mem::transmute::<
                    &dyn Fn(&ResolvedSpawnItem) -> Option<TaskSkillSummary>,
                    &(dyn Fn(&ResolvedSpawnItem) -> Option<TaskSkillSummary> + Send + Sync),
                >(skill_summary_ref),
            )
        };
        Ok(execute_batch(&ExecuteBatchInput {
            manager: deps.manager,
            items: &items,
            signal,
            ctx,
            run_in_background: params.run_in_background == Some(true),
            start_item: start_item_static,
            skill_summary_for: Some(skill_summary_static),
            options: self.options.clone(),
        }))
    }
}
