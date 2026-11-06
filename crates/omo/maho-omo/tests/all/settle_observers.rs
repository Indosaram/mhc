use maho_ext_api::{
    ExtensionContext, ExtensionMode, ExtensionUi, ExtensionUiDialogOptions, JsonValue, Model,
    ModelRegistry, NotificationType, SessionEntry, SessionManager, Theme, ToolSessionManager,
    UiFuture,
};
use std::sync::{Arc, Mutex};

struct TestSession(String);
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str {
        &self.0
    }
    fn session_file(&self) -> Option<&std::path::Path> {
        None
    }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

struct TestModels;
impl ModelRegistry for TestModels {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(
        &'a self,
        _: &'a str,
    ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Ok(JsonValue::Null) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

fn test_context(session_id: &str) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(TestUi),
        mode: ExtensionMode::Tui,
        has_ui: true,
        cwd: std::path::PathBuf::from("/workspace"),
        agent_dir: std::path::PathBuf::from("/workspace/.omo"),
        session_manager: Arc::new(TestSession(session_id.to_string())),
        model_registry: Arc::new(TestModels),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(maho_ext_api::BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        logger: None,
        idle_coordinator: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{
    ComponentLogger, EventResult, ExtensionEvent, ExtensionFailure, ExtensionHandler,
};
use maho_omo::settle_observers::{SettleObservers, current_settle_observer};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Warnings(Mutex<Vec<(String, Option<JsonValue>)>>);
impl ComponentLogger for Warnings {
    fn info(&self, _: &str, _: Option<&JsonValue>) {}
    fn warn(&self, message: &str, details: Option<&JsonValue>) {
        self.0
            .lock()
            .expect("settle observer fixture")
            .push((message.to_owned(), details.cloned()));
    }
    fn error(&self, _: &str, _: Option<&JsonValue>) {}
}

fn handler(
    body: impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    + Send
    + Sync
    + 'static,
) -> ExtensionHandler {
    Arc::new(move |_, _| {
        let work = body();
        Box::pin(async move {
            work.await;
            Ok(EventResult::None)
        })
    })
}

#[tokio::test]
async fn an_expired_budget_logs_the_stalled_phase_and_aborts_the_signal() {
    let warnings = Arc::new(Warnings::default());
    let observers =
        SettleObservers::with_budget(warnings.clone(), std::time::Duration::from_millis(30));
    let (signal_tx, signal_rx) = std::sync::mpsc::channel::<AbortSignal>();
    let signal_tx = Mutex::new(signal_tx);
    let wrapped = observers.wrap(
        "memory",
        Some("dreamTrigger"),
        handler(move || {
            let observer = current_settle_observer().expect("observer");
            observer
                .phase("awaiting-dream")
                .expect("settle observer fixture");
            signal_tx
                .lock()
                .expect("settle observer fixture")
                .send(observer.signal().clone())
                .expect("settle observer fixture");
            Box::pin(std::future::pending())
        }),
    );
    let context = test_context("s-1");
    let mut event = ExtensionEvent::AgentSettled;
    assert!(matches!(
        wrapped(&mut event, &context).await,
        Ok(EventResult::None)
    ));
    assert!(signal_rx.recv().expect("settle observer fixture").aborted());
    assert_eq!(observers.pending_count(), 0);
    let warnings = warnings.0.lock().expect("settle observer fixture");
    assert_eq!(warnings.len(), 1);
    let details = warnings[0].1.as_ref().expect("settle observer fixture");
    assert_eq!(warnings[0].0, "extension_handler_stalled");
    assert_eq!(details["hook"], "awaiting-dream");
    assert_eq!(details["component"], "memory");
    assert_eq!(details["sessionId"], "s-1");
}

#[tokio::test]
async fn retiring_a_session_ends_its_observer_and_leaves_other_sessions_running() {
    let warnings = Arc::new(Warnings::default());
    let observers =
        SettleObservers::with_budget(warnings.clone(), std::time::Duration::from_secs(60));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let tx = Mutex::new(tx);
    let wrapped = observers.wrap(
        "memory",
        None,
        handler(move || {
            tx.lock()
                .expect("settle observer fixture")
                .send(())
                .expect("settle observer fixture");
            Box::pin(std::future::pending())
        }),
    );
    let context = test_context("a");
    let mut event = ExtensionEvent::AgentSettled;
    let retire = async {
        rx.recv().await.expect("settle observer fixture");
        observers.retire(Some("other"));
        assert_eq!(
            observers.pending_count(),
            1,
            "another session's retirement leaves this observer"
        );
        observers.retire(Some("a"));
    };
    let (result, ()) = tokio::join!(wrapped(&mut event, &context), retire);
    assert!(matches!(result, Ok(EventResult::None)));
    assert_eq!(observers.pending_count(), 0);
    assert!(
        warnings
            .0
            .lock()
            .expect("settle observer fixture")
            .is_empty(),
        "retirement is not a stall"
    );
}

#[tokio::test]
async fn concurrent_settles_sharing_work_when_observer_cancels_absorbs_cancellation() {
    let warnings = Arc::new(Warnings::default());
    let observers =
        SettleObservers::with_budget(warnings.clone(), std::time::Duration::from_secs(60));

    let (entered1_tx, entered1_rx) = tokio::sync::oneshot::channel::<AbortSignal>();
    let (entered2_tx, entered2_rx) = tokio::sync::oneshot::channel::<AbortSignal>();
    let entered1_tx = Mutex::new(Some(entered1_tx));
    let entered2_tx = Mutex::new(Some(entered2_tx));

    let (shared_tx, _shared_rx) =
        tokio::sync::watch::channel::<Option<Result<(), ExtensionFailure>>>(None);

    let calls = Arc::new(AtomicUsize::new(0));

    let calls_clone = calls.clone();
    let shared_tx_clone = shared_tx.clone();
    let reentrant = observers.clone();
    let handler_fn: ExtensionHandler = Arc::new(
        move |_event: &mut ExtensionEvent, _context: &ExtensionContext| {
            let call = calls_clone.fetch_add(1, Ordering::SeqCst);
            let observer = current_settle_observer().expect("observer");
            let mut rx = shared_tx_clone.subscribe();
            if call == 0 {
                let shared_tx_inner = shared_tx_clone.clone();
                let reentrant = reentrant.clone();
                observer.signal().add_abort_listener(move |reason| {
                    assert_eq!(reentrant.pending_count(), 2);
                    shared_tx_inner
                        .send(Some(Err(SettleObservers::cancellation_failure(reason))))
                        .expect("shared cancellation receiver");
                });
                if let Some(tx) = entered1_tx.lock().expect("settle observer fixture").take() {
                    let _ = tx.send(observer.signal().clone());
                }
            } else if let Some(tx) = entered2_tx.lock().expect("settle observer fixture").take() {
                let _ = tx.send(observer.signal().clone());
            }

            Box::pin(async move {
                loop {
                    if let Some(res) = &*rx.borrow_and_update() {
                        return match res {
                            Ok(()) => Ok(EventResult::None),
                            Err(err) => Err(err.clone()),
                        };
                    }
                    if rx.changed().await.is_err() {
                        return Ok(EventResult::None);
                    }
                }
            })
        },
    );

    let wrapped1 = observers.wrap("fixture", Some("first"), handler_fn.clone());
    let wrapped2 = observers.wrap("fixture", Some("second"), handler_fn);

    let task1 = {
        let wrapped1 = wrapped1.clone();
        tokio::spawn(async move {
            let mut event = ExtensionEvent::AgentSettled;
            let context = test_context("first");
            wrapped1(&mut event, &context).await
        })
    };

    let signal1 = entered1_rx.await.expect("task1 entered");

    let task2 = {
        let wrapped2 = wrapped2.clone();
        tokio::spawn(async move {
            let mut event = ExtensionEvent::AgentSettled;
            let context = test_context("second");
            wrapped2(&mut event, &context).await
        })
    };

    let signal2 = entered2_rx.await.expect("task2 entered");

    observers.retire(Some("first"));
    let (res1, res2) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(task1, task2)
    })
    .await
    .expect("both observers settle after explicit retirement");
    let res1 = res1.expect("task1 joined");
    let res2 = res2.expect("task2 joined");

    assert!(signal1.aborted(), "first session was retired");
    assert!(!signal2.aborted(), "task2 observer should not have aborted");
    assert!(
        matches!(res1, Ok(EventResult::None)),
        "first settle absorbs its own retirement"
    );
    assert!(
        matches!(res2, Ok(EventResult::None)),
        "second settle absorbs the known observer cancellation from first"
    );
    let reason = signal1.reason().expect("retirement reason");
    let known = SettleObservers::cancellation_failure(&reason);
    assert!(observers.is_cancellation(&known.clone()));
    assert!(!SettleObservers::new(warnings).is_cancellation(&known));
    let unrelated = ExtensionFailure::classified(reason.message, Some(reason.name), None);
    assert!(!observers.is_cancellation(&unrelated));
    let expected = unrelated.clone();
    let wrapped = observers.wrap(
        "fixture",
        None,
        Arc::new(move |_, _| {
            let unrelated = unrelated.clone();
            Box::pin(async move { Err(unrelated) })
        }),
    );
    let mut event = ExtensionEvent::AgentSettled;
    let context = test_context("unrelated");
    assert!(matches!(wrapped(&mut event, &context).await, Err(error) if error == expected));
}

#[tokio::test]
async fn concurrent_settles_sharing_work_when_unrelated_fails_propagates_error() {
    let warnings = Arc::new(Warnings::default());
    let observers =
        SettleObservers::with_budget(warnings.clone(), std::time::Duration::from_secs(10));

    let (entered1_tx, entered1_rx) = tokio::sync::oneshot::channel::<AbortSignal>();
    let (entered2_tx, entered2_rx) = tokio::sync::oneshot::channel::<AbortSignal>();
    let entered1_tx = Mutex::new(Some(entered1_tx));
    let entered2_tx = Mutex::new(Some(entered2_tx));

    let (shared_tx, _shared_rx) =
        tokio::sync::watch::channel::<Option<Result<(), ExtensionFailure>>>(None);

    let calls = Arc::new(AtomicUsize::new(0));

    let calls_clone = calls.clone();
    let shared_tx_clone = shared_tx.clone();
    let handler_fn: ExtensionHandler = Arc::new(
        move |_event: &mut ExtensionEvent, _context: &ExtensionContext| {
            let call = calls_clone.fetch_add(1, Ordering::SeqCst);
            let observer = current_settle_observer().expect("observer");
            let mut rx = shared_tx_clone.subscribe();
            if call == 0 {
                let shared_tx_inner = shared_tx_clone.clone();
                observer.signal().add_abort_listener(move |reason| {
                    let _ = shared_tx_inner
                        .send(Some(Err(SettleObservers::cancellation_failure(reason))));
                });
                if let Some(tx) = entered1_tx.lock().expect("settle observer fixture").take() {
                    let _ = tx.send(observer.signal().clone());
                }
            } else if let Some(tx) = entered2_tx.lock().expect("settle observer fixture").take() {
                let _ = tx.send(observer.signal().clone());
            }

            Box::pin(async move {
                loop {
                    if let Some(res) = &*rx.borrow_and_update() {
                        return match res {
                            Ok(()) => Ok(EventResult::None),
                            Err(err) => Err(err.clone()),
                        };
                    }
                    if rx.changed().await.is_err() {
                        return Ok(EventResult::None);
                    }
                }
            })
        },
    );

    let wrapped1 = observers.wrap("fixture", Some("first"), handler_fn.clone());
    let wrapped2 = observers.wrap("fixture", Some("second"), handler_fn);

    let task1 = {
        let wrapped1 = wrapped1.clone();
        tokio::spawn(async move {
            let mut event = ExtensionEvent::AgentSettled;
            let context = test_context("session");
            wrapped1(&mut event, &context).await
        })
    };

    let signal1 = entered1_rx.await.expect("task1 entered");

    let task2 = {
        let wrapped2 = wrapped2.clone();
        tokio::spawn(async move {
            let mut event = ExtensionEvent::AgentSettled;
            let context = test_context("session");
            wrapped2(&mut event, &context).await
        })
    };

    let signal2 = entered2_rx.await.expect("task2 entered");

    let failure = ExtensionFailure::classified(
        "independent timeout",
        Some("TimeoutError".to_string()),
        None,
    );
    shared_tx
        .send(Some(Err(failure.clone())))
        .expect("send failure");

    let (res1, res2) = tokio::join!(task1, task2);
    let res1 = res1.expect("task1 joined");
    let res2 = res2.expect("task2 joined");

    assert!(!signal1.aborted(), "task1 should not be aborted");
    assert!(!signal2.aborted(), "task2 should not be aborted");
    assert!(
        matches!(res1, Err(ref err) if err.message == "independent timeout"),
        "task1 propagates unrelated error"
    );
    assert!(
        matches!(res2, Err(ref err) if err.message == "independent timeout"),
        "task2 propagates unrelated error"
    );
}

#[tokio::test]
async fn named_subscriber_that_never_resolves_aborts_and_next_subscriber_executes() {
    let warnings = Arc::new(Warnings::default());
    let observers =
        SettleObservers::with_budget(warnings.clone(), std::time::Duration::from_millis(30));

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<AbortSignal>();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (_release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let release_rx = Mutex::new(Some(release_rx));

    let sub1 = observers.wrap(
        "fixture",
        Some("pending-write"),
        Arc::new(move |_, _| {
            let observer = current_settle_observer().expect("observer");
            if let Some(tx) = entered_tx.lock().expect("settle observer fixture").take() {
                let _ = tx.send(observer.signal().clone());
            }
            let rx = release_rx.lock().expect("settle observer fixture").take();
            Box::pin(async move {
                if let Some(rx) = rx {
                    let _ = rx.await;
                }
                Ok(EventResult::None)
            })
        }),
    );

    let next_counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = next_counter.clone();
    let sub2 = observers.wrap(
        "fixture",
        Some("following"),
        Arc::new(move |_, _| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(EventResult::None) })
        }),
    );

    let mut event = ExtensionEvent::AgentSettled;
    let context = test_context("session");

    let res1 = sub1(&mut event, &context).await;
    let res2 = sub2(&mut event, &context).await;

    let signal1 = entered_rx.await.expect("sub1 entered");
    assert!(signal1.aborted(), "sub1 signal should be aborted");
    assert!(
        matches!(res1, Ok(EventResult::None)),
        "timed-out subscriber returns Ok(EventResult::None)"
    );
    assert!(
        matches!(res2, Ok(EventResult::None)),
        "subsequent subscriber executes and returns Ok"
    );
    assert_eq!(next_counter.load(Ordering::SeqCst), 1);

    let warnings = warnings.0.lock().expect("settle observer fixture");
    assert_eq!(warnings.len(), 1);
    let details = warnings[0].1.as_ref().expect("settle observer fixture");
    assert_eq!(warnings[0].0, "extension_handler_stalled");
    assert_eq!(details["component"], "fixture");
    assert_eq!(details["hook"], "pending-write");
    assert_eq!(details["sessionId"], "session");
    assert_eq!(details["handlerEvent"], "agent_settled");
    assert_eq!(details["stage"], "timeout");
}

