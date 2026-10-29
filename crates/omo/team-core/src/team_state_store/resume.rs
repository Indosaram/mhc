//! `resumeAllTeams` and its helpers (creating/active/deleting resume, reservation reconciliation,
//! session liveness, worker status).

use std::collections::HashSet;
use std::fs;
use std::io;

use serde_json::{Value, json};

use crate::clock::now_ms;
use crate::config::TeamModeConfig;
use crate::error::{Result, TeamCoreError};
use crate::logger::log;
use crate::session_client::{
    SessionClientError, TeamSessionContext, get_messages_data, value_contains_message_id,
};
use crate::team_mailbox::ack::ack_messages;
use crate::team_mailbox::reservation::reclaim_stale_reservations;
use crate::team_registry::paths::{get_runtime_state_dir, resolve_base_dir};
use crate::types::{AgentType, MemberStatus, RuntimeState, RuntimeStateMember, RuntimeStatus};

use super::store::{list_active_teams, load_runtime_state, transition_runtime_state};

const CREATING_TIMEOUT_MS: i64 = 30 * 60 * 1000;
const STALE_RESERVATION_TTL_MS: i64 = 10 * 60 * 1000;

/// `ResumeReport`.
#[derive(Debug, Default)]
pub struct ResumeReport {
    pub resumed: usize,
    pub marked_failed: usize,
    pub marked_orphaned: usize,
    pub cleaned: usize,
    pub errors: Vec<TeamCoreError>,
}

// --- error normalization ------------------------------------------------------------------

