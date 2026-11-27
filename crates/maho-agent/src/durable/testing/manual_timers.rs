//! Port of senpi `packages/agent/src/durable/testing/manual-timers.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::durable::wake_host_timer::{SetTimer, TimerCallback, TimerCancel};
use crate::transport::transport::{TransportClock, TransportTimerCancel};

struct TimerEntry {
    due: i64,
    callback: TimerCallback,
}

struct TimersState {
    time: i64,
    next_id: u64,
    timers: BTreeMap<u64, TimerEntry>,
}

/// Manual timers for a host under test: a clock that moves only through `advance_to`,
/// which runs due timers in order.
#[derive(Clone)]
pub struct ManualTimers {
    state: Arc<Mutex<TimersState>>,
}

impl ManualTimers {
    pub fn now(&self) -> i64 {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).time
    }

    pub fn now_fn(&self) -> Arc<dyn Fn() -> i64 + Send + Sync> {
        let state = Arc::clone(&self.state);
        Arc::new(move || state.lock().unwrap_or_else(|p| p.into_inner()).time)
    }

    pub fn set_timer_async(&self, callback: TimerCallback, ms: i64) -> TimerCancel {
        let id = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let id = state.next_id;
            state.next_id += 1;
            let due = state.time + ms.max(0);
            state.timers.insert(id, TimerEntry { due, callback });
            id
        };
        let state = Arc::clone(&self.state);
        Arc::new(move || {
            state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .timers
                .remove(&id);
        })
    }

    pub fn set_timer_fn(&self) -> SetTimer {
        let timers = self.clone();
        Arc::new(move |callback, ms| timers.set_timer_async(callback, ms as i64))
    }

    pub fn set_timer_sync(
        &self,
        callback: Box<dyn FnOnce() + Send + 'static>,
        ms: i64,
    ) -> TimerCancel {
        self.set_timer_async(
            Box::new(move || {
                callback();
                Box::pin(async {})
            }),
            ms,
        )
    }

    pub async fn advance_to(&self, at: i64) -> Result<(), String> {
        for step in 0..=10_000 {
            if step == 10_000 {
                return Err("manual timers: more than 10000 timers fired in one advance".to_owned());
            }
            let next_entry = {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let best_id = state
                    .timers
                    .iter()
                    .filter(|(_, entry)| entry.due <= at)
                    .min_by_key(|(id, entry)| (entry.due, *id))
                    .map(|(id, _)| *id);
                let Some(id) = best_id else {
                    state.time = state.time.max(at);
                    return Ok(());
                };
                let entry = state.timers.remove(&id);
                if let Some(entry) = entry {
                    state.time = state.time.max(entry.due);
                    Some(entry.callback)
                } else {
                    None
                }
            };
            if let Some(callback) = next_entry {
                callback().await;
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            }
        }
        Ok(())
    }
}

impl TransportClock for ManualTimers {
    fn now(&self) -> i64 {
        self.now()
    }

    fn set_timer(
        &self,
        callback: Box<dyn FnOnce() + Send + 'static>,
        ms: i64,
    ) -> TransportTimerCancel {
        self.set_timer_sync(callback, ms)
    }
}

pub fn create_manual_timers(start: i64) -> ManualTimers {
    ManualTimers {
        state: Arc::new(Mutex::new(TimersState {
            time: start,
            next_id: 1,
            timers: BTreeMap::new(),
        })),
    }
}
