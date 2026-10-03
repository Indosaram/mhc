use super::idle_timeout::TimeoutPauseHandle;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunBudgetEvent {
    pub cell_id: String,
    pub budget_ms: u64,
    pub error: String,
}

pub fn run_budget_error(cell_id: &str, budget_seconds: f64) -> String {
    format!("Eval cell {cell_id} exhausted its {budget_seconds}s run budget (own execution time; host tool calls excluded) and was killed.")
}

struct State {
    charged: Duration,
    running_since: Option<Instant>,
    pause_depth: usize,
    settled: bool,
}

impl State {
    fn consumed(&self) -> Duration {
        self.charged + self.running_since.map_or(Duration::ZERO, |since| Instant::now() - since)
    }
}

pub struct RunBudget {
    pub budget_ms: u64,
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    signal: watch::Receiver<Option<RunBudgetEvent>>,
    task: tokio::task::JoinHandle<()>,
}

impl RunBudget {
    pub fn new(cell_id: String, budget_ms: u64) -> Self {
        let budget_ms = budget_ms.max(1);
        let budget = Duration::from_millis(budget_ms);
        let state = Arc::new(Mutex::new(State { charged: Duration::ZERO, running_since: Some(Instant::now()), pause_depth: 0, settled: false }));
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
                    let state = worker_state.lock().expect("run budget state lock poisoned");
                    if state.settled { return; }
                    state.running_since.map(|since| since + budget.saturating_sub(state.charged))
                };
                if let Some(deadline) = deadline {
                    tokio::select! {
                        () = &mut notified => continue,
                        () = tokio::time::sleep_until(deadline) => {}
                    }
                } else {
                    notified.await;
                    continue;
                }
                let mut state = worker_state.lock().expect("run budget state lock poisoned");
                if state.settled { return; }
                if state.pause_depth > 0 || state.consumed() < budget { continue; }
                state.charged = state.consumed();
                state.running_since = None;
                state.settled = true;
                sender.send_replace(Some(RunBudgetEvent { error: run_budget_error(&cell_id, budget.as_secs_f64()), cell_id, budget_ms }));
                return;
            }
        });
        Self { budget_ms, state, changed, signal, task }
    }

    pub fn consumed(&self) -> Duration { self.state.lock().expect("run budget state lock poisoned").consumed() }
    pub fn signal(&self) -> watch::Receiver<Option<RunBudgetEvent>> { self.signal.clone() }
    pub fn dispose(&self) {
        self.state.lock().expect("run budget state lock poisoned").settled = true;
        self.changed.notify_one();
    }
}

impl TimeoutPauseHandle for RunBudget {
    fn pause(&self) {
        let mut state = self.state.lock().expect("run budget state lock poisoned");
        if state.settled { return; }
        state.pause_depth += 1;
        if state.pause_depth == 1 {
            state.charged = state.consumed();
            state.running_since = None;
            self.changed.notify_one();
        }
    }
    fn resume(&self) {
        let mut state = self.state.lock().expect("run budget state lock poisoned");
        if state.settled || state.pause_depth == 0 { return; }
        state.pause_depth -= 1;
        if state.pause_depth == 0 {
            state.running_since = Some(Instant::now());
            self.changed.notify_one();
        }
    }
}

impl Drop for RunBudget {
    fn drop(&mut self) { self.task.abort(); }
}
