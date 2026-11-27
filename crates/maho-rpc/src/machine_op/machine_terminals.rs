//! Port of `packages/coding-agent/src/modes/rpc/machine-op/machine-terminals.ts`.
//!
//! The engine's ONE terminal manager for `machine_op` (`senpi#6752`), created on the first
//! `exec_command` and torn down with the host, so terminals outlive any client connection.

use crate::machine_op::live_handle::{After, LiveHandle, MachineLifecycle};
use maho_machine_ops::process::exec::curated_env;
use maho_machine_ops::process::in_process_holder::create_in_process_holder_link;
use maho_machine_ops::{HolderLink, HolderSpawnRequest, MachineOpError, OpErrorCode};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

/// Live terminals at once; a further `exec_command` fails until one ends.
pub const MACHINE_TERMINALS_MAX: usize = 256;
/// How long a kill waits for the process to end after signaling before answering.
pub const MACHINE_TERMINAL_KILL_WAIT_MS: u64 = 5_000;
const KILL_ESCALATE_MS: u64 = 1_000;
const FINISHED_KEPT: usize = 64;
const RETIRED_KEPT: usize = 4_096;
const PTY_COLS: u16 = 80;
const PTY_ROWS: u16 = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleRecord {
    pub sender: String,
    pub state: MachineLifecycle,
    pub exit_code: Option<i32>,
    pub started_ms: u64,
}

#[derive(Clone, Debug)]
pub struct SpawnAsk {
    pub sender: String,
    pub command: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_id_scope() -> String {
    let mut bytes = [0u8; 6];
    let _ = getrandom::fill(&mut bytes);
    hex::encode(bytes)
}

fn shell_and_args(command: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        let path = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_owned());
        (
            path,
            vec![
                "/d".to_owned(),
                "/s".to_owned(),
                "/c".to_owned(),
                command.to_owned(),
            ],
        )
    } else {
        let path = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".to_owned());
        (path, vec!["-c".to_owned(), command.to_owned()])
    }
}

struct TerminalMaps {
    live: HashMap<String, Arc<LiveHandle>>,
    finished: VecDeque<String>,
    retired: HashMap<String, HandleRecord>,
    retired_order: VecDeque<String>,
}

pub struct MachineTerminals {
    holder: Arc<dyn HolderLink>,
    id_scope: String,
    next_id: AtomicU64,
    maps: Mutex<TerminalMaps>,
    after: After,
}

impl MachineTerminals {
    pub fn new(after: After) -> Arc<Self> {
        Self::with_holder(after, create_in_process_holder_link())
    }

    pub fn with_holder(after: After, holder: Arc<dyn HolderLink>) -> Arc<Self> {
        Arc::new(Self {
            holder,
            id_scope: random_id_scope(),
            next_id: AtomicU64::new(1),
            maps: Mutex::new(TerminalMaps {
                live: HashMap::new(),
                finished: VecDeque::new(),
                retired: HashMap::new(),
                retired_order: VecDeque::new(),
            }),
            after,
        })
    }

    pub async fn spawn(self: &Arc<Self>, ask: SpawnAsk) -> Result<Arc<LiveHandle>, MachineOpError> {
        if self.running() >= MACHINE_TERMINALS_MAX {
            return Err(MachineOpError::new(
                OpErrorCode::LimitExceeded,
                format!("too many live terminals (limit {MACHINE_TERMINALS_MAX})"),
            ));
        }

        let seq = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("term-{}-{seq}", self.id_scope);
        let (shell_path, args) = shell_and_args(&ask.command);

        let mut env: BTreeMap<String, Option<String>> = BTreeMap::new();
        for (k, v) in curated_env() {
            env.insert(k, Some(v));
        }
        env.insert("TERM".to_owned(), Some("xterm-256color".to_owned()));
        for (k, v) in ask.env {
            env.insert(k, Some(v));
        }

        let started_ms = now_ms();
        let held = self
            .holder
            .spawn(HolderSpawnRequest {
                id: id.clone(),
                command: shell_path,
                args,
                cwd: Some(PathBuf::from(&ask.cwd)),
                env,
                cols: ask.cols.unwrap_or(PTY_COLS),
                rows: ask.rows.unwrap_or(PTY_ROWS),
            })
            .await?;

        let handle = LiveHandle::new(
            id.clone(),
            ask.sender,
            held,
            started_ms,
            Arc::clone(&self.after),
        );
        {
            let mut maps = self.maps.lock().unwrap_or_else(PoisonError::into_inner);
            maps.live.insert(id, Arc::clone(&handle));
        }

        let terminals = Arc::clone(self);
        let handle_for_retire = Arc::clone(&handle);
        tokio::spawn(async move {
            handle_for_retire.wait_ended().await;
            terminals.retire(&handle_for_retire);
        });

        Ok(handle)
    }

    pub fn get(&self, id: &str) -> Option<Arc<LiveHandle>> {
        self.maps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .get(id)
            .cloned()
    }

    pub fn record(&self, id: &str) -> Option<HandleRecord> {
        self.maps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retired
            .get(id)
            .cloned()
    }

    pub fn owner_of(&self, id: &str) -> Option<String> {
        let maps = self.maps.lock().unwrap_or_else(PoisonError::into_inner);
        maps.live
            .get(id)
            .map(|h| h.sender.clone())
            .or_else(|| maps.retired.get(id).map(|r| r.sender.clone()))
    }

