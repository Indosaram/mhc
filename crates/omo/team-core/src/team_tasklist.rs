//! Shared team tasklist: create, claim, update, list, get and dependency checks.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::clock::now_ms;
use crate::config::TeamModeConfig;
use crate::error::{Result, TeamCoreError};
use crate::logger::log;
use crate::team_registry::paths::{
    get_task_claim_lock_path, get_task_claims_dir, get_task_file_path, get_tasks_dir,
    resolve_base_dir,
};
use crate::team_state_store::locks::{
    LockOptions, atomic_write, detect_stale_lock, reap_stale_lock, with_lock,
};
use crate::types::{Task, TaskStatus};

const HIGH_WATERMARK_FILE: &str = ".highwatermark";
const CLAIM_STALE_AFTER_MS: i64 = 300_000;

/// `createTask`'s input: a task without `id`/`createdAt`/`updatedAt`/`version`.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskInput {
    pub subject: String,
    pub description: String,
    pub active_form: Option<String>,
    pub status: TaskStatus,
    pub owner: Option<String>,
    pub blocks: Vec<String>,
    pub blocked_by: Vec<String>,
    pub metadata: Option<Map<String, Value>>,
    pub claimed_at: Option<i64>,
}

impl Default for TaskInput {
    /// The `createTaskInput()` fixture defaults.
    fn default() -> Self {
        Self {
            subject: "task subject".to_owned(),
            description: "task description".to_owned(),
            active_form: None,
            status: TaskStatus::Pending,
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: None,
            claimed_at: None,
        }
    }
}

/// `listTasks` filter.
#[derive(Debug, Clone, Default)]
pub struct TaskListFilter {
    pub status: Option<TaskStatus>,
    pub owner: Option<String>,
}

