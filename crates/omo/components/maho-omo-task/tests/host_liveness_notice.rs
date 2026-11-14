use crate::support;
use idle_injection::{IdleInjection, IdleInjectionCoordinator, IdleInjectionSource};
use maho_ext_api::*;
use maho_omo_task::host_liveness_notice::{
    HostLivenessStatus, child_liveness_notice, host_liveness_text, notify_host_liveness,
};
use maho_omo_task::runtime_context::TaskRuntimeContext;
use senpi_task::state::{TaskRecordInput, create_task_record};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Actions(Mutex<Vec<(CustomMessage, SendMessageOptions)>>);
impl ExtensionActions for Actions {
    fn send_message(
        &self,
        message: CustomMessage,
        options: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.0.lock().expect("messages").push((message, options));
        Ok(())
    }
    fn send_user_message(
        &self,
        _: UserMessageContent,
        _: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        panic!("not user message")
    }
    fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        panic!("not entry")
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(vec![])
    }
}

#[derive(Default)]
struct Queue {
    queued: Mutex<Vec<IdleInjection>>,
    calls: Mutex<Vec<&'static str>>,
}
impl IdleInjectionCoordinator for Queue {
    fn enqueue(&self, injection: IdleInjection) -> bool {
        self.queued.lock().expect("queued").push(injection);
        true
    }
    fn schedule_flush(&self) {
        self.calls.lock().expect("calls").push("schedule_flush");
    }
    fn flush_soon(&self) {
        self.calls.lock().expect("calls").push("flush_soon");
    }
    fn flush_on_idle(&self) -> usize {
        0
    }
    fn pending_count(&self) -> usize {
        self.queued.lock().expect("queued").len()
    }
    fn remove(&self, _: &str) -> bool {
        false
    }
}

const SINCE: i64 = 1_700_000_000_000;

#[test]
fn text_names_the_state_and_what_the_host_is_doing() {
    assert_eq!(
        host_liveness_text(HostLivenessStatus::Running),
        "running: the original session is responding again"
    );
    assert_eq!(
        host_liveness_text(HostLivenessStatus::Unresponsive { since: SINCE }),
        "unresponsive since 2023-11-14T22:13:20.000Z, retrying; the session may still be running"
    );
    assert!(
        host_liveness_text(HostLivenessStatus::TerminalPending { since: SINCE })
            .contains("close pending")
    );
}

#[test]
fn without_a_coordinator_the_notice_steers_directly() {
    let actions = Actions::default();
    notify_host_liveness(
        &actions,
        None,
        "st_1",
        3,
        HostLivenessStatus::Running,
        || false,
    )
    .expect("sent");
    let sent = actions.0.lock().expect("messages");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.custom_type, "senpi-task.status");
    assert!(sent[0].0.display);
    assert!(sent[0].1.trigger_turn);
    assert_eq!(sent[0].1.deliver_as, Some(DeliverAs::Steer));
    assert_eq!(sent[0].0.details.as_ref().expect("details")["run_epoch"], 3);
}

#[test]
fn a_coordinator_queues_under_the_task_status_source_and_flush_follows_the_turn_state() {
    for (streaming, expected) in [(true, "schedule_flush"), (false, "flush_soon")] {
        let actions = Actions::default();
        let queue = Arc::new(Queue::default());
        let coordinator: Arc<dyn IdleInjectionCoordinator> = queue.clone();
        notify_host_liveness(
            &actions,
            Some(&coordinator),
            "st_1",
            3,
            HostLivenessStatus::Unresponsive { since: SINCE },
            || streaming,
        )
        .expect("queued");
        assert!(actions.0.lock().expect("messages").is_empty());
        let queued = queue.queued.lock().expect("queued");
        assert_eq!(queued[0].key, "task-status:st_1");
        assert_eq!(queued[0].source, IdleInjectionSource::TaskStatus);
        assert!(
            queued[0]
                .content
                .starts_with("Task st_1: unresponsive since")
        );
        assert!(
            queued[0]
                .content
                .ends_with("Keep the original task; do not re-dispatch it.")
        );
        assert_eq!(*queue.calls.lock().expect("calls"), vec![expected]);
    }
}

