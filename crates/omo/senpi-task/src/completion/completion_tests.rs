use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pretty_assertions::assert_eq;

use super::*;
use crate::host::HostError;
use crate::state::{
    ResidencyState, ResolvedModelRecord, ResolvedModelSource, TaskNotification, TaskRecord,
    TaskRecordInput, TaskRunStats, TaskStatus, create_task_record,
};
use crate::test_support::{MemoryStore, base_record};

// ---- fixtures ---------------------------------------------------------------------------------

fn completed_record() -> TaskRecord {
    let mut record = base_record("st_deadbeef", "parent-session");
    record.root_session_id = "root-session".to_string();
    record.name = Some("summarize-logs".to_string());
    record.final_response = Some("the final answer".to_string());
    record
}

fn session_record(task_id: &str) -> TaskRecord {
    let mut record = base_record(task_id, "session-a");
    record.name = Some("retry-me".to_string());
    record
}

fn epochs(run_epoch: i64, notified_epoch: i64, failed: Option<i64>) -> TaskNotification {
    TaskNotification {
        run_epoch,
        notified_epoch,
        notification_failed_epoch: failed,
        liveness_notified_epoch: None,
    }
}

fn model(provider: &str, model_id: &str) -> ResolvedModelRecord {
    ResolvedModelRecord::new(ResolvedModelSource::Category, provider, model_id)
}

/// Parent notifier that throws for the first `failures` enqueues, then records messages.
struct ScriptedNotifier {
    remaining_failures: Mutex<usize>,
    attempts: AtomicUsize,
    calls: Mutex<Vec<ParentNotifierMessage>>,
}

