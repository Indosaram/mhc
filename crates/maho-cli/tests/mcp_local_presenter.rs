// senpi packages/coding-agent/test/mcp/presenter-local.test.ts (senpi-interactive-21): the terminal's
// `local_tui` presenter over its own presenter authority.
use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_cli::cli::mcp_local_presenter::register_local_mcp_presenter;
use maho_core::mcp_form_types::{
    McpFormInput, McpFormOutcome, McpFormOutcomeKind, McpFormOutcomeSource, McpFormServer,
    McpServerSource, mark_mcp_form_presented,
};
use maho_ext_api::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

type Select = Arc<
    dyn Fn(&str, &[String], ExtensionUiDialogOptions) -> UiFuture<'static, Option<String>>
        + Send
        + Sync,
>;

/// senpi `mrtrTui(select)`: a terminal whose selector is scripted; `events` would record any generic
/// blocked event (private forms never emit one).
struct ScriptedUi {
    select: Select,
    events: Mutex<Vec<String>>,
}
impl ExtensionUi for ScriptedUi {
    fn select<'a>(
        &'a self,
        title: &'a str,
        choices: &'a [String],
        options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        if !options.ephemeral {
            self.events.lock().unwrap().push(title.to_owned());
        }
        (self.select)(title, choices, options)
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        panic!("private MCP forms never use confirm")
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
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("custom UI must not be used".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

/// Gives a closure the higher-ranked selector signature.
fn select<F>(select: F) -> Select
where
    F: Fn(&str, &[String], ExtensionUiDialogOptions) -> UiFuture<'static, Option<String>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(select)
}

fn tui(select: Select) -> Arc<ScriptedUi> {
    Arc::new(ScriptedUi {
        select,
        events: Mutex::default(),
    })
}

fn mark(options: &ExtensionUiDialogOptions) {
    mark_mcp_form_presented(options.presented.as_ref().map(|key| &key.0));
}

/// Marks the form presented and answers Submit on the review, the first choice elsewhere.
fn submitting() -> Select {
    select(|_, choices, options| {
        mark(&options);
        let answer = if choices.iter().any(|choice| choice == "Submit") {
            "Submit".to_owned()
        } else {
            choices[0].clone()
        };
        Box::pin(async move { Some(answer) })
    })
}

fn input(presentation_signal: AbortSignal) -> McpFormInput {
    McpFormInput {
        operation_id: "operation".into(),
        input_id: "input".into(),
        round: 0,
        server: McpFormServer {
            label: "configured-server".into(),
            source: McpServerSource::User,
        },
        message: "private-message".into(),
        schema: json!({"type": "object", "properties": {}}),
        operation_signal: AbortController::new().signal(),
        operation_current: None,
        presentation_signal,
        deadline_ms: 60_000.,
    }
}

fn counter() -> (Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
    let count = Arc::new(AtomicUsize::new(0));
    let admitted = Arc::clone(&count);
    (
        count,
        Arc::new(move || {
            admitted.fetch_add(1, Ordering::SeqCst);
        }),
    )
}

fn accepted(outcome: &McpFormOutcome) -> bool {
    matches!(&outcome.kind, McpFormOutcomeKind::Result(result) if result["action"] == "accept")
}

#[tokio::test]
async fn the_local_presenter_admits_once_after_complete_form_review_not_per_primitive() {
    let (admission, admit) = counter();
    let observed = Arc::clone(&admission);
    let ui = tui(select(move |_, choices, options| {
        assert_eq!(observed.load(Ordering::SeqCst), 0);
        mark(&options);
        let answer = if choices.iter().any(|choice| choice == "Submit") {
            "Submit".to_owned()
        } else {
            choices[0].clone()
        };
        Box::pin(async move { Some(answer) })
    }));
    let lifetime = AbortController::new();
    let binding = register_local_mcp_presenter(ui.clone(), "session", lifetime.signal(), admit);
    let presentation = (binding.capture)().expect("local presenter");
    let outcome = (presentation.present)(input(AbortController::new().signal())).await;
    assert_eq!(
        outcome.kind,
        McpFormOutcomeKind::Result(json!({"action": "accept", "content": {}}))
    );
    assert_eq!(outcome.source, McpFormOutcomeSource::PresenterAction);
    assert!(outcome.human_admission());
    assert_eq!(admission.load(Ordering::SeqCst), 1);
    assert!(ui.events.lock().unwrap().is_empty());
    lifetime.abort(None);
}

async fn presentation_loss(rendered: bool) {
    let (started_tx, started) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Mutex::new(Some(started_tx));
    let ui = tui(select(move |_, _, options| {
        let signal = options.signal.clone().expect("modal lifetime");
        if rendered {
            mark(&options);
        }
        if let Some(started) = started_tx.lock().unwrap().take() {
            drop(started.send(()));
        }
        Box::pin(async move {
            signal.cancelled().await;
            None
        })
    }));
    let lifetime = AbortController::new();
    let lost = AbortController::new();
    let (admission, admit) = counter();
    let binding = register_local_mcp_presenter(ui.clone(), "session", lifetime.signal(), admit);
    let presentation = (binding.capture)().expect("local presenter");
    let result = tokio::spawn((presentation.present)(input(lost.signal())));
    started.await.expect("the form is shown");
    lost.abort(None);
    let outcome = result.await.expect("present settles");
    if rendered {
        assert_eq!(
            outcome.kind,
            McpFormOutcomeKind::Result(json!({"action": "cancel"}))
        );
    } else {
        assert_eq!(outcome.kind, McpFormOutcomeKind::ProcessingFailure);
    }
    assert!(!outcome.human_admission());
    assert_eq!(admission.load(Ordering::SeqCst), 0);
    assert!(ui.events.lock().unwrap().is_empty());
    lifetime.abort(None);
}

#[tokio::test]
async fn presentation_loss_before_the_form_rendered_is_a_processing_failure() {
    presentation_loss(false).await;
}

#[tokio::test]
async fn presentation_loss_after_the_form_rendered_is_a_host_cancel() {
    presentation_loss(true).await;
}

/// A selector whose first Submit review waits on `submit`; `forms` counts reviews shown.
fn gated(
    forms: Arc<AtomicUsize>,
    shown: tokio::sync::oneshot::Sender<()>,
    submit: tokio::sync::watch::Receiver<bool>,
    hold_every_form: bool,
) -> Select {
    let shown = Mutex::new(Some(shown));
    select(move |_, choices, options| {
        mark(&options);
        if !choices.iter().any(|choice| choice == "Submit") {
            let first = choices[0].clone();
            return Box::pin(async move { Some(first) });
        }
        let count = forms.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(shown) = shown.lock().unwrap().take() {
            drop(shown.send(()));
        }
        let mut submit = submit.clone();
        let hold = hold_every_form || count == 1;
        Box::pin(async move {
            if hold {
                drop(submit.wait_for(|submitted| *submitted).await);
            }
            Some("Submit".to_owned())
        })
    })
}

// senpi#4210: concurrent operations must retain the terminal's FIFO presentation contract.
#[tokio::test]
async fn queues_local_authority_forms_until_the_preceding_form_settles() {
    let forms = Arc::new(AtomicUsize::new(0));
    let (shown_tx, shown) = tokio::sync::oneshot::channel();
    let (submit, submitted) = tokio::sync::watch::channel(false);
    let ui = tui(gated(Arc::clone(&forms), shown_tx, submitted, false));
    let lifetime = AbortController::new();
    let (admission, admit) = counter();
    let binding = register_local_mcp_presenter(ui, "session", lifetime.signal(), admit);
    let presentation = (binding.capture)().expect("local presenter");
    let first = tokio::spawn((presentation.present)(input(lifetime.signal())));
    shown.await.expect("first form shown");
    let mut next = input(lifetime.signal());
    next.operation_id = "second".into();
    let second = tokio::spawn((presentation.present)(next));
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        forms.load(Ordering::SeqCst),
        1,
        "the second form waits its turn"
    );
    submit.send_replace(true);
    assert!(accepted(&first.await.expect("first settles")));
    assert!(accepted(&second.await.expect("second settles")));
    assert_eq!(forms.load(Ordering::SeqCst), 2);
    assert_eq!(admission.load(Ordering::SeqCst), 2);
    lifetime.abort(None);
}