#[test]
fn the_manager_hook_notifies_only_this_sessions_children_and_follows_the_turn_state() {
    let actions = Arc::new(Actions::default());
    let queue = Arc::new(Queue::default());
    let coordinator: Arc<dyn IdleInjectionCoordinator> = queue.clone();
    let mut context = support::context();
    context.is_idle_fn = Arc::new(|| false);
    let mut runtime = TaskRuntimeContext::new("/tmp".into());
    runtime.capture_from(&context);
    let hook = child_liveness_notice(
        actions.clone(),
        Some(coordinator),
        Arc::new(Mutex::new(runtime)),
    );
    let foreign = create_task_record(
        TaskRecordInput {
            parent_session_id: "another-session".into(),
            ..Default::default()
        },
        Some(1),
    )
    .expect("foreign record");
    hook(&foreign, HostLivenessStatus::Unresponsive { since: SINCE });
    assert!(queue.queued.lock().expect("queued").is_empty());
    let own = create_task_record(
        TaskRecordInput {
            parent_session_id: "session".into(),
            ..Default::default()
        },
        Some(2),
    )
    .expect("own record");
    hook(&own, HostLivenessStatus::TerminalPending { since: SINCE });
    let queued = queue.queued.lock().expect("queued");
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].key, format!("task-status:{}", own.task_id));
    assert_eq!(
        queued[0].details.as_ref().expect("details")["state"],
        "terminal-pending"
    );
    assert_eq!(
        queued[0].details.as_ref().expect("details")["run_epoch"],
        own.notification.run_epoch
    );
    assert_eq!(*queue.calls.lock().expect("calls"), vec!["schedule_flush"]);
    assert!(actions.0.lock().expect("messages").is_empty());
}

/// A coordinator retired by session shutdown: it refuses every enqueue but still records the
/// attempt, so the test can count hand-offs and flushes.
#[derive(Default)]
struct RetiredQueue {
    attempts: Mutex<Vec<IdleInjection>>,
    calls: Mutex<Vec<&'static str>>,
}
impl IdleInjectionCoordinator for RetiredQueue {
    fn enqueue(&self, injection: IdleInjection) -> bool {
        self.attempts.lock().expect("attempts").push(injection);
        false
    }
    fn schedule_flush(&self) {
        self.calls.lock().expect("calls").push("schedule_flush");
    }
    fn flush_soon(&self) {
        self.calls.lock().expect("calls").push("flush_soon");
    }
    fn flush_on_idle(&self) -> usize {
        0
    }
    fn pending_count(&self) -> usize {
        0
    }
    fn remove(&self, _: &str) -> bool {
        false
    }
}

/// The caller (`child_liveness_notice`) is the single reporting site: it diagnoses a returned
/// `Err`. So the refusal is asserted on the returned error alone, never on a second report.
#[test]
fn a_refused_notice_returns_the_failure_and_schedules_no_flush() {
    for streaming in [true, false] {
        let actions = Actions::default();
        let queue = Arc::new(RetiredQueue::default());
        let coordinator: Arc<dyn IdleInjectionCoordinator> = queue.clone();
        let error = notify_host_liveness(
            &actions,
            Some(&coordinator),
            "st_1",
            3,
            HostLivenessStatus::Unresponsive { since: SINCE },
            || streaming,
        )
        .expect_err("a refused enqueue must not report success");
        assert!(
            error.message.contains("refused") && error.message.contains("taskId=st_1"),
            "unexpected error: {}",
            error.message
        );
        let attempts = queue.attempts.lock().expect("attempts");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].key, "task-status:st_1");
        assert!(
            attempts[0].on_delivery_failed.is_some(),
            "a notice with no retry owner after enqueue must register a delivery-failure callback"
        );
        assert!(
            queue.calls.lock().expect("calls").is_empty(),
            "no flush for a queue the notice never entered"
        );
        assert!(
            actions.0.lock().expect("messages").is_empty(),
            "a refusal is reported, not silently re-sent"
        );
    }
}
