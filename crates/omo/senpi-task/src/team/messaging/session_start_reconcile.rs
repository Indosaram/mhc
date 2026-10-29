//! Component `session_start` reclaim of dangling mailbox delivery reservations.

use serde_json::json;
use utils::logger::log;
use team_core::team_state_store::list_active_teams;

use crate::store::StateDirConfig;
use crate::team::member_map::read_member_task_map;
use crate::team::messaging::reclaim::reclaim_stale_team_reservations;
use crate::team::normalize::TEAM_LEAD_SENTINEL;
use crate::team::runtime_config::TeamCoreConfig;
use crate::team::storage::resolve_team_runtime_dirs;

/// Mirrors team-core's own resume default: a reservation older than ten minutes is treated as
/// abandoned by a crash mid-delivery.
pub const DEFAULT_STALE_RESERVATION_TTL_MS: i64 = 10 * 60 * 1000;

pub struct ReconcileTeamMailboxDeps {
    pub state_dir: StateDirConfig,
    pub config: TeamCoreConfig,
    pub stale_ttl_ms: Option<i64>,
    pub current_lead_session_id: Option<String>,
}

fn reclaim_one_team(
    deps: &ReconcileTeamMailboxDeps,
    team_run_id: &str,
    include_lead: bool,
    stale_ttl_ms: i64,
) -> team_core::Result<()> {
    let runtime_dir = resolve_team_runtime_dirs(&deps.state_dir, team_run_id)?.runtime_dir;
    let mut recipients: Vec<String> = read_member_task_map(&runtime_dir).into_keys().collect();
    if include_lead && !recipients.iter().any(|name| name == TEAM_LEAD_SENTINEL) {
        recipients.push(TEAM_LEAD_SENTINEL.to_string());
    }
    if recipients.is_empty() {
        return Ok(());
    }
    reclaim_stale_team_reservations(team_run_id, &recipients, &deps.config, stale_ttl_ms)?;
    Ok(())
}

/// Component `session_start` reclaim: for every active team run, restore any delivery reservation
/// left dangling by a crash mid-delivery back to unread so the on-revive injection fallback can
/// redeliver it. Member names come from OUR sidecar map, never team-core session-liveness.
/// Best-effort per team: a single team's read/reclaim failure is logged and skipped so a stale
/// corner can never abort the sweep or block session start.
pub fn reconcile_team_mailbox_on_session_start(deps: &ReconcileTeamMailboxDeps) {
    let stale_ttl_ms = deps.stale_ttl_ms.unwrap_or(DEFAULT_STALE_RESERVATION_TTL_MS);

    let teams = match list_active_teams(&deps.config) {
        Ok(teams) => teams,
        Err(error) => {
            log(
                "senpi-task team session-start reclaim skipped: listActiveTeams failed",
                Some(&json!({ "error": error.to_string() })),
            );
            return;
        }
    };

    for team in teams {
        let include_lead = team.lead_session_id == deps.current_lead_session_id;
        if let Err(error) = reclaim_one_team(deps, &team.team_run_id, include_lead, stale_ttl_ms) {
            log(
                "senpi-task team session-start reclaim skipped one team",
                Some(&json!({ "teamRunId": team.team_run_id, "error": error.to_string() })),
            );
        }
    }
}