fn create_private_dir_all(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

fn schema_error(issues: &crate::types::SchemaIssues) -> TeamCoreError {
    TeamCoreError::message(issues.to_string())
}

fn serialize_task(task: &Task) -> Result<String> {
    let text = serde_json::to_string_pretty(task)
        .map_err(|error| TeamCoreError::message(error.to_string()))?;
    Ok(format!("{text}\n"))
}

fn js_parse_int(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let (sign, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (-1, &trimmed[1..]),
        Some(b'+') => (1, &trimmed[1..]),
        _ => (1, trimmed),
    };
    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
    digits[..end].parse::<i64>().ok().map(|value| sign * value)
}

fn read_high_watermark(watermark_path: &Path) -> Result<i64> {
    match fs::read_to_string(watermark_path) {
        Ok(content) => Ok(js_parse_int(content.trim())
            .filter(|value| *value >= 0)
            .unwrap_or(0)),
        Err(_) => {
            atomic_write(watermark_path, "0")?;
            Ok(0)
        }
    }
}

/// `createTask`: allocate the next id from `.highwatermark` under the tasks lock.
pub fn create_task(team_run_id: &str, input: TaskInput, config: &TeamModeConfig) -> Result<Task> {
    let base_dir = resolve_base_dir(config);
    let tasks_dir = get_tasks_dir(&base_dir, team_run_id)?;
    create_private_dir_all(&tasks_dir)?;
    create_private_dir_all(&tasks_dir.join("claims"))?;
    with_lock(
        &tasks_dir.join(".lock"),
        || {
            let watermark_path = tasks_dir.join(HIGH_WATERMARK_FILE);
            let next_task_id = read_high_watermark(&watermark_path)? + 1;
            atomic_write(&watermark_path, next_task_id.to_string())?;
            let now = now_ms();
            let task = Task {
                version: 1,
                id: next_task_id.to_string(),
                subject: input.subject,
                description: input.description,
                active_form: input.active_form,
                status: input.status,
                owner: input.owner,
                blocks: input.blocks,
                blocked_by: input.blocked_by,
                metadata: input.metadata,
                created_at: now,
                updated_at: now,
                claimed_at: input.claimed_at,
            }
            .validated()
            .map_err(|issues| schema_error(&issues))?;
            atomic_write(
                &get_task_file_path(&base_dir, team_run_id, &task.id)?,
                serialize_task(&task)?,
            )?;
            Ok(task)
        },
        Some(&LockOptions::owner(format!("create-task:{team_run_id}"))),
    )
}

/// `getTask`. A missing task surfaces as `Io(NotFound)`.
pub fn get_task(team_run_id: &str, task_id: &str, config: &TeamModeConfig) -> Result<Task> {
    let content = fs::read_to_string(get_task_file_path(
        &resolve_base_dir(config),
        team_run_id,
        task_id,
    )?)?;
    let raw: Value = serde_json::from_str(&content)
        .map_err(|error| TeamCoreError::message(error.to_string()))?;
    Task::safe_parse(&raw).map_err(|issues| schema_error(&issues))
}

/// `listTasks`: valid tasks sorted numerically by id; malformed files are logged and skipped.
pub fn list_tasks(
    team_run_id: &str,
    config: &TeamModeConfig,
    filter: Option<&TaskListFilter>,
) -> Result<Vec<Task>> {
    let tasks_dir = get_tasks_dir(&resolve_base_dir(config), team_run_id)?;
    let Ok(entries) = fs::read_dir(&tasks_dir) else {
        return Ok(Vec::new());
    };
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_ok_and(|kind| kind.is_dir())
            || name.starts_with('.')
            || !name.ends_with(".json")
        {
            continue;
        }
        let task_path = tasks_dir.join(&name);
        let parsed = fs::read_to_string(&task_path)
            .map_err(|error| error.to_string())
            .and_then(|content| {
                serde_json::from_str::<Value>(&content).map_err(|error| error.to_string())
            });
        match parsed {
            Ok(raw) => match Task::safe_parse(&raw) {
                Ok(task) => tasks.push(task),
                Err(issues) => {
                    let issues: Vec<Value> = issues
                        .0
                        .iter()
                        .map(|issue| json!({ "path": issue.path, "message": issue.message }))
                        .collect();
                    log(
                        "team-tasklist skipped malformed task",
                        Some(
                            json!({ "event": "team-tasklist-malformed-task", "taskPath": task_path, "issues": issues }),
                        ),
                    );
                }
            },
            Err(error) => log(
                "team-tasklist skipped malformed task",
                Some(
                    json!({ "event": "team-tasklist-malformed-task", "taskPath": task_path, "error": error }),
                ),
            ),
        }
    }
    let mut tasks: Vec<Task> = tasks
        .into_iter()
        .filter(|task| {
            filter.is_none_or(|filter| {
                filter.status.is_none_or(|status| task.status == status)
                    && filter
                        .owner
                        .as_ref()
                        .is_none_or(|owner| task.owner.as_ref() == Some(owner))
            })
        })
        .collect();
    tasks.sort_by_key(|task| js_parse_int(&task.id).unwrap_or(0));
    Ok(tasks)
}

/// `canClaim`: every known blocker is completed (unknown blockers do not block).
#[must_use]
pub fn can_claim(task: &Task, all_tasks: &[Task]) -> bool {
    task.blocked_by.iter().all(|blocker_id| {
        all_tasks
            .iter()
            .find(|candidate| &candidate.id == blocker_id)
            .is_none_or(|blocker| blocker.status == TaskStatus::Completed)
    })
}

fn get_blocking_task_ids(task: &Task, all_tasks: &[Task]) -> Vec<String> {
    task.blocked_by
        .iter()
        .filter(|blocker_id| {
            all_tasks
                .iter()
                .find(|candidate| &candidate.id == *blocker_id)
                .is_some_and(|blocker| blocker.status != TaskStatus::Completed)
        })
        .cloned()
        .collect()
}

