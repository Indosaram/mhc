use std::sync::Arc;
use maho_ext_api::{ExtensionUi, Theme};
use maho_interactive::interactive_extension_ui::{InteractiveExtensionUi, UiRequest};

#[test]
fn request_render_from_worker_thread_enqueues_exactly_one_widget_frame() {
    let (ui, mut receiver) = InteractiveExtensionUi::channel(Theme::default());
    let worker: Arc<dyn ExtensionUi> = ui;
    std::thread::spawn(move || worker.request_render())
        .join()
        .expect("the repaint worker thread must not panic")
        .expect("the interactive context must accept the repaint request");
    assert!(matches!(receiver.try_recv(), Ok(UiRequest::WidgetFrame)), "the request must arrive as a WidgetFrame");
    assert!(receiver.try_recv().is_err(), "one request must enqueue exactly one frame");
}

#[test]
fn request_render_on_closed_channel_reports_closure() {
    let (ui, receiver) = InteractiveExtensionUi::channel(Theme::default());
    drop(receiver);
    let failure = ui.request_render().expect_err("a closed request channel must not report success");
    assert!(failure.message.contains("closed"), "closure reason is carried on the failure: {}", failure.message);
}
