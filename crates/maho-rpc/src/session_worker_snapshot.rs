//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-snapshot.ts`.
//!
//! The worker isolate's whole-state snapshot of its one session, built only when a message carries it.

use crate::connection_handler::build_rpc_session_state;
use crate::handoff_activity::is_handoff_busy;
use crate::session_path_key::canonical_session_path;
use crate::session_registry::RpcSessionEntry;
use crate::session_worker_protocol::WorkerSnapshot;
use maho_core::agent_session::AgentSession;
use maho_core::session_archived::is_session_archived;
use maho_core::session_settled::{SettledFields, read_session_settled, settled_fields};
use maho_core::session_write_reservation::live_session_write_paths;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Extended worker snapshot including `archived` and `settled` fields when serialized or inspected
/// alongside [`WorkerSnapshot`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullWorkerSnapshot {
    #[serde(flatten)]
    pub base: WorkerSnapshot,
    pub archived: bool,
    #[serde(flatten)]
    pub settled: SettledFields,
}

/// Builds the [`WorkerSnapshot`] from a live [`AgentSession`].
pub fn build_worker_snapshot(session: &AgentSession) -> WorkerSnapshot {
    let session_path = session
        .session_file()
        .map(|p| canonical_session_path(Path::new(&p)).display().to_string());
    let mut seen = HashSet::new();
    let mut live_session_paths = Vec::new();
    for raw in live_session_write_paths() {
        let canon = canonical_session_path(Path::new(&raw))
            .display()
            .to_string();
        if seen.insert(canon.clone()) {
            live_session_paths.push(canon);
        }
    }
    if let Some(ref path) = session_path
        && seen.insert(path.clone())
    {
        live_session_paths.push(path.clone());
    }
    let activity = session.activity_snapshot();
    WorkerSnapshot {
        state: build_rpc_session_state(session, None),
        session_path,
        live_session_paths,
        busy: session.is_session_busy(),
        handoff_busy: Some(is_handoff_busy(activity)),
        streaming: session.is_streaming(),
    }
}

/// Builds the full worker snapshot (including `archived` and `settled` fields) from a session.
pub fn build_full_worker_snapshot(session: &AgentSession) -> FullWorkerSnapshot {
    let base = build_worker_snapshot(session);
    let archived = session.with_session_manager(is_session_archived);
    let settled = session.with_session_manager(|m| settled_fields(&read_session_settled(m)));
    FullWorkerSnapshot {
        base,
        archived,
        settled,
    }
}

/// Builds the worker isolate's snapshot from an [`RpcSessionEntry`], failing if its runtime is not ready.
pub fn worker_snapshot(
    entry: Option<&RpcSessionEntry>,
) -> Result<FullWorkerSnapshot, &'static str> {
    let runtime = entry
        .and_then(|e| e.runtime.as_ref())
        .ok_or("Session runtime is not ready")?;
    Ok(build_full_worker_snapshot(runtime.session()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_snapshot_requires_ready_runtime() {
        let err = worker_snapshot(None).unwrap_err();
        assert_eq!(err, "Session runtime is not ready");
    }
}
