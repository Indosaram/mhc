use maho_ext_api::*;
use maho_omo_task::parent_notifier::{CompletionCoordinator, TaskParentNotifier};
use senpi_task::{
    completion::{ParentNotifier, ParentNotifierMessage},
    host::HostError,
};
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
struct Coordinator {
    calls: Mutex<Vec<String>>,
    messages: Mutex<Vec<ParentNotifierMessage>>,
}
impl CompletionCoordinator for Coordinator {
    fn enqueue(
        &self,
        key: &str,
        source: &str,
        message: &ParentNotifierMessage,
        _callbacks: senpi_task::completion::DeliveryCallbacks,
    ) -> Result<bool, HostError> {
        assert_eq!(key, "task-completion:st_completed");
        assert_eq!(source, "task-completion");
        self.messages
            .lock()
            .expect("messages")
            .push(message.clone());
        self.calls.lock().expect("calls").push("enqueue".into());
        Ok(true)
    }
    fn schedule_flush(&self) {
        self.calls.lock().expect("calls").push("schedule".into());
    }
    fn flush_soon(&self) {
        self.calls.lock().expect("calls").push("soon".into());
    }
}
fn message() -> ParentNotifierMessage {
    ParentNotifierMessage {
        custom_type: "senpi-task.completion",
        content: "done".into(),
        display: false,
        details: vec![],
        trigger_turn: Some(true),
    }
}
fn completion_message(ids: &[&str]) -> ParentNotifierMessage {
    let mut message = message();
    message.details = ids.iter().map(|id| senpi_task::completion::CompletionDetails {
        parent_model: None,
        parent_model_fallback: None,
        run_ref: None,
        task_id: (*id).into(),
        name: (*id).into(),
        status: senpi_task::state::TaskStatus::Completed,
        category: None,
        agent_type: None,
        model: "faux/faux".into(),
        requested_model: None,
        fallback_models: None,
        resolved_model: None,
        duration_ms: 1000,
        tokens: None,
        run_stats: None,
        final_response: "finished".into(),
        final_response_file: None,
        continuation_hint: "continue".into(),
    }).collect();
    message
}
#[test]
fn direct_fallback_preserves_hidden_renderer_channel_and_steers() {
    let actions = Arc::new(Actions::default());
    let notifier = TaskParentNotifier {
        actions: actions.clone(),
        coordinator: None,
        is_streaming: Arc::new(|| false),
        on_delivery_failed: None,
    };
    notifier.enqueue(&message()).expect("enqueue");
    let sent = actions.0.lock().expect("messages");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.custom_type, "senpi-task.completion");
    assert!(!sent[0].0.display);
    assert!(sent[0].1.trigger_turn);
    assert_eq!(sent[0].1.deliver_as, Some(DeliverAs::Steer));
}
#[test]
fn streaming_parent_schedules_coordinator_without_direct_delivery() {
    let actions = Arc::new(Actions::default());
    let coordinator = Arc::new(Coordinator::default());
    let notifier = TaskParentNotifier {
        actions: actions.clone(),
        coordinator: Some(coordinator.clone()),
        is_streaming: Arc::new(|| true),
        on_delivery_failed: None,
    };
    notifier
        .enqueue(&completion_message(&["st_completed"]))
        .expect("enqueue");
    assert!(
        actions.0.lock().expect("messages").is_empty(),
        "a coordinated delivery never steers directly"
    );
    assert_eq!(*coordinator.calls.lock().expect("calls"), ["enqueue", "schedule"]);
    assert_eq!(coordinator.messages.lock().expect("messages").len(), 1);
}
#[test]
fn idle_parent_requests_soon_flush_after_enqueue() {
    let coordinator = Arc::new(Coordinator::default());
    let notifier = TaskParentNotifier {
        actions: Arc::new(Actions::default()),
        coordinator: Some(coordinator.clone()),
        is_streaming: Arc::new(|| false),
        on_delivery_failed: None,
    };
    notifier
        .enqueue(&completion_message(&["st_completed"]))
        .expect("enqueue");
    assert_eq!(*coordinator.calls.lock().expect("calls"), ["enqueue", "soon"]);
    assert_eq!(coordinator.messages.lock().expect("messages").len(), 1);
}
#[test]
fn structured_completion_ids_form_stable_coordinator_key_and_preserve_details() {
    #[derive(Default)]
    struct Capture(Mutex<Vec<(String, ParentNotifierMessage)>>);
    impl CompletionCoordinator for Capture {
        fn enqueue(
            &self,
            key: &str,
            source: &str,
            message: &ParentNotifierMessage,
            _callbacks: senpi_task::completion::DeliveryCallbacks,
        ) -> Result<bool, HostError> {
            assert_eq!(source, "task-completion");
            self.0
                .lock()
                .expect("captured")
                .push((key.into(), message.clone()));
            Ok(true)
        }
        fn schedule_flush(&self) {}
        fn flush_soon(&self) {}
    }
    let actions = Arc::new(Actions::default());
    let coordinator = Arc::new(Capture::default());
    let notifier = TaskParentNotifier {
        actions: actions.clone(),
        coordinator: Some(coordinator.clone()),
        is_streaming: Arc::new(|| true),
        on_delivery_failed: None,
    };
    let message = completion_message(&["st_first", "st_second"]);
    notifier.enqueue(&message).expect("enqueue");
    let captured = coordinator.0.lock().expect("captured");
    assert_eq!(captured.len(), 2, "one keyed injection per completion");
    assert_eq!(captured[0].0, "task-completion:st_first");
    assert_eq!(captured[1].0, "task-completion:st_second");
    assert_eq!(captured[0].1.details.len(), 1);
    assert_eq!(captured[0].1.details[0].task_id, "st_first");
    assert_eq!(captured[1].1.details[0].task_id, "st_second");
    assert!(actions.0.lock().expect("messages").is_empty());
}
#[test]
fn direct_and_coordinated_delivery_settle_the_callback_exactly_once() {
    use senpi_task::completion::{DeliveryCallbacks, DeliveryState};
    let actions = Arc::new(Actions::default());
    let direct = TaskParentNotifier {
        actions: actions.clone(),
        coordinator: None,
        is_streaming: Arc::new(|| false),
        on_delivery_failed: None,
    };
    let callbacks = DeliveryCallbacks::new(|_| {});
    direct
        .enqueue_with_callbacks(&message(), callbacks.clone())
        .expect("direct enqueue");
    assert_eq!(callbacks.state(), DeliveryState::Delivered);
    callbacks.failed(HostError {
        message: "late rejection".into(),
    });
    assert_eq!(callbacks.state(), DeliveryState::Delivered);
    #[derive(Default)]
    struct Settling(Mutex<Vec<String>>);
    impl CompletionCoordinator for Settling {
        fn enqueue(
            &self,
            _: &str,
            _: &str,
            _: &ParentNotifierMessage,
            callbacks: senpi_task::completion::DeliveryCallbacks,
        ) -> Result<bool, HostError> {
            self.0.lock().expect("calls").push("enqueue".into());
            callbacks.delivered();
            Ok(true)
        }
        fn schedule_flush(&self) {}
        fn flush_soon(&self) {}
    }
    let coordinator = Arc::new(Settling::default());
    let forwarded = DeliveryCallbacks::new(|_| {});
    // A fresh collector: the direct delivery above already steered into `actions`, so only a
    // collector used solely by the coordinated notifier can prove it takes no direct action.
    let coordinated_actions = Arc::new(Actions::default());
    let notifier = TaskParentNotifier {
        actions: coordinated_actions.clone(),
        coordinator: Some(coordinator.clone()),
        is_streaming: Arc::new(|| true),
        on_delivery_failed: None,
    };
    notifier
        .enqueue_with_callbacks(&completion_message(&["st_completed"]), forwarded.clone())
        .expect("coordinated enqueue");
    assert_eq!(forwarded.state(), DeliveryState::Delivered);
    assert_eq!(
        *coordinator.0.lock().expect("calls"),
        ["enqueue".to_string()]
    );
    assert!(
        coordinated_actions.0.lock().expect("messages").is_empty(),
        "coordinated delivery must not steer directly"
    );
}
#[test]
fn different_runs_keep_their_own_receipts_and_failures() {
    use senpi_task::completion::{DeliveryCallbacks, DeliveryState};
    struct PerKey(Mutex<Vec<(String, senpi_task::completion::DeliveryCallbacks)>>);
    impl CompletionCoordinator for PerKey {
        fn enqueue(
            &self,
            key: &str,
            _: &str,
            _: &ParentNotifierMessage,
            callbacks: senpi_task::completion::DeliveryCallbacks,
        ) -> Result<bool, HostError> {
            self.0.lock().expect("keys").push((key.into(), callbacks));
            Ok(true)
        }
        fn schedule_flush(&self) {}
        fn flush_soon(&self) {}
    }
    let failures = Arc::new(Mutex::new(Vec::<(Vec<String>, String)>::new()));
    let hook = failures.clone();
    let coordinator = Arc::new(PerKey(Mutex::new(Vec::new())));
    let notifier = TaskParentNotifier {
        actions: Arc::new(Actions::default()),
        coordinator: Some(coordinator.clone()),
        is_streaming: Arc::new(|| false),
        on_delivery_failed: Some(Arc::new(move |ids, error| {
            hook.lock().expect("failures").push((ids.to_vec(), error))
        })),
    };
    let mut message = message();
    message.details = ["st_a", "st_b"]
        .into_iter()
        .map(|id| senpi_task::completion::CompletionDetails {
            parent_model: None,
            parent_model_fallback: None,
            run_ref: None,
            task_id: id.into(),
            name: id.into(),
            status: senpi_task::state::TaskStatus::Completed,
            category: None,
            agent_type: None,
            model: "faux/faux".into(),
            requested_model: None,
            fallback_models: None,
            resolved_model: None,
            duration_ms: 1000,
            tokens: None,
            run_stats: None,
            final_response: "finished".into(),
            final_response_file: None,
            continuation_hint: "continue".into(),
        })
        .collect();
    let caller = DeliveryCallbacks::new(|_| {});
    notifier
        .enqueue_with_callbacks(&message, caller.clone())
        .expect("enqueue");
    let receipts = coordinator.0.lock().expect("keys");
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].0, "task-completion:st_a");
    assert_eq!(receipts[1].0, "task-completion:st_b");
    // One run's failure is reported for THAT run only, and the caller's receipt still settles once.
    receipts[0].1.failed(HostError {
        message: "st_a failed".into(),
    });
    assert_eq!(receipts[0].1.state(), DeliveryState::Failed);
    assert_eq!(receipts[1].1.state(), DeliveryState::Pending);
    assert_eq!(caller.state(), DeliveryState::Pending);
    receipts[1].1.delivered();
    assert_eq!(caller.state(), DeliveryState::Delivered);
    let failures = failures.lock().expect("failures");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, vec!["st_a".to_string()]);
}

