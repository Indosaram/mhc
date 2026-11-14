use crate::status_ui::StatusUiTimers;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Queue {
    next: u64,
    /// Keyed by deadline then handle: the first entry is next due, and `clear` finds the handle.
    armed: BTreeMap<(Instant, u64), Box<dyn FnOnce() + Send>>,
    stopped: AtomicBool,
}

#[derive(Default)]
pub struct HostTimers {
    queue: Arc<Mutex<Queue>>,
    signal: Arc<Condvar>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl HostTimers {
    fn ensure_worker(&self) {
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        if worker.is_some() {
            return;
        }
        let queue = Arc::clone(&self.queue);
        let signal = Arc::clone(&self.signal);
        *worker = std::thread::Builder::new()
            .name("host-timers".into())
            .spawn(move || run_worker(queue, signal))
            .ok();
    }
}

/// One worker for every timer; arming used to spawn an OS thread per timer per tick.
fn run_worker(queue: Arc<Mutex<Queue>>, signal: Arc<Condvar>) {
    let mut ready = Vec::new();
    loop {
        {
            let mut guard = queue.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.stopped.load(Ordering::SeqCst) {
                guard.armed.clear();
                return;
            }
            let now = Instant::now();
            let due: Vec<_> = guard
                .armed
                .keys()
                .take_while(|(at, _)| *at <= now)
                .copied()
                .collect();
            for key in due {
                if let Some(callback) = guard.armed.remove(&key) {
                    ready.push(callback);
                }
            }
            if ready.is_empty() {
                match guard.armed.keys().next().copied() {
                    Some((at, _)) => {
                        let wait = signal
                            .wait_timeout(guard, at.saturating_duration_since(now))
                            .unwrap_or_else(PoisonError::into_inner);
                        drop(wait);
                    }
                    None => drop(signal.wait(guard).unwrap_or_else(PoisonError::into_inner)),
                }
                continue;
            }
        }
        for callback in ready.drain(..) {
            callback();
        }
    }
}

impl StatusUiTimers for HostTimers {
    fn set(&self, callback: Box<dyn FnOnce() + Send>, milliseconds: u64) -> u64 {
        self.ensure_worker();
        let mut guard = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        guard.next += 1;
        let handle = guard.next;
        guard.armed.insert(
            (Instant::now() + Duration::from_millis(milliseconds), handle),
            callback,
        );
        drop(guard);
        self.signal.notify_all();
        handle
    }
    fn clear(&self, handle: u64) {
        let mut guard = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        guard.armed.retain(|(_, armed), _| *armed != handle);
    }
}

impl Drop for HostTimers {
    fn drop(&mut self) {
        {
            let mut guard = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            guard.stopped.store(true, Ordering::SeqCst);
            guard.armed.clear();
        }
        self.signal.notify_all();
    }
}
