//! `tools/task/execute-batch.ts`: spawns a batch of tasks and renders the aggregate result.
//! `Promise.allSettled` over foreground waits becomes scoped threads joined in order.

use serde::{Deserialize, Serialize};

use crate::manager::types::{PlanResolutionCode, PlanResolutionError, StartResult, StartedTask};
use crate::manager::{AbortSignal, TaskManager, WaitError};
use crate::state::TaskRecord;
use crate::steering::CancelOptions;
use crate::tools::task::foreground_wait::{
    ForegroundWaitInput, ForegroundWaitOptions, ForegroundWaitResult, TaskToolContext,
    wait_for_foreground_task,
};
use crate::tools::task::params::MAX_TASK_BATCH_ITEMS;
use crate::tools::task::skill_result::append_missing_skills;
use crate::tools::task::start_presentation::{
    StartLabels, StartedView, background_conversion_text,
};
use crate::tools::task::types::{
    ResolvedSpawnItem, SpawnTarget, TaskSkillSummary, TaskToolDetails, TaskToolItemDetail,
    TaskToolMode,
};

/// One `{ type: "text", text }` content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolTextContent {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

/// `AgentToolResult<TaskToolDetails>`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskToolResult {
    pub content: Vec<ToolTextContent>,
    pub details: TaskToolDetails,
}

impl TaskToolResult {
    /// Concatenated text of all content blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Starts one item; `Err` models a thrown start error.
pub type StartItem = dyn Fn(&ResolvedSpawnItem) -> Result<StartResult, String> + Send + Sync;
pub type SkillSummaryFor =
    dyn Fn(&ResolvedSpawnItem) -> Option<TaskSkillSummary> + Send + Sync;

pub struct ExecuteBatchInput<'a> {
    pub manager: &'a TaskManager,
    pub items: &'a [ResolvedSpawnItem],
    pub signal: Option<&'a AbortSignal>,
    pub ctx: &'a TaskToolContext,
    pub run_in_background: bool,
    pub start_item: &'a StartItem,
    pub skill_summary_for: Option<&'a SkillSummaryFor>,
    pub options: ForegroundWaitOptions,
}

enum BatchStart<'a> {
    Started {
        item: &'a ResolvedSpawnItem,
        result: StartedTask,
        skills: Option<TaskSkillSummary>,
    },
    Failed {
        detail: TaskToolItemDetail,
        skills: Option<TaskSkillSummary>,
    },
}

impl BatchStart<'_> {
    fn skills(&self) -> Option<&TaskSkillSummary> {
        match self {
            BatchStart::Started { skills, .. } | BatchStart::Failed { skills, .. } => {
                skills.as_ref()
            }
        }
    }
}

struct BatchItemOutput {
    detail: TaskToolItemDetail,
    body: String,
    continuation: bool,
}

fn result(text: String, details: TaskToolDetails) -> TaskToolResult {
    TaskToolResult {
        content: vec![ToolTextContent {
            kind: "text".to_string(),
            text,
        }],
        details,
    }
}

fn continuation_footer(task_id: &str) -> String {
    format!("\n\n[task_id: {task_id} - continue with task_send(to=\"{task_id}\", message=\"...\")]")
}

fn target_fields(target: &SpawnTarget) -> (Option<String>, Option<String>) {
    match target {
        SpawnTarget::Category(category) => (Some(category.clone()), None),
        SpawnTarget::SubagentType(agent) => (None, Some(agent.clone())),
    }
}

fn failed_start_detail(
    item: &ResolvedSpawnItem,
    start: &StartResult,
    skills: Option<TaskSkillSummary>,
) -> TaskToolItemDetail {
    match start {
        StartResult::Started(started) => started_detail(item, started, skills.as_ref()),
        StartResult::PlanUnresolved(error) => item_error(
            item,
            "",
            &format!("{}{}", error.message, category_list_suffix(error)),
            skills,
        ),
        StartResult::DepthDenied { reason, .. } => item_error(item, "", reason, skills),
        StartResult::StartFailed(failure) => TaskToolItemDetail {
            task_id: failure.task_id.clone(),
            name: Some(failure.name.clone()),
            status: "error".to_string(),
            error_message: Some(failure.error_message.clone()),
            skills,
            ..Default::default()
        },
        StartResult::ResidencyDenied { reason, .. } => item_error(item, "", reason, skills),
    }
}