#[tokio::test]
async fn pending_settle_when_session_retires_releases_immediately_and_handles_late_rejection() {
    let warnings = Arc::new(Warnings::default());
    let observers =
        SettleObservers::with_budget(warnings.clone(), std::time::Duration::from_secs(60));

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<AbortSignal>();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) =
        tokio::sync::oneshot::channel::<Result<EventResult, ExtensionFailure>>();
    let release_rx = Mutex::new(Some(release_rx));

    struct DropGuard(Arc<AtomicBool>);
    impl Drop for DropGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let future_dropped = Arc::new(AtomicBool::new(false));
    let dropped_clone = future_dropped.clone();

    let wrapped = observers.wrap(
        "fixture",
        Some("pending"),
        Arc::new(move |_, _| {
            let observer = current_settle_observer().expect("observer");
            if let Some(tx) = entered_tx.lock().expect("settle observer fixture").take() {
                let _ = tx.send(observer.signal().clone());
            }
            let guard = DropGuard(dropped_clone.clone());
            let rx = release_rx.lock().expect("settle observer fixture").take();
            Box::pin(async move {
                let _guard = guard;
                if let Some(rx) = rx {
                    rx.await.unwrap_or(Ok(EventResult::None))
                } else {
                    Ok(EventResult::None)
                }
            })
        }),
    );

    let dispatch = tokio::spawn(async move {
        let mut event = ExtensionEvent::AgentSettled;
        let context = test_context("retiring");
        wrapped(&mut event, &context).await
    });

    let signal = entered_rx.await.expect("entered");
    assert!(
        !signal.aborted(),
        "observer should not be aborted before retire"
    );

    observers.retire(Some("retiring"));

    let result = dispatch.await.expect("dispatch task completed");
    assert!(
        matches!(result, Ok(EventResult::None)),
        "retiring session releases observation immediately"
    );
    assert!(signal.aborted(), "observer signal is aborted on retirement");

    assert!(
        future_dropped.load(Ordering::SeqCst),
        "borrowed future was dropped when observation was retired"
    );

    let send_res = release_tx.send(Err(ExtensionFailure::new("late I/O failure")));
    assert!(
        send_res.is_err(),
        "receiver was already dropped with the cancelled future, preventing unhandled error"
    );

    assert!(
        warnings
            .0
            .lock()
            .expect("settle observer fixture")
            .is_empty(),
        "retirement is not a stall warning"
    );
}
