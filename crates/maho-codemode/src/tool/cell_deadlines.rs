use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::{sync::watch, task::JoinHandle};
use crate::timeouts::{idle_timeout::TimeoutPauseHandle, run_budget::RunBudget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellDeadlineKind { HardLimit, RunBudget }

#[derive(Clone, Debug, PartialEq)]
pub struct CellDeadlineExpiry { pub kind: CellDeadlineKind, pub error: String }

pub fn hard_limit_error(cell_id: &str, seconds: f64) -> String {
    format!("Eval cell {cell_id} was killed at the {seconds}s hard limit.")
}

pub struct CellDeadlines {
    pub hard_limit_seconds: f64,
    pub run_budget_seconds: f64,
    budget: Arc<RunBudget>,
    settled: Arc<AtomicBool>,
    signal: watch::Receiver<Option<CellDeadlineExpiry>>,
    task: JoinHandle<()>,
}

impl CellDeadlines {
    pub fn new(cell_id: String, hard_limit_seconds: f64, run_budget_seconds: f64) -> Self {
        let hard_deadline = tokio::time::Instant::now() + Duration::from_secs_f64(hard_limit_seconds);
        let budget = Arc::new(RunBudget::new(cell_id.clone(), (run_budget_seconds * 1000.0) as u64));
        let mut budget_signal = budget.signal();
        let worker_budget = Arc::clone(&budget);
        let settled = Arc::new(AtomicBool::new(false));
        let worker_settled = Arc::clone(&settled);
        let (sender, signal) = watch::channel(None);
        let task = tokio::spawn(async move {
            let expiry = tokio::select! {
                () = tokio::time::sleep_until(hard_deadline) => CellDeadlineExpiry { kind: CellDeadlineKind::HardLimit, error: hard_limit_error(&cell_id, hard_limit_seconds) },
                result = budget_signal.changed() => {
                    if result.is_err() { return; }
                    let Some(event) = budget_signal.borrow_and_update().clone() else { return; };
                    CellDeadlineExpiry { kind: CellDeadlineKind::RunBudget, error: event.error }
                }
            };
            if !worker_settled.swap(true, Ordering::SeqCst) {
                worker_budget.dispose();
                sender.send_replace(Some(expiry));
            }
        });
        Self { hard_limit_seconds, run_budget_seconds, budget, settled, signal, task }
    }

    pub fn signal(&self) -> watch::Receiver<Option<CellDeadlineExpiry>> { self.signal.clone() }

    pub fn clear(&self) {
        if self.settled.swap(true, Ordering::SeqCst) { return; }
        self.budget.dispose();
        self.task.abort();
    }
}

impl TimeoutPauseHandle for CellDeadlines {
    fn pause(&self) { self.budget.pause(); }
    fn resume(&self) { self.budget.resume(); }
}

impl Drop for CellDeadlines {
    fn drop(&mut self) { self.clear(); self.task.abort(); }
}
