use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use maho_ext_api::{IdleInjectionCoordinator, IdleInjectionSource};
use maho_omo::TaskCoordinator;
use maho_omo_task::{lead_poller_lifecycle::LeadInjectionCoordinator, parent_notifier::CompletionCoordinator};
use senpi_task::{completion::ParentNotifierMessage, team::messaging::lead_poller_types::LeadInjection};

#[test]
fn task_team_and_continuation_share_one_delivery_and_acknowledge_after_flush() {
    let (queue, log, scheduler) = crate::support::manual_coordinator();
    let bridge = TaskCoordinator(Arc::new(queue.clone()));
    let acknowledgements = Arc::new(AtomicUsize::new(0));
    let acknowledged = Arc::clone(&acknowledgements);
    queue.enqueue(crate::support::injection("ulw", IdleInjectionSource::UlwContinuation, "continue"));
    LeadInjectionCoordinator::enqueue(&bridge, LeadInjection {
        key: "team-message:1".into(), source: "team-message", content: "team ready".into(),
        on_flushed: Some(Box::new(move || { acknowledged.fetch_add(1, Ordering::SeqCst); })),
        on_delivery_failed: None,
    }, "senpi-task:team-message", false);
    CompletionCoordinator::enqueue(&bridge, "task-completion:1", "task-completion", &ParentNotifierMessage {
        custom_type: "senpi-task.completion", content: "task done".into(), display: false,
        details: Vec::new(), trigger_turn: Some(true),
    }, senpi_task::completion::DeliveryCallbacks::new(|_| {})).unwrap();
    CompletionCoordinator::flush_soon(&bridge);
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 0);
    scheduler.run_pending();
    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.content, "task done\n\nteam ready\n\ncontinue");
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 1);
    assert_eq!(queue.pending_count(), 0);
}

#[tokio::test]
async fn deferred_wake_rejection_settles_completion_liveness_and_team_failure() {
    use maho_omo::{DeferredScheduler, Delivery, IdleInjectionQueue, TurnBarrier};
    use senpi_task::completion::{DeliveryCallbacks, DeliveryState};
    use std::sync::Mutex;
    use tokio::sync::oneshot;
    let (resolve, delivery) = oneshot::channel::<Result<(), String>>();
    let delivery = Arc::new(Mutex::new(Some(delivery)));
    let scheduler = DeferredScheduler::manual(TurnBarrier::new());
    let queue = IdleInjectionQueue::new(Arc::new(move |_, _| {
        let delivery = delivery.lock().expect("delivery").take().expect("one wake");
        Delivery::Pending(Box::pin(async move { delivery.await.expect("resolution") }))
    }), scheduler.clone(), scheduler.clone());
    let bridge = TaskCoordinator(Arc::new(queue.clone()));
    let (task_settled, task_result) = oneshot::channel();
    let task = DeliveryCallbacks::new(move |result| { task_settled.send(result.is_err()).expect("task receiver"); });
    CompletionCoordinator::enqueue(&bridge, "task-completion:1", "task-completion", &ParentNotifierMessage {
        custom_type: "senpi-task.completion", content: "done".into(), display: false, details: vec![], trigger_turn: Some(true),
    }, task.clone()).expect("completion");
    let (liveness_settled, liveness_result) = oneshot::channel();
    let liveness = DeliveryCallbacks::new(move |result| { liveness_settled.send(result.is_err()).expect("liveness receiver"); });
    bridge.enqueue_liveness("team-member-liveness:1", maho_ext_api::CustomMessage {
        custom_type: "senpi-task.team-member-liveness".into(), content: vec![maho_ext_api::ToolContent::text("lost")], display: false, details: None,
    }, liveness.clone()).expect("liveness");
    let (team_settled, team_result) = oneshot::channel();
    LeadInjectionCoordinator::enqueue(&bridge, LeadInjection {
        key: "team-message:1".into(), source: "team-message", content: "mail".into(),
        on_flushed: Some(Box::new(|| panic!("rejection is not success"))),
        on_delivery_failed: Some(Box::new(move |_| { team_settled.send(()).expect("team receiver"); })),
    }, "senpi-task:team-message", false);
    assert_eq!(queue.flush_on_idle(), 3);
    assert_eq!(task.state(), DeliveryState::Pending);
    assert_eq!(liveness.state(), DeliveryState::Pending);
    resolve.send(Err("wake rejected".into())).expect("pending delivery");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        assert!(task_result.await.expect("task result"));
        assert!(liveness_result.await.expect("liveness result"));
        team_result.await.expect("team result");
    }).await.expect("settlement deadline");
    assert_eq!(task.state(), DeliveryState::Failed);
    assert_eq!(liveness.state(), DeliveryState::Failed);
}

#[tokio::test]
async fn deferred_wake_success_acknowledges_only_after_resolution() {
    use maho_omo::{DeferredScheduler, Delivery, IdleInjectionQueue, TurnBarrier};
    use senpi_task::completion::{DeliveryCallbacks, DeliveryState};
    use std::sync::Mutex;
    use tokio::sync::oneshot;
    let (resolve, delivery) = oneshot::channel::<Result<(), String>>();
    let delivery = Arc::new(Mutex::new(Some(delivery)));
    let scheduler = DeferredScheduler::manual(TurnBarrier::new());
    let queue = IdleInjectionQueue::new(Arc::new(move |_, _| {
        let delivery = delivery.lock().expect("delivery").take().expect("one wake");
        Delivery::Pending(Box::pin(async move { delivery.await.expect("resolution") }))
    }), scheduler.clone(), scheduler);
    let bridge = TaskCoordinator(Arc::new(queue.clone()));
    let (settled, result) = oneshot::channel();
    let callbacks = DeliveryCallbacks::new(move |result| { settled.send(result.is_ok()).expect("receiver"); });
    CompletionCoordinator::enqueue(&bridge, "task-completion:1", "task-completion", &ParentNotifierMessage {
        custom_type: "senpi-task.completion", content: "done".into(), display: false, details: vec![], trigger_turn: Some(true),
    }, callbacks.clone()).expect("completion");
    queue.flush_on_idle();
    assert_eq!(callbacks.state(), DeliveryState::Pending);
    resolve.send(Ok(())).expect("pending delivery");
    assert!(tokio::time::timeout(std::time::Duration::from_secs(5), result).await.expect("settlement deadline").expect("result"));
    assert_eq!(callbacks.state(), DeliveryState::Delivered);
}
