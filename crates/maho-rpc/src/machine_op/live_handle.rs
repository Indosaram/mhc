//! Port of `packages/coding-agent/src/modes/rpc/machine-op/live-handle.ts`.
//!
//! One `exec_command` terminal as the relay host's wire sees it (`senpi#6752`), over a
//! `HeldTerminal`. The raw PTY bytes are counted and the last `RETAINED_OUTPUT_BYTES` kept,
//! so `lastSeq` and `totalBytes` stay byte offsets.

use maho_machine_ops::{HeldExit, HeldTerminal};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::{broadcast, watch};

pub const OUTPUT_SETTLE_MS: u64 = 800;
pub const RETAINED_OUTPUT_BYTES: usize = 1024 * 1024;
const EVENT_BROADCAST_CAPACITY: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MachineLifecycle {
    Starting,
    Running,
    WaitingInput,
    Exited,
    Killed,
    Lost,
}

pub const MACHINE_LIFECYCLES: &[MachineLifecycle] = &[
    MachineLifecycle::Starting,
    MachineLifecycle::Running,
    MachineLifecycle::WaitingInput,
    MachineLifecycle::Exited,
    MachineLifecycle::Killed,
    MachineLifecycle::Lost,
];

pub const ENDED: &[MachineLifecycle] = &[
    MachineLifecycle::Exited,
    MachineLifecycle::Killed,
    MachineLifecycle::Lost,
];

impl MachineLifecycle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::WaitingInput => "waiting_input",
            Self::Exited => "exited",
            Self::Killed => "killed",
            Self::Lost => "lost",
        }
    }

    pub fn from_str_name(s: &str) -> Option<Self> {
        match s {
            "starting" => Some(Self::Starting),
            "running" => Some(Self::Running),
            "waiting_input" => Some(Self::WaitingInput),
            "exited" => Some(Self::Exited),
            "killed" => Some(Self::Killed),
            "lost" => Some(Self::Lost),
            _ => None,
        }
    }

    pub const fn is_ended(self) -> bool {
        matches!(self, Self::Exited | Self::Killed | Self::Lost)
    }
}

pub const fn ended(state: MachineLifecycle) -> bool {
    state.is_ended()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleExit {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
}

impl From<HeldExit> for HandleExit {
    fn from(value: HeldExit) -> Self {
        Self {
            exit_code: value.exit_code,
            signal: value.signal,
        }
    }
}

#[derive(Clone, Debug)]
pub enum HandleEvent {
    Output { seq: usize, bytes: Vec<u8> },
    State(MachineLifecycle),
    Idle { foreground: Option<bool> },
}

pub type CancelTimer = Box<dyn FnOnce() + Send + 'static>;
pub type After = Arc<dyn Fn(u64, Box<dyn FnOnce() + Send + 'static>) -> CancelTimer + Send + Sync>;

pub fn tokio_after() -> After {
    Arc::new(|ms, cb| {
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            cb();
        });
        Box::new(move || {
            handle.abort();
        })
    })
}

const PASTE_ON: &[u8] = b"\x1b[?2004h";
const PASTE_OFF: &[u8] = b"\x1b[?2004l";

