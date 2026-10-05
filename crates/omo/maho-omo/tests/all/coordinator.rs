//! Port of `omo-senpi/src/extension/idle-injection-coordinator.test.ts` (12 cases).

use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{DeliverAs, IdleInjection, IdleInjectionCoordinator, IdleInjectionSource, JsonValue};
use maho_omo::{DeferredScheduler, Delivery, IdleInjectionDetail, IdleInjectionMessage, TurnBarrier, WAKE_CUSTOM_TYPE};
use crate::support::{DeliveryLog, injection, manual_coordinator};

fn inline_pair() -> (DeferredScheduler, DeferredScheduler) {
    (DeferredScheduler::manual(TurnBarrier::new()), DeferredScheduler::manual(TurnBarrier::new()))
}

#[test]
fn hidden_metadata_survives_one_merged_custom_message() {
    let log = DeliveryLog::new();
    let (flush, soon) = inline_pair();
    let coordinator = maho_omo::IdleInjectionQueue::new(log.delivery(), flush, soon);
    let mut queued = injection("st_1", IdleInjectionSource::TaskCompletion, "task st_1 completed");
    queued.custom_type = Some("senpi-task:completion".to_owned());
    queued.display = Some(false);
    queued.details = Some(serde_json::json!({ "taskId": "st_1" }));

    coordinator.enqueue(queued);
    coordinator.flush_on_idle();

    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].message,
        IdleInjectionMessage {
            custom_type: WAKE_CUSTOM_TYPE,
            content: "task st_1 completed".to_owned(),
            display: false,
            details: vec![IdleInjectionDetail {
                custom_type: "senpi-task:completion".to_owned(),
                details: Some(serde_json::json!({ "taskId": "st_1" })),
            }],
        }
    );
    assert_eq!(calls[0].deliver_as, DeliverAs::Steer);
}

#[test]
fn completion_and_continuation_on_one_edge_deliver_once_in_deterministic_order() {
    let (coordinator, log, _scheduler) = manual_coordinator();
    coordinator.enqueue(injection("st_1", IdleInjectionSource::TaskCompletion, "task st_1 completed"));
    coordinator.enqueue(injection("ulw", IdleInjectionSource::UlwContinuation, "continue the run"));

    let collapsed = coordinator.flush_on_idle();

    assert_eq!(collapsed, 2);
    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.content, "task st_1 completed\n\ncontinue the run");
    assert_eq!(calls[0].deliver_as, DeliverAs::Steer);
}

#[test]
fn one_task_completion_steers_immediately() {
    let (coordinator, log, _scheduler) = manual_coordinator();
    coordinator.enqueue(injection("st_1", IdleInjectionSource::TaskCompletion, "task st_1 completed"));

    coordinator.flush_on_idle();

    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.content, "task st_1 completed");
    assert_eq!(calls[0].deliver_as, DeliverAs::Steer);
}

#[test]
fn two_task_completions_on_one_edge_share_one_steer() {
    let (coordinator, log, _scheduler) = manual_coordinator();
    coordinator.enqueue(injection("st_1", IdleInjectionSource::TaskCompletion, "task st_1 completed"));
    coordinator.enqueue(injection("st_2", IdleInjectionSource::TaskCompletion, "task st_2 completed"));

    coordinator.flush_on_idle();

    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.content, "task st_1 completed\n\ntask st_2 completed");
}

#[test]
fn repeated_continuation_enqueues_collapse_to_one_keyed_injection() {
    let (coordinator, log, _scheduler) = manual_coordinator();
    coordinator.enqueue(injection("ulw", IdleInjectionSource::UlwContinuation, "continue A"));
    coordinator.enqueue(injection("ulw", IdleInjectionSource::UlwContinuation, "continue B"));

    assert_eq!(coordinator.pending_count(), 1);
    coordinator.flush_on_idle();

    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.content, "continue B");
}