fn extract_error_message(error: &Value) -> Option<String> {
    match error {
        Value::String(text) => Some(text.clone()),
        Value::Object(record) => record
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn is_not_found_parts(status: Option<i64>, message: Option<&str>) -> bool {
    if status == Some(404) {
        return true;
    }
    message.is_some_and(|message| {
        let lowered = message.to_lowercase();
        lowered.contains("not found") || lowered.contains("missing")
    })
}

fn is_session_not_found_value(error: &Value) -> bool {
    let status = error.get("status").and_then(Value::as_i64);
    is_not_found_parts(status, extract_error_message(error).as_deref())
}

fn value_to_error(error: &Value) -> TeamCoreError {
    TeamCoreError::message(match error {
        Value::String(text) => text.clone(),
        other => extract_error_message(other).unwrap_or_else(|| {
            if other.is_object() {
                "[object Object]".to_owned()
            } else {
                other.to_string()
            }
        }),
    })
}

fn client_error(error: SessionClientError) -> TeamCoreError {
    TeamCoreError::message(error.message.unwrap_or_default())
}

// --- session liveness ---------------------------------------------------------------------

struct WorkerLiveness {
    name: String,
    was_spawned: bool,
    still_alive: bool,
}

fn session_exists(ctx: &TeamSessionContext<'_>, session_id: &str) -> Result<bool> {
    match ctx.client.get(session_id) {
        Ok(response) => {
            if let Some(error) = response.error.as_ref().filter(|error| !error.is_null()) {
                if is_session_not_found_value(error) {
                    return Ok(false);
                }
                return Err(value_to_error(error));
            }
            Ok(response.data.as_ref().is_some_and(|data| !data.is_null()))
        }
        Err(error) if is_not_found_parts(error.status, error.message.as_deref()) => Ok(false),
        Err(error) => Err(client_error(error)),
    }
}

fn inspect_worker_members(
    ctx: &TeamSessionContext<'_>,
    state: &RuntimeState,
) -> Result<Vec<WorkerLiveness>> {
    state
        .members
        .iter()
        .filter(|member| member.agent_type != AgentType::Leader)
        .map(|member| {
            if member.status == MemberStatus::Errored {
                return Ok(WorkerLiveness {
                    name: member.name.clone(),
                    was_spawned: true,
                    still_alive: false,
                });
            }
            let Some(session_id) = &member.session_id else {
                return Ok(WorkerLiveness {
                    name: member.name.clone(),
                    was_spawned: false,
                    still_alive: true,
                });
            };
            Ok(WorkerLiveness {
                name: member.name.clone(),
                was_spawned: true,
                still_alive: session_exists(ctx, session_id)?,
            })
        })
        .collect()
}

fn mark_dead_workers_errored(mut state: RuntimeState, dead: &HashSet<String>) -> RuntimeState {
    for member in &mut state.members {
        if dead.contains(&member.name) {
            member.status = MemberStatus::Errored;
            member.session_id = None;
        }
    }
    state
}

// --- runtime cleanup ----------------------------------------------------------------------

fn remove_runtime_directory(team_run_id: &str, config: &TeamModeConfig) -> Result<bool> {
    let directory = get_runtime_state_dir(&resolve_base_dir(config), team_run_id)?;
    match fs::metadata(&directory) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    match fs::remove_dir_all(&directory) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(true),
    }
}

fn cleanup_member_worktrees(state: &RuntimeState) -> Result<()> {
    for member in &state.members {
        let Some(worktree_path) = member
            .worktree_path
            .as_deref()
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        let result = match fs::symlink_metadata(worktree_path) {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(worktree_path),
            Ok(_) => fs::remove_file(worktree_path),
            Err(error) => Err(error),
        };
        match result {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}

// --- reservation reconciliation -----------------------------------------------------------

fn find_accepted_reclaimed_message_ids(
    ctx: &TeamSessionContext<'_>,
    member: &RuntimeStateMember,
    message_ids: &[String],
) -> Vec<String> {
    let Some(session_id) = member.session_id.as_deref() else {
        return Vec::new();
    };
    if message_ids.is_empty() {
        return Vec::new();
    }
    match ctx.client.messages(session_id) {
        None => Vec::new(),
        Some(Ok(response)) => {
            let messages = get_messages_data(&response);
            message_ids
                .iter()
                .filter(|message_id| {
                    messages
                        .iter()
                        .any(|message| value_contains_message_id(message, message_id))
                })
                .cloned()
                .collect()
        }
        Some(Err(error)) => {
            log(
                "team mailbox reclaimed reservation history check failed",
                Some(json!({
                    "event": "team-mailbox-reclaim-history-check-failed",
                    "member": member.name,
                    "sessionID": session_id,
                    "error": error.message.unwrap_or_default(),
                })),
            );
            Vec::new()
        }
    }
}

fn reconcile_stale_reservations_for_member(
    ctx: &TeamSessionContext<'_>,
    team_run_id: &str,
    member: &RuntimeStateMember,
    config: &TeamModeConfig,
    stale_reservation_ttl_ms: i64,
) -> Result<()> {
    let reclaimed =
        reclaim_stale_reservations(team_run_id, &member.name, config, stale_reservation_ttl_ms)?;
    if reclaimed.is_empty() {
        return Ok(());
    }
    let accepted = find_accepted_reclaimed_message_ids(ctx, member, &reclaimed);
    if !accepted.is_empty() {
        ack_messages(team_run_id, &member.name, &accepted, config)?;
    }
    let reclaimed: HashSet<String> = reclaimed.into_iter().collect();
    transition_runtime_state(
        team_run_id,
        |mut state| {
            for current in &mut state.members {
                if current.name == member.name {
                    current
                        .pending_injected_message_ids
                        .retain(|id| !reclaimed.contains(id));
                }
            }
            state
        },
        config,
    )?;
    Ok(())
}

// --- per-status resume --------------------------------------------------------------------

enum ActiveResumeOutcome {
    Resumed,
    MarkedOrphaned,
}

fn set_status(
    team_run_id: &str,
    status: RuntimeStatus,
    config: &TeamModeConfig,
) -> Result<RuntimeState> {
    transition_runtime_state(
        team_run_id,
        |mut state| {
            state.status = status;
            state
        },
        config,
    )
}

fn resume_active_team(
    ctx: &TeamSessionContext<'_>,
    state: &RuntimeState,
    config: &TeamModeConfig,
    stale_reservation_ttl_ms: i64,
) -> Result<ActiveResumeOutcome> {
    let lead_alive = match state.lead_session_id.as_deref().filter(|id| !id.is_empty()) {
        Some(lead_session_id) => session_exists(ctx, lead_session_id)?,
        None => false,
    };
    if !lead_alive {
        set_status(&state.team_run_id, RuntimeStatus::Orphaned, config)?;
        return Ok(ActiveResumeOutcome::MarkedOrphaned);
    }
    for member in &state.members {
        if let Err(error) = reconcile_stale_reservations_for_member(
            ctx,
            &state.team_run_id,
            member,
            config,
            stale_reservation_ttl_ms,
        ) {
            log(
                "team mailbox reservation reclaim failed",
                Some(json!({
                    "event": "team-mailbox-reclaim-failed",
                    "teamRunId": state.team_run_id,
                    "member": member.name,
                    "error": error.to_string(),
                })),
            );
        }
    }
    let workers = inspect_worker_members(ctx, state)?;
    let dead: HashSet<String> = workers
        .iter()
        .filter(|worker| worker.was_spawned && !worker.still_alive)
        .map(|worker| worker.name.clone())
        .collect();
    let has_any_worker = !workers.is_empty();
    let has_alive_worker = workers.iter().any(|worker| worker.still_alive);
    if has_any_worker && !has_alive_worker {
        transition_runtime_state(
            &state.team_run_id,
            |current| {
                let mut next = mark_dead_workers_errored(current, &dead);
                next.status = RuntimeStatus::Orphaned;
                next
            },
            config,
        )?;
        return Ok(ActiveResumeOutcome::MarkedOrphaned);
    }
    if !dead.is_empty() {
        transition_runtime_state(
            &state.team_run_id,
            |current| mark_dead_workers_errored(current, &dead),
            config,
        )?;
    }
    Ok(ActiveResumeOutcome::Resumed)
}

fn resume_one(
    ctx: &TeamSessionContext<'_>,
    team_run_id: &str,
    now: i64,
    config: &TeamModeConfig,
    report: &mut ResumeReport,
) -> Result<()> {
    let state = load_runtime_state(team_run_id, config)?;
    match state.status {
        RuntimeStatus::Creating => {
            if now - state.created_at > CREATING_TIMEOUT_MS {
                set_status(&state.team_run_id, RuntimeStatus::Failed, config)?;
                cleanup_member_worktrees(&state)?;
                report.marked_failed += 1;
            }
        }
        RuntimeStatus::Active => {
            match resume_active_team(ctx, &state, config, STALE_RESERVATION_TTL_MS)? {
                ActiveResumeOutcome::MarkedOrphaned => report.marked_orphaned += 1,
                ActiveResumeOutcome::Resumed => report.resumed += 1,
            }
        }
        RuntimeStatus::Deleting => {
            cleanup_member_worktrees(&state)?;
            set_status(&state.team_run_id, RuntimeStatus::Deleted, config)?;
            if remove_runtime_directory(&state.team_run_id, config)? {
                report.cleaned += 1;
            }
        }
        RuntimeStatus::Deleted | RuntimeStatus::Failed => {
            if remove_runtime_directory(&state.team_run_id, config)? {
                report.cleaned += 1;
            }
        }
        RuntimeStatus::ShutdownRequested | RuntimeStatus::Orphaned => {}
    }
    Ok(())
}

/// `resumeAllTeams`: reconcile every listed team run after a restart.
pub fn resume_all_teams(
    ctx: &TeamSessionContext<'_>,
    config: &TeamModeConfig,
) -> Result<ResumeReport> {
    let mut report = ResumeReport::default();
    let now = now_ms();
    for team in list_active_teams(config)? {
        if let Err(error) = resume_one(ctx, &team.team_run_id, now, config, &mut report) {
            log(
                "team runtime resume failed",
                Some(json!({
                    "event": "team-runtime-resume-failed",
                    "teamRunId": team.team_run_id,
                    "teamName": team.team_name,
                    "status": team.status.as_str(),
                    "error": error.to_string(),
                })),
            );
            report.errors.push(error);
        }
    }
    Ok(report)
}
