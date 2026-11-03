//! Delegated task completion transport and wake batch limiting (`docs/cleanroom/specs/ultrawork.md` §9, GAP-5).
pub mod batch;
pub mod cap;

pub use batch::{
    ChildCompletion, DEFAULT_COMPLETION_BATCH_LIMIT, MAX_COMPLETION_BATCH_LIMIT,
    MIN_COMPLETION_BATCH_LIMIT, WakeBatchOptions, WakeBatchResult, WakeOverflowManifest,
    build_wake_batch, deliver_batch, read_overflow_manifest,
};
pub use cap::{
    CompletionCapOptions, DEFAULT_COMPLETION_TRANSPORT_LIMIT, MAX_COMPLETION_TRANSPORT_LIMIT,
    MIN_COMPLETION_TRANSPORT_LIMIT, SettledDelivery, build_capped_result_envelope,
    cap_completion_response, count_utf16_code_units, deliver_settled, slice_utf16_bounded,
};

use crate::manager::{SettledCallback, TaskManager};
use maho_ext_api::ExtensionApi;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Coalesces settled background results into bounded parent wake batches (§9.2).
///
/// The first settler becomes the flusher: it drains the queue in a loop until it is empty, and
/// each drained batch becomes one message bounded by the batch limit. A result that settles while
/// a delivery is in flight is only queued and joins the next batch, so a burst of N settles
/// produces a handful of wakes instead of N. No timer delays a delivery.
///
/// Delivered before waiters wake (manager.rs `launch`): the manager fires `done_tx` only after the
/// settled callback returns, and `submit` returns only after the batch holding its own result was
/// handed to the sender, so the waiter wake follows the flush. Deadlock freedom: `deliver`
/// clones the callback and releases the `on_settled` lock before calling it, `state` is never
/// held across `send_message`, and the flusher never waits on a settler, so every waiting settler
/// is woken by a flusher that is making progress. On a current-thread runtime the flusher holds
/// the only thread for the whole drain, so no settler can ever be waiting.
#[derive(Default)]
pub struct SettledBatcher {
    state: Mutex<BatchState>,
    changed: Condvar,
}

#[derive(Default)]
struct BatchState {
    queue: Vec<ChildCompletion>,
    queued_seq: u64,
    flushed_seq: u64,
    flushing: bool,
}

impl SettledBatcher {
    fn submit(&self, completion: ChildCompletion, deliver: impl Fn(&[ChildCompletion])) {
        let mut state = lock(&self.state);
        state.queue.push(completion);
        state.queued_seq += 1;
        let seq = state.queued_seq;
        self.changed.notify_all();
        if state.flushing {
            let _flushed = self
                .changed
                .wait_while(state, |s| s.flushed_seq < seq)
                .unwrap_or_else(PoisonError::into_inner);
            return;
        }
        state.flushing = true;
        drop(state);
        let _reset = FlushReset(self);
        loop {
            let (batch, upto) = {
                let mut state = lock(&self.state);
                if state.queue.is_empty() {
                    state.flushing = false;
                    return;
                }
                (std::mem::take(&mut state.queue), state.queued_seq)
            };
            deliver(&batch);
            lock(&self.state).flushed_seq = upto;
            self.changed.notify_all();
        }
    }

    /// Blocks until at least `count` results wait for the next batch; false when `timeout` passed.
    /// Lets a caller (and the coalescing test) synchronise on queued settles without sleeping.
    pub fn wait_queued(&self, count: usize, timeout: Duration) -> bool {
        let state = lock(&self.state);
        let (_state, waited) = self
            .changed
            .wait_timeout_while(state, timeout, |s| s.queue.len() < count)
            .unwrap_or_else(PoisonError::into_inner);
        !waited.timed_out()
    }
}

/// Releases the settlers waiting on a flusher that unwound inside `deliver`.
struct FlushReset<'a>(&'a SettledBatcher);

impl Drop for FlushReset<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let mut state = lock(&self.0.state);
            state.flushing = false;
            state.flushed_seq = state.queued_seq;
            drop(state);
            self.0.changed.notify_all();
        }
    }
}

/// Wires settled background task delivery through capping and wake batch limiting.
pub fn bind_settled_batcher(
    manager: &TaskManager,
    sender: Arc<ExtensionApi>,
    transport_limit: usize,
    batch_limit: usize,
) -> Arc<SettledBatcher> {
    let state_dir = manager.state().root.clone();
    let batcher = Arc::new(SettledBatcher::default());
    let flusher = Arc::clone(&batcher);

    let callback: SettledCallback = Arc::new(move |record, _fallback_text| {
        let state = crate::record::StateDir::new(&state_dir);
        let disk_output = state.read_output(&record.id);
        let raw = if disk_output.trim().is_empty() {
            state.read_log_tail(&record.id, 4_000)
        } else {
            disk_output
        };
        let (envelope, details) =
            build_capped_result_envelope(&state_dir, record, &raw, transport_limit);
        let completion = ChildCompletion::new(&record.id, record.status.as_str(), envelope)
            .with_details(details);
        flusher.submit(completion, |tasks| {
            if let Err(error) = deliver_batch(&sender, &state_dir, batch_limit, tasks) {
                let _ = sender.append_entry(
                    "task_delivery_error",
                    Some(serde_json::json!({
                        "error": error.to_string(),
                        "task_ids": tasks.iter().map(|t| &t.id).collect::<Vec<_>>(),
                    })),
                );
            }
        });
    });
    manager.set_on_settled(Some(callback));
    batcher
}
