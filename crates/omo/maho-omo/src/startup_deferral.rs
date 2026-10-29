use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::scheduler::DeferredTask;

const BACKSTOP: Duration = Duration::from_millis(750);

#[derive(Default)]
struct State {
    opened: bool,
    retired: bool,
    next: u64,
    pending: BTreeMap<u64, DeferredTask>,
}

#[derive(Clone, Default)]
pub struct StartupDeferral(Arc<Mutex<State>>);

impl StartupDeferral {
    pub fn schedule(&self, work: DeferredTask) {
        let (id, delay) = {
            let mut state = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired { return; }
            let id = state.next;
            state.next += 1;
            state.pending.insert(id, work);
            (id, if state.opened { Duration::ZERO } else { BACKSTOP })
        };
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            this.run_one(id);
        });
    }

    fn run_one(&self, id: u64) {
        let work = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.remove(&id);
        if let Some(work) = work { work(); }
    }

    pub fn open(&self) {
        let ids = {
            let mut state = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired || state.opened { return; }
            state.opened = true;
            state.pending.keys().copied().collect::<Vec<_>>()
        };
        for id in ids { self.run_one(id); }
    }

    pub fn retire(&self) {
        let pending = {
            let mut state = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.retired = true;
            std::mem::take(&mut state.pending)
        };
        drop(pending);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn opening_delivers_once_and_retirement_drops_waiting_work() {
        let gate = StartupDeferral::default();
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        gate.schedule(Box::new(move || { observed.fetch_add(1, Ordering::SeqCst); }));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        gate.open();
        gate.open();
        gate.run_one(0);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let retired = StartupDeferral::default();
        let observed = count.clone();
        retired.schedule(Box::new(move || { observed.fetch_add(1, Ordering::SeqCst); }));
        retired.retire();
        retired.open();
        retired.run_one(0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