#[test]
fn remove_by_key_reports_true_once_and_the_flush_no_ops() {
    let (coordinator, log, _scheduler) = manual_coordinator();
    coordinator.enqueue(injection("team-message:m1", IdleInjectionSource::TeamMessage, "x"));

    assert!(coordinator.remove("team-message:m1"));
    assert!(!coordinator.remove("team-message:m1"));
    assert_eq!(coordinator.flush_on_idle(), 0);
    assert!(log.calls().is_empty());
}

#[test]
fn an_empty_queue_delivers_nothing() {
    let (coordinator, log, _scheduler) = manual_coordinator();

    assert_eq!(coordinator.flush_on_idle(), 0);
    assert!(log.calls().is_empty());
}

#[test]
fn a_deferred_schedule_flush_delivers_on_the_idle_tick_not_synchronously() {
    let (coordinator, log, scheduler) = manual_coordinator();
    coordinator.enqueue(injection("ulw", IdleInjectionSource::UlwContinuation, "continue"));

    coordinator.schedule_flush();
    assert!(log.calls().is_empty());

    scheduler.run_pending();
    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.content, "continue");
    assert_eq!(calls[0].deliver_as, DeliverAs::Steer);
}

#[test]
fn a_synchronous_wake_drains_first_and_the_deferred_pass_no_ops() {
    let (coordinator, log, scheduler) = manual_coordinator();
    coordinator.enqueue(injection("ulw", IdleInjectionSource::UlwContinuation, "continue the run"));
    coordinator.schedule_flush();

    coordinator.enqueue(injection("st_1", IdleInjectionSource::TaskCompletion, "task st_1 completed"));
    coordinator.flush_on_idle();

    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.content, "task st_1 completed\n\ncontinue the run");

    scheduler.run_pending();
    assert_eq!(log.calls().len(), 1);
}

#[test]
fn repeated_schedule_flush_requests_coalesce_to_one_flush() {
    let (coordinator, _log, manual) = manual_coordinator();

    coordinator.enqueue(injection("ulw", IdleInjectionSource::UlwContinuation, "continue"));
    coordinator.schedule_flush();
    coordinator.schedule_flush();
    coordinator.schedule_flush();

    assert_eq!(manual.pending_count(), 1);
    manual.run_pending();
    assert_eq!(manual.pending_count(), 0);

    coordinator.enqueue(injection("ulw", IdleInjectionSource::UlwContinuation, "again"));
    coordinator.schedule_flush();
    assert_eq!(manual.pending_count(), 1);
    manual.run_pending();
    assert_eq!(manual.pending_count(), 0);
}

#[tokio::test]
async fn an_async_delivery_rejection_notifies_the_producer_and_skips_on_flushed() {
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(tx)));
    let (flush, soon) = inline_pair();
    let coordinator = maho_omo::IdleInjectionQueue::new(
        Arc::new(|_, _| Delivery::Pending(Box::pin(async { Err("provider rejected".to_owned()) }))),
        flush,
        soon,
    );
    let flushed_events = Arc::clone(&events);
    let failed_events = Arc::clone(&events);
    coordinator.enqueue(IdleInjection {
        key: "team-liveness:1".to_owned(),
        source: IdleInjectionSource::TeamLiveness,
        custom_type: None,
        content: "member failed".to_owned(),
        display: None,
        details: None,
        on_flushed: Some(Arc::new(move || flushed_events.lock().unwrap_or_else(PoisonError::into_inner).push("flushed".to_owned()))),
        on_delivery_failed: Some(Arc::new(move |error| {
            failed_events.lock().unwrap_or_else(PoisonError::into_inner).push(error.to_owned());
            if let Some(sender) = sender.lock().unwrap_or_else(PoisonError::into_inner).take() {
                let _ = sender.send(());
            }
        })),
    });

    coordinator.flush_on_idle();
    tokio::time::timeout(std::time::Duration::from_secs(5), rx).await.expect("bounded receipt").expect("failure receipt");

    assert_eq!(*events.lock().unwrap_or_else(PoisonError::into_inner), vec!["provider rejected".to_owned()]);
}

