use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use tokio::task::AbortHandle;

use crate::logger::log;

use super::reservations::{get_active_reservation, set_expired_reservation_handler};
use super::session_idle_dispatch::{DispatchAfterSessionIdleArgs, dispatch_after_session_idle};
use super::timing::{Clock, current_clock};
use super::types::{InternalPromptDispatchResult, QueuedInternalPrompt};

static PROMPT_QUEUES: LazyLock<RwLock<HashMap<String, Vec<QueuedInternalPrompt>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static PROMPT_QUEUE_DRAINING: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));
static PROMPT_QUEUE_IN_FLIGHT: LazyLock<RwLock<HashMap<String, QueuedInternalPrompt>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static PROMPT_QUEUE_TIMERS: LazyLock<RwLock<HashMap<String, AbortHandle>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static PROMPT_QUEUE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

static QUEUE_INIT: LazyLock<()> = LazyLock::new(|| {
    set_expired_reservation_handler(Arc::new(|session_id| {
        let clock = current_clock();
        schedule_prompt_queue_drain(session_id, 0, clock.as_ref());
    }));
});

pub fn ensure_queue_initialized() {
    LazyLock::force(&QUEUE_INIT);
}

pub fn next_prompt_queue_id() -> u64 {
    PROMPT_QUEUE_SEQUENCE.fetch_add(1, Ordering::SeqCst)
}

fn queued_result(queued_by: &str, position: usize) -> InternalPromptDispatchResult {
    InternalPromptDispatchResult::Queued {
        queued_by: queued_by.to_string(),
        position,
    }
}

fn clear_prompt_queue_timer(session_id: &str) {
    let mut timers = PROMPT_QUEUE_TIMERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(handle) = timers.remove(session_id) {
        handle.abort();
    }
}

pub fn schedule_prompt_queue_drain(session_id: &str, delay_ms: u64, clock: &dyn Clock) {
    ensure_queue_initialized();
    {
        let queues = PROMPT_QUEUES
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let queue = queues.get(session_id);
        if queue.is_none() || queue.is_some_and(Vec::is_empty) {
            clear_prompt_queue_timer(session_id);
            return;
        }
    }

    clear_prompt_queue_timer(session_id);
    let session_id_owned = session_id.to_string();
    let sleep_future = clock.sleep_ms(delay_ms);

    let join_handle = tokio::spawn(async move {
        sleep_future.await;
        {
            let mut timers = PROMPT_QUEUE_TIMERS
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            timers.remove(&session_id_owned);
        }
        let clk = current_clock();
        let _ = drain_prompt_queue(&session_id_owned, None, clk.as_ref()).await;
    });

    let abort_handle = join_handle.abort_handle();
    let mut timers = PROMPT_QUEUE_TIMERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    timers.insert(session_id.to_string(), abort_handle);
}

fn remove_prompt_queue_entry(session_id: &str, entry_id: u64) {
    let mut queues = PROMPT_QUEUES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(queue) = queues.get_mut(session_id) {
        queue.retain(|queued| queued.id != entry_id);
        if queue.is_empty() {
            queues.remove(session_id);
        }
    }
}

pub fn get_queued_prompt_blocker(session_id: &str) -> Option<String> {
    {
        let in_flight = PROMPT_QUEUE_IN_FLIGHT
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = in_flight.get(session_id) {
            return Some(entry.source.clone());
        }
    }
    let queues = PROMPT_QUEUES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    queues
        .get(session_id)
        .and_then(|q| q.first())
        .map(|entry| entry.source.clone())
}

pub fn is_prompt_queue_draining(session_id: &str) -> bool {
    let draining = PROMPT_QUEUE_DRAINING
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    draining.contains(session_id)
}

pub fn release_in_flight_prompt_matching_dedupe(session_id: &str, dedupe_key: &str) {
    let mut in_flight = PROMPT_QUEUE_IN_FLIGHT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = in_flight.get(session_id)
        && entry.dedupe_key == dedupe_key
    {
        let entry_id = entry.id;
        in_flight.remove(session_id);
        drop(in_flight);
        remove_prompt_queue_entry(session_id, entry_id);
        let mut draining = PROMPT_QUEUE_DRAINING
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        draining.remove(session_id);
    }
}

pub fn clear_prompt_queue_state_for_testing() {
    PROMPT_QUEUES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    PROMPT_QUEUE_DRAINING
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    PROMPT_QUEUE_IN_FLIGHT
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    let mut timers = PROMPT_QUEUE_TIMERS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (_, handle) in timers.drain() {
        handle.abort();
    }
}