impl ScriptedNotifier {
    fn new(failures: usize) -> Arc<Self> {
        Arc::new(Self {
            remaining_failures: Mutex::new(failures),
            attempts: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<ParentNotifierMessage> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ParentNotifier for ScriptedNotifier {
    fn enqueue(&self, message: &ParentNotifierMessage) -> Result<(), HostError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        let mut remaining = self
            .remaining_failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *remaining > 0 {
            *remaining -= 1;
            return Err(HostError {
                message: "parent unavailable".to_string(),
            });
        }
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message.clone());
        Ok(())
    }
}

/// Captures scheduled retries; `run(index)` fires one timer.
#[derive(Default)]
struct FakeScheduler {
    tasks: Mutex<Vec<Option<ScheduledTask>>>,
    delays: Mutex<Vec<u64>>,
}

impl FakeScheduler {
    fn schedule(self: &Arc<Self>) -> CompletionRetrySchedule {
        let this = Arc::clone(self);
        Arc::new(move |task, delay_ms| {
            this.tasks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Some(task));
            this.delays
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(delay_ms);
            Box::new(|| {})
        })
    }

    fn run(&self, index: usize) {
        let task = self.tasks.lock().unwrap_or_else(PoisonError::into_inner)[index]
            .take()
            .expect("scheduled call");
        task();
    }

    fn delays(&self) -> Vec<u64> {
        self.delays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn notifier_with(store: &Arc<MemoryStore>, parent: &Arc<ScriptedNotifier>) -> CompletionNotifier {
    create_completion_notifier(CompletionNotifierDeps::new(parent.clone(), store.clone()))
}

fn retry_notifier(
    store: &Arc<MemoryStore>,
    parent: &Arc<ScriptedNotifier>,
    scheduler: &Arc<FakeScheduler>,
    current_session: Arc<Mutex<String>>,
) -> CompletionNotifier {
    let mut deps = CompletionNotifierDeps::new(parent.clone(), store.clone());
    deps.schedule = Some(scheduler.schedule());
    deps.get_current_session_id = Some(Arc::new(move || {
        Some(
            current_session
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        )
    }));
    deps.get_parent_state = Some(Arc::new(|| ParentState::Idle));
    create_completion_notifier(deps)
}

fn session(value: &str) -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(value.to_string()))
}

fn notify(
    completion: &CompletionNotifier,
    record: &TaskRecord,
    parent_state: ParentState,
    run_in_background: bool,
) -> NotifyResult {
    completion
        .notify_terminal(&CompletionRequest {
            record: record.clone(),
            parent_state,
            run_in_background,
            tokens: None,
        })
        .expect("notify")
}

fn reconcile(completion: &CompletionNotifier, session_id: &str, parent_state: ParentState) {
    completion
        .reconcile_unnotified_notifications(ReconcileUnnotifiedNotificationsInput {
            session_id,
            parent_state,
        })
        .expect("reconcile");
}

fn flush(completion: &CompletionNotifier, session_id: &str, replaced: bool) -> FlushResult {
    completion
        .flush_buffered(&FlushInput {
            session_id: session_id.to_string(),
            replaced,
        })
        .expect("flush")
}

fn detail_ids(message: &ParentNotifierMessage) -> Vec<String> {
    message
        .details
        .iter()
        .map(|detail| detail.task_id.clone())
        .collect()
}

fn wake() -> NotifyResult {
    NotifyResult::Delivered(DeliveredDecision::Wake)
}

fn assert_base_delay(delay: u64) {
    assert!((500..700).contains(&delay), "delay {delay}");
}

// ---- completion.test.ts ---------------------------------------------------------------------------

fn completed_fallback_record() -> TaskRecord {
    let mut record = base_record("st_fallback", "parent-session");
    record.root_session_id = "root-session".to_string();
    record.name = Some("fallback-worker".to_string());
    record.model = "vendor-b/fallback-model".to_string();
    record.requested_model = Some(model("vendor-a", "primary-model"));
    record.resolved_model = Some(model("vendor-b", "fallback-model"));
    record.fallback_models = Some(vec![model("vendor-c", "final-model")]);
    record.created_at = "2026-07-28T08:00:00.000Z".to_string();
    record.updated_at = "2026-07-28T08:00:03.000Z".to_string();
    record.final_response = Some("completed on the fallback model".to_string());
    record
}

#[test]
fn rerouted_task_reports_model_fallback_once_at_terminal_completion() {
    let record = completed_fallback_record();
    let message = build_completion_message(&[build_completion_details(
        &record,
        BuildDetailsOptions::default(),
    )]);
    assert!(
        message
            .content
            .contains("fallback:vendor-a/primary-model->vendor-b/fallback-model")
    );
    assert_eq!(message.content.matches("fallback:").count(), 1);
    assert!(!message.content.contains("quota"));
    assert!(!message.content.contains("403"));
}

#[test]
fn no_model_reroute_keeps_completion_output_unchanged() {
    let mut record = completed_fallback_record();
    record.requested_model = Some(model("vendor-a", "primary-model"));
    record.resolved_model = Some(model("vendor-a", "primary-model"));
    record.model = "vendor-a/primary-model".to_string();
    let message = build_completion_message(&[build_completion_details(
        &record,
        BuildDetailsOptions::default(),
    )]);
    assert!(message.content.contains("model:vendor-a/primary-model"));
    assert!(!message.content.contains("fallback:"));
}

// ---- completion/routing.test.ts -------------------------------------------------------------------

#[test]
fn idle_parent_always_wakes() {
    assert_eq!(route_completion(ParentState::Idle), RoutingDecision::Wake);
}

#[test]
fn streaming_parent_is_delivered_into_the_running_turn() {
    assert_eq!(
        route_completion(ParentState::Streaming),
        RoutingDecision::DeliverStreaming
    );
}

#[test]
fn compacting_parent_buffers_with_compacting_reason() {
    assert_eq!(
        route_completion(ParentState::Compacting),
        RoutingDecision::Buffer(TransitionReason::Compacting)
    );
}

#[test]
fn session_switching_parent_buffers_with_switching_reason() {
    assert_eq!(
        route_completion(ParentState::SessionSwitching),
        RoutingDecision::Buffer(TransitionReason::SessionSwitching)
    );
}

#[test]
fn session_shutdown_parent_buffers_with_shutdown_reason() {
    assert_eq!(
        route_completion(ParentState::SessionShutdown),
        RoutingDecision::Buffer(TransitionReason::SessionShutdown)
    );
}

#[test]
fn external_terminals_notify() {
    assert!(should_notify_status(TaskStatus::Completed));
    assert!(should_notify_status(TaskStatus::Error));
    assert!(should_notify_status(TaskStatus::Lost));
}

#[test]
fn parent_initiated_terminals_do_not_notify() {
    assert!(!should_notify_status(TaskStatus::Cancelled));
    assert!(!should_notify_status(TaskStatus::Interrupted));
}

#[test]
fn non_terminal_statuses_do_not_notify() {
    assert!(!should_notify_status(TaskStatus::Pending));
    assert!(!should_notify_status(TaskStatus::Running));
}

// ---- completion/model-visibility.test.ts ----------------------------------------------------------

#[test]
fn resolved_category_model_is_named_in_completion() {
    let record = create_task_record(
        TaskRecordInput {
            parent_session_id: "parent-session".to_string(),
            root_session_id: "parent-session".to_string(),
            depth: 1,
            execution_mode: "in-process".to_string(),
            category: Some("quick".to_string()),
            model: "requested/model".to_string(),
            resolved_model: Some(model("quotio-openai", "gpt-5.6-luna-fast")),
            ..TaskRecordInput::default()
        },
        None,
    )
    .expect("record");
    let completed = TaskRecord {
        status: TaskStatus::Completed,
        final_response: Some("done".to_string()),
        ..record
    };
    let message = build_completion_message(&[build_completion_details(
        &completed,
        BuildDetailsOptions::default(),
    )]);
    assert!(
        message
            .content
            .contains("category:quick(quotio-openai/gpt-5.6-luna-fast)")
    );
    assert!(!message.content.contains("requested/model"));
}

// ---- completion/notification.test.ts --------------------------------------------------------------

fn details_of(record: &TaskRecord) -> CompletionDetails {
    build_completion_details(record, BuildDetailsOptions::default())
}

#[test]
fn completed_record_details_carry_core_facts_full_result_and_duration() {
    let details = details_of(&completed_record());
    assert_eq!(details.task_id, "st_deadbeef");
    assert_eq!(details.name, "summarize-logs");
    assert_eq!(details.status, TaskStatus::Completed);
    assert_eq!(details.duration_ms, 3000);
    assert_eq!(details.final_response, "the final answer");
    assert!(details.continuation_hint.contains("st_deadbeef"));
}

#[test]
fn result_longer_than_former_cap_is_included_in_full() {
    let result = "x".repeat(2_000);
    let details = details_of(&TaskRecord {
        final_response: Some(result.clone()),
        ..completed_record()
    });
    assert_eq!(details.final_response, result);
}

#[test]
fn result_beyond_transport_capacity_is_spilled_to_a_local_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let result = "x".repeat(32_001);
    let details = build_completion_details(
        &TaskRecord {
            final_response: Some(result.clone()),
            ..completed_record()
        },
        BuildDetailsOptions {
            tokens: None,
            state_dir: Some(dir.path()),
        },
    );
    let file = details.final_response_file.clone().expect("spill file");
    assert!(file.starts_with("local://"));
    assert!(details.final_response.len() < result.len());
    let spill_path = &file["local://".len()..];
    assert_eq!(std::fs::read_to_string(spill_path).expect("spill"), result);
}

