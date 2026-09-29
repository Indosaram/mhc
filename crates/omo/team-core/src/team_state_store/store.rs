//! Runtime-state persistence and the team-run state machine.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::clock::{now_ms, system_time_ms};
use crate::config::TeamModeConfig;
use crate::error::{Result, TeamCoreError};
use crate::logger::log;
use crate::team_registry::paths::{get_runtime_state_dir, resolve_base_dir};
use crate::types::{
    ActiveTeamSummary, AgentType, RuntimeBounds, RuntimeState, RuntimeStateMember, RuntimeStatus,
    SpecSource, TeamSpec,
};

use super::locks::{LockOptions, atomic_write, with_lock};

const STATE_FILE_NAME: &str = "state.json";
pub const STALE_DELETING_TTL_MS: i64 = 60_000;

/// `ALLOWED_RUNTIME_TRANSITIONS` plus the same-status and `-> orphaned` rules.
#[must_use]
pub fn is_valid_transition(from: RuntimeStatus, to: RuntimeStatus) -> bool {
    use RuntimeStatus::{Active, Creating, Deleted, Deleting, Failed, ShutdownRequested};
    if from == to || to == RuntimeStatus::Orphaned {
        return true;
    }
    matches!(
        (from, to),
        (Creating, Active | Failed)
            | (Active, ShutdownRequested | Deleting)
            | (ShutdownRequested, Deleting)
            | (Deleting, Deleted)
    )
}

fn get_state_path(base_dir: &Path, team_run_id: &str) -> Result<PathBuf> {
    Ok(get_runtime_state_dir(base_dir, team_run_id)?.join(STATE_FILE_NAME))
}

fn remove_runtime_directory_best_effort(base_dir: &Path, team_run_id: &str, reason: &str) {
    let result =
        get_runtime_state_dir(base_dir, team_run_id).and_then(
            |directory| match fs::remove_dir_all(directory) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            },
        );
    if let Err(error) = result {
        log(
            "team runtime cleanup failed",
            Some(json!({
                "event": "team-runtime-cleanup-failed",
                "teamRunId": team_run_id,
                "reason": reason,
                "error": error.to_string(),
            })),
        );
    }
}