#[tokio::test]
async fn a_failed_local_collector_settles_its_form_without_revoking_future_forms() {
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let ui = tui(select(move |_, choices, options| {
        if fail.swap(false, Ordering::SeqCst) {
            // senpi throws from the selector; here the collector task fails.
            panic!("synthetic collector failure");
        }
        mark(&options);
        let answer = if choices.iter().any(|choice| choice == "Submit") {
            "Submit".to_owned()
        } else {
            choices[0].clone()
        };
        Box::pin(async move { Some(answer) })
    }));
    let lifetime = AbortController::new();
    let (_, admit) = counter();
    let binding = register_local_mcp_presenter(ui, "session", lifetime.signal(), admit);
    let presentation = (binding.capture)().expect("local presenter");
    let failed = (presentation.present)(input(lifetime.signal())).await;
    assert_eq!(failed.kind, McpFormOutcomeKind::ProcessingFailure);
    assert!(!failed.human_admission());
    assert!((binding.capture)().is_some_and(|presentation| (presentation.ready)()));
    assert!(accepted(
        &(presentation.present)(input(lifetime.signal())).await
    ));
    lifetime.abort(None);
}

async fn settles_a_queued_form_without_rendering_it(cause: &str) {
    let forms = Arc::new(AtomicUsize::new(0));
    let (shown_tx, shown) = tokio::sync::oneshot::channel();
    let (submit, submitted) = tokio::sync::watch::channel(false);
    let ui = tui(gated(Arc::clone(&forms), shown_tx, submitted, true));
    let lifetime = AbortController::new();
    let operation = AbortController::new();
    let view = AbortController::new();
    let (_, admit) = counter();
    let binding = register_local_mcp_presenter(ui, "session", lifetime.signal(), admit);
    let presentation = (binding.capture)().expect("local presenter");
    let first = tokio::spawn((presentation.present)(input(lifetime.signal())));
    shown.await.expect("first form shown");
    let mut queued = input(view.signal());
    queued.operation_signal = operation.signal();
    queued.deadline_ms = 10.;
    let second = tokio::spawn((presentation.present)(queued));
    match cause {
        "operation" => operation.abort(None),
        "presentation" => view.abort(None),
        // The paused clock reaches the queued form's 10ms deadline once every task is idle; the
        // other causes fire first, before the runtime is ever idle.
        _ => {}
    }
    let outcome = second.await.expect("queued form settles");
    assert_eq!(
        outcome.kind,
        if cause == "operation" {
            McpFormOutcomeKind::OperationAborted
        } else {
            McpFormOutcomeKind::ProcessingFailure
        }
    );
    assert!(!outcome.human_admission());
    submit.send_replace(true);
    assert!(accepted(&first.await.expect("first settles")));
    assert_eq!(forms.load(Ordering::SeqCst), 1);
    lifetime.abort(None);
}

#[tokio::test(start_paused = true)]
async fn settles_a_queued_form_on_operation_abort_without_rendering_it() {
    settles_a_queued_form_without_rendering_it("operation").await;
}

#[tokio::test(start_paused = true)]
async fn settles_a_queued_form_on_presentation_loss_without_rendering_it() {
    settles_a_queued_form_without_rendering_it("presentation").await;
}

#[tokio::test(start_paused = true)]
async fn settles_a_queued_form_on_timeout_without_rendering_it() {
    settles_a_queued_form_without_rendering_it("timeout").await;
}

#[tokio::test]
async fn an_aborted_lifetime_captures_no_presentation() {
    let lifetime = AbortController::new();
    lifetime.abort(None);
    let (_, admit) = counter();
    let binding =
        register_local_mcp_presenter(tui(submitting()), "session", lifetime.signal(), admit);
    assert!((binding.capture)().is_none());
}
