//! Binds a team-core tasklist to a single senpi team run.

use serde_json::{Map, Value};
use team_core::team_tasklist::{
    TaskInput, TaskListFilter, can_claim, claim_task, create_task, get_task, list_tasks, update_task_status,
};
use team_core::types::{Task, TaskStatus};

use crate::team::runtime_config::TeamCoreConfig;

/// Binds a team-core tasklist to a single senpi team run: the run id plus the team-core config that
/// pins storage under the senpi state dir. Every orchestration call curries these two so callers
/// never re-thread them.
pub struct TeamTasklistContext {
    pub team_run_id: String,
    pub config: TeamCoreConfig,
}

#[derive(Debug, Clone)]
pub struct CreateTeamTaskInput {
    pub subject: String,
    pub description: String,
    pub status: TaskStatus,
    pub owner: Option<String>,
    pub active_form: Option<String>,
    pub blocks: Option<Vec<String>>,
    pub blocked_by: Option<Vec<String>>,
    pub metadata: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default)]
pub struct TeamTaskFilter {
    pub status: Option<TaskStatus>,
    pub owner: Option<String>,
}

pub fn create_team_task(ctx: &TeamTasklistContext, input: CreateTeamTaskInput) -> team_core::Result<Task> {
    create_task(
        &ctx.team_run_id,
        TaskInput {
            subject: input.subject,
            description: input.description,
            status: input.status,
            owner: input.owner,
            active_form: input.active_form,
            blocks: input.blocks.unwrap_or_default(),
            blocked_by: input.blocked_by.unwrap_or_default(),
            metadata: input.metadata,
            claimed_at: None,
        },
        &ctx.config,
    )
}

pub fn list_team_tasks(ctx: &TeamTasklistContext, filter: Option<&TeamTaskFilter>) -> team_core::Result<Vec<Task>> {
    let filter = filter.map(|filter| TaskListFilter {
        status: filter.status,
        owner: filter.owner.clone(),
    });
    list_tasks(&ctx.team_run_id, &ctx.config, filter.as_ref())
}

pub fn get_team_task(ctx: &TeamTasklistContext, task_id: &str) -> team_core::Result<Task> {
    get_task(&ctx.team_run_id, task_id, &ctx.config)
}

pub fn claim_team_task(ctx: &TeamTasklistContext, task_id: &str, member_name: &str) -> team_core::Result<Task> {
    claim_task(&ctx.team_run_id, task_id, member_name, &ctx.config)
}

pub fn update_team_task_status(
    ctx: &TeamTasklistContext,
    task_id: &str,
    status: TaskStatus,
    member_name: &str,
) -> team_core::Result<Task> {
    update_task_status(&ctx.team_run_id, task_id, status, member_name, &ctx.config)
}

/// Whether the task's `blockedBy` dependencies are all satisfied (completed or absent), so a member
/// may claim it now. Reads the task and the full run tasklist and applies team-core's `can_claim`.
pub fn can_claim_team_task(ctx: &TeamTasklistContext, task_id: &str) -> team_core::Result<bool> {
    let task = get_team_task(ctx, task_id)?;
    let all_tasks = list_team_tasks(ctx, None)?;
    Ok(can_claim(&task, &all_tasks))
}
