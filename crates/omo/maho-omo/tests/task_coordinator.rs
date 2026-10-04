mod support;

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use maho_ext_api::{IdleInjectionCoordinator, IdleInjectionSource};
use maho_omo::TaskCoordinator;
use maho_omo_task::{lead_poller_lifecycle::LeadInjectionCoordinator, parent_notifier::CompletionCoordinator};
use senpi_task::{completion::ParentNotifierMessage, team::messaging::lead_poller_types::LeadInjection};

#[test]
fn task_team_and_continuation_share_one_delivery_and_acknowledge_after_flush() {
    let (queue, log, scheduler) = support::manual_coordinator();
    let bridge = TaskCoordinator(Arc::new(queue.clone()));
    let acknowledgements = Arc::new(AtomicUsize::new(0));
    let acknowledged = Arc::clone(&acknowledgements);
    queue.enqueue(support::injection("ulw", IdleInjectionSource::UlwContinuation, "continue"));
    LeadInjectionCoordinator::enqueue(&bridge, LeadInjection {
        key: "team-message:1".into(), source: "team-message", content: "team ready".into(),
        on_flushed: Some(Box::new(move || { acknowledged.fetch_add(1, Ordering::SeqCst); })),
    }, "senpi-task:team-message", false);
    CompletionCoordinator::enqueue(&bridge, "task-completion:1", "task-completion", &ParentNotifierMessage {
        custom_type: "senpi-task.completion", content: "task done".into(), display: false,
        details: Vec::new(), trigger_turn: Some(true),
    }).unwrap();
    CompletionCoordinator::flush_soon(&bridge);
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 0);
    scheduler.run_pending();
    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message.content, "task done\n\nteam ready\n\ncontinue");
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 1);
    assert_eq!(queue.pending_count(), 0);
}
