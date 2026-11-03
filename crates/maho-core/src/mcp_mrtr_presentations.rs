//! Port of senpi packages/coding-agent/src/core/mcp-mrtr-presentations.ts.
//!
//! Local host references only. RPC capabilities and serialized UI objects cannot enter this registry.
//!
//! PERF-DIVERGENCE(senpi WeakMap): upstream keys presentations by UI object identity in a WeakMap.
//! Here a UI is an `Arc<dyn ExtensionUi>`; entries are keyed by its data pointer and hold a `Weak`,
//! so a dropped UI (or a reused address) never resolves to a stale presentation. Dead entries are
//! pruned on every registration.

use crate::mcp_form_types::McpFormBinding;
use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::types::ExtensionUi;
use std::sync::{Arc, Mutex, OnceLock, Weak};

pub type PresentationCallback = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct McpMrtrPresentation {
    pub ui: Arc<dyn ExtensionUi>,
    pub signal: AbortSignal,
    pub admit: Option<PresentationCallback>,
    pub presented: Option<PresentationCallback>,
    pub private_forms: Option<McpFormBinding>,
}

struct Entry {
    ui: Weak<dyn ExtensionUi>,
    presentation: McpMrtrPresentation,
}

static PRESENTATIONS: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();

fn presentations() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    PRESENTATIONS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn key(ui: &Arc<dyn ExtensionUi>) -> *const () {
    Arc::as_ptr(ui).cast::<()>()
}

fn set(ui: &Arc<dyn ExtensionUi>, presentation: McpMrtrPresentation) {
    let mut entries = presentations();
    entries
        .retain(|entry| entry.ui.strong_count() > 0 && entry.ui.as_ptr().cast::<()>() != key(ui));
    entries.push(Entry {
        ui: Arc::downgrade(ui),
        presentation,
    });
}

pub fn register_mcp_mrtr_presentation(
    ui: Arc<dyn ExtensionUi>,
    signal: AbortSignal,
    admit: Option<PresentationCallback>,
    private_forms: Option<McpFormBinding>,
) -> Arc<dyn ExtensionUi> {
    set(
        &ui,
        McpMrtrPresentation {
            ui: ui.clone(),
            signal,
            admit,
            presented: None,
            private_forms,
        },
    );
    ui
}

pub fn get_mcp_mrtr_presentation(ui: &Arc<dyn ExtensionUi>) -> Option<McpMrtrPresentation> {
    presentations()
        .iter()
        .find(|entry| entry.ui.strong_count() > 0 && entry.ui.as_ptr().cast::<()>() == key(ui))
        .map(|entry| entry.presentation.clone())
        .filter(|presentation| !presentation.signal.aborted())
}

/// Preserve host identity across runner wrapping; private dialogs still use the raw host UI.
pub fn inherit_mcp_mrtr_presentation(
    source: &Arc<dyn ExtensionUi>,
    wrapper: Arc<dyn ExtensionUi>,
) -> Arc<dyn ExtensionUi> {
    if let Some(presentation) = get_mcp_mrtr_presentation(source) {
        set(&wrapper, presentation);
    }
    wrapper
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use maho_ai::utils::abort::AbortController;
    use maho_ext_api::*;

    pub(crate) struct Ui;
    impl ExtensionUi for Ui {
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
        fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<ComponentFactory>) {}
        fn set_footer(&self, _: Option<ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String {
            String::new()
        }
        fn custom(
            &self,
            _: ComponentFactory,
            _: CustomUiOptions,
        ) -> ExtensionFuture<'_, JsonValue> {
            Box::pin(async { Err(ExtensionFailure::new("UI not available")) })
        }
        fn theme(&self) -> Theme {
            Theme::default()
        }
    }

    #[test]
    fn a_wrapper_inherits_the_host_presentation_and_an_abort_hides_it() {
        let host: Arc<dyn ExtensionUi> = Arc::new(Ui);
        let wrapper: Arc<dyn ExtensionUi> = Arc::new(Ui);
        let abort = AbortController::new();
        register_mcp_mrtr_presentation(host.clone(), abort.signal(), None, None);
        assert!(get_mcp_mrtr_presentation(&wrapper).is_none());
        let wrapper = inherit_mcp_mrtr_presentation(&host, wrapper);
        let inherited = get_mcp_mrtr_presentation(&wrapper).expect("inherited");
        assert!(Arc::ptr_eq(&inherited.ui, &host));
        abort.abort(None);
        assert!(get_mcp_mrtr_presentation(&host).is_none());
        assert!(get_mcp_mrtr_presentation(&wrapper).is_none());
    }

    #[test]
    fn an_unregistered_source_leaves_the_wrapper_bare() {
        let source: Arc<dyn ExtensionUi> = Arc::new(Ui);
        let wrapper = inherit_mcp_mrtr_presentation(&source, Arc::new(Ui));
        assert!(get_mcp_mrtr_presentation(&wrapper).is_none());
    }
}
