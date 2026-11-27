//! Port of `packages/machine-ops/src/process/handles.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::process::exec::curated_env;
use crate::process::holder_link::{
    HeldExit, HeldTerminal, HolderEnv, HolderLink, HolderSpawnRequest,
};
use crate::process::scopes::{RUNTIME_ENV, SCOPE_ENV, ScopeRegistry, new_scope_token};
use crate::types::{
    HandleInfo, HandleLifecycle, HandleStatus, Limits, OpFuture, OutputChunk, ProcessHandle,
    SpawnRequest, StdinEffect, WaitReason, WaitResult, WaitSpec,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use regex::Regex;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, watch};

const SEEN_MAX_CHARS: usize = 64 * 1024;
const EVENT_BROADCAST_CAPACITY: usize = 1024;
const OUTPUT_SUBSCRIBER_CAPACITY: usize = 64;

static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")
        .expect("valid ANSI regex")
});

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_handle_id() -> String {
    let mut buf = [0u8; 9];
    let _ = getrandom::fill(&mut buf);
    format!("h_{}", URL_SAFE_NO_PAD.encode(buf))
}

fn slice_last_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_owned();
    }
    s.chars().skip(count - max_chars).collect()
}

struct OutputState {
    chunks: VecDeque<OutputChunk>,
    retained: usize,
    omitted: usize,
    missed_input_chunks: u64,
    seq: u64,
}

pub struct LocalProcessHandle {
    pub id: String,
    pub pid: u32,
    pub scope_token: String,
    pub principal: String,
    pub command: String,
    held: Arc<dyn HeldTerminal>,
    scopes: Arc<ScopeRegistry>,
    retain_limit: usize,
    started_at_ms: u64,
    output: Mutex<OutputState>,
    exit_rx: watch::Receiver<Option<HeldExit>>,
    kill_requested: AtomicBool,
    chunk_tx: broadcast::Sender<OutputChunk>,
    write_lock: tokio::sync::Mutex<()>,
}