#[test]
fn on_flushed_runs_synchronously_after_a_synchronous_delivery() {
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let deliver_order = Arc::clone(&order);
    let (flush, soon) = inline_pair();
    let coordinator = maho_omo::IdleInjectionQueue::new(
        Arc::new(move |_, _| {
            deliver_order.lock().unwrap_or_else(PoisonError::into_inner).push("deliver".to_owned());
            Delivery::Delivered
        }),
        flush,
        soon,
    );
    let flushed_order = Arc::clone(&order);
    let mut queued = injection("team-message:m1", IdleInjectionSource::TeamMessage, "alpha: ready");
    queued.on_flushed = Some(Arc::new(move || flushed_order.lock().unwrap_or_else(PoisonError::into_inner).push("flushed".to_owned())));

    coordinator.enqueue(queued);
    coordinator.flush_on_idle();

    assert_eq!(*order.lock().unwrap_or_else(PoisonError::into_inner), vec!["deliver".to_owned(), "flushed".to_owned()]);
}

#[test]
fn details_preserve_only_injections_that_supply_a_custom_type() {
    let log = DeliveryLog::new();
    let (flush, soon) = inline_pair();
    let coordinator = maho_omo::IdleInjectionQueue::new(log.delivery(), flush, soon);
    let mut with_type = injection("a", IdleInjectionSource::TaskCompletion, "a");
    with_type.custom_type = Some("senpi-task:completion".to_owned());
    with_type.details = Some(serde_json::json!({ "taskId": "st_1" }));
    coordinator.enqueue(with_type);
    coordinator.enqueue(injection("b", IdleInjectionSource::DagRun, "b"));

    coordinator.flush_on_idle();

    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.details.len(), 1);
    let details: JsonValue = serde_json::json!([{ "customType": "senpi-task:completion", "details": { "taskId": "st_1" } }]);
    assert_eq!(calls[0].message.to_custom_message().details, Some(details));
}

#[test]
fn flush_soon_defers_through_one_microtask() {
    let log = DeliveryLog::new();
    let scheduler = DeferredScheduler::manual(TurnBarrier::new());
    let coordinator = maho_omo::IdleInjectionQueue::new(log.delivery(), scheduler.clone(), scheduler.clone());
    coordinator.enqueue(injection("ulw", IdleInjectionSource::UlwContinuation, "continue"));

    coordinator.flush_soon();
    assert!(log.calls().is_empty());

    scheduler.run_pending();
    assert_eq!(log.calls().len(), 1);
}

#[test]
fn a_synchronous_delivery_failure_notifies_every_failure_callback() {
    let reasons: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (flush, soon) = inline_pair();
    let coordinator = maho_omo::IdleInjectionQueue::new(Arc::new(|_, _| Delivery::Failed("boom".to_owned())), flush, soon);
    let first = Arc::clone(&reasons);
    let second = Arc::clone(&reasons);
    let mut a = injection("a", IdleInjectionSource::TaskCompletion, "a");
    a.on_delivery_failed = Some(Arc::new(move |reason| first.lock().unwrap_or_else(PoisonError::into_inner).push(reason.to_owned())));
    let mut b = injection("b", IdleInjectionSource::TaskCompletion, "b");
    b.on_delivery_failed = Some(Arc::new(move |reason| second.lock().unwrap_or_else(PoisonError::into_inner).push(reason.to_owned())));
    coordinator.enqueue(a);
    coordinator.enqueue(b);

    assert_eq!(coordinator.flush_on_idle(), 2);
    assert_eq!(*reasons.lock().unwrap_or_else(PoisonError::into_inner), vec!["boom".to_owned(), "boom".to_owned()]);
}
