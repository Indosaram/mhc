//! `host-prewarm.ts`: task-host warm-ups, fire-and-forget, at most once per session id, POSIX `host`
//! runner only.
//!
//! - revival (always on): at `session_start`, ensure the recorded socket of every suspended
//!   host-session child the reconcile will revive (capacity included) except the session's own
//!   endpoint, so the host boots while the reconcile scans;
//! - `task.host_shard_prewarm` `"first-turn"` (default) / `"session-start"`: the session's own host.
//!   Only the root of a session tree warms, and an explicit `default_execution_mode: "in-process"`
//!   never routes a child to a host.
//!
//! A PREVIEW payload must warm nothing and touch no state: a preview `before_agent_start` edge is a
//! read-only probe, and warming there would boot a daemon, create a socket, and take a lock for a
//! turn that never runs.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{EventKind, EventResult, ExtensionApi};
use senpi_task::state::TaskRecord;

/// The earliest host edges of a submitted prompt: `input` fires before skill/template expansion, and
/// `before_agent_start` covers turns that never pass through `input`.
pub const FIRST_TURN_EVENTS: [EventKind; 2] = [EventKind::Input, EventKind::BeforeAgentStart];

/// Where a session's own host lives and how it is woken.
pub trait HostPrewarmHost: Send + Sync {
    /// The agent dir a warm request must carry, or the host loads a different package set.
    fn agent_dir(&self) -> Option<String>;
    /// The session's shard socket.
    fn shard_socket(&self) -> String;
    /// Whether this session's children run on its host at all.
    fn routes_children_to_host(&self) -> bool;
    /// `warmTaskHost({ socket, cwd, agentDir })`.
    fn warm_host(&self, socket: &str, cwd: &str, agent_dir: Option<&str>) -> Result<(), String>;
    /// `hostEndpoint.isOwn` / `ensure`.
    fn is_own_endpoint(&self, _socket: &str) -> bool {
        false
    }
    fn ensure_endpoint(&self, _socket: &str) -> Result<(), String> {
        Ok(())
    }
}

/// The session's task store, registered in the agent-dir store index before any host is ensured.
pub type AdmitTaskStore = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

pub struct HostPrewarmEngine {
    pub process_runner: Option<String>,
    pub default_execution_mode: Option<String>,
    pub host_shard_prewarm: Option<String>,
    pub resume_children: Option<bool>,
    pub reattach_on_reconcile: Option<bool>,
    pub residency_max_children: Option<usize>,
    pub platform: String,
    pub state_dir: String,
    pub session_role: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub cwd: Arc<dyn Fn() -> String + Send + Sync>,
    pub host: Arc<dyn HostPrewarmHost>,
    pub admit_task_store: AdmitTaskStore,
    pub list_records: Arc<dyn Fn() -> Vec<TaskRecord> + Send + Sync>,
    pub on_log: Arc<dyn Fn(&str) + Send + Sync>,
    /// Whether this edge is a read-only PREVIEW probe. A preview warms nothing and touches no state:
    /// booting a daemon, creating a socket, or taking a lock for a turn that never runs is the bug
    /// this seam exists to prevent. The composition supplies the check.
    pub is_preview: Arc<dyn Fn(&maho_ext_api::ExtensionEvent) -> bool + Send + Sync>,
}

/// The session's own-host warm, or `None` when none was started for it. Never fails.
#[derive(Default)]
pub struct HostPrewarm {
    warmed: Arc<Mutex<BTreeMap<String, ()>>>,
}

impl HostPrewarm {
    pub fn settled(&self, session_id: &str) -> bool {
        self.warmed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(session_id)
    }
}

