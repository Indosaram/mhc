use maho_ext_api::ExtensionUi;
pub const COMMENT_CHECKER_WIDGET_KEY: &str = "pi-comment-checker";

pub fn sync_comment_checker_widget(ui: &dyn ExtensionUi) {
    ui.set_widget(COMMENT_CHECKER_WIDGET_KEY, None, Default::default());
}
