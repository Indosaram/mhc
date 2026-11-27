//! Port of `packages/coding-agent/src/modes/rpc/machine-op/handler.ts`.
//!
//! The host-level `machine_op` handler (`senpi#6752`): one per RPC host, created with it and
//! disposed at its shutdown. Owns the `machine-ops` instance (`<agentDir>/machine`), the
//! engine-owned terminals, the `op_id` answer cache, and each running op's abort signal.

use crate::delegation_wire::DelegationResponse;
use crate::machine_op::answer_cache::MachineOpAnswerCache;
use crate::machine_op::executor::{ExecutorDeps, run_machine_op};
use crate::machine_op::live_handle::{After, tokio_after};
use crate::machine_op::machine_terminals::MachineTerminals;
use crate::machine_op::terminal_ops::TerminalOps;
use crate::machine_op::wire::{
    Args, MachineOpCommand, MachineOpRequest, OpOutcome, machine_op_answer, machine_op_conflict,
    machine_op_malformed, machine_op_unavailable, non_empty_text, parse_machine_op,
    parse_owned_op_lookup,
};
use maho_core::engine_build_identity::ENGINE_VERSION;
use maho_machine_ops::{
    AbortSignal, DEFAULT_LIMITS, HeldTerminal, HolderLink, HolderSpawnRequest,
    LocalMachineOpsOptions, MachineOpError, MachineOps, OpErrorCode, OpFuture,
    create_local_machine_ops,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The relay host's read cap (`RELAYD_READ_MAX_BYTES`): the session blob cap minus a stored result's
/// own cost around the bytes, so a read the relay host can carry is one the engine answers.
const RELAY_HOST_READ_MAX_BYTES: usize = 16_772_963;

/// `machine-ops`'s own PTY spawn is not offered: terminals run on `MachineTerminals`.
struct NoPtyHolder;

impl HolderLink for NoPtyHolder {
    fn spawn(&self, _req: HolderSpawnRequest) -> OpFuture<'_, Arc<dyn HeldTerminal>> {
        Box::pin(async {
            Err(MachineOpError::new(
                OpErrorCode::InvalidArgument,
                "terminal spawn is not offered by this host",
            ))
        })
    }

    fn close(&self) -> OpFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct RunningEntry {
    signal: AbortSignal,
    sender: String,
}

struct RunningGuard<'a> {
    running: &'a Mutex<HashMap<String, RunningEntry>>,
    op_id: String,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.op_id);
    }
}

struct AbortTaskGuard(tokio::task::JoinHandle<()>);

impl Drop for AbortTaskGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct MachineOpHandler {
    state_dir: PathBuf,
    terminals: Arc<MachineTerminals>,
    terminal_ops: TerminalOps,
    cache: MachineOpAnswerCache<OpOutcome>,
    stopping: AbortSignal,
    running: Mutex<HashMap<String, RunningEntry>>,
    ops: tokio::sync::Mutex<Option<Arc<dyn MachineOps>>>,
    held: AtomicBool,
}

impl MachineOpHandler {
    pub fn new(agent_dir: impl Into<PathBuf>) -> Self {
        Self::with_after(agent_dir, tokio_after())
    }

    pub fn with_after(agent_dir: impl Into<PathBuf>, after: After) -> Self {
        let terminals = MachineTerminals::new(Arc::clone(&after));
        let terminal_ops = TerminalOps::new(Arc::clone(&terminals), after);
        Self {
            state_dir: agent_dir.into(),
            terminals,
            terminal_ops,
            cache: MachineOpAnswerCache::new(),
            stopping: AbortSignal::default(),
            running: Mutex::new(HashMap::new()),
            ops: tokio::sync::Mutex::new(None),
            held: AtomicBool::new(false),
        }
    }