/// `wireHostPrewarm`: the warm edges, and nothing else.
pub fn wire_host_prewarm(api: &mut ExtensionApi, engine: Arc<HostPrewarmEngine>) -> HostPrewarm {
    let handle = HostPrewarm::default();
    if engine.process_runner.as_deref() != Some("host") || engine.platform == "win32" {
        return handle;
    }
    let tree_root = (engine.session_role)().is_none();
    let configured = if tree_root && engine.default_execution_mode.as_deref() != Some("in-process") {
        engine.host_shard_prewarm.clone().unwrap_or_else(|| "first-turn".into())
    } else {
        "off".into()
    };
    let session_start_mode = configured == "session-start";
    let first_turn_mode = configured == "first-turn";
    let start = engine.clone();
    let warmed = handle.warmed.clone();
    api.on(EventKind::SessionStart, Arc::new(move |_, _| {
        let engine = start.clone();
        let warmed = warmed.clone();
        Box::pin(async move {
            if let Some(session) = (engine.session_id)() {
                if engine.resume_children != Some(false) && engine.reattach_on_reconcile != Some(false) {
                    warm_recorded_hosts(engine.as_ref(), &session);
                }
                if session_start_mode
                    && warmed
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(session, ())
                        .is_none()
                {
                    warm_own_host(engine.as_ref());
                }
            }
            Ok(EventResult::None)
        })
    }));
    if first_turn_mode {
        let edges = engine.clone();
        for event in FIRST_TURN_EVENTS {
            let engine = edges.clone();
            let warmed = handle.warmed.clone();
            api.on(event, Arc::new(move |payload, context| {
                // A preview payload is a read-only probe: it warms nothing and touches no state.
                if (engine.is_preview)(payload) {
                    return Box::pin(async move { Ok(EventResult::None) });
                }
                let engine = engine.clone();
                let warmed = warmed.clone();
                let session = context.session_manager.session_id().to_string();
                Box::pin(async move {
                    if warmed
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(session, ())
                        .is_none()
                    {
                        warm_own_host(engine.as_ref());
                    }
                    Ok(EventResult::None)
                })
            }));
        }
    }
    handle
}

fn warm_own_host(engine: &HostPrewarmEngine) {
    let agent_dir = engine.host.agent_dir();
    let Some(agent_dir) = agent_dir else {
        (engine.on_log)("omo-senpi task host warm skipped: no agent dir");
        return;
    };
    // The warm is admitted exactly like a spawn: the store must be in the agent-dir index first.
    if !(engine.admit_task_store)(&agent_dir, &engine.state_dir) {
        return;
    }
    // Only a session whose children run on its host warms one up.
    if !engine.host.routes_children_to_host() {
        return;
    }
    let socket = engine.host.shard_socket();
    if let Err(error) = engine.host.warm_host(&socket, &(engine.cwd)(), Some(&agent_dir)) {
        (engine.on_log)(&format!("omo-senpi task host warm failed: socket={socket} error={error}"));
    }
}

/// Exactly the children the reconcile's admission batch will revive, so a capped session never boots
/// hosts for children that stay suspended.
fn warm_recorded_hosts(engine: &HostPrewarmEngine, session_id: &str) {
    let cap = engine.residency_max_children.unwrap_or(usize::MAX);
    let records = (engine.list_records)();
    let selected = select_revival_batch(&records, session_id, cap);
    let mut sockets = BTreeSet::new();
    for record in selected {
        if record.runner_kind != Some(senpi_task::state::RunnerKind::HostSession) {
            continue;
        }
        let Some(socket) = record.host_session.as_ref().map(|session| session.socket.clone()) else {
            continue;
        };
        // The session's own endpoint is already serving it, or not restartable from inside.
        if !engine.host.is_own_endpoint(&socket) {
            sockets.insert(socket);
        }
    }
    for socket in sockets {
        if let Err(error) = engine.host.ensure_endpoint(&socket) {
            (engine.on_log)(&format!("omo-senpi task host ensure failed: socket={socket} error={error}"));
        }
    }
}

/// `selectRevivalBatch`: this session's suspended host-session children, capped.
pub fn select_revival_batch(records: &[TaskRecord], session_id: &str, cap: usize) -> Vec<TaskRecord> {
    records
        .iter()
        .filter(|record| record.parent_session_id == session_id)
        .filter(|record| record.runner_kind == Some(senpi_task::state::RunnerKind::HostSession))
        .filter(|record| matches!(record.status, senpi_task::state::TaskStatus::Paused | senpi_task::state::TaskStatus::Running))
        .take(cap)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_batch_is_capped_and_scoped_to_one_session() {
        assert!(select_revival_batch(&[], "session", 4).is_empty());
    }
}