fn item_error(
    item: &ResolvedSpawnItem,
    task_id: &str,
    message: &str,
    skills: Option<TaskSkillSummary>,
) -> TaskToolItemDetail {
    TaskToolItemDetail {
        task_id: task_id.to_string(),
        name: item.name.clone(),
        status: "error".to_string(),
        error_message: Some(message.to_string()),
        skills,
        ..Default::default()
    }
}

fn started_detail(
    item: &ResolvedSpawnItem,
    start: &StartedTask,
    skills: Option<&TaskSkillSummary>,
) -> TaskToolItemDetail {
    let (category, subagent_type) = target_fields(&item.target);
    TaskToolItemDetail {
        task_id: start.task_id().to_string(),
        task_summary: item.task_summary.clone(),
        name: Some(start.name().to_string()),
        category,
        subagent_type,
        model: item.model.clone(),
        resolved_model: start.resolved_model().cloned(),
        status: start.status().to_string(),
        queue_position: start.queue_position(),
        skills: skills.cloned(),
        ..Default::default()
    }
}

fn start_all<'a>(input: &ExecuteBatchInput<'a>) -> Vec<BatchStart<'a>> {
    let mut starts = Vec::with_capacity(input.items.len());
    for item in input.items {
        match (input.start_item)(item) {
            Ok(start) => {
                let skills = input.skill_summary_for.and_then(|summary| summary(item));
                starts.push(match start {
                    StartResult::Started(result) => BatchStart::Started {
                        item,
                        result,
                        skills,
                    },
                    other => BatchStart::Failed {
                        detail: failed_start_detail(item, &other, skills.clone()),
                        skills,
                    },
                });
            }
            Err(message) => starts.push(BatchStart::Failed {
                detail: item_error(item, "", &message, None),
                skills: None,
            }),
        }
    }
    starts
}

fn category_list_suffix(error: &PlanResolutionError) -> String {
    let Some(available) = error
        .available_categories
        .as_ref()
        .filter(|available| !available.is_empty())
    else {
        return String::new();
    };
    // A model_unavailable failure means the category name IS valid; listing it under "Available
    // categories" told models to retry the same broken binding. Name the vocabulary honestly and
    // point at the omo.json config escape hatch.
    if matches!(error.code, PlanResolutionCode::ModelUnavailable) {
        return format!(
            " Valid category names: {}. Retry one of these, or configure categories.<name>.models in omo.json — model overrides cannot be combined with category.",
            available.join(", ")
        );
    }
    format!(" Available categories: {}.", available.join(", "))
}

fn oversized_batch_result() -> TaskToolResult {
    let reason = format!("tasks supports at most {MAX_TASK_BATCH_ITEMS} items.");
    result(
        reason.clone(),
        TaskToolDetails {
            task_id: String::new(),
            status: "invalid_arguments".to_string(),
            mode: TaskToolMode::Spawn,
            reason: Some(reason),
            ..Default::default()
        },
    )
}

fn join_batch(status: &str, lines: impl Iterator<Item = String>) -> String {
    std::iter::once(format!("Batch {status}."))
        .chain(lines)
        .collect::<Vec<_>>()
        .join("\n")
}

