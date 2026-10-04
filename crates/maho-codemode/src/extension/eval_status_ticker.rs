use std::{sync::{Arc, Mutex}, time::Duration};
use tokio::task::JoinHandle;
use crate::tool::detached_cell_contract::EvalDetachedCellStatusEntry;
use super::eval_status::format_eval_cell_status;

pub const EVAL_STATUS_TICK_INTERVAL_MS: u64 = 1000;
pub type EvalStatusRender = Arc<dyn Fn(Option<String>) + Send + Sync>;
pub type EvalStatusClock = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Default)]
struct TickerState {
    entries: Vec<EvalDetachedCellStatusEntry>,
    last_rendered: Option<String>,
    has_rendered: bool,
}

pub struct EvalStatusTicker {
    state: Arc<Mutex<TickerState>>,
    render: EvalStatusRender,
    now: EvalStatusClock,
    interval: Option<JoinHandle<()>>,
}

impl EvalStatusTicker {
    pub fn new(render: EvalStatusRender, now: EvalStatusClock) -> Self {
        Self { state: Arc::new(Mutex::new(TickerState::default())), render, now, interval: None }
    }

    pub fn running(&self) -> bool { self.interval.is_some() }

    pub fn sync(&mut self, entries: Vec<EvalDetachedCellStatusEntry>) {
        let empty = entries.is_empty();
        {
            let mut state = self.state.lock().expect("eval ticker state poisoned");
            state.entries = entries;
            state.has_rendered = false;
        }
        tick(&self.state, &self.render, &self.now);
        if empty {
            self.stop_interval();
            return;
        }
        if self.interval.is_some() { return; }
        let state = Arc::clone(&self.state);
        let render = Arc::clone(&self.render);
        let now = Arc::clone(&self.now);
        self.interval = Some(tokio::spawn(async move {
            let cadence = Duration::from_millis(EVAL_STATUS_TICK_INTERVAL_MS);
            let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + cadence, cadence);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                tick(&state, &render, &now);
            }
        }));
    }

    pub fn stop(&mut self) {
        self.stop_interval();
        *self.state.lock().expect("eval ticker state poisoned") = TickerState::default();
    }

    fn stop_interval(&mut self) {
        if let Some(handle) = self.interval.take() { handle.abort(); }
    }
}

impl Drop for EvalStatusTicker {
    fn drop(&mut self) { self.stop_interval(); }
}

fn tick(state: &Mutex<TickerState>, render: &EvalStatusRender, now: &EvalStatusClock) {
    let mut state = state.lock().expect("eval ticker state poisoned");
    let status = format_eval_cell_status(&state.entries, now());
    if state.has_rendered && status == state.last_rendered { return; }
    state.has_rendered = true;
    state.last_rendered = status.clone();
    render(status);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendering_owns_state_until_callback_returns() {
        let state=Arc::new(Mutex::new(TickerState::default()));
        let observed=state.clone();
        let render:EvalStatusRender=Arc::new(move |_| {
            assert!(matches!(observed.try_lock(),Err(std::sync::TryLockError::WouldBlock)),"stop must not reset state while an admitted render is executing");
        });
        let now:EvalStatusClock=Arc::new(||0.0);
        tick(&state,&render,&now);
        assert!(state.try_lock().is_ok());
    }
}