impl LocalProcessHandle {
    fn new(
        id: String,
        scope_token: String,
        principal: String,
        command: String,
        held: Arc<dyn HeldTerminal>,
        scopes: Arc<ScopeRegistry>,
        limits: Limits,
    ) -> Arc<Self> {
        let (chunk_tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        let pid = held.pid();
        let exit_rx = held.subscribe_exit();
        let mut data_rx = held.subscribe_data();
        let handle = Arc::new(Self {
            id,
            pid,
            scope_token,
            principal,
            command,
            held,
            scopes,
            retain_limit: limits.retained_output_bytes,
            started_at_ms: now_ms(),
            output: Mutex::new(OutputState {
                chunks: VecDeque::new(),
                retained: 0,
                omitted: 0,
                missed_input_chunks: 0,
                seq: 0,
            }),
            exit_rx,
            kill_requested: AtomicBool::new(false),
            chunk_tx,
            write_lock: tokio::sync::Mutex::new(()),
        });

        let weak_handle = Arc::downgrade(&handle);
        tokio::spawn(async move {
            loop {
                match data_rx.recv().await {
                    Ok(bytes) => {
                        let Some(h) = weak_handle.upgrade() else {
                            break;
                        };
                        h.append(bytes);
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        let Some(h) = weak_handle.upgrade() else {
                            break;
                        };
                        h.append_gap(count);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        handle
    }

    fn append(&self, bytes: Vec<u8>) {
        self.append_chunk(bytes, 0);
    }

    fn append_gap(&self, missed_chunks: u64) {
        self.append_chunk(Vec::new(), missed_chunks);
    }

    fn append_chunk(&self, bytes: Vec<u8>, missed_chunks: u64) {
        let chunk = {
            let mut state = self
                .output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.seq += 1;
            state.missed_input_chunks += missed_chunks;
            let len = bytes.len();
            let chunk = OutputChunk {
                seq: state.seq,
                bytes,
                missed_chunks,
            };
            state.chunks.push_back(chunk.clone());
            state.retained += len;
            while (state.retained > self.retain_limit
                || state.chunks.len() > self.retain_limit.max(1))
                && state.chunks.len() > 1
            {
                if let Some(dropped) = state.chunks.pop_front() {
                    state.retained -= dropped.bytes.len();
                    state.omitted += dropped.bytes.len();
                }
            }
            chunk
        };
        let _ = self.chunk_tx.send(chunk);
    }

    fn lifecycle(&self) -> HandleLifecycle {
        if self.exit_rx.borrow().is_none() {
            HandleLifecycle::Running
        } else if self.kill_requested.load(Ordering::Relaxed) {
            HandleLifecycle::Killed
        } else {
            HandleLifecycle::Exited
        }
    }

    fn info(&self) -> HandleInfo {
        HandleInfo {
            id: self.id.clone(),
            pid: self.pid,
            principal: self.principal.clone(),
            scope_token: self.scope_token.clone(),
            command: self.command.clone(),
            state: self.lifecycle(),
        }
    }
}

async fn send_output_chunk(
    tx: &mpsc::Sender<OutputChunk>,
    chunk: OutputChunk,
    last_sent_seq: &mut u64,
) -> bool {
    if chunk.seq <= *last_sent_seq {
        return true;
    }
    if chunk.seq > last_sent_seq.saturating_add(1) {
        let gap = OutputChunk {
            seq: chunk.seq - 1,
            bytes: Vec::new(),
            missed_chunks: chunk.seq - *last_sent_seq - 1,
        };
        if tx.send(gap).await.is_err() {
            return false;
        }
    }
    let seq = chunk.seq;
    if tx.send(chunk).await.is_err() {
        return false;
    }
    *last_sent_seq = seq;
    true
}

impl ProcessHandle for LocalProcessHandle {
    fn id(&self) -> &str {
        &self.id
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn scope_token(&self) -> &str {
        &self.scope_token
    }

    fn status(&self) -> HandleStatus {
        let state_guard = self
            .output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let exit = self.exit_rx.borrow().clone();
        HandleStatus {
            id: self.id.clone(),
            pid: self.pid,
            state: self.lifecycle(),
            exit_code: exit.as_ref().and_then(|e| e.exit_code),
            signal: exit.and_then(|e| e.signal),
            last_seq: state_guard.seq,
            retained_bytes: state_guard.retained,
            omitted_bytes: state_guard.omitted,
            missed_input_chunks: state_guard.missed_input_chunks,
            started_at_ms: self.started_at_ms,
        }
    }

    fn subscribe_output(&self, from_seq: u64) -> mpsc::Receiver<OutputChunk> {
        let (tx, rx) = mpsc::channel(OUTPUT_SUBSCRIBER_CAPACITY);
        let mut live_rx = self.chunk_tx.subscribe();
        let mut exit_rx = self.exit_rx.clone();

        let backlog = {
            let state = self
                .output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .chunks
                .iter()
                .filter(|c| c.seq >= from_seq)
                .cloned()
                .collect::<Vec<OutputChunk>>()
        };

        tokio::spawn(async move {
            let mut last_sent_seq = from_seq.saturating_sub(1);
            for item in backlog {
                if !send_output_chunk(&tx, item, &mut last_sent_seq).await {
                    return;
                }
            }
            loop {
                if exit_rx.borrow().is_some() {
                    // Drain any already-queued live chunks before closing.
                    loop {
                        match live_rx.try_recv() {
                            Ok(chunk) => {
                                if !send_output_chunk(&tx, chunk, &mut last_sent_seq).await {
                                    return;
                                }
                            }
                            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                            Err(_) => {
                                return;
                            }
                        }
                    }
                }
                tokio::select! {
                    biased;
                    chunk_res = live_rx.recv() => {
                        match chunk_res {
                            Ok(chunk) => {
                                if !send_output_chunk(&tx, chunk, &mut last_sent_seq).await {
                                    return;
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => return,
                        }
                    }
                    changed = exit_rx.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
            }
        });

        rx
    }

    fn screen(&self, history: Option<usize>) -> OpFuture<'_, String> {
        Box::pin(async move { self.held.screen(history).await })
    }

    fn write<'a>(
        &'a self,
        bytes: &'a [u8],
        expect_activity_ms: Option<u64>,
    ) -> OpFuture<'a, StdinEffect> {
        Box::pin(async move {
            let _guard = self.write_lock.lock().await;
            if self.exit_rx.borrow().is_some() {
                return Err(MachineOpError::new(
                    OpErrorCode::InvalidArgument,
                    format!("handle {} has exited", self.id),
                ));
            }
            let Some(expect_ms) = expect_activity_ms else {
                self.held.write(bytes);
                return Ok(StdinEffect {
                    accepted_bytes: bytes.len(),
                    observed: None,
                });
            };
            let mut live_rx = self.chunk_tx.subscribe();
            let mut exit_rx = self.exit_rx.clone();
            self.held.write(bytes);
            let deadline = tokio::time::sleep(Duration::from_millis(expect_ms));
            tokio::pin!(deadline);
            let observed = loop {
                tokio::select! {
                    biased;
                    recv = live_rx.recv() => {
                        match recv {
                            Ok(_) => break true,
                            Err(broadcast::error::RecvError::Lagged(_)) => break true,
                            Err(broadcast::error::RecvError::Closed) => break false,
                        }
                    }
                    _ = exit_rx.changed() => {
                        if exit_rx.borrow().is_some() {
                            break false;
                        }
                    }
                    () = &mut deadline => break false,
                }
            };
            Ok(StdinEffect {
                accepted_bytes: bytes.len(),
                observed: Some(observed),
            })
        })
    }

    fn wait(&self, spec: WaitSpec) -> OpFuture<'_, WaitResult> {
        Box::pin(async move {
            if self.exit_rx.borrow().is_some() {
                return Ok(WaitResult {
                    status: self.status(),
                    reason: WaitReason::Exited,
                });
            }
            let mut live_rx = self.chunk_tx.subscribe();
            let mut exit_rx = self.exit_rx.clone();
            let deadline = tokio::time::sleep(Duration::from_millis(spec.timeout_ms));
            tokio::pin!(deadline);
            let mut seen = String::new();
            let mut live_closed = false;

            let reason = loop {
                if self.exit_rx.borrow().is_some() {
                    break WaitReason::Exited;
                }
                tokio::select! {
                    biased;
                    chunk_res = live_rx.recv(), if !live_closed => {
                        match chunk_res {
                            Ok(chunk) => {
                                if let Some(target) = &spec.output {
                                    seen.push_str(&String::from_utf8_lossy(&chunk.bytes));
                                    seen = slice_last_chars(&seen, SEEN_MAX_CHARS);
                                    if seen.contains(target) {
                                        break WaitReason::Matched;
                                    }
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                live_closed = true;
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                        }
                    }
                    changed = exit_rx.changed() => {
                        if changed.is_err() || exit_rx.borrow().is_some() {
                            break WaitReason::Exited;
                        }
                    }
                    () = &mut deadline => break WaitReason::Timeout,
                }
            };

            Ok(WaitResult {
                status: self.status(),
                reason,
            })
        })
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.held.resize(cols, rows);
    }

    fn kill(&self, signal: Option<&str>) -> OpFuture<'_, ()> {
        let signal = signal.map(str::to_owned);
        Box::pin(async move {
            if self.exit_rx.borrow().is_some() {
                return Ok(());
            }
            self.kill_requested.store(true, Ordering::Relaxed);
            if let Some(sig) = signal {
                self.held.signal(&sig);
                return Ok(());
            }
            let mut exit_rx = self.exit_rx.clone();
            self.scopes
                .sweep(std::slice::from_ref(&self.scope_token))
                .await;
            if self.exit_rx.borrow().is_none() {
                self.held.signal("SIGKILL");
            }
            while exit_rx.borrow().is_none() {
                if exit_rx.changed().await.is_err() {
                    break;
                }
            }
            Ok(())
        })
    }
}

pub fn strip_ansi(text: &str) -> String {
    ANSI_RE.replace_all(text, "").into_owned()
}

/// Live and finished handles of one `MachineOps` instance.
pub struct HandleRegistry {
    state: Mutex<HandleRegistryState>,
    holder: Arc<dyn HolderLink>,
    scopes: Option<Arc<ScopeRegistry>>,
    limits: Limits,
}

#[derive(Default)]
struct HandleRegistryState {
    handles: HashMap<String, Arc<LocalProcessHandle>>,
    pending: usize,
}

struct SpawnReservation<'a> {
    state: &'a Mutex<HandleRegistryState>,
    active: bool,
}

impl SpawnReservation<'_> {
    fn insert(mut self, id: String, handle: Arc<LocalProcessHandle>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending -= 1;
        state.handles.insert(id, handle);
        self.active = false;
    }
}

impl Drop for SpawnReservation<'_> {
    fn drop(&mut self) {
        if self.active {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pending -= 1;
        }
    }
}

impl HandleRegistry {
    pub fn new(
        holder: Arc<dyn HolderLink>,
        scopes: Option<Arc<ScopeRegistry>>,
        limits: Limits,
    ) -> Self {
        Self {
            state: Mutex::new(HandleRegistryState::default()),
            holder,
            scopes,
            limits,
        }
    }

    pub async fn spawn(
        &self,
        req: SpawnRequest,
        principal: &str,
        limits: Limits,
    ) -> Result<Arc<dyn ProcessHandle>, MachineOpError> {
        let Some(scopes) = &self.scopes else {
            return Err(MachineOpError::new(
                OpErrorCode::InvalidArgument,
                "process.spawn needs a stateDir",
            ));
        };
        let cap = limits.max_pty_sessions.or(self.limits.max_pty_sessions);
        let reservation = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let live = state
                .handles
                .values()
                .filter(|h| matches!(h.lifecycle(), HandleLifecycle::Running))
                .count()
                + state.pending;
            if let Some(cap) = cap
                && live >= cap
            {
                return Err(MachineOpError::new(
                    OpErrorCode::LimitExceeded,
                    format!("{live} PTY sessions are live; the cap is {cap}"),
                ));
            }
            state.pending += 1;
            SpawnReservation {
                state: &self.state,
                active: true,
            }
        };

        let id = new_handle_id();
        let scope_token = new_scope_token();
        let cmd_preview: String = req.command.chars().take(64).collect();
        scopes.record(&scope_token, principal, &cmd_preview).await?;

        let mut env: HolderEnv = BTreeMap::new();
        for (k, v) in curated_env() {
            env.insert(k, Some(v));
        }
        if let Some(extra) = &req.env {
            for (k, v) in extra {
                env.insert(k.clone(), Some(v.clone()));
            }
        }
        env.insert(SCOPE_ENV.to_owned(), Some(scope_token.clone()));
        env.insert(RUNTIME_ENV.to_owned(), None);

        let held = match self
            .holder
            .spawn(HolderSpawnRequest {
                id: id.clone(),
                command: req.command.clone(),
                args: req.args.clone(),
                cwd: req.cwd.clone(),
                env,
                cols: req.cols.unwrap_or(80),
                rows: req.rows.unwrap_or(24),
            })
            .await
        {
            Ok(held) => held,
            Err(err) => {
                scopes.release(&scope_token).await;
                return Err(err);
            }
        };

        scopes.attach(&scope_token, held.pid());
        let command_line = if req.args.is_empty() {
            req.command.clone()
        } else {
            format!("{} {}", req.command, req.args.join(" "))
        };
        let handle = LocalProcessHandle::new(
            id.clone(),
            scope_token,
            principal.to_owned(),
            command_line,
            held,
            Arc::clone(scopes),
            limits,
        );
        reservation.insert(id, Arc::clone(&handle));
        Ok(handle)
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ProcessHandle>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .handles
            .get(id)
            .cloned()
            .map(|h| h as Arc<dyn ProcessHandle>)
    }

    pub fn list(&self, principal: Option<&str>) -> Vec<HandleInfo> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .handles
            .values()
            .filter(|h| principal.is_none_or(|p| h.principal == p))
            .map(|h| h.info())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct WaitingHolder(mpsc::UnboundedSender<()>);

    impl HolderLink for WaitingHolder {
        fn spawn(&self, _: HolderSpawnRequest) -> OpFuture<'_, Arc<dyn HeldTerminal>> {
            Box::pin(async move {
                self.0.send(()).expect("test observes holder spawn");
                std::future::pending().await
            })
        }

        fn close(&self) -> OpFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    struct TestTerminal {
        data: broadcast::Sender<Vec<u8>>,
        exit: watch::Sender<Option<HeldExit>>,
    }

    impl HeldTerminal for TestTerminal {
        fn id(&self) -> &str {
            "test-terminal"
        }
        fn pid(&self) -> u32 {
            1
        }
        fn subscribe_data(&self) -> broadcast::Receiver<Vec<u8>> {
            self.data.subscribe()
        }
        fn subscribe_exit(&self) -> watch::Receiver<Option<HeldExit>> {
            self.exit.subscribe()
        }
        fn write(&self, _: &[u8]) {}
        fn resize(&self, _: u16, _: u16) {}
        fn signal(&self, _: &str) {}
        fn screen(&self, _: Option<usize>) -> OpFuture<'_, String> {
            Box::pin(async { Ok(String::new()) })
        }
    }

    #[tokio::test]
    async fn collector_reports_a_gap_when_terminal_output_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = Arc::new(ScopeRegistry::new(
            dir.path().to_path_buf(),
            Arc::new(|_| {}),
            None,
        ));
        let (data, _) = broadcast::channel(2);
        let (exit, _) = watch::channel(None);
        let terminal = Arc::new(TestTerminal { data, exit });
        let handle = LocalProcessHandle::new(
            "id".into(),
            "scope".into(),
            "test".into(),
            "command".into(),
            terminal.clone(),
            scopes,
            Limits::default(),
        );
        let mut output = handle.subscribe_output(0);
        for _ in 0..5 {
            terminal.data.send(b"x".to_vec()).unwrap();
        }
        let first = tokio::time::timeout(Duration::from_secs(5), output.recv())
            .await
            .expect("collector sends output")
            .expect("output stream remains open");
        assert!(first.bytes.is_empty());
        assert_eq!(first.missed_chunks, 3);
        assert_eq!(handle.status().missed_input_chunks, 3);
    }

    #[tokio::test]
    async fn slow_output_subscriber_has_bounded_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = Arc::new(ScopeRegistry::new(
            dir.path().to_path_buf(),
            Arc::new(|_| {}),
            None,
        ));
        let (data, _) = broadcast::channel(2);
        let (exit, _) = watch::channel(None);
        let handle = LocalProcessHandle::new(
            "id".into(),
            "scope".into(),
            "test".into(),
            "command".into(),
            Arc::new(TestTerminal { data, exit }),
            scopes,
            Limits::default(),
        );
        for _ in 0..200 {
            handle.append(b"x".to_vec());
        }
        let mut output = handle.subscribe_output(1);
        tokio::time::timeout(Duration::from_secs(5), output.recv())
            .await
            .expect("backlog begins")
            .expect("output stream remains open");
        assert!(output.len() <= 64, "subscriber backlog is unbounded");
        for _ in 0..1200 {
            handle.append(b"x".to_vec());
        }
        for _ in 1..200 {
            tokio::time::timeout(Duration::from_secs(5), output.recv())
                .await
                .expect("retained backlog is delivered")
                .expect("output stream remains open");
        }
        let gap = tokio::time::timeout(Duration::from_secs(5), output.recv())
            .await
            .expect("subscriber receives gap")
            .expect("output stream remains open");
        assert!(gap.bytes.is_empty());
        assert_eq!(gap.missed_chunks, 1200 - EVENT_BROADCAST_CAPACITY as u64);
    }

