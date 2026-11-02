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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Wires settled background task delivery through capping and wake batch limiting.
pub fn bind_settled_batcher(
    manager: &TaskManager,
    sender: Arc<ExtensionApi>,
    transport_limit: usize,
    batch_limit: usize,
) {
    let state_dir = manager.state().root.clone();
    let queue: Arc<Mutex<Vec<ChildCompletion>>> = Arc::default();
    let is_flushing: Arc<AtomicBool> = Arc::default();

    let callback: SettledCallback = Arc::new(move |record, fallback_text| {
        let state = crate::record::StateDir::new(&state_dir);
        let disk_output = state.read_output(&record.id);
        let raw = if !disk_output.trim().is_empty() {
            disk_output
        } else if !fallback_text.trim().is_empty() {
            fallback_text.to_owned()
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

        let mut q = queue.lock().unwrap();
        q.push(completion);
        drop(q);

        if is_flushing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let queue = Arc::clone(&queue);
            let is_flushing = Arc::clone(&is_flushing);
            let sender = Arc::clone(&sender);
            let state_dir = state_dir.clone();
            tokio::spawn(async move {
                loop {
                    tokio::task::yield_now().await;
                    let tasks = {
                        let mut q = queue.lock().unwrap();
                        if q.is_empty() {
                            is_flushing.store(false, Ordering::SeqCst);
                            break;
                        }
                        std::mem::take(&mut *q)
                    };
                    let _ = deliver_batch(&sender, &state_dir, batch_limit, &tasks);
                }
            });
        }
    });
    manager.set_on_settled(Some(callback));
}
