//! Tracks in-flight cache reads so shutdown can wait for child git/store reads to settle (#11135, #11671).

use std::sync::{Condvar, Mutex};

/// RAII guard returned by [`InFlightReads::enter`]; decrements active read count on drop.
pub struct InFlightReadGuard<'a> {
    reads: &'a InFlightReads,
}

impl Drop for InFlightReadGuard<'_> {
    fn drop(&mut self) {
        let mut active = self.reads.active.lock().unwrap_or_else(|e| e.into_inner());
        *active = active.saturating_sub(1);
        if *active == 0 {
            self.reads.settled_signal.notify_all();
        }
    }
}

/// Counts cache reads that have started and not yet finished so shutdown can await `settled()`.
#[derive(Default)]
pub struct InFlightReads {
    active: Mutex<usize>,
    settled_signal: Condvar,
}

impl InFlightReads {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts tracking a read until the returned guard drops.
    pub fn enter(&self) -> InFlightReadGuard<'_> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        *active += 1;
        InFlightReadGuard { reads: self }
    }

    /// Executes `work` while tracked as an in-flight read, ensuring decrement even on panic.
    pub fn track<T>(&self, work: impl FnOnce() -> T) -> T {
        let _guard = self.enter();
        work()
    }

    /// Executes an async `work` future while tracked as an in-flight read.
    pub async fn track_async<T>(&self, work: impl std::future::Future<Output = T>) -> T {
        let _guard = self.enter();
        work.await
    }

    /// Blocks until every currently active tracked read has finished, including nested reads started while running.
    pub fn settled(&self) {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        while *active > 0 {
            active = self
                .settled_signal
                .wait(active)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Number of reads currently in flight.
    pub fn active_count(&self) -> usize {
        *self.active.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn settled_waits_for_in_flight_track_to_complete() {
        let reads = Arc::new(InFlightReads::new());
        let started = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

        let reads_worker = reads.clone();
        let started_worker = started.clone();
        let handle = std::thread::spawn(move || {
            reads_worker.track(|| {
                started_worker.store(true, Ordering::Release);
                let _ = release_rx.recv();
            });
        });

        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert_eq!(reads.active_count(), 1);

        let settled_done = Arc::new(AtomicBool::new(false));
        let reads_waiter = reads.clone();
        let settled_flag = settled_done.clone();
        let waiter = std::thread::spawn(move || {
            reads_waiter.settled();
            settled_flag.store(true, Ordering::Release);
        });

        // Release worker; waiter must observe settled_done afterwards.
        release_tx.send(()).unwrap();
        waiter.join().unwrap();
        handle.join().unwrap();
        assert!(settled_done.load(Ordering::Acquire));
        assert_eq!(reads.active_count(), 0);
    }
}