#[test]
fn continuation_hint_names_task_send_but_never_task_output() {
    let details = details_of(&completed_record());
    assert!(details.continuation_hint.contains("task_send"));
    assert!(!details.continuation_hint.contains("task_output"));
}

#[test]
fn task_send_hint_uses_to_and_message_params() {
    let details = details_of(&completed_record());
    assert!(details.continuation_hint.contains("task_send({ to:"));
    assert!(details.continuation_hint.contains("message:"));
    assert!(!details.continuation_hint.contains("task_send({ task_id:"));
    assert!(!details.continuation_hint.contains("prompt:"));
}

#[test]
fn error_message_feeds_the_full_result() {
    let details = details_of(&TaskRecord {
        status: TaskStatus::Error,
        final_response: None,
        error_message: Some("child crashed".to_string()),
        ..completed_record()
    });
    assert_eq!(details.status, TaskStatus::Error);
    assert_eq!(details.final_response, "child crashed");
}

#[test]
fn provided_tokens_are_attached() {
    let details = build_completion_details(
        &completed_record(),
        BuildDetailsOptions {
            tokens: Some(1234),
            state_dir: None,
        },
    );
    assert_eq!(details.tokens, Some(1234));
}

#[test]
fn complete_result_body_has_no_follow_up_task_output_instruction() {
    let full_result = "child final text ".repeat(100);
    let details = details_of(&TaskRecord {
        final_response: Some(full_result.clone()),
        ..completed_record()
    });
    let message = build_completion_message(std::slice::from_ref(&details));
    assert_eq!(message.custom_type, "senpi-task.completion");
    assert!(!message.display);
    assert_eq!(message.details, vec![details]);
    for needle in [
        "task completion",
        "name:summarize-logs",
        "id:st_deadbeef",
        "status:completed",
        "duration:3s",
        full_result.as_str(),
        "task_send",
    ] {
        assert!(message.content.contains(needle), "missing {needle}");
    }
    for forbidden in ["task_output", "<task-notification>", "<head>"] {
        assert!(
            !message.content.contains(forbidden),
            "unexpected {forbidden}"
        );
    }
}

#[test]
fn spilled_result_body_names_its_local_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let details = build_completion_details(
        &TaskRecord {
            final_response: Some("x".repeat(32_001)),
            ..completed_record()
        },
        BuildDetailsOptions {
            tokens: None,
            state_dir: Some(dir.path()),
        },
    );
    let message = build_completion_message(std::slice::from_ref(&details));
    assert!(
        message
            .content
            .contains(details.final_response_file.as_deref().expect("spill file"))
    );
}

