//! Senpi team runtime: create and delete team runs over the task manager.

use std::io;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use team_core::TeamCoreError;
use utils::logger::log;
use team_core::team_state_store::{create_runtime_state, load_runtime_state, transition_runtime_state};
use team_core::types::{RuntimeState, RuntimeStatus, TeamSpec};

use crate::lifecycle::port::DestroyCause;
use crate::team::member_map::{MemberTaskMap, read_member_task_map, write_member_task_map};
use crate::team::member_respawn::build_team_config_json;
use crate::team::registry::TeamSpecSource;
use crate::team::runtime_config::{TeamCoreConfig, to_team_core_config, to_team_core_spec_source};
use crate::team::runtime_types::{
    CreateTeamDeps, CreateTeamResult, CreatedMemberInfo, CreatedMemberRole, DeleteTeamDeps, DeleteTeamResult,
    SenpiTeamRuntimeError, SenpiTeamRuntimeErrorCode, SpawnMemberExtensionConfig, TeamCancelOutcome,
    TeamMemberReadPort, TeamNowFn,
};
use crate::team::spawn_members::{
    SpawnMembersInput, SpawnMembersResult, SpawnedMember, member_task_name, spawn_team_members,
};
use crate::team::storage::{TeamStorageError, ensure_team_runtime_dirs, resolve_team_runtime_dirs, team_storage_base_dir};

const MS_PER_MINUTE: i64 = 60_000;
const PROMPT_EXCERPT_MAX: usize = 120;