    pub fn pid_of(&self, id: &str) -> Option<u32> {
        let h = self.get(id)?;
        if h.state().is_ended() {
            None
        } else {
            Some(h.pid)
        }
    }

    pub fn running(&self) -> usize {
        self.maps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .values()
            .filter(|h| !h.state().is_ended())
            .count()
    }

    pub async fn kill(&self, h: &Arc<LiveHandle>) {
        if h.exit().is_some() {
            return;
        }
        h.mark_killed();
        h.signal("SIGTERM");

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let cancel_wait = (self.after)(
            MACHINE_TERMINAL_KILL_WAIT_MS,
            Box::new(move || {
                let _ = done_tx.send(());
            }),
        );
        let h_for_kill = Arc::clone(h);
        let cancel_escalate = (self.after)(
            KILL_ESCALATE_MS,
            Box::new(move || {
                if h_for_kill.exit().is_none() {
                    h_for_kill.signal("SIGKILL");
                }
            }),
        );

        tokio::select! {
            _ = h.wait_ended() => {}
            _ = done_rx => {}
        }
        cancel_escalate();
        cancel_wait();
    }

    pub async fn dispose(&self) {
        let live_handles: Vec<Arc<LiveHandle>> = {
            let maps = self.maps.lock().unwrap_or_else(PoisonError::into_inner);
            maps.live.values().cloned().collect()
        };
        for h in &live_handles {
            if h.exit().is_none() {
                h.mark_killed();
            }
        }
        let _ = self.holder.close().await;
        for h in &live_handles {
            h.dispose();
        }
        let mut maps = self.maps.lock().unwrap_or_else(PoisonError::into_inner);
        maps.live.clear();
    }

    fn retire(&self, h: &Arc<LiveHandle>) {
        let mut maps = self.maps.lock().unwrap_or_else(PoisonError::into_inner);
        if !maps.live.contains_key(&h.id) {
            return;
        }
        maps.finished.push_back(h.id.clone());
        while maps.finished.len() > FINISHED_KEPT {
            let Some(old_id) = maps.finished.pop_front() else {
                break;
            };
            let Some(old) = maps.live.remove(&old_id) else {
                continue;
            };
            old.dispose();
            let exit_code = old.exit().and_then(|e| e.exit_code);
            let record = HandleRecord {
                sender: old.sender.clone(),
                state: old.state(),
                exit_code,
                started_ms: old.started_ms,
            };
            maps.retired.insert(old.id.clone(), record);
            maps.retired_order.push_back(old.id.clone());
            while maps.retired.len() > RETIRED_KEPT {
                let Some(evict_id) = maps.retired_order.pop_front() else {
                    break;
                };
                maps.retired.remove(&evict_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_op::live_handle::tokio_after;
    use maho_machine_ops::{HeldExit, HeldTerminal, OpFuture};
    use std::time::Duration;
    use tokio::sync::{broadcast, watch};

    struct StuckHeldTerminal {
        id: String,
        pid: u32,
        data_tx: broadcast::Sender<Vec<u8>>,
        exit_tx: watch::Sender<Option<HeldExit>>,
    }

    impl StuckHeldTerminal {
        fn new(id: &str, pid: u32) -> Arc<Self> {
            let (data_tx, _) = broadcast::channel(8);
            let (exit_tx, _) = watch::channel(None);
            Arc::new(Self {
                id: id.to_owned(),
                pid,
                data_tx,
                exit_tx,
            })
        }
    }

    impl HeldTerminal for StuckHeldTerminal {
        fn id(&self) -> &str {
            &self.id
        }
        fn pid(&self) -> u32 {
            self.pid
        }
        fn subscribe_data(&self) -> broadcast::Receiver<Vec<u8>> {
            self.data_tx.subscribe()
        }
        fn subscribe_exit(&self) -> watch::Receiver<Option<HeldExit>> {
            self.exit_tx.subscribe()
        }
        fn write(&self, _bytes: &[u8]) {}
        fn resize(&self, _cols: u16, _rows: u16) {}
        fn signal(&self, _signal: &str) {}
        fn screen(&self, _history: Option<usize>) -> OpFuture<'_, String> {
            Box::pin(async { Ok(String::new()) })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn kill_answers_after_bounded_wait_when_process_outlives_signal() {
        let terminals = MachineTerminals::new(tokio_after());
        let stuck = LiveHandle::new(
            "h-stuck".to_owned(),
            "dev-1".to_owned(),
            StuckHeldTerminal::new("h-stuck", 424242),
            100,
            tokio_after(),
        );

        let settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let settled_flag = Arc::clone(&settled);
        let t = Arc::clone(&terminals);
        let s = Arc::clone(&stuck);
        let task = tokio::spawn(async move {
            t.kill(&s).await;
            settled_flag.store(true, Ordering::SeqCst);
        });

        tokio::time::advance(Duration::from_millis(MACHINE_TERMINAL_KILL_WAIT_MS - 1)).await;
        tokio::task::yield_now().await;
        assert!(!settled.load(Ordering::SeqCst));

        tokio::time::advance(Duration::from_millis(1)).await;
        task.await.expect("kill joined");
        assert!(settled.load(Ordering::SeqCst));
        terminals.dispose().await;
    }
}