    pub async fn answer(
        &self,
        command: MachineOpCommand,
        record: &Value,
        id: Option<&str>,
    ) -> DelegationResponse {
        let empty = Args::new();
        let fields = record.as_object().unwrap_or(&empty);
        match command {
            MachineOpCommand::MachineOp => {
                let request = match parse_machine_op(fields) {
                    Ok(req) => req,
                    Err(problem) => return machine_op_malformed(id, command, &problem),
                };
                if self.held.load(Ordering::Acquire) {
                    return machine_op_unavailable(
                        id,
                        "the engine is handing over or shutting down; retry the op",
                    );
                }
                if let Some(owner) = self.cache.owner_of(&request.op_id)
                    && owner != request.sender
                {
                    return machine_op_conflict(id, command);
                }
                let op_id = request.op_id.clone();
                let sender = request.sender.clone();
                match self
                    .cache
                    .answer(&op_id, &sender, || self.run(request))
                    .await
                {
                    Ok(outcome) => {
                        let data = serde_json::to_value(outcome).unwrap_or(Value::Null);
                        machine_op_answer(id, command, data)
                    }
                    Err(detail) => {
                        // Review L3: the op never started (the machine state dir could not be made); nothing is cached.
                        machine_op_unavailable(
                            id,
                            &format!("the engine's machine state could not be prepared: {detail}"),
                        )
                    }
                }
            }
            MachineOpCommand::MachineOpCancel => {
                let lookup = match parse_owned_op_lookup(fields) {
                    Ok(lookup) => lookup,
                    Err(problem) => return machine_op_malformed(id, command, &problem),
                };
                let cancelled = {
                    let running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
                    match running.get(&lookup.op_id) {
                        Some(entry) if entry.sender != lookup.sender && !lookup.cross_owner => {
                            return machine_op_conflict(id, command);
                        }
                        Some(entry) => {
                            let was_active = !entry.signal.is_aborted();
                            entry.signal.abort();
                            was_active
                        }
                        None => false,
                    }
                };
                machine_op_answer(id, command, json!({ "cancelled": cancelled }))
            }
            MachineOpCommand::MachineOpResult => {
                let lookup = match parse_owned_op_lookup(fields) {
                    Ok(lookup) => lookup,
                    Err(problem) => return machine_op_malformed(id, command, &problem),
                };
                if let Some(owner) = self.cache.owner_of(&lookup.op_id)
                    && owner != lookup.sender
                    && !lookup.cross_owner
                {
                    return machine_op_conflict(id, command);
                }
                let data = match self.cache.settled(&lookup.op_id) {
                    None => json!({ "known": false }),
                    Some(outcome) => json!({ "known": true, "outcome": outcome }),
                };
                machine_op_answer(id, command, data)
            }
            MachineOpCommand::MachineOpOwner => {
                let Some(handle_id) = non_empty_text(fields, "handle_id") else {
                    return machine_op_malformed(
                        id,
                        command,
                        "handle_id must be a non-empty string",
                    );
                };
                let data = match self.terminals.owner_of(handle_id) {
                    None => json!({}),
                    Some(sender) => json!({ "sender": sender }),
                };
                machine_op_answer(id, command, data)
            }
            MachineOpCommand::MachineOpHealth => machine_op_answer(
                id,
                command,
                json!({
                    "ok": true,
                    "terminals": self.terminals.running(),
                    "cache": self.cache.len(),
                    "version": ENGINE_VERSION,
                }),
            ),
        }
    }

    /// Review M3: an op in flight or a live terminal keeps the host from being idle.
    pub fn busy(&self) -> bool {
        let running = !self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty();
        running || self.terminals.running() > 0
    }

    /// The host is ending (a handover switching, an empty exit): from now on a new `machine_op` is
    /// refused `machine_op_unavailable` and runs nothing.
    pub fn hold(&self) {
        self.held.store(true, Ordering::Release);
    }

    /// A blocked handover: this host keeps serving machine ops.
    pub fn release(&self) {
        self.held.store(false, Ordering::Release);
    }

    pub async fn dispose(&self) {
        self.stopping.abort();
        self.terminals.dispose().await;
    }