#[derive(Debug, thiserror::Error)]
pub enum TeamRuntimeError {
    #[error(transparent)]
    Runtime(#[from] SenpiTeamRuntimeError),
    #[error(transparent)]
    Core(#[from] TeamCoreError),
    #[error(transparent)]
    Storage(#[from] TeamStorageError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Destruction(String),
}

fn team_core_config(deps_state_dir: &crate::store::StateDirConfig, bounds: &crate::team::runtime_config::TeamTaskBounds) -> Result<TeamCoreConfig, TeamRuntimeError> {
    let base_dir = team_storage_base_dir(deps_state_dir).to_string_lossy().into_owned();
    to_team_core_config(bounds, &base_dir).map_err(TeamRuntimeError::Config)
}

/// Creates a team run over the task manager: enforce the `max_members` bound BEFORE any spawn, seed
/// team-core runtime state (`creating`), spawn members as background children under a wall-clock
/// deadline, then either roll back (cancel spawned members + transition to `failed`) on the first
/// failure or persist the member sidecar and transition to `active`. The current session is always
/// the lead sentinel; no member is ever elected lead.
pub fn create_team(
    spec: &TeamSpec,
    source: TeamSpecSource,
    deps: &CreateTeamDeps,
) -> Result<CreateTeamResult, TeamRuntimeError> {
    let max_members = deps.team_bounds.max_members;
    let member_count = spec.members.len();
    if member_count as u64 > max_members {
        return Err(SenpiTeamRuntimeError::new(
            format!(
                "team '{}' declares {member_count} members, exceeding max_members {max_members}",
                spec.name
            ),
            SenpiTeamRuntimeErrorCode::BoundsExceeded,
            spec.name.clone(),
        )
        .into());
    }

    let now: TeamNowFn = deps
        .now
        .clone()
        .unwrap_or_else(|| Arc::new(|| chrono::Utc::now().timestamp_millis()));
    let config = team_core_config(&deps.state_dir, &deps.team_bounds)?;
    let runtime_state = create_runtime_state(
        spec,
        Some(deps.lead_session_id.as_str()),
        to_team_core_spec_source(source),
        &config,
    )?;
    let team_run_id = runtime_state.team_run_id.clone();
    let member_names: Vec<String> = spec.members.iter().map(|member| member.name.clone()).collect();
    ensure_team_runtime_dirs(&deps.state_dir, &team_run_id, &member_names)?;

    let member_extension = deps.member_extension.as_ref().map(|extension| SpawnMemberExtensionConfig {
        entry_path: extension.entry_path.clone(),
        inherited_extensions: extension.inherited_extensions.clone(),
        team_config: build_team_config_json(&config, &deps.state_dir, &member_names),
    });
    let wall_clock_ms = i64::try_from(deps.team_bounds.max_wall_clock_minutes)
        .unwrap_or(i64::MAX / MS_PER_MINUTE)
        .saturating_mul(MS_PER_MINUTE);
    let now_fn = || now();
    let result = spawn_team_members(&SpawnMembersInput {
        spec,
        team_run_id: &team_run_id,
        manager: deps.manager.as_ref(),
        lead_session_id: &deps.lead_session_id,
        spawn_depth: deps.spawn_depth,
        max_parallel: deps.team_bounds.max_parallel_members,
        deadline_at: now().saturating_add(wall_clock_ms),
        now: &now_fn,
        member_extension,
    });

    if let Some(failure) = &result.failure {
        rollback_failed_create(&team_run_id, &result, deps, &config)?;
        return Err(failure.clone().into());
    }

    let member_task_ids = to_member_task_map(&result.spawned);
    // Persist the member sidecar AFTER spawn success but BEFORE the ->active transition: an active
    // team with no discoverable/cancellable members is a leak, so a write failure rolls the whole
    // create back (cancel spawned members + ->failed) instead of activating an orphaned run.
    let write_outcome = resolve_team_runtime_dirs(&deps.state_dir, &team_run_id)
        .map_err(|error| error.to_string())
        .and_then(|dirs| {
            match &deps.write_member_map {
                Some(write) => write(&dirs.runtime_dir, &member_task_ids),
                None => write_member_task_map(&dirs.runtime_dir, &member_task_ids),
            }
            .map_err(|error| error.to_string())
        });
    if let Err(message) = write_outcome {
        rollback_failed_create(&team_run_id, &result, deps, &config)?;
        return Err(SenpiTeamRuntimeError::new(
            format!("team '{}' member sidecar write failed: {message}", spec.name),
            SenpiTeamRuntimeErrorCode::SidecarWriteFailed,
            spec.name.clone(),
        )
        .into());
    }

    let activated = activate_team(&team_run_id, &result, &config)?;
    let members = to_created_member_infos(spec, &result, &activated);
    Ok(CreateTeamResult {
        runtime_state: activated,
        member_task_ids,
        members,
    })
}

/// Collapses whitespace runs to single spaces, trims, and caps at 120 characters plus `...`.
pub fn excerpt_prompt(prompt: &str) -> String {
    let collapsed = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= PROMPT_EXCERPT_MAX {
        collapsed
    } else {
        let head: String = collapsed.chars().take(PROMPT_EXCERPT_MAX).collect();
        format!("{head}...")
    }
}

// Builds the caller-facing per-member views from the spec (role, prompt), the spawn outcomes (task
// id, resolved model), and the activated runtime state (live status). Spawn success guarantees every
// spec member has an outcome; a member missing from the map is skipped defensively.
fn to_created_member_infos(spec: &TeamSpec, spawned: &SpawnMembersResult, state: &RuntimeState) -> Vec<CreatedMemberInfo> {
    spec.members
        .iter()
        .filter_map(|member| {
            let outcome = spawned.get(&member.name)?;
            let state_member = state.members.iter().find(|candidate| candidate.name == member.name);
            let role = if member.kind.as_str() == "category" {
                CreatedMemberRole::Category {
                    category: member.category.clone().unwrap_or_default(),
                }
            } else {
                CreatedMemberRole::SubagentType {
                    subagent_type: member.subagent_type.clone().unwrap_or_default(),
                }
            };
            Some(CreatedMemberInfo {
                name: member.name.clone(),
                task_id: outcome.task_id.clone(),
                status: state_member.map_or(outcome.status, |state_member| state_member.status),
                role,
                model: outcome.resolved_model.clone(),
                prompt_excerpt: member.prompt.as_deref().map(excerpt_prompt),
                task_summary: member.task_summary.clone(),
            })
        })
        .collect()
}

fn apply_spawn_outcome(state: &mut RuntimeState, spawned: &SpawnMembersResult) {
    for member in &mut state.members {
        let Some(outcome) = spawned.get(&member.name) else {
            continue;
        };
        member.status = outcome.status;
        if outcome.session_id.is_some() {
            member.session_id.clone_from(&outcome.session_id);
        }
    }
}

fn activate_team(
    team_run_id: &str,
    spawned: &SpawnMembersResult,
    config: &TeamCoreConfig,
) -> Result<RuntimeState, TeamRuntimeError> {
    transition_runtime_state(
        team_run_id,
        |state| {
            #[allow(clippy::redundant_clone)]
            let mut next = state.clone();
            apply_spawn_outcome(&mut next, spawned);
            next
        },
        config,
    )?;
    Ok(transition_runtime_state(
        team_run_id,
        |state| {
            #[allow(clippy::redundant_clone)]
            let mut next = state.clone();
            next.status = RuntimeStatus::Active;
            next
        },
        config,
    )?)
}

fn rollback_failed_create(
    team_run_id: &str,
    result: &SpawnMembersResult,
    deps: &CreateTeamDeps,
    config: &TeamCoreConfig,
) -> Result<(), TeamRuntimeError> {
    let reason = format!("team {team_run_id} create rollback");
    for (_, member) in &result.spawned {
        let outcome = deps.manager.cancel_task(&member.task_id, Some(&reason));
        if outcome.kind() != "cancelled" {
            log(
                "senpi-task team create rollback cancel skipped",
                Some(&json!({ "teamRunId": team_run_id, "taskId": member.task_id, "outcome": outcome.kind() })),
            );
        }
    }
    transition_runtime_state(
        team_run_id,
        |state| {
            #[allow(clippy::redundant_clone)]
            let mut next = state.clone();
            next.status = RuntimeStatus::Failed;
            next
        },
        config,
    )?;
    Ok(())
}

fn to_member_task_map(spawned: &[(String, SpawnedMember)]) -> MemberTaskMap {
    spawned
        .iter()
        .map(|(name, member)| (name.clone(), member.task_id.clone()))
        .collect()
}

/// Deletes a team run: transition `active`/`shutdown_requested` -> `deleting`, cancel every mapped
/// member task, transition -> `deleted`, then remove the team-core runtime directory. A missing
/// runtime state is treated as an already-deleted no-op (idempotent double delete).
///
/// The manager and destruction ports are synchronous and not `Send`, so a delete runs to completion
/// on the calling thread; there is no concurrent in-flight operation to coalesce onto.
pub fn delete_team(team_run_id: &str, deps: &DeleteTeamDeps) -> Result<DeleteTeamResult, TeamRuntimeError> {
    let config = team_core_config(&deps.state_dir, &deps.team_bounds)?;
    let runtime_dir = resolve_team_runtime_dirs(&deps.state_dir, team_run_id)?.runtime_dir;

    let Some(runtime_state) = load_runtime_state_or_none(team_run_id, &config)? else {
        return Ok(DeleteTeamResult {
            team_run_id: team_run_id.to_string(),
            cancelled_task_ids: Vec::new(),
        });
    };

    let status = runtime_state.status.as_str();
    if status == "active" || status == "shutdown_requested" {
        transition_runtime_state(
            team_run_id,
            |state| {
                #[allow(clippy::redundant_clone)]
                let mut next = state.clone();
                next.status = RuntimeStatus::Deleting;
                next
            },
            &config,
        )?;
    } else if status != "deleting" && status != "deleted" {
        return Err(SenpiTeamRuntimeError::new(
            format!("team '{team_run_id}' cannot be deleted from status '{status}'"),
            SenpiTeamRuntimeErrorCode::InvalidDeleteState,
            team_run_id,
        )
        .into());
    }

    let cancelled_task_ids = cancel_member_tasks(team_run_id, &runtime_dir, deps)?;

    if status != "deleted" {
        transition_runtime_state(
            team_run_id,
            |state| {
                #[allow(clippy::redundant_clone)]
                let mut next = state.clone();
                next.status = RuntimeStatus::Deleted;
                next
            },
            &config,
        )?;
    }
    match std::fs::remove_dir_all(&runtime_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(DeleteTeamResult {
        team_run_id: team_run_id.to_string(),
        cancelled_task_ids,
    })
}

fn cancel_member_tasks(
    team_run_id: &str,
    runtime_dir: &Path,
    deps: &DeleteTeamDeps,
) -> Result<Vec<String>, TeamRuntimeError> {
    let manager = deps.manager.as_ref();
    let map = read_member_task_map(runtime_dir);
    let reason = format!("delete team {team_run_id}");
    let mut cancelled = Vec::new();
    for (member_name, task_id) in &map {
        let expected_name = member_task_name(team_run_id, member_name);
        let record_name = TeamMemberReadPort::get(manager, task_id).and_then(|record| record.name);
        if record_name.as_deref() != Some(expected_name.as_str()) {
            continue;
        }
        let outcome = manager.cancel_task(task_id, Some(&reason));
        if let TeamCancelOutcome::Cancelled { .. } = outcome {
            cancelled.push(task_id.clone());
            continue;
        }
        // Terminal cancellation is an intentional noop (completed residents stay revivable), but team
        // deletion owns member teardown: route the resident through the lifecycle single-writer port.
        // A `cancelled` noop means an in-flight cancellation already owns destruction, and a
        // non-resident record has nothing left to tear down, so neither path may destroy again.
        // Re-read immediately before destruction so a revive between the noop and residency checks
        // wins; a narrower revive window remains and is serialized by lifecycle teardown.
        let observed_resident = TeamMemberReadPort::get(manager, task_id)
            .is_some_and(|record| record.residency_state.as_str() == "resident");
        let noop_not_cancelled = matches!(
            &outcome,
            TeamCancelOutcome::Noop { status, .. } if status.as_str() != "cancelled"
        );
        if noop_not_cancelled && observed_resident {
            let current_destroyable = TeamMemberReadPort::get(manager, task_id).is_some_and(|current| {
                let status = current.status.as_str();
                status != "pending" && status != "running" && current.residency_state.as_str() == "resident"
            });
            if current_destroyable {
                deps.destruction
                    .destroy_resident_task(task_id, DestroyCause::Cancel)
                    .map_err(TeamRuntimeError::Destruction)?;
            }
        }
    }
    Ok(cancelled)
}

fn load_runtime_state_or_none(
    team_run_id: &str,
    config: &TeamCoreConfig,
) -> Result<Option<RuntimeState>, TeamRuntimeError> {
    match load_runtime_state(team_run_id, config) {
        Ok(state) => Ok(Some(state)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}
