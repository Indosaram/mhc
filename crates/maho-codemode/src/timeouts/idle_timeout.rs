use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

pub const DEFAULT_MAX_PAUSE_GRACE_MS: u64 = 600_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdleTimeoutEvent {
    pub cell_id: String,
    pub error: String,
}

pub trait TimeoutPauseHandle {
    fn pause(&self);
    fn resume(&self);
}

pub struct IdleTimeoutOptions {
    pub cell_id: String,
    pub timeout_ms: u64,
    pub max_pause_grace_ms: Option<u64>,
    pub deadline: Option<Instant>,
}

struct State {
    deadline: Instant,
    paused_deadline: Option<Instant>,
    pause_depth: usize,
    settled: bool,
}

pub struct IdleTimeout {
    pub timeout_ms: u64,
    pub max_pause_grace_ms: u64,
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    signal: watch::Receiver<Option<IdleTimeoutEvent>>,
    task: tokio::task::JoinHandle<()>,
}

impl IdleTimeout {
    pub fn new(options: IdleTimeoutOptions) -> Self {
        let timeout_ms = options.timeout_ms.max(1);
        let max_pause_grace_ms = options.max_pause_grace_ms.unwrap_or(DEFAULT_MAX_PAUSE_GRACE_MS).max(timeout_ms);
        let state = Arc::new(Mutex::new(State {
            deadline: Instant::now() + Duration::from_millis(timeout_ms),
            paused_deadline: None,
            pause_depth: 0,
            settled: false,
        }));
        let changed = Arc::new(Notify::new());
        let (sender, signal) = watch::channel(None);
        let worker_state = Arc::clone(&state);
        let worker_changed = Arc::clone(&changed);
        let task = tokio::spawn(async move {
            loop {
                let notified = worker_changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let deadline = {
                    let state = worker_state.lock().expect("timeout state lock poisoned");
                    if state.settled { return; }
                    let deadline = state.paused_deadline.unwrap_or(state.deadline);
                    options.deadline.map_or(deadline, |absolute| absolute.min(deadline))
                };
                tokio::select! {
                    () = &mut notified => continue,
                    () = tokio::time::sleep_until(deadline) => {}
                }
                let mut state = worker_state.lock().expect("timeout state lock poisoned");
                if state.settled { return; }
                let deadline = state.paused_deadline.unwrap_or(state.deadline);
                let deadline = options.deadline.map_or(deadline, |absolute| absolute.min(deadline));
                if Instant::now() < deadline { continue; }
                let error = if state.paused_deadline.is_some() {
                    format!("Cell timed out after {max_pause_grace_ms}ms waiting on a host tool call")
                } else {
                    format!("Cell timed out after {timeout_ms}ms")
                };
                state.settled = true;
                state.paused_deadline = None;
                sender.send_replace(Some(IdleTimeoutEvent { cell_id: options.cell_id, error }));
                return;
            }
        });
        Self { timeout_ms, max_pause_grace_ms, state, changed, signal, task }
    }

    pub fn signal(&self) -> watch::Receiver<Option<IdleTimeoutEvent>> { self.signal.clone() }

    pub fn dispose(&self) {
        let mut state = self.state.lock().expect("timeout state lock poisoned");
        state.settled = true;
        state.paused_deadline = None;
        self.changed.notify_one();
    }
}

impl TimeoutPauseHandle for IdleTimeout {
    fn pause(&self) {
        let mut state = self.state.lock().expect("timeout state lock poisoned");
        if state.settled { return; }
        state.pause_depth += 1;
        if state.pause_depth == 1 {
            state.paused_deadline = Some(Instant::now() + Duration::from_millis(self.max_pause_grace_ms));
            self.changed.notify_one();
        }
    }

    fn resume(&self) {
        let mut state = self.state.lock().expect("timeout state lock poisoned");
        if state.settled || state.pause_depth == 0 { return; }
        state.pause_depth -= 1;
        if state.pause_depth == 0 {
            state.paused_deadline = None;
            state.deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
            self.changed.notify_one();
        }
    }
}

impl Drop for IdleTimeout {
    fn drop(&mut self) { self.task.abort(); }
}
