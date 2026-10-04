//! `team/fallback-notification.test.ts`

use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;

use crate::completion::{
    CompletionNotifierDeps, CompletionNotifierStore, CompletionRequest, ParentNotifier,
    ParentNotifierMessage, ParentState, create_completion_notifier,
};
use crate::host::HostError;
use crate::state::{
    ResidencyState, ResolvedModelRecord, ResolvedModelSource, TaskNotification, TaskRecord, TaskStatus,
};
use crate::test_support::{MemoryStore, base_record};

/// Mirrors the TS `completedTeamRecord()` fixture. Nested model records and the
/// notification state are built from the same JSON shape the TS fixture uses.
fn completed_team_record() -> TaskRecord {
    TaskRecord {
        name: Some("team-worker".into()),
        agent_type: Some("worker".into()),
        execution_mode: "process".into(),
        model: "vendor-b/fallback-model".into(),
        requested_model: Some(ResolvedModelRecord::new(
            ResolvedModelSource::Agent,
            "vendor-a",
            "primary-model",
        )),
        resolved_model: Some(ResolvedModelRecord::new(
            ResolvedModelSource::Agent,
            "vendor-b",
            "fallback-model",
        )),
        fallback_models: Some(Vec::new()),
        notify_on_terminal: false,
        status: TaskStatus::Completed,
        residency_state: ResidencyState::Resident,
        created_at: "2026-07-28T08:00:00.000Z".into(),
        updated_at: "2026-07-28T08:00:03.000Z".into(),
        final_response: Some("team worker completed".into()),
        notification: TaskNotification {
            run_epoch: 1,
            notified_epoch: -1,
            notification_failed_epoch: None,
            liveness_notified_epoch: None,
        },
        ..base_record("st_team", "lead-session")
    }
}

fn completed_team_request() -> CompletionRequest {
    CompletionRequest {
        record: completed_team_record(),
        parent_state: ParentState::Streaming,
        run_in_background: true,
        tokens: None,
    }
}

#[derive(Default)]
struct CapturingNotifier {
    messages: Mutex<Vec<ParentNotifierMessage>>,
}

impl ParentNotifier for CapturingNotifier {
    fn enqueue_with_callbacks(&self, message: &ParentNotifierMessage, callbacks: crate::completion::DeliveryCallbacks) -> Result<(), HostError> {
        self.messages.lock().expect("lock").push(message.clone());
        callbacks.delivered();
        Ok(())
    }
}

#[test]
fn given_duplicate_lead_lifecycle_triggers_when_fallback_notification_delivers_then_the_lead_receives_one_factual_reroute()
 {
    // given
    let request = completed_team_request();
    let store = MemoryStore::with([request.record.clone()]);
    let store_dyn: Arc<dyn CompletionNotifierStore> = store.clone();
    let notifier = Arc::new(CapturingNotifier::default());
    let completion =
        create_completion_notifier(CompletionNotifierDeps::new(notifier.clone(), store_dyn));

    // when
    completion.notify_terminal(&request).expect("first notify");
    completion
        .notify_terminal(&completed_team_request())
        .expect("second notify");

    // then
    let messages = notifier.messages.lock().expect("lock");
    assert_eq!(messages.len(), 1);
    let content = &messages[0].content;
    assert_eq!(content.matches("fallback:").count(), 1);
    assert!(content.contains("fallback:vendor-a/primary-model->vendor-b/fallback-model"));
}
