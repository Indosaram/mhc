//! Crash recovery for dangling mailbox delivery reservations.

use std::collections::BTreeMap;

use team_core::team_mailbox::reclaim_stale_reservations;

use crate::team::runtime_config::TeamCoreConfig;

pub type ReclaimResult = BTreeMap<String, Vec<String>>;

/// Reclaims delivery reservations left dangling by a crash mid-delivery, one member inbox at a time,
/// restoring any `.delivering-` file older than `stale_ttl_ms` back to unread so the next
/// poll/injection redelivers it. Called on component `session_start`; staleness is mtime-based
/// (team-core's own reclaim), and the caller decides WHICH members to sweep from OUR task-store
/// liveness, never from team-core's opencode session-liveness helper.
pub fn reclaim_stale_team_reservations<S: AsRef<str>>(
    team_run_id: &str,
    member_names: &[S],
    config: &TeamCoreConfig,
    stale_ttl_ms: i64,
) -> team_core::Result<ReclaimResult> {
    let mut result = ReclaimResult::new();
    for member_name in member_names {
        let member_name = member_name.as_ref();
        let reclaimed = reclaim_stale_reservations(team_run_id, member_name, config, stale_ttl_ms)?;
        result.insert(member_name.to_string(), reclaimed);
    }
    Ok(result)
}
