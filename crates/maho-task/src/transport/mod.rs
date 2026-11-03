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
use std::sync::{Arc, Mutex};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Wires settled background task delivery through capping and wake batch limiting.
pub fn bind_settled_batcher(
    manager: &TaskManager,
    sender: Arc<ExtensionApi>,
    transport_limit: usize,
    batch_limit: usize,
) {
    let state_dir = manager.state().root.clone();
    let queue: Arc<Mutex<Vec<ChildCompletion>>> = Arc::default();
    let flusher_lock: Arc<Mutex<()>> = Arc::default();

    let callback: SettledCallback = Arc::new(move |record, _fallback_text| {
        let state = crate::record::StateDir::new(&state_dir);
        let disk_output = state.read_output(&record.id);
        let raw = if !disk_output.trim().is_empty() {
            disk_output
        } else {
            state.read_log_tail(&record.id, 4_000)
        };

        let (envelope, details) = build_capped_result_envelope(
            &state_dir,
            record,
            &raw,
            transport_limit,
        );
        let completion = ChildCompletion::new(&record.id, record.status.as_str(), envelope)
            .with_details(details);

        // 1. Push completion to queue
        {
            let mut q = lock(&queue);
            q.push(completion);
        }

        // 2. Synchronous flush: acquire flusher lock so waiters only wake after delivery completes.
        let _guard = lock(&flusher_lock);
        let tasks = {
            let mut q = lock(&queue);
            std::mem::take(&mut *q)
        };
        if !tasks.is_empty() {
            if let Err(error) = deliver_batch(&sender, &state_dir, batch_limit, &tasks) {
                let _ = sender.append_entry(
                    "task_delivery_error",
                    Some(serde_json::json!({
                        "error": error.to_string(),
                        "task_ids": tasks.iter().map(|t| &t.id).collect::<Vec<_>>(),
                    })),
                );
            }
        }
    });
    manager.set_on_settled(Some(callback));
}