#[test]
fn two_details_render_in_one_content_block() {
    let first = details_of(&TaskRecord {
        task_id: "st_aaaa".to_string(),
        name: Some("one".to_string()),
        ..completed_record()
    });
    let second = details_of(&TaskRecord {
        task_id: "st_bbbb".to_string(),
        name: Some("two".to_string()),
        ..completed_record()
    });
    let message = build_completion_message(&[first, second]);
    assert_eq!(message.details.len(), 2);
    assert!(message.content.contains("one"));
    assert!(message.content.contains("two"));
    assert_eq!(message.content.matches("task completion").count(), 2);
    assert!(!message.content.contains("<task-notification>"));
}

#[test]
fn run_stats_and_token_fallback_are_attached() {
    let details = details_of(&TaskRecord {
        run_stats: Some(TaskRunStats {
            runtime_ms: 3_000,
            turns: 2,
            tool_calls: 4,
            output_tokens: Some(500),
            total_tokens: Some(1_800),
            generation_ms: Some(2_000),
            tokens_per_second: Some(250.0),
            ..TaskRunStats::default()
        }),
        ..completed_record()
    });
    let message = build_completion_message(std::slice::from_ref(&details));
    assert_eq!(
        details
            .run_stats
            .as_ref()
            .and_then(|stats| stats.tokens_per_second),
        Some(250.0)
    );
    assert_eq!(details.tokens, Some(1_800));
    assert!(message.content.contains("tps:250"));
    assert!(message.content.contains("tools:4"));
}

#[test]
fn explicit_tokens_win_over_run_stats() {
    let details = build_completion_details(
        &TaskRecord {
            run_stats: Some(TaskRunStats {
                runtime_ms: 3_000,
                turns: 1,
                tool_calls: 0,
                total_tokens: Some(1_800),
                ..TaskRunStats::default()
            }),
            ..completed_record()
        },
        BuildDetailsOptions {
            tokens: Some(42),
            state_dir: None,
        },
    );
    assert_eq!(details.tokens, Some(42));
}

// ---- completion/notifier.test.ts ------------------------------------------------------------------

#[test]
fn double_terminal_replay_delivers_exactly_once() {
    let record = completed_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        wake()
    );
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        NotifyResult::Skipped(SkipReason::AlreadyNotified)
    );
    assert_eq!(parent.calls().len(), 1);
    assert_eq!(
        store
            .mutated()
            .last()
            .map(|r| r.notification.notified_epoch),
        Some(0)
    );
}

#[test]
fn revived_task_at_epoch_one_notifies_again() {
    let record = TaskRecord {
        notification: epochs(1, 0, None),
        ..completed_record()
    };
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        wake()
    );
    assert_eq!(parent.calls().len(), 1);
    assert_eq!(
        store
            .mutated()
            .last()
            .map(|r| r.notification.notified_epoch),
        Some(1)
    );
}

#[test]
fn persisted_notified_epoch_prevents_re_notify_after_resume() {
    let record = TaskRecord {
        notification: epochs(0, 0, None),
        ..completed_record()
    };
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        NotifyResult::Skipped(SkipReason::AlreadyNotified)
    );
    assert!(parent.calls().is_empty());
}

#[test]
fn synchronous_foreground_task_never_notifies() {
    let record = completed_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, false),
        NotifyResult::Skipped(SkipReason::SyncTask)
    );
    assert!(parent.calls().is_empty());
}