fn is_deleting_runtime_stale(base_dir: &Path, team_run_id: &str, now: i64) -> Result<bool> {
    match fs::metadata(get_state_path(base_dir, team_run_id)?) {
        Ok(metadata) => Ok(now - system_time_ms(metadata.modified()?) > STALE_DELETING_TTL_MS),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn serialize_runtime_state(runtime_state: &RuntimeState) -> Result<String> {
    let validated = runtime_state
        .clone()
        .validated()
        .map_err(|issues| TeamCoreError::message(issues.to_string()))?;
    let text = serde_json::to_string_pretty(&validated)
        .map_err(|error| TeamCoreError::message(error.to_string()))?;
    Ok(format!("{text}\n"))
}

fn strip_legacy_runtime_state_fields(mut raw_state: Value) -> Value {
    if let Some(members) = raw_state.get_mut("members").and_then(Value::as_array_mut) {
        for member in members {
            if let Some(member) = member.as_object_mut() {
                member.shift_remove("delegateTaskCallsUsed");
            }
        }
    }
    raw_state
}

fn invalid_runtime_state(team_run_id: &str, detail: &str) -> TeamCoreError {
    TeamCoreError::RuntimeState {
        message: format!("runtime state invalid for {team_run_id}: {detail}"),
        code: "invalid_runtime_state".to_owned(),
    }
}

fn validate_runtime_state(raw_state: Value, team_run_id: &str) -> Result<RuntimeState> {
    RuntimeState::safe_parse(&strip_legacy_runtime_state_fields(raw_state))
        .map_err(|issues| invalid_runtime_state(team_run_id, &issues.to_string()))
}

fn validate_typed(runtime_state: RuntimeState, team_run_id: &str) -> Result<RuntimeState> {
    runtime_state
        .validated()
        .map_err(|issues| invalid_runtime_state(team_run_id, &issues.to_string()))
}

/// `createRuntimeState`: a fresh `creating` run with one pending member per spec member.
pub fn create_runtime_state(
    spec: &TeamSpec,
    lead_session_id: Option<&str>,
    spec_source: SpecSource,
    config: &TeamModeConfig,
) -> Result<RuntimeState> {
    let base_dir = resolve_base_dir(config);
    let team_run_id = uuid::Uuid::new_v4().to_string();
    let runtime_directory = get_runtime_state_dir(&base_dir, &team_run_id)?;
    let members = spec
        .members
        .iter()
        .map(|member| {
            let agent_type = if spec.lead_agent_id == member.name {
                AgentType::Leader
            } else {
                AgentType::GeneralPurpose
            };
            RuntimeStateMember {
                color: member.color.clone(),
                worktree_path: member.worktree_path.clone(),
                ..RuntimeStateMember::new(&member.name, agent_type)
            }
        })
        .collect();
    let runtime_state = validate_typed(
        RuntimeState {
            version: 1,
            team_run_id: team_run_id.clone(),
            team_name: spec.name.clone(),
            spec_source,
            created_at: now_ms(),
            status: RuntimeStatus::Creating,
            lead_session_id: lead_session_id.map(str::to_owned),
            tmux_layout: None,
            members,
            shutdown_requests: Vec::new(),
            bounds: RuntimeBounds {
                max_members: config.max_members,
                max_parallel_members: config.max_parallel_members,
                max_messages_per_run: config.max_messages_per_run,
                max_wall_clock_minutes: config.max_wall_clock_minutes,
                max_member_turns: config.max_member_turns,
            },
        },
        &team_run_id,
    )?;
    fs::create_dir_all(&runtime_directory)?;
    atomic_write(
        &get_state_path(&base_dir, &team_run_id)?,
        serialize_runtime_state(&runtime_state)?,
    )?;
    Ok(runtime_state)
}

/// `loadRuntimeState`. A missing file surfaces as `Io(NotFound)` (Node's `ENOENT`).
pub fn load_runtime_state(team_run_id: &str, config: &TeamModeConfig) -> Result<RuntimeState> {
    let base_dir = resolve_base_dir(config);
    let content = fs::read_to_string(get_state_path(&base_dir, team_run_id)?)?;
    let raw: Value = serde_json::from_str(&content)
        .map_err(|error| invalid_runtime_state(team_run_id, &error.to_string()))?;
    validate_runtime_state(raw, team_run_id)
}

/// `saveRuntimeState`.
pub fn save_runtime_state(runtime_state: &RuntimeState, config: &TeamModeConfig) -> Result<()> {
    let base_dir = resolve_base_dir(config);
    atomic_write(
        &get_state_path(&base_dir, &runtime_state.team_run_id)?,
        serialize_runtime_state(runtime_state)?,
    )?;
    Ok(())
}

/// `transitionRuntimeState`: under `state.lock`, apply `transition` and enforce the state machine.
pub fn transition_runtime_state(
    team_run_id: &str,
    transition: impl FnOnce(RuntimeState) -> RuntimeState,
    config: &TeamModeConfig,
) -> Result<RuntimeState> {
    let base_dir = resolve_base_dir(config);
    let runtime_directory = get_runtime_state_dir(&base_dir, team_run_id)?;
    with_lock(
        &runtime_directory.join("state.lock"),
        || {
            let current = load_runtime_state(team_run_id, config)?;
            let from = current.status;
            let next = validate_typed(transition(current), team_run_id)?;
            if !is_valid_transition(from, next.status) {
                return Err(TeamCoreError::InvalidTransition {
                    from,
                    to: next.status,
                });
            }
            save_runtime_state(&next, config)?;
            Ok(next)
        },
        Some(&LockOptions::owner("team-state-store")),
    )
}

/// `listActiveTeams`: prunes deleted/failed/stale-deleting runs, sorted by name then run id.
pub fn list_active_teams(config: &TeamModeConfig) -> Result<Vec<ActiveTeamSummary>> {
    let base_dir = resolve_base_dir(config);
    let now = now_ms();
    let entries = match fs::read_dir(base_dir.join("runtime")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut active_teams = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let team_run_id = entry.file_name().to_string_lossy().into_owned();
        let outcome = (|| -> Result<Option<ActiveTeamSummary>> {
            let runtime_state = load_runtime_state(&team_run_id, config)?;
            if matches!(
                runtime_state.status,
                RuntimeStatus::Deleted | RuntimeStatus::Failed
            ) {
                remove_runtime_directory_best_effort(
                    &base_dir,
                    &team_run_id,
                    runtime_state.status.as_str(),
                );
                return Ok(None);
            }
            if runtime_state.status == RuntimeStatus::Deleting
                && is_deleting_runtime_stale(&base_dir, &team_run_id, now)?
            {
                remove_runtime_directory_best_effort(&base_dir, &team_run_id, "stale_deleting");
                return Ok(None);
            }
            Ok(Some(ActiveTeamSummary {
                team_run_id: runtime_state.team_run_id,
                team_name: runtime_state.team_name,
                status: runtime_state.status,
                lead_session_id: runtime_state.lead_session_id,
                member_count: runtime_state.members.len(),
                scope: runtime_state.spec_source,
            }))
        })();
        match outcome {
            Ok(Some(summary)) => active_teams.push(summary),
            Ok(None) => {}
            Err(error) => log(
                "team runtime state skipped",
                Some(json!({
                    "event": "team-runtime-state-skipped",
                    "teamRunId": team_run_id,
                    "error": error.to_string(),
                })),
            ),
        }
    }
    active_teams.sort_by(|left, right| {
        left.team_name
            .cmp(&right.team_name)
            .then_with(|| left.team_run_id.cmp(&right.team_run_id))
    });
    Ok(active_teams)
}