pub async fn drain_prompt_queue(
    session_id: &str,
    awaited_entry: Option<&QueuedInternalPrompt>,
    clock: &dyn Clock,
) -> Option<InternalPromptDispatchResult> {
    ensure_queue_initialized();
    {
        let mut draining = PROMPT_QUEUE_DRAINING
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if draining.contains(session_id) {
            return awaited_entry.map(|e| queued_result(&e.source, 1));
        }
        draining.insert(session_id.to_string());
    }
    clear_prompt_queue_timer(session_id);

    let mut awaited_result: Option<InternalPromptDispatchResult> = None;
    let awaited_id = awaited_entry.map(|e| e.id);

    let entry_opt = {
        let queues = PROMPT_QUEUES
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queues.get(session_id).and_then(|q| q.first()).cloned()
    };

    if let Some(entry) = entry_opt {
        {
            let mut in_flight = PROMPT_QUEUE_IN_FLIGHT
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            in_flight.insert(session_id.to_string(), entry.clone());
        }

        let dispatch_fn = entry.dispatch.clone();
        let result = dispatch_after_session_idle(DispatchAfterSessionIdleArgs {
            session_name: entry.session_name,
            client: &entry.client,
            session_id: &entry.session_id,
            input: &entry.input,
            source: &entry.source,
            dedupe_key: &entry.dedupe_key,
            settle_ms: entry.settle_ms,
            post_dispatch_hold_ms: entry.post_dispatch_hold_ms,
            semantic_dedupe_hold_ms: entry.semantic_dedupe_hold_ms,
            dispatch_timeout_ms: entry.dispatch_timeout_ms,
            check_status: entry.check_status,
            check_tool_state: entry.check_tool_state,
            dispatch: move |inp: &serde_json::Value| {
                let inp = inp.clone();
                async move { dispatch_fn(inp).await }
            },
            clock,
        })
        .await;

        {
            let mut in_flight = PROMPT_QUEUE_IN_FLIGHT
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(inf) = in_flight.get(session_id)
                && inf.id == entry.id
            {
                in_flight.remove(session_id);
            }
        }

        match &result {
            InternalPromptDispatchResult::Active
            | InternalPromptDispatchResult::Reserved { .. } => {
                let queued_by = match &result {
                    InternalPromptDispatchResult::Reserved { reserved_by } => reserved_by.as_str(),
                    _ => entry.source.as_str(),
                };
                let queued = queued_result(queued_by, 1);
                if awaited_id == Some(entry.id) {
                    awaited_result = Some(queued);
                }
                schedule_prompt_queue_drain(session_id, entry.queue_retry_ms, clock);
            }
            _ => {
                remove_prompt_queue_entry(session_id, entry.id);
                if awaited_id == Some(entry.id) {
                    awaited_result = Some(result);
                }

                let remaining_not_empty = {
                    let queues = PROMPT_QUEUES
                        .read()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    queues.get(session_id).is_some_and(|q| !q.is_empty())
                };

                if remaining_not_empty {
                    schedule_prompt_queue_drain(session_id, entry.post_dispatch_hold_ms, clock);
                }
            }
        }
    }

    {
        let mut draining = PROMPT_QUEUE_DRAINING
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        draining.remove(session_id);
    }

    awaited_result
}

pub async fn enqueue_internal_prompt(
    entry: QueuedInternalPrompt,
    clock: &dyn Clock,
) -> InternalPromptDispatchResult {
    ensure_queue_initialized();
    let active_res = get_active_reservation(&entry.session_id, clock.now_ms());
    if let Some(res) = active_res
        && res.dedupe_key == entry.dedupe_key
    {
        log(
            "[prompt-async-gate] queued prompt coalesced with recent dispatch",
            Some(&serde_json::json!({
                "sessionID": &entry.session_id,
                "source": &entry.source,
                "queuedBy": &res.source,
            })),
        );
        return queued_result(&res.source, 0);
    }

    let (queue_len, entry_source, session_id, entry_clone) = {
        let mut queues = PROMPT_QUEUES
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let queue = queues.entry(entry.session_id.clone()).or_default();

        if let Some(idx) = queue.iter().position(|q| q.dedupe_key == entry.dedupe_key) {
            let existing = &queue[idx];
            let existing_source = existing.source.clone();
            log(
                "[prompt-async-gate] queued prompt coalesced with pending dispatch",
                Some(&serde_json::json!({
                    "sessionID": &entry.session_id,
                    "source": &entry.source,
                    "queuedBy": &existing_source,
                    "position": idx + 1,
                })),
            );
            return queued_result(&existing_source, idx + 1);
        }

        let entry_source = entry.source.clone();
        let session_id = entry.session_id.clone();
        let entry_clone = entry.clone();
        queue.push(entry);
        let queue_len = queue.len();
        (queue_len, entry_source, session_id, entry_clone)
    };

    log(
        "[prompt-async-gate] queued prompt accepted",
        Some(&serde_json::json!({
            "sessionID": &session_id,
            "source": &entry_source,
            "position": queue_len,
        })),
    );

    let is_draining = is_prompt_queue_draining(&session_id);
    if queue_len > 1 || is_draining {
        schedule_prompt_queue_drain(&session_id, 0, clock);
        return queued_result(&entry_source, queue_len);
    }

    let result = drain_prompt_queue(&session_id, Some(&entry_clone), clock).await;
    result.unwrap_or_else(|| queued_result(&entry_source, 1))
}