#[test]
fn parent_initiated_cancel_or_interrupt_never_notifies() {
    let cancelled = TaskRecord {
        status: TaskStatus::Cancelled,
        final_response: None,
        ..completed_record()
    };
    let interrupted = TaskRecord {
        status: TaskStatus::Interrupted,
        ..cancelled.clone()
    };
    let store = MemoryStore::with([cancelled.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    let skipped = NotifyResult::Skipped(SkipReason::NonNotifyingTerminal);
    assert_eq!(
        notify(&completion, &cancelled, ParentState::Idle, true),
        skipped
    );
    // The store holds the cancelled record under the same id, so load() answers for both calls.
    assert_eq!(
        notify(&completion, &interrupted, ParentState::Idle, true),
        skipped
    );
    assert!(parent.calls().is_empty());
}

#[test]
fn still_running_task_is_skipped_as_non_terminal() {
    let record = TaskRecord {
        status: TaskStatus::Running,
        final_response: None,
        ..completed_record()
    };
    let store = MemoryStore::with([record.clone()]);
    let completion = notifier_with(&store, &ScriptedNotifier::new(0));
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        NotifyResult::Skipped(SkipReason::NotTerminal)
    );
}

fn route(parent_state: ParentState) -> (NotifyResult, Vec<ParentNotifierMessage>) {
    let record = completed_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    let result = notify(&completion, &record, parent_state, true);
    (result, parent.calls())
}

#[test]
fn idle_parent_delivery_records_trigger_turn_unconditionally() {
    let (result, calls) = route(ParentState::Idle);
    assert_eq!(result, wake());
    assert_eq!(calls[0].trigger_turn, Some(true));
}

#[test]
fn streaming_parent_delivery_records_trigger_turn_for_batched_steer() {
    let (result, calls) = route(ParentState::Streaming);
    assert_eq!(
        result,
        NotifyResult::Delivered(DeliveredDecision::DeliverStreaming)
    );
    assert_eq!(calls[0].trigger_turn, Some(true));
}

#[test]
fn compacting_parent_buffers_without_delivery() {
    let (result, calls) = route(ParentState::Compacting);
    assert_eq!(result, NotifyResult::Buffered(TransitionReason::Compacting));
    assert!(calls.is_empty());
}

#[test]
fn session_switching_parent_buffers_without_delivery() {
    let (result, calls) = route(ParentState::SessionSwitching);
    assert_eq!(
        result,
        NotifyResult::Buffered(TransitionReason::SessionSwitching)
    );
    assert!(calls.is_empty());
}

#[test]
fn two_buffered_completions_flush_as_one_wake() {
    let first = TaskRecord {
        task_id: "st_aaaa".to_string(),
        name: Some("one".to_string()),
        ..completed_record()
    };
    let second = TaskRecord {
        task_id: "st_bbbb".to_string(),
        name: Some("two".to_string()),
        ..completed_record()
    };
    let store = MemoryStore::with([first.clone(), second.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    notify(&completion, &first, ParentState::Compacting, true);
    notify(&completion, &second, ParentState::Compacting, true);
    assert_eq!(
        flush(&completion, "parent-session", false),
        FlushResult::Flushed(2)
    );
    let calls = parent.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].trigger_turn, Some(true));
    assert_eq!(calls[0].details.len(), 2);
    let notified: Vec<i64> = store
        .mutated()
        .iter()
        .map(|record| record.notification.notified_epoch)
        .collect();
    assert_eq!(notified, vec![0, 0]);
}

#[test]
fn buffered_completion_of_replaced_session_is_dropped() {
    let record = completed_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    notify(&completion, &record, ParentState::SessionShutdown, true);
    assert_eq!(
        flush(&completion, "parent-session", true),
        FlushResult::Dropped(1)
    );
    assert!(parent.calls().is_empty());
    assert!(store.mutated().is_empty());
    assert!(
        store
            .events()
            .iter()
            .any(|(_, event)| event.event_type == "notification_dropped")
    );
}

#[test]
fn flush_without_buffered_notifications_is_empty() {
    let record = completed_record();
    let store = MemoryStore::with([record]);
    let completion = notifier_with(&store, &ScriptedNotifier::new(0));
    assert_eq!(
        flush(&completion, "parent-session", false),
        FlushResult::Empty
    );
}

#[test]
fn first_enqueue_failure_is_retried_once_and_epoch_advances() {
    let record = completed_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(1);
    let completion = notifier_with(&store, &parent);
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        wake()
    );
    assert_eq!(parent.calls().len(), 1);
    assert_eq!(
        store
            .mutated()
            .last()
            .map(|r| r.notification.notified_epoch),
        Some(0)
    );
}

#[test]
fn two_enqueue_failures_record_failure_without_advancing_epoch() {
    let record = completed_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(2);
    let mut deps = CompletionNotifierDeps::new(parent.clone(), store.clone());
    // Park the retry timer so it cannot fire during the assertions.
    deps.schedule = Some(Arc::new(FakeScheduler::default()).schedule());
    let completion = create_completion_notifier(deps);
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        NotifyResult::Failed
    );
    assert!(parent.calls().is_empty());
    assert!(
        store
            .events()
            .iter()
            .any(|(_, event)| event.event_type == "notification_failed")
    );
    let persisted = store.mutated().last().cloned().expect("persisted");
    assert_eq!(persisted.notification.notification_failed_epoch, Some(0));
    assert_eq!(persisted.notification.notified_epoch, -1);
}

// ---- completion/notifier-buffer-dedupe.test.ts ----------------------------------------------------