    async fn run(&self, request: MachineOpRequest) -> Result<OpOutcome, String> {
        let cancel = AbortSignal::default();
        // Registered before the first await, so a cancel read right behind the op on the same connection finds it.
        {
            let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
            running.insert(
                request.op_id.clone(),
                RunningEntry {
                    signal: cancel.clone(),
                    sender: request.sender.clone(),
                },
            );
        }
        let _running_guard = RunningGuard {
            running: &self.running,
            op_id: request.op_id.clone(),
        };

        let combined = AbortSignal::default();
        let deadline_remaining_ms = request
            .deadline_ms
            .map(|deadline| deadline.saturating_sub(now_ms()));
        if self.stopping.is_aborted() || cancel.is_aborted() || deadline_remaining_ms == Some(0) {
            combined.abort();
        }
        let _abort_guard = {
            let stopping = self.stopping.clone();
            let cancel = cancel.clone();
            let combined = combined.clone();
            AbortTaskGuard(tokio::spawn(async move {
                let deadline_fut = async {
                    match deadline_remaining_ms {
                        Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::select! {
                    _ = stopping.cancelled() => combined.abort(),
                    _ = cancel.cancelled() => combined.abort(),
                    _ = deadline_fut => combined.abort(),
                }
            }))
        };

        let ops = self.machine_ops().await?;
        let deps = ExecutorDeps {
            ops: &ops,
            terminal: &self.terminal_ops,
            pid_of: &|handle_id| self.terminals.pid_of(handle_id),
            owner_of: &|handle_id| self.terminals.owner_of(handle_id),
        };
        Ok(run_machine_op(&request, &combined, &deps).await)
    }

    async fn machine_ops(&self) -> Result<Arc<dyn MachineOps>, String> {
        let mut guard = self.ops.lock().await;
        if let Some(ops) = guard.as_ref() {
            return Ok(Arc::clone(ops));
        }

        // A failed preparation is never kept: the next op tries again (review L3).
        let machine_dir = self.state_dir.join("machine");
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let dir_for_create = machine_dir.clone();
            tokio::task::spawn_blocking(move || {
                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(true).mode(0o700);
                builder.create(&dir_for_create)
            })
            .await
            .map_err(|err| err.to_string())?
            .map_err(|err| err.to_string())?;
        }
        #[cfg(not(unix))]
        {
            tokio::fs::create_dir_all(&machine_dir)
                .await
                .map_err(|err| err.to_string())?;
        }

        let mut limits = DEFAULT_LIMITS;
        limits.read_max_bytes = Some(RELAY_HOST_READ_MAX_BYTES);
        let ops = create_local_machine_ops(LocalMachineOpsOptions {
            state_dir: Some(self.state_dir.clone()),
            holder: Arc::new(NoPtyHolder),
            limits: Some(limits),
            kill_process_tree: None,
            resolve_fd: None,
        });
        *guard = Some(Arc::clone(&ops));
        Ok(ops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn answers_unavailable_when_machine_state_dir_cannot_be_made_then_recovers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent_dir = temp.path();
        let blocker = agent_dir.join("machine");
        std::fs::write(&blocker, "a file where the machine state dir goes").expect("write blocker");

        let handler = MachineOpHandler::new(agent_dir);
        let record = json!({
            "type": "machine_op",
            "op": "shell",
            "args": { "command": "printf ok" },
            "op_id": "op-l3",
            "scope": { "root": agent_dir, "confined": false },
            "sender": "device-l3",
        });

        let first = handler
            .answer(MachineOpCommand::MachineOp, &record, Some("a1"))
            .await
            .to_value();
        assert_eq!(first["id"], "a1");
        assert_eq!(first["type"], "response");
        assert_eq!(first["command"], "machine_op");
        assert_eq!(first["success"], false);
        assert_eq!(first["errorCode"], "machine_op_unavailable");

        std::fs::remove_file(&blocker).expect("remove blocker");
        let second = handler
            .answer(MachineOpCommand::MachineOp, &record, Some("a2"))
            .await
            .to_value();
        assert_eq!(second["success"], true);
        assert_eq!(second["data"]["state"], "applied");
        assert_eq!(second["data"]["response"]["stdout"], "ok");

        handler.dispose().await;
    }

    #[tokio::test]
    async fn refuses_new_ops_while_held_and_reports_health() {
        let temp = tempfile::tempdir().expect("tempdir");
        let handler = MachineOpHandler::new(temp.path());

        let health = handler
            .answer(MachineOpCommand::MachineOpHealth, &json!({}), Some("h1"))
            .await
            .to_value();
        assert_eq!(health["success"], true);
        assert_eq!(health["data"]["ok"], true);
        assert_eq!(health["data"]["terminals"], 0);
        assert_eq!(health["data"]["cache"], 0);
        assert_eq!(health["data"]["version"], ENGINE_VERSION);

        handler.hold();
        let record = json!({
            "type": "machine_op",
            "op": "shell",
            "args": { "command": "printf ok" },
            "op_id": "op-held",
            "scope": { "root": temp.path(), "confined": false },
            "sender": "device-1",
        });
        let refused_resp = handler
            .answer(MachineOpCommand::MachineOp, &record, Some("r1"))
            .await
            .to_value();
        assert_eq!(refused_resp["success"], false);
        assert_eq!(refused_resp["errorCode"], "machine_op_unavailable");

        handler.release();
        let ok_resp = handler
            .answer(MachineOpCommand::MachineOp, &record, Some("r2"))
            .await
            .to_value();
        assert_eq!(ok_resp["success"], true);
        assert_eq!(ok_resp["data"]["response"]["stdout"], "ok");

        handler.dispose().await;
    }
}
