use std::collections::HashSet;
use std::path::Path;

use super::record_store::TaskRecordStore;
use super::rollback_r0_contract::{
    R0_COST_STATUSES, R0_FAILURE_KINDS, R0_FAILURE_REASONS, R0_SUSPENSION_REASONS,
};
use super::types::{PersistedTaskEvent, StateDirConfig, StoreError, TaskRecordDiagnostic};
use crate::state::{CostReportStatus, TaskRecord};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSessionMigrationWarning {
    pub code: &'static str,
    pub task_ids: Vec<String>,
    pub message: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSessionMigrationPlan {
    pub store_dir: std::path::PathBuf,
    pub migrate: usize,
    pub skipped: usize,
    pub sockets: Vec<String>,
    pub warnings: Vec<HostSessionMigrationWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSessionMigrationResult {
    pub plan: HostSessionMigrationPlan,
    pub migrated: usize,
}

fn store_at(store_dir: &Path) -> TaskRecordStore {
    TaskRecordStore::new(&StateDirConfig {
        project_dir: store_dir.into(),
        task_state_dir: Some(store_dir.into()),
    })
}

fn unsupported(value: Option<&str>, accepted: &[&str]) -> bool {
    value.is_some_and(|value| !accepted.contains(&value))
}

fn cost_status(record: &TaskRecord) -> Option<&'static str> {
    record
        .run_stats
        .as_ref()?
        .cost_status
        .map(|status| match status {
            CostReportStatus::Reported => "reported",
            CostReportStatus::Partial => "partial",
            CostReportStatus::Invalid => "invalid",
            CostReportStatus::Unavailable => "unavailable",
        })
}

pub fn needs_migration(record: &TaskRecord, to: &str) -> bool {
    record
        .host_session
        .as_ref()
        .is_some_and(|host| host.socket != to)
        || record.suspension_reason.is_some_and(|reason| {
            let reason = match reason {
                crate::state::SuspensionReason::DaemonUnavailable => "daemon_unavailable",
                crate::state::SuspensionReason::HostDraining => "host_draining",
                _ => "unsupported",
            };
            !R0_SUSPENSION_REASONS.contains(&reason)
        })
        || unsupported(record.failure_reason.as_deref(), R0_FAILURE_REASONS)
        || unsupported(record.failure_kind.as_deref(), R0_FAILURE_KINDS)
        || unsupported(cost_status(record), R0_COST_STATUSES)
}

pub fn plan_host_session_socket_migration(
    store_dir: &Path,
    to: &str,
) -> Result<HostSessionMigrationPlan, StoreError> {
    let listing = store_at(store_dir).list()?;
    if listing
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic, TaskRecordDiagnostic::ParseError { .. }))
    {
        return Err(StoreError::Parse(format!(
            "rollback migration refused malformed records in {}",
            store_dir.display()
        )));
    }
    let candidates: Vec<_> = listing
        .records
        .iter()
        .filter(|record| needs_migration(record, to))
        .collect();
    let mut strict_task_ids: Vec<_> = listing
        .records
        .iter()
        .filter(|record| {
            record
                .fallback_closing_child
                .as_ref()
                .is_some_and(|child| child.requires_confirmation == Some(true))
        })
        .map(|record| record.task_id.clone())
        .collect();
    strict_task_ids.sort();
    let warnings = if strict_task_ids.is_empty() {
        Vec::new()
    } else {
        vec![HostSessionMigrationWarning {
            code: "strict_closure_not_retried",
            task_ids: strict_task_ids,
            message: "The old daemon session may still be running and will no longer be retried for close by the older binary. A newer binary can resume closing only if the obligation field survives older writes.",
        }]
    };
    let mut sockets: Vec<_> = candidates
        .iter()
        .filter_map(|record| record.host_session.as_ref())
        .filter(|host| host.socket != to)
        .map(|host| host.socket.clone())
        .collect();
    sockets.sort();
    sockets.dedup();
    Ok(HostSessionMigrationPlan {
        store_dir: store_dir.into(),
        migrate: candidates.len(),
        skipped: listing.records.len() - candidates.len(),
        sockets,
        warnings,
    })
}

pub fn migrate_host_session_sockets(
    store_dir: &Path,
    to: &str,
    dead_endpoints: &HashSet<String>,
    dry_run: bool,
) -> Result<HostSessionMigrationResult, StoreError> {
    let plan = plan_host_session_socket_migration(store_dir, to)?;
    for socket in &plan.sockets {
        if !dead_endpoints.contains(socket) {
            return Err(StoreError::Parse(format!(
                "rollback migration refused live or unverified endpoint {socket}"
            )));
        }
    }
    if dry_run {
        return Ok(HostSessionMigrationResult { plan, migrated: 0 });
    }
    for warning in &plan.warnings {
        utils::logger::log(
            warning.message,
            Some(&serde_json::json!({
                "level": "warn", "code": warning.code, "task_ids": warning.task_ids,
            })),
        );
    }
    let store = store_at(store_dir);
    let mut migrated = 0;
    for record in store.list()?.records {
        if !needs_migration(&record, to) {
            continue;
        }
        let next = store.mutate(&record.task_id, |current| {
            let mut next = current.clone();
            if !needs_migration(current, to) {
                return next;
            }
            if let Some(host) = next.host_session.as_mut() {
                host.socket = to.into();
            }
            if !matches!(
                next.suspension_reason,
                None | Some(
                    crate::state::SuspensionReason::DaemonUnavailable
                        | crate::state::SuspensionReason::HostDraining
                )
            ) {
                next.suspension_reason = None;
            }
            if unsupported(next.failure_reason.as_deref(), R0_FAILURE_REASONS) {
                next.failure_reason = None;
            }
            if unsupported(next.failure_kind.as_deref(), R0_FAILURE_KINDS) {
                next.failure_kind = (next.failure_kind.as_deref() == Some("suspended_unresumable"))
                    .then(|| "session_unavailable".into());
            }
            if unsupported(cost_status(&next), R0_COST_STATUSES)
                && let Some(stats) = next.run_stats.as_mut() {
                    stats.cost_status = Some(CostReportStatus::Reported);
                }
            next
        })?;
        if next.is_none() {
            continue;
        }
        if let Some(from) = record
            .host_session
            .as_ref()
            .map(|host| &host.socket)
            .filter(|from| from.as_str() != to)
        {
            store.append_event(
                &record.task_id,
                &PersistedTaskEvent {
                    event_type: "host_session_migrated".into(),
                    payload: serde_json::json!({ "from": from, "to": to, "reason": "rollback" }),
                },
            )?;
        }
        migrated += 1;
    }
    Ok(HostSessionMigrationResult { plan, migrated })
}