#[test]
fn a_retired_coordinator_refuses_instead_of_claiming_delivery() {
    struct Retired;
    impl CompletionCoordinator for Retired {
        fn enqueue(
            &self,
            _: &str,
            _: &str,
            _: &ParentNotifierMessage,
            _: senpi_task::completion::DeliveryCallbacks,
        ) -> Result<bool, HostError> {
            Ok(false)
        }
        fn schedule_flush(&self) {}
        fn flush_soon(&self) {}
    }
    let failures = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let hook = failures.clone();
    let notifier = TaskParentNotifier {
        actions: Arc::new(Actions::default()),
        coordinator: Some(Arc::new(Retired)),
        is_streaming: Arc::new(|| true),
        on_delivery_failed: Some(Arc::new(move |ids, _| {
            hook.lock().expect("failures").push(ids.to_vec())
        })),
    };
    let mut message = message();
    message.details = ["st_retired"]
        .into_iter()
        .map(|id| senpi_task::completion::CompletionDetails {
            parent_model: None,
            parent_model_fallback: None,
            run_ref: None,
            task_id: id.into(),
            name: id.into(),
            status: senpi_task::state::TaskStatus::Completed,
            category: None,
            agent_type: None,
            model: "faux/faux".into(),
            requested_model: None,
            fallback_models: None,
            resolved_model: None,
            duration_ms: 1000,
            tokens: None,
            run_stats: None,
            final_response: "finished".into(),
            final_response_file: None,
            continuation_hint: "continue".into(),
        })
        .collect();
    let error = notifier
        .enqueue(&message)
        .expect_err("a retired coordinator refuses");
    assert!(error.message.contains("retired"));
    // The task is named so its notified_epoch is never persisted for a delivery nobody took.
    assert_eq!(
        *failures.lock().expect("failures"),
        vec![vec!["st_retired".to_string()]]
    );
}