/// Linux: whether the terminal's foreground process group is the leader's own (field 8 of `/proc/<pid>/stat`).
async fn foreground_of(pid: u32) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
            .await
            .ok()?;
        let rparen = stat.rfind(')')?;
        let rest = stat.get(rparen + 2..)?;
        let tpgid = rest.split(' ').nth(8 - 3)?.parse::<u32>().ok()?;
        Some(tpgid == pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

struct LiveHandleState {
    chunks: VecDeque<Vec<u8>>,
    retained: usize,
    produced: usize,
    state: MachineLifecycle,
    exit: Option<HandleExit>,
    killed: bool,
    quiet: bool,
    paste: bool,
    paste_carry: Vec<u8>,
    cancel_settle: Option<CancelTimer>,
    disposed: bool,
}

pub struct LiveHandle {
    pub id: String,
    pub pid: u32,
    pub sender: String,
    pub started_ms: u64,
    held: Arc<dyn HeldTerminal>,
    after: After,
    inner: Mutex<LiveHandleState>,
    events_tx: broadcast::Sender<HandleEvent>,
    ended_tx: watch::Sender<bool>,
}

impl LiveHandle {
    pub fn new(
        id: String,
        sender: String,
        held: Arc<dyn HeldTerminal>,
        started_ms: u64,
        after: After,
    ) -> Arc<Self> {
        let pid = held.pid();
        let mut data_rx = held.subscribe_data();
        let mut exit_rx = held.subscribe_exit();
        let (events_tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        let (ended_tx, _) = watch::channel(false);

        let handle = Arc::new(Self {
            id,
            pid,
            sender,
            started_ms,
            held,
            after,
            inner: Mutex::new(LiveHandleState {
                chunks: VecDeque::new(),
                retained: 0,
                produced: 0,
                state: MachineLifecycle::Running,
                exit: None,
                killed: false,
                quiet: false,
                paste: false,
                paste_carry: Vec::new(),
                cancel_settle: None,
                disposed: false,
            }),
            events_tx,
            ended_tx,
        });

        handle.arm_settle();

        let weak = Arc::downgrade(&handle);
        tokio::spawn(async move {
            loop {
                if let Some(held_exit) = exit_rx.borrow().clone() {
                    while let Ok(bytes) = data_rx.try_recv() {
                        if let Some(h) = weak.upgrade() {
                            h.on_data(bytes);
                        }
                    }
                    if let Some(h) = weak.upgrade() {
                        h.on_exit(held_exit.into());
                    }
                    break;
                }
                tokio::select! {
                    biased;
                    chunk_res = data_rx.recv() => {
                        match chunk_res {
                            Ok(bytes) => {
                                let Some(h) = weak.upgrade() else {
                                    break;
                                };
                                h.on_data(bytes);
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => {
                                // Wait for exit_rx if still open; otherwise synthesize exit.
                                if exit_rx.changed().await.is_err() {
                                    if let Some(h) = weak.upgrade() {
                                        h.on_exit(HandleExit { exit_code: None, signal: None });
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    changed = exit_rx.changed() => {
                        if changed.is_err() {
                            while let Ok(bytes) = data_rx.try_recv() {
                                if let Some(h) = weak.upgrade() {
                                    h.on_data(bytes);
                                }
                            }
                            if let Some(h) = weak.upgrade() {
                                h.on_exit(HandleExit { exit_code: None, signal: None });
                            }
                            break;
                        }
                    }
                }
            }
        });

        handle
    }

    fn on_data(self: &Arc<Self>, bytes: Vec<u8>) {
        let mut emit_state = None;
        let seq = {
            let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            if st.disposed || st.exit.is_some() {
                return;
            }
            st.produced += bytes.len();
            keep_bytes(&mut st, bytes.clone());
            track_paste(&mut st, &bytes);
            st.quiet = false;
            if st.state == MachineLifecycle::WaitingInput {
                st.state = MachineLifecycle::Running;
                emit_state = Some(MachineLifecycle::Running);
            }
            st.produced
        };
        if let Some(next_state) = emit_state {
            let _ = self.events_tx.send(HandleEvent::State(next_state));
        }
        let _ = self.events_tx.send(HandleEvent::Output { seq, bytes });
        self.arm_settle();
    }

    fn on_exit(&self, exit: HandleExit) {
        let mut emit_state = None;
        let cancel_prev = {
            let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            if st.exit.is_some() {
                return;
            }
            st.exit = Some(exit);
            let prev = st.cancel_settle.take();
            let next = if st.killed {
                MachineLifecycle::Killed
            } else {
                MachineLifecycle::Exited
            };
            if st.state != next {
                st.state = next;
                emit_state = Some(next);
            }
            prev
        };
        if let Some(cancel) = cancel_prev {
            cancel();
        }
        if let Some(next_state) = emit_state {
            let _ = self.events_tx.send(HandleEvent::State(next_state));
        }
        self.ended_tx.send_replace(true);
    }

    fn arm_settle(self: &Arc<Self>) {
        let at = {
            let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(cancel) = st.cancel_settle.take() {
                cancel();
            }
            if st.exit.is_some() || st.disposed {
                return;
            }
            st.produced
        };

        let weak = Arc::downgrade(self);
        let pid = self.pid;
        let cancel = (self.after)(
            OUTPUT_SETTLE_MS,
            Box::new(move || {
                let Some(handle) = weak.upgrade() else {
                    return;
                };
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.spawn(async move {
                        let foreground = foreground_of(pid).await;
                        handle.finish_settle(at, foreground);
                    });
                } else {
                    handle.finish_settle(at, None);
                }
            }),
        );

        let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if st.exit.is_some() || st.disposed || st.produced != at {
            drop(st);
            cancel();
            return;
        }
        st.cancel_settle = Some(cancel);
    }

    fn finish_settle(&self, at: usize, foreground: Option<bool>) {
        let mut emit_state = None;
        {
            let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            if st.exit.is_some() || st.disposed || st.produced != at {
                return;
            }
            st.quiet = true;
            if foreground != Some(false) && st.state != MachineLifecycle::WaitingInput {
                st.state = MachineLifecycle::WaitingInput;
                emit_state = Some(MachineLifecycle::WaitingInput);
            }
        }
        if let Some(next_state) = emit_state {
            let _ = self.events_tx.send(HandleEvent::State(next_state));
        }
        let _ = self.events_tx.send(HandleEvent::Idle { foreground });
    }

    pub fn state(&self) -> MachineLifecycle {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .state
    }

    pub fn exit(&self) -> Option<HandleExit> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .exit
            .clone()
    }

    pub fn produced(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .produced
    }

    pub fn retained_from(&self) -> usize {
        let st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        st.produced - st.retained
    }

    /// The retained bytes from `seq` (or from the oldest retained byte, when `seq` is older) to the end.
    pub fn since(&self, seq: usize) -> Vec<u8> {
        let st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let from = st.produced - st.retained;
        let skip = seq.min(st.produced).saturating_sub(from);
        let mut out = Vec::with_capacity(st.retained.saturating_sub(skip));
        let mut offset = 0usize;
        for chunk in &st.chunks {
            let chunk_end = offset + chunk.len();
            if chunk_end > skip {
                let start = skip.saturating_sub(offset);
                out.extend_from_slice(&chunk[start..]);
            }
            offset = chunk_end;
        }
        out
    }

    pub fn quiet(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .quiet
    }

    pub fn bracketed_paste(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .paste
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HandleEvent> {
        self.events_tx.subscribe()
    }

    pub fn write(&self, bytes: &[u8]) {
        self.held.write(bytes);
    }

    pub fn signal(&self, signal: &str) {
        self.held.signal(signal);
    }

    pub fn mark_killed(&self) {
        let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        st.killed = true;
    }

    pub async fn wait_ended(&self) {
        let mut rx = self.ended_tx.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    pub fn dispose(&self) {
        let cancel_prev = {
            let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            st.disposed = true;
            st.cancel_settle.take()
        };
        if let Some(cancel) = cancel_prev {
            cancel();
        }
    }
}

fn keep_bytes(st: &mut LiveHandleState, bytes: Vec<u8>) {
    let len = bytes.len();
    st.chunks.push_back(bytes);
    st.retained += len;
    while st.retained > RETAINED_OUTPUT_BYTES {
        let cut = st.retained - RETAINED_OUTPUT_BYTES;
        if let Some(front) = st.chunks.front_mut() {
            if front.len() <= cut {
                let removed = front.len();
                st.chunks.pop_front();
                st.retained -= removed;
            } else {
                front.drain(..cut);
                st.retained -= cut;
            }
        } else {
            break;
        }
    }
}

fn find_last_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// The program's bracketed-paste mode, from the last mode switch in its output (a switch may span two chunks).
fn track_paste(st: &mut LiveHandleState, bytes: &[u8]) {
    let mut seen = Vec::with_capacity(st.paste_carry.len() + bytes.len());
    seen.extend_from_slice(&st.paste_carry);
    seen.extend_from_slice(bytes);
    let on = find_last_subslice(&seen, PASTE_ON);
    let off = find_last_subslice(&seen, PASTE_OFF);
    if on != off {
        st.paste = on > off;
    }
    let keep = PASTE_ON.len() - 1;
    let start = seen.len().saturating_sub(keep);
    st.paste_carry.clear();
    st.paste_carry.extend_from_slice(&seen[start..]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_machine_ops::OpFuture;

    pub struct FakeHeldTerminal {
        pub id: String,
        pub pid: u32,
        pub data_tx: broadcast::Sender<Vec<u8>>,
        pub exit_tx: watch::Sender<Option<HeldExit>>,
        pub written: Mutex<Vec<Vec<u8>>>,
        pub signals: Mutex<Vec<String>>,
    }

    impl FakeHeldTerminal {
        pub fn new(id: &str, pid: u32) -> Arc<Self> {
            let (data_tx, _) = broadcast::channel(64);
            let (exit_tx, _) = watch::channel(None);
            Arc::new(Self {
                id: id.to_owned(),
                pid,
                data_tx,
                exit_tx,
                written: Mutex::new(Vec::new()),
                signals: Mutex::new(Vec::new()),
            })
        }
    }

    impl HeldTerminal for FakeHeldTerminal {
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
        fn write(&self, bytes: &[u8]) {
            self.written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(bytes.to_vec());
        }
        fn resize(&self, _cols: u16, _rows: u16) {}
        fn signal(&self, signal: &str) {
            self.signals
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(signal.to_owned());
        }
        fn screen(&self, _history: Option<usize>) -> OpFuture<'_, String> {
            Box::pin(async { Ok(String::new()) })
        }
    }

    #[tokio::test]
    async fn tracks_retained_bytes_split_paste_sequences_and_exit() {
        let held = FakeHeldTerminal::new("h1", 999999);
        let handle = LiveHandle::new(
            "h1".into(),
            "dev-1".into(),
            held.clone(),
            100,
            tokio_after(),
        );
        let mut rx = handle.subscribe();

        held.data_tx.send(b"\x1b[?20".to_vec()).expect("send1");
        held.data_tx.send(b"04hhello".to_vec()).expect("send2");

        while handle.produced() < 13 {
            let _ = rx.recv().await;
        }
        assert!(handle.bracketed_paste());
        assert_eq!(handle.since(8), b"hello");

        held.exit_tx
            .send(Some(HeldExit {
                exit_code: Some(0),
                signal: None,
            }))
            .expect("exit");
        handle.wait_ended().await;
        assert_eq!(handle.state(), MachineLifecycle::Exited);
        assert_eq!(
            handle.exit(),
            Some(HandleExit {
                exit_code: Some(0),
                signal: None
            })
        );
    }
}
