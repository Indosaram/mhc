use std::{sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::task::JoinHandle;
use crate::tool::detached_cell_contract::EvalDetachedCellStatusEntry;
use super::eval_status::format_eval_cell_status;

pub const EVAL_STATUS_TICK_INTERVAL_MS: u64 = 1000;
pub type EvalStatusRender = Arc<dyn Fn(Option<String>) -> Result<(), String> + Send + Sync>;
pub type EvalStatusClock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type EvalStatusTryRender = Arc<dyn Fn(Option<String>) -> Result<(), String> + Send + Sync>;

pub fn is_stale_extension_context_error(message: &str) -> bool {
    message.starts_with("This extension ctx is stale after session replacement or reload.") || message == "stale extension generation after reload"
}

#[derive(Default)]
struct TickerState {
    entries: Vec<EvalDetachedCellStatusEntry>,
    last_rendered: Option<String>,
    has_rendered: bool,
}

pub struct EvalStatusTicker {
    state: Arc<Mutex<TickerState>>,
    render: EvalStatusTryRender,
    now: EvalStatusClock,
    retired: Arc<AtomicBool>,
    interval: Option<JoinHandle<()>>,
}

impl EvalStatusTicker {
    pub fn new(render: EvalStatusRender, now: EvalStatusClock) -> Self {
        Self::new_with_try_render(render, now)
    }

    pub fn new_with_try_render(render: EvalStatusTryRender, now: EvalStatusClock) -> Self {
        Self { state: Arc::new(Mutex::new(TickerState::default())), render, now, retired: Arc::new(AtomicBool::new(false)), interval: None }
    }

    pub fn running(&self) -> bool { self.interval.is_some() }

    pub fn sync(&mut self, entries: Vec<EvalDetachedCellStatusEntry>) {
        let empty = entries.is_empty();
        if self.retired.load(Ordering::SeqCst) { self.stop_interval(); }
        {
            let mut state = self.state.lock().expect("eval ticker state poisoned");
            state.entries = entries;
            state.has_rendered = false;
        }
        self.retired.store(false, Ordering::SeqCst);
        if tick(&self.state, &self.render, &self.now, &self.retired) { self.stop_interval(); }
        if empty { self.stop_interval(); return; }
        if self.interval.is_some() { return; }
        let state = Arc::clone(&self.state);
        let render = Arc::clone(&self.render);
        let now = Arc::clone(&self.now);
        let retired = Arc::clone(&self.retired);
        self.interval = Some(tokio::spawn(async move {
            let cadence = Duration::from_millis(EVAL_STATUS_TICK_INTERVAL_MS);
            let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + cadence, cadence);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop { interval.tick().await; if tick(&state, &render, &now, &retired) { break; } }
        }));
    }

    pub fn stop(&mut self) {
        self.stop_interval();
        *self.state.lock().expect("eval ticker state poisoned") = TickerState::default();
    }

    fn stop_interval(&mut self) { if let Some(handle) = self.interval.take() { handle.abort(); } }
}

impl Drop for EvalStatusTicker { fn drop(&mut self) { self.stop_interval(); } }

fn tick(state: &Mutex<TickerState>, render: &EvalStatusTryRender, now: &EvalStatusClock, retired: &AtomicBool) -> bool {
    let mut state = state.lock().expect("eval ticker state poisoned");
    let status = format_eval_cell_status(&state.entries, now());
    if state.has_rendered && status == state.last_rendered { return false; }
    state.has_rendered = true;
    state.last_rendered = status.clone();
    if retired.load(Ordering::SeqCst) { return true; }
    match render(status) {
        Ok(()) => false,
        Err(message) if is_stale_extension_context_error(&message) => {
            *state = TickerState::default();
            retired.store(true, Ordering::SeqCst);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_context_retires_ticker_state_for_next_sync() {
        let state = Mutex::new(TickerState::default());
        let retired = AtomicBool::new(false);
        let render: EvalStatusTryRender = Arc::new(|_| Err("stale extension generation after reload".into()));
        let now: EvalStatusClock = Arc::new(|| 0.0);
        assert!(tick(&state, &render, &now, &retired));
        assert!(retired.load(Ordering::SeqCst));
        assert!(state.lock().expect("ticker state").entries.is_empty());
    }

    #[test]
    fn stale_error_identification_matches_both_upstream_messages() {
        assert!(is_stale_extension_context_error("This extension ctx is stale after session replacement or reload. details"));
        assert!(is_stale_extension_context_error("stale extension generation after reload"));
        assert!(!is_stale_extension_context_error("renderer failed"));
    }
}