#[test]
fn same_task_epoch_buffered_twice_notifies_the_parent_once() {
    let record = TaskRecord {
        task_id: "st_dedupe".to_string(),
        name: Some("dedupe-me".to_string()),
        final_response: Some("final".to_string()),
        ..completed_record()
    };
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    notify(&completion, &record, ParentState::Compacting, true);
    notify(&completion, &record, ParentState::Compacting, true);
    assert_eq!(completion.buffered_count(&record.parent_session_id), 1);
    assert_eq!(
        flush(&completion, &record.parent_session_id, false),
        FlushResult::Flushed(1)
    );
    let calls = parent.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].details.len(), 1);
}

// ---- completion/notifier-reconcile-unnotified.test.ts ---------------------------------------------

fn reconcile_record() -> TaskRecord {
    TaskRecord {
        task_id: "st_reconcile".to_string(),
        name: Some("reconcile-me".to_string()),
        residency_state: ResidencyState::PersistedOnly,
        notify_on_terminal: true,
        ..session_record("st_reconcile")
    }
}

fn claiming_store(record: TaskRecord) -> Arc<MemoryStore> {
    let mut store = MemoryStore::seeded([record]);
    store.claim_on_persist = Some(Box::new(|record: &mut TaskRecord| {
        record.residency_state = ResidencyState::Resident;
        record.host_pid = Some(4242);
    }));
    Arc::new(store)
}

#[test]
fn unnotified_opt_in_record_reconciled_twice_delivers_exactly_once() {
    let record = reconcile_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    reconcile(&completion, "session-a", ParentState::Idle);
    reconcile(&completion, "session-a", ParentState::Idle);
    let calls = parent.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(detail_ids(&calls[0]), vec!["st_reconcile"]);
    assert_eq!(
        store
            .get(&record.task_id)
            .map(|r| r.notification.notified_epoch),
        Some(0)
    );
}

