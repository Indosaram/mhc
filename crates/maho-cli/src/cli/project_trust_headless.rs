//! Port of `packages/coding-agent/src/cli/project-trust-headless.ts` (senpi#6185).

use std::sync::Arc;

use maho_core::project_trust::AppMode;
use maho_ext_api::{
    ExtensionFuture, ExtensionMode, ExtensionUi, ExtensionUiDialogOptions, NotificationType,
    UiFuture,
};

pub fn to_extension_mode(mode: AppMode) -> ExtensionMode {
    match mode {
        AppMode::Interactive => ExtensionMode::Tui,
        AppMode::Print => ExtensionMode::Print,
        AppMode::Json => ExtensionMode::Json,
        AppMode::Rpc => ExtensionMode::Rpc,
    }
}

/// The `project_trust` handler UI of a run that cannot ask: every question answers "no answer",
/// and notices go to stderr outside the TUI. It imports no startup UI, so the RPC session path
/// can build it without reaching the terminal's synchronous config and package probes.
pub struct HeadlessProjectTrustUi {
    pub mode: AppMode,
}

impl ExtensionUi for HeadlessProjectTrustUi {
    fn select<'a>(
        &'a self,
        _title: &'a str,
        _options: &'a [String],
        _dialog: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn confirm<'a>(
        &'a self,
        _title: &'a str,
        _message: &'a str,
        _dialog: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn input<'a>(
        &'a self,
        _title: &'a str,
        _placeholder: Option<&'a str>,
        _dialog: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }

    fn notify(&self, message: &str, kind: NotificationType) {
        if self.mode != AppMode::Interactive {
            let color = match kind {
                NotificationType::Error => "\x1b[31m",
                NotificationType::Warning => "\x1b[33m",
                NotificationType::Info => "\x1b[36m",
            };
            maho_core::output_guard::maho_write_stderr(&format!("{color}{message}\x1b[39m\n"));
        }
    }

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
    ) -> ExtensionFuture<'_, serde_json::Value> {
        Box::pin(async { Ok(serde_json::Value::Null) })
    }

    fn theme(&self) -> maho_ext_api::Theme {
        Default::default()
    }
}

pub fn create_headless_project_trust_ui(mode: AppMode) -> Arc<dyn ExtensionUi> {
    Arc::new(HeadlessProjectTrustUi { mode })
}