/// `claimTask`: claim a pending, unblocked task under its claim lock.
pub fn claim_task(
    team_run_id: &str,
    task_id: &str,
    member_name: &str,
    config: &TeamModeConfig,
) -> Result<Task> {
    let base_dir = resolve_base_dir(config);
    let claims_dir = get_task_claims_dir(&base_dir, team_run_id)?;
    let task_path = get_task_file_path(&base_dir, team_run_id, task_id)?;
    let claim_lock_path = get_task_claim_lock_path(&base_dir, team_run_id, task_id)?;
    create_private_dir_all(&claims_dir)?;

    let task = get_task(team_run_id, task_id, config)?;
    if task.status != TaskStatus::Pending {
        return Err(TeamCoreError::AlreadyClaimed);
    }
    let all_tasks = list_tasks(team_run_id, config, None)?;
    if !can_claim(&task, &all_tasks) {
        return Err(TeamCoreError::BlockedBy(get_blocking_task_ids(
            &task, &all_tasks,
        )));
    }
    if detect_stale_lock(&claim_lock_path, CLAIM_STALE_AFTER_MS) {
        reap_stale_lock(&claim_lock_path);
    } else if fs::symlink_metadata(&claim_lock_path).is_ok() {
        return Err(TeamCoreError::AlreadyClaimed);
    }

    with_lock(
        &claim_lock_path,
        || {
            let refreshed = get_task(team_run_id, task_id, config)?;
            if refreshed.status != TaskStatus::Pending {
                return Err(TeamCoreError::AlreadyClaimed);
            }
            let refreshed_tasks = list_tasks(team_run_id, config, None)?;
            if !can_claim(&refreshed, &refreshed_tasks) {
                return Err(TeamCoreError::BlockedBy(get_blocking_task_ids(
                    &refreshed,
                    &refreshed_tasks,
                )));
            }
            let now = now_ms();
            let updated = Task {
                status: TaskStatus::Claimed,
                owner: Some(member_name.to_owned()),
                claimed_at: Some(now),
                updated_at: now,
                ..refreshed
            }
            .validated()
            .map_err(|issues| schema_error(&issues))?;
            atomic_write(&task_path, serialize_task(&updated)?)?;
            Ok(updated)
        },
        Some(&LockOptions {
            owner_tag: Some(member_name.to_owned()),
            stale_after_ms: Some(CLAIM_STALE_AFTER_MS),
        }),
    )
}

fn is_valid_task_transition(current: TaskStatus, next: TaskStatus) -> bool {
    use TaskStatus::{Claimed, Completed, Deleted, InProgress, Pending};
    current == next
        || matches!(
            (current, next),
            (Pending, Claimed | Deleted)
                | (Claimed, InProgress | Deleted)
                | (InProgress, Completed | Deleted)
                | (Completed, Deleted)
        )
}

/// `updateTaskStatus`: forward-only transitions by the owner; `pending -> in_progress` claims first.
pub fn update_task_status(
    team_run_id: &str,
    task_id: &str,
    new_status: TaskStatus,
    member_name: &str,
    config: &TeamModeConfig,
) -> Result<Task> {
    let task = get_task(team_run_id, task_id, config)?;
    if task.status == new_status {
        return Ok(task);
    }
    if task.status == TaskStatus::Pending && new_status == TaskStatus::InProgress {
        claim_task(team_run_id, task_id, member_name, config)?;
        return update_task_status(team_run_id, task_id, new_status, member_name, config);
    }
    if !is_valid_task_transition(task.status, new_status) {
        return Err(TeamCoreError::InvalidTaskTransition {
            from: task.status,
            to: new_status,
        });
    }
    if new_status != TaskStatus::Deleted && task.owner.as_deref() != Some(member_name) {
        return Err(TeamCoreError::CrossOwnerUpdate);
    }
    let updated = Task {
        status: new_status,
        updated_at: now_ms(),
        ..task
    }
    .validated()
    .map_err(|issues| schema_error(&issues))?;
    atomic_write(
        &get_task_file_path(&resolve_base_dir(config), team_run_id, task_id)?,
        serialize_task(&updated)?,
    )?;
    Ok(updated)
}