#[test]
fn already_notified_epoch_is_skipped_by_reconcile() {
    let record = TaskRecord {
        notification: epochs(0, 0, None),
        ..reconcile_record()
    };
    let store = MemoryStore::with([record]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    reconcile(&completion, "session-a", ParentState::Idle);
    assert!(parent.calls().is_empty());
}

#[test]
fn unnotified_record_of_another_session_is_skipped() {
    let record = TaskRecord {
        parent_session_id: "session-b".to_string(),
        ..reconcile_record()
    };
    let store = MemoryStore::with([record]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    reconcile(&completion, "session-a", ParentState::Idle);
    assert!(parent.calls().is_empty());
}

#[test]
fn unnotified_record_without_opt_in_or_failed_delivery_is_skipped() {
    let record = TaskRecord {
        notify_on_terminal: false,
        ..reconcile_record()
    };
    let store = MemoryStore::with([record]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    reconcile(&completion, "session-a", ParentState::Idle);
    assert!(parent.calls().is_empty());
}

#[test]
fn legacy_failed_delivery_record_keeps_its_in_flight_retry() {
    let record = TaskRecord {
        notify_on_terminal: false,
        notification: epochs(0, -1, Some(0)),
        ..reconcile_record()
    };
    let store = MemoryStore::with([record]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    reconcile(&completion, "session-a", ParentState::Idle);
    let calls = parent.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(detail_ids(&calls[0]), vec!["st_reconcile"]);
}

#[test]
fn failed_reconcile_delivery_records_failed_epoch_and_schedules_retry() {
    let record = reconcile_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(usize::MAX);
    let scheduler = Arc::new(FakeScheduler::default());
    let mut deps = CompletionNotifierDeps::new(parent.clone(), store.clone());
    deps.schedule = Some(scheduler.schedule());
    let completion = create_completion_notifier(deps);
    reconcile(&completion, "session-a", ParentState::Idle);
    assert_eq!(parent.attempts.load(Ordering::SeqCst), 2);
    let persisted = store.get(&record.task_id).expect("record");
    assert_eq!(persisted.notification.notification_failed_epoch, Some(0));
    assert_eq!(persisted.notification.notified_epoch, -1);
    assert_eq!(scheduler.delays().len(), 1);
}

#[test]
fn buffered_completion_is_skipped_by_reconcile_and_flushed_exactly_once() {
    let record = reconcile_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    notify(&completion, &record, ParentState::Compacting, true);
    assert_eq!(completion.buffered_count("session-a"), 1);
    reconcile(&completion, "session-a", ParentState::Idle);
    assert!(parent.calls().is_empty());
    let flushed = flush(&completion, "session-a", false);
    reconcile(&completion, "session-a", ParentState::Idle);
    assert_eq!(flushed, FlushResult::Flushed(1));
    assert_eq!(parent.calls().len(), 1);
    assert_eq!(
        store
            .get(&record.task_id)
            .map(|r| r.notification.notified_epoch),
        Some(0)
    );
}

#[test]
fn residency_claim_between_read_and_notified_persist_survives() {
    let record = reconcile_record();
    let store = claiming_store(record.clone());
    let completion = notifier_with(&store, &ScriptedNotifier::new(0));
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        wake()
    );
    let persisted = store.get(&record.task_id).expect("record");
    assert_eq!(persisted.notification.notified_epoch, 0);
    assert_eq!(persisted.host_pid, Some(4242));
    assert_eq!(persisted.residency_state, ResidencyState::Resident);
}

#[test]
fn residency_claim_before_failed_delivery_persist_survives() {
    let record = reconcile_record();
    let store = claiming_store(record.clone());
    let scheduler = Arc::new(FakeScheduler::default());
    let mut deps = CompletionNotifierDeps::new(ScriptedNotifier::new(usize::MAX), store.clone());
    deps.schedule = Some(scheduler.schedule());
    let completion = create_completion_notifier(deps);
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        NotifyResult::Failed
    );
    let persisted = store.get(&record.task_id).expect("record");
    assert_eq!(persisted.notification.notification_failed_epoch, Some(0));
    assert_eq!(persisted.host_pid, Some(4242));
    assert_eq!(persisted.residency_state, ResidencyState::Resident);
}

// ---- completion/notifier-retry.test.ts ------------------------------------------------------------

#[test]
fn first_timer_after_two_failures_persists_delivery_exactly_once() {
    let record = session_record("st_retry");
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(2);
    let scheduler = Arc::new(FakeScheduler::default());
    let completion = retry_notifier(&store, &parent, &scheduler, session("session-a"));
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        NotifyResult::Failed
    );
    scheduler.run(0);
    assert_base_delay(scheduler.delays()[0]);
    assert_eq!(parent.calls().len(), 1);
    let notified = store
        .mutated()
        .iter()
        .filter(|record| record.notification.notified_epoch == 0)
        .count();
    assert_eq!(notified, 1);
}

#[test]
fn revive_before_retry_drops_stale_delivery() {
    let record = session_record("st_retry");
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(2);
    let scheduler = Arc::new(FakeScheduler::default());
    let completion = retry_notifier(&store, &parent, &scheduler, session("session-a"));
    notify(&completion, &record, ParentState::Idle, true);
    let revived = TaskRecord {
        notification: epochs(1, -1, None),
        ..session_record("st_retry")
    };
    store.set(revived.clone());
    scheduler.run(0);
    let next_epoch = notify(&completion, &revived, ParentState::Idle, true);
    assert_eq!(parent.calls().len(), 1);
    assert_eq!(next_epoch, wake());
    assert_eq!(
        store.get("st_retry").map(|r| r.notification.notified_epoch),
        Some(1)
    );
}

#[test]
fn timer_skips_work_already_flushed_from_the_buffer() {
    let record = session_record("st_retry");
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(2);
    let scheduler = Arc::new(FakeScheduler::default());
    let completion = retry_notifier(&store, &parent, &scheduler, session("session-a"));
    notify(&completion, &record, ParentState::Idle, true);
    notify(&completion, &record, ParentState::Compacting, true);
    flush(&completion, "session-a", false);
    scheduler.run(0);
    assert_eq!(parent.calls().len(), 1);
}

#[test]
fn eight_exhausted_timers_cap_the_cycle_and_a_later_reconcile_restarts_fresh() {
    let record = session_record("st_retry");
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(usize::MAX);
    let scheduler = Arc::new(FakeScheduler::default());
    let completion = retry_notifier(&store, &parent, &scheduler, session("session-a"));
    notify(&completion, &record, ParentState::Idle, true);
    for index in 0..8 {
        scheduler.run(index);
    }
    assert_eq!(scheduler.delays().len(), 8);
    assert_eq!(
        store
            .get("st_retry")
            .and_then(|r| r.notification.notification_failed_epoch),
        Some(0)
    );
    reconcile(&completion, "session-a", ParentState::Idle);
    let delays = scheduler.delays();
    assert_eq!(delays.len(), 9);
    assert_base_delay(delays[8]);
}

#[test]
fn mixed_session_failed_records_reconciled_twice_deliver_eligible_work_once() {
    let failed = |task_id: &str| TaskRecord {
        notification: epochs(0, -1, Some(0)),
        ..session_record(task_id)
    };
    let eligible = failed("st_eligible");
    let already_notified = TaskRecord {
        notification: epochs(0, 0, Some(0)),
        ..failed("st_notified")
    };
    let other_session = TaskRecord {
        parent_session_id: "session-b".to_string(),
        ..failed("st_other")
    };
    let non_notifying = TaskRecord {
        status: TaskStatus::Cancelled,
        final_response: None,
        ..failed("st_cancelled")
    };
    let not_failed = session_record("st_clean");
    let store = MemoryStore::with([
        eligible,
        already_notified,
        other_session,
        non_notifying,
        not_failed,
    ]);
    let parent = ScriptedNotifier::new(0);
    let scheduler = Arc::new(FakeScheduler::default());
    let completion = retry_notifier(&store, &parent, &scheduler, session("session-a"));
    reconcile(&completion, "session-a", ParentState::Idle);
    reconcile(&completion, "session-a", ParentState::Idle);
    let calls = parent.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(detail_ids(&calls[0]), vec!["st_eligible"]);
}

#[test]
fn failed_record_reconciled_during_compaction_buffers_normally() {
    let record = TaskRecord {
        notification: epochs(0, -1, Some(0)),
        ..session_record("st_retry")
    };
    let store = MemoryStore::with([record]);
    let parent = ScriptedNotifier::new(0);
    let scheduler = Arc::new(FakeScheduler::default());
    let completion = retry_notifier(&store, &parent, &scheduler, session("session-a"));
    reconcile(&completion, "session-a", ParentState::Compacting);
    assert!(parent.calls().is_empty());
    assert_eq!(completion.buffered_count("session-a"), 1);
}

#[test]
fn rescheduled_retry_delays_follow_the_capped_ladder() {
    let record = session_record("st_retry");
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(usize::MAX);
    let scheduler = Arc::new(FakeScheduler::default());
    let completion = retry_notifier(&store, &parent, &scheduler, session("session-a"));
    notify(&completion, &record, ParentState::Idle, true);
    for index in 0..8 {
        scheduler.run(index);
    }
    let bases = [500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000];
    let delays = scheduler.delays();
    assert_eq!(delays.len(), bases.len());
    for (delay, base) in delays.iter().zip(bases) {
        assert!(
            *delay >= base && *delay <= (base + 199).min(30_000),
            "delay {delay} base {base}"
        );
    }
}

#[test]
fn retry_dropped_for_another_session_restarts_backoff_at_the_base_delay() {
    let record = session_record("st_retry");
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(usize::MAX);
    let scheduler = Arc::new(FakeScheduler::default());
    let current = session("session-b");
    let completion = retry_notifier(&store, &parent, &scheduler, current.clone());
    notify(&completion, &record, ParentState::Idle, true);
    scheduler.run(0);
    *current.lock().unwrap_or_else(PoisonError::into_inner) = "session-a".to_string();
    reconcile(&completion, "session-a", ParentState::Idle);
    assert_base_delay(scheduler.delays()[1]);
}

#[test]
fn retry_owned_by_session_a_drops_while_b_is_current_until_a_reconciles() {
    let record = session_record("st_retry");
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(2);
    let scheduler = Arc::new(FakeScheduler::default());
    let current = session("session-b");
    let completion = retry_notifier(&store, &parent, &scheduler, current.clone());
    notify(&completion, &record, ParentState::Idle, true);
    scheduler.run(0);
    assert!(parent.calls().is_empty());
    assert_eq!(completion.buffered_count("session-a"), 0);
    assert_eq!(
        store
            .get("st_retry")
            .and_then(|r| r.notification.notification_failed_epoch),
        Some(0)
    );
    *current.lock().unwrap_or_else(PoisonError::into_inner) = "session-a".to_string();
    reconcile(&completion, "session-a", ParentState::Idle);
    assert_eq!(parent.calls().len(), 1);
    assert_eq!(
        store.get("st_retry").map(|r| r.notification.notified_epoch),
        Some(0)
    );
}

// ---- completion/unconditional-wake.test.ts --------------------------------------------------------

#[test]
fn idle_parent_background_completion_always_wakes() {
    assert_eq!(route_completion(ParentState::Idle), RoutingDecision::Wake);
}

#[test]
fn idle_parent_end_to_end_enqueues_trigger_turn_message() {
    let record = completed_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    assert_eq!(
        notify(&completion, &record, ParentState::Idle, true),
        wake()
    );
    let calls = parent.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].trigger_turn, Some(true));
}

#[test]
fn streaming_parent_delivery_carries_trigger_turn_for_batched_steer() {
    let record = completed_record();
    let store = MemoryStore::with([record.clone()]);
    let parent = ScriptedNotifier::new(0);
    let completion = notifier_with(&store, &parent);
    assert_eq!(
        notify(&completion, &record, ParentState::Streaming, true),
        NotifyResult::Delivered(DeliveredDecision::DeliverStreaming)
    );
    assert_eq!(parent.calls()[0].trigger_turn, Some(true));
}
