//! Port of senpi packages/ai/src/api/cursor-agent/exec-lifecycle.ts.
// ported by todo 12

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

pub trait ExecHeartbeatWriter: Send + Sync + 'static {
    fn is_closed(&self) -> bool;
    fn write_heartbeat(&self) -> Result<(), ()>;
}

struct ExecHeartbeatState<W: ExecHeartbeatWriter> {
    writer: W,
    interval: Duration,
}

/// Handle for one exec-scoped heartbeat; dropping or calling `disarm` stops it.
///
/// The next timer starts only after the current write callback succeeds, so a
/// slow write can never accumulate overlapping heartbeat frames.
pub struct ExecHeartbeat {
    active: Arc<AtomicBool>,
    stop: Arc<Notify>,
    task: Option<JoinHandle<()>>,
}

impl ExecHeartbeat {
    pub fn disarm(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.active.store(false, Ordering::SeqCst);
        self.stop.notify_waiters();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for ExecHeartbeat {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// Arm one exec-scoped heartbeat at a time.
pub fn arm_exec_heartbeat<W: ExecHeartbeatWriter>(interval: Duration, writer: W) -> ExecHeartbeat {
    let state = Arc::new(ExecHeartbeatState { writer, interval });
    let active = Arc::new(AtomicBool::new(true));
    let stop = Arc::new(Notify::new());

    let loop_active = active.clone();
    let loop_stop = stop.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = tokio::time::sleep(state.interval) => {}
                () = loop_stop.notified() => break,
            }
            if !loop_active.load(Ordering::SeqCst) || state.writer.is_closed() {
                break;
            }
            if state.writer.write_heartbeat().is_err() {
                break;
            }
            if !loop_active.load(Ordering::SeqCst) || state.writer.is_closed() {
                break;
            }
        }
    });

    ExecHeartbeat { active, stop, task: Some(task) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct CountingWriter {
        closed: Arc<AtomicBool>,
        writes: Arc<Mutex<u32>>,
        notify: Arc<Notify>,
    }

    impl ExecHeartbeatWriter for CountingWriter {
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }

        fn write_heartbeat(&self) -> Result<(), ()> {
            let mut writes = self.writes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *writes += 1;
            self.notify.notify_waiters();
            Ok(())
        }
    }

    #[tokio::test]
    async fn given_armed_heartbeat_when_interval_elapses_then_writes_fire() {
        let closed = Arc::new(AtomicBool::new(false));
        let writes = Arc::new(Mutex::new(0u32));
        let notify = Arc::new(Notify::new());
        let writer = CountingWriter { closed: closed.clone(), writes: writes.clone(), notify: notify.clone() };
        let heartbeat = arm_exec_heartbeat(Duration::from_millis(5), writer);

        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("heartbeat should write within timeout");

        heartbeat.disarm();
        assert!(*writes.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) >= 1);
    }

    #[tokio::test]
    async fn given_closed_writer_when_heartbeat_fires_then_no_further_writes_scheduled() {
        let closed = Arc::new(AtomicBool::new(true));
        let writes = Arc::new(Mutex::new(0u32));
        let notify = Arc::new(Notify::new());
        let writer = CountingWriter { closed: closed.clone(), writes: writes.clone(), notify: notify.clone() };
        let heartbeat = arm_exec_heartbeat(Duration::from_millis(5), writer);

        tokio::time::sleep(Duration::from_millis(50)).await;
        heartbeat.disarm();
        assert_eq!(*writes.lock().unwrap_or_else(|poisoned| poisoned.into_inner()), 0);
    }

    #[tokio::test]
    async fn given_disarmed_heartbeat_when_dropped_then_task_stops() {
        let closed = Arc::new(AtomicBool::new(false));
        let writes = Arc::new(Mutex::new(0u32));
        let notify = Arc::new(Notify::new());
        let writer = CountingWriter { closed: closed.clone(), writes: writes.clone(), notify: notify.clone() };
        let heartbeat = arm_exec_heartbeat(Duration::from_millis(5), writer);
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("heartbeat should write within timeout");
        drop(heartbeat);

        let count_after_drop = *writes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let count_after_wait = *writes.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(count_after_drop, count_after_wait);
    }
}