fn background_text(starts: &[BatchStart<'_>], status: &str) -> String {
    let lines = starts.iter().enumerate().map(|(index, start)| match start {
        BatchStart::Failed { detail, .. } => format!(
            "{}. {} (error): {}",
            index + 1,
            detail.name.as_deref().unwrap_or("task"),
            detail.error_message.as_deref().unwrap_or("start failed")
        ),
        BatchStart::Started { result, .. } => {
            let queue = result
                .queue_position()
                .map(|position| format!(" queue:{position}"))
                .unwrap_or_default();
            format!(
                "{}. {} {} ({}){}{}",
                index + 1,
                result.name(),
                result.task_id(),
                result.status(),
                queue,
                continuation_footer(result.task_id())
            )
        }
    });
    join_batch(status, lines)
}

fn live_starts<'s>(starts: &'s [BatchStart<'_>]) -> Vec<&'s StartedTask> {
    starts
        .iter()
        .filter_map(|start| match start {
            BatchStart::Started { result, .. } => Some(result),
            BatchStart::Failed { .. } => None,
        })
        .collect()
}

fn skill_summaries<'s>(starts: &'s [BatchStart<'_>]) -> Vec<Option<&'s TaskSkillSummary>> {
    starts.iter().map(BatchStart::skills).collect()
}

fn background_result(starts: &[BatchStart<'_>]) -> TaskToolResult {
    let live = live_starts(starts);
    let status = if live.is_empty() { "error" } else { "running" };
    let task_id = live
        .first()
        .map(|start| start.task_id().to_string())
        .unwrap_or_default();
    let items = starts
        .iter()
        .map(|start| match start {
            BatchStart::Started {
                item,
                result,
                skills,
            } => started_detail(item, result, skills.as_ref()),
            BatchStart::Failed { detail, .. } => detail.clone(),
        })
        .collect();
    result(
        append_missing_skills(&background_text(starts, status), &skill_summaries(starts)),
        TaskToolDetails {
            task_id,
            status: status.to_string(),
            mode: TaskToolMode::Spawn,
            run_in_background: Some(true),
            items: Some(items),
            ..Default::default()
        },
    )
}

fn record_output(
    record: &TaskRecord,
    start: &StartedTask,
    skills: Option<&TaskSkillSummary>,
) -> BatchItemOutput {
    let status = record.status.as_str().to_string();
    let body = record
        .final_response
        .clone()
        .or_else(|| record.error_message.clone())
        .unwrap_or_else(|| format!("Task {status}"));
    BatchItemOutput {
        detail: TaskToolItemDetail {
            task_id: record.task_id.clone(),
            name: record
                .name
                .clone()
                .or_else(|| Some(start.name().to_string())),
            status,
            error_message: record.error_message.clone(),
            skills: skills.cloned(),
            ..Default::default()
        },
        body,
        continuation: true,
    }
}

fn promoted_output(
    item: &ResolvedSpawnItem,
    start: &StartedTask,
    skills: Option<&TaskSkillSummary>,
    budget_seconds: i64,
) -> BatchItemOutput {
    let labels = StartLabels {
        task_summary: item.task_summary.clone(),
        description: item.description.clone(),
    };
    BatchItemOutput {
        detail: TaskToolItemDetail {
            run_in_background: Some(true),
            ..started_detail(item, start, skills)
        },
        body: background_conversion_text(start, &labels, budget_seconds),
        continuation: false,
    }
}

fn rejected_output(
    start: &StartedTask,
    skills: Option<&TaskSkillSummary>,
    aborted: bool,
    reason: &str,
) -> BatchItemOutput {
    let message = if aborted {
        "parent turn aborted".to_string()
    } else {
        reason.to_string()
    };
    BatchItemOutput {
        detail: TaskToolItemDetail {
            task_id: start.task_id().to_string(),
            name: Some(start.name().to_string()),
            status: if aborted { "cancelled" } else { "error" }.to_string(),
            error_message: Some(message.clone()),
            skills: skills.cloned(),
            ..Default::default()
        },
        body: message,
        continuation: true,
    }
}

fn aggregate_status(items: &[TaskToolItemDetail], aborted: bool) -> &'static str {
    let any = |statuses: &[&str]| {
        items
            .iter()
            .any(|item| statuses.contains(&item.status.as_str()))
    };
    if any(&["running", "pending"]) {
        return "running";
    }
    if any(&["error", "lost"]) {
        return "error";
    }
    if aborted || any(&["cancelled", "interrupted"]) {
        return "cancelled";
    }
    "completed"
}

fn sync_text(status: &str, outputs: &[BatchItemOutput]) -> String {
    let lines = outputs.iter().enumerate().map(|(index, output)| {
        let detail = &output.detail;
        let fallback = if detail.task_id.is_empty() {
            "task"
        } else {
            detail.task_id.as_str()
        };
        let label = detail.name.as_deref().unwrap_or(fallback);
        let footer = if output.continuation {
            continuation_footer(&detail.task_id)
        } else {
            String::new()
        };
        format!(
            "{}. {} ({}): {}{}",
            index + 1,
            label,
            detail.status,
            output.body,
            footer
        )
    });
    join_batch(status, lines)
}

type Settled = Option<Result<ForegroundWaitResult, WaitError>>;

fn wait_all(input: &ExecuteBatchInput<'_>, live: &[&StartedTask]) -> Vec<Settled> {
    let manager = input.manager;
    let signal = input.signal;
    let ctx = input.ctx;
    let options = &input.options;
    std::thread::scope(|scope| {
        let handles: Vec<_> = live
            .iter()
            .map(|start| {
                let task_id = start.task_id();
                scope.spawn(move || {
                    wait_for_foreground_task(&ForegroundWaitInput {
                        manager,
                        task_id,
                        signal,
                        ctx,
                        options: options.clone(),
                    })
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().ok())
            .collect()
    })
}

fn sync_result(input: &ExecuteBatchInput<'_>, starts: &[BatchStart<'_>]) -> TaskToolResult {
    let live = live_starts(starts);
    let settled = wait_all(input, &live);
    let signal_aborted = input.signal.is_some_and(AbortSignal::aborted);
    let batch_aborted = signal_aborted && settled.iter().any(|entry| matches!(entry, Some(Err(_))));
    if batch_aborted {
        for (index, entry) in settled.iter().enumerate() {
            if !matches!(entry, Some(Err(_))) {
                continue;
            }
            if let Some(start) = live.get(index) {
                let _ = input.manager.cancel_task(
                    start.task_id(),
                    "parent turn aborted".into(),
                    CancelOptions::default(),
                );
            }
        }
    }

    let mut live_index = 0;
    let outputs: Vec<BatchItemOutput> = starts
        .iter()
        .map(|start| match start {
            BatchStart::Failed { detail, .. } => BatchItemOutput {
                detail: detail.clone(),
                body: detail
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "start failed".to_string()),
                continuation: false,
            },
            BatchStart::Started {
                item,
                result,
                skills,
            } => {
                let entry = settled.get(live_index);
                live_index += 1;
                match entry {
                    None | Some(None) => {
                        rejected_output(result, skills.as_ref(), false, "missing wait result")
                    }
                    Some(Some(Ok(ForegroundWaitResult::Completed { record }))) => {
                        record_output(record, result, skills.as_ref())
                    }
                    Some(Some(Ok(ForegroundWaitResult::Promoted { budget_seconds }))) => {
                        promoted_output(item, result, skills.as_ref(), *budget_seconds)
                    }
                    Some(Some(Err(error))) => rejected_output(
                        result,
                        skills.as_ref(),
                        signal_aborted,
                        &error.to_string(),
                    ),
                }
            }
        })
        .collect();
    let items: Vec<TaskToolItemDetail> =
        outputs.iter().map(|output| output.detail.clone()).collect();
    let status = aggregate_status(&items, batch_aborted);
    let task_id = live
        .first()
        .map(|start| start.task_id().to_string())
        .unwrap_or_default();
    let run_in_background = items
        .iter()
        .any(|item| item.run_in_background == Some(true));
    result(
        append_missing_skills(&sync_text(status, &outputs), &skill_summaries(starts)),
        TaskToolDetails {
            task_id,
            status: status.to_string(),
            mode: TaskToolMode::Spawn,
            run_in_background: Some(run_in_background),
            items: Some(items),
            ..Default::default()
        },
    )
}

pub fn execute_batch(input: &ExecuteBatchInput<'_>) -> TaskToolResult {
    if input.signal.is_some_and(AbortSignal::aborted) {
        let reason = "Parent aborted before spawn".to_string();
        return result(
            reason.clone(),
            TaskToolDetails {
                task_id: String::new(),
                status: "cancelled".to_string(),
                mode: TaskToolMode::Spawn,
                reason: Some(reason),
                ..Default::default()
            },
        );
    }
    if input.items.len() > MAX_TASK_BATCH_ITEMS {
        return oversized_batch_result();
    }
    let starts = start_all(input);
    if input.run_in_background {
        background_result(&starts)
    } else {
        sync_result(input, &starts)
    }
}