    #[tokio::test]
    async fn concurrent_spawns_reserve_capacity_before_waiting_for_holder() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = Arc::new(ScopeRegistry::new(
            dir.path().to_path_buf(),
            Arc::new(|_| {}),
            None,
        ));
        let (started, mut observed) = mpsc::unbounded_channel();
        let limits = Limits {
            max_pty_sessions: Some(1),
            ..Limits::default()
        };
        let registry = Arc::new(HandleRegistry::new(
            Arc::new(WaitingHolder(started)),
            Some(scopes),
            limits,
        ));
        let first_registry = registry.clone();
        let first_limits = limits;
        let first = tokio::spawn(async move {
            first_registry
                .spawn(SpawnRequest::default(), "test", first_limits)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), observed.recv())
            .await
            .expect("first holder spawn starts")
            .expect("holder remains open");
        let second = registry.spawn(SpawnRequest::default(), "test", limits);
        let error = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = second => match result {
                    Err(error) => error,
                    Ok(_) => panic!("second spawn exceeded the cap"),
                },
                _ = observed.recv() => panic!("second spawn reached holder despite the cap"),
            }
        })
        .await
        .expect("capacity decision completes");
        assert_eq!(error.code, OpErrorCode::LimitExceeded);
        first.abort();
        let _ = first.await;
        let third_registry = registry.clone();
        let third = tokio::spawn(async move {
            third_registry
                .spawn(SpawnRequest::default(), "test", limits)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), observed.recv())
            .await
            .expect("cancelled spawn releases its reservation")
            .expect("holder remains open");
        third.abort();
    }
}
