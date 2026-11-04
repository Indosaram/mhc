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

use crate::manager::{SettledAck, SettledAckCallback, TaskManager};
use maho_ext_api::ExtensionApi;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::oneshot;

type Delivery = Box<dyn Fn(&[ChildCompletion]) -> Result<(), String> + Send + Sync>;
type ErrorReport = Box<dyn Fn(&[ChildCompletion], &str) + Send + Sync>;

/// How a flushed batch leaves the process and how a failed one is recorded.
struct Outlet {
    deliver: Delivery,
    report: ErrorReport,
}

/// Coalesces settled background results into bounded parent wake batches (§9.2).
///
/// A settle only enqueues its result and returns a receiver. The first settle with no flush
/// pending spawns ONE flush task; that task yields once, so every settle that is already runnable
/// on the same thread enqueues first (this is what makes a current-thread runtime coalesce), then
/// drains the whole queue into bounded wakes. The flush task alone signals the receivers, after
/// the batch holding their result went through `send_message` (or its failure was recorded), so a
/// waiter never wakes before its result was delivered. No timer or sleep is involved, no thread
/// blocks, and `state` is never held across an `.await` or a delivery.
///
/// A failed or panicking delivery appends an error entry for exactly that batch; its waiters are
/// released like any other and later queued results are still delivered by the same loop.
#[derive(Default)]
pub struct SettledBatcher {
    state: Mutex<BatchState>,
}

#[derive(Default)]
struct BatchState {
    queue: Vec<(ChildCompletion, oneshot::Sender<()>)>,
    flush_pending: bool,
}

impl SettledBatcher {
    fn state(&self) -> std::sync::MutexGuard<'_, BatchState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn submit(self: &Arc<Self>, completion: ChildCompletion, outlet: &Arc<Outlet>) -> SettledAck {
        let (ack, wake) = oneshot::channel();
        let spawn_flush = {
            let mut state = self.state();
            state.queue.push((completion, ack));
            !std::mem::replace(&mut state.flush_pending, true)
        };
        if spawn_flush {
            let batcher = Arc::clone(self);
            let outlet = Arc::clone(outlet);
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                batcher.flush(&outlet);
            });
        }
        wake
    }

    fn flush(&self, outlet: &Outlet) {
        let _pending = FlushPending(self);
        loop {
            let (items, acks): (Vec<_>, Vec<_>) = {
                let mut state = self.state();
                if state.queue.is_empty() {
                    state.flush_pending = false;
                    return;
                }
                std::mem::take(&mut state.queue).into_iter().unzip()
            };
            match catch_unwind(AssertUnwindSafe(|| (outlet.deliver)(&items))) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => (outlet.report)(&items, &error),
                Err(_) => (outlet.report)(&items, "delivery panicked"),
            }
            for ack in acks {
                let _ = ack.send(());
            }
        }
    }
}

/// Lets the next settle spawn a new flush when this one unwound; queued items stay queued.
struct FlushPending<'a>(&'a SettledBatcher);

impl Drop for FlushPending<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.state().flush_pending = false;
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
    let state_dir: PathBuf = manager.state().root.clone();
    let batcher = Arc::new(SettledBatcher::default());
    let flusher = Arc::clone(&batcher);
    let deliver_dir = state_dir.clone();
    let report_sender = Arc::clone(&sender);
    let outlet = Arc::new(Outlet {
        deliver: Box::new(move |tasks| {
            deliver_batch(&sender, &deliver_dir, batch_limit, tasks)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }),
        report: Box::new(move |tasks, error| {
            let _ = report_sender.append_entry(
                "task_delivery_error",
                Some(serde_json::json!({
                    "error": error,
                    "task_ids": tasks.iter().map(|t| &t.id).collect::<Vec<_>>(),
                })),
            );
        }),
    });

    let callback: SettledAckCallback = Arc::new(move |record, _fallback_text| {
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
        flusher.submit(completion, &outlet)
    });
    manager.set_on_settled_ack(Some(callback));
    batcher
}
