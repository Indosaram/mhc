use maho_ext_api::ExtensionUi;
pub const COMMENT_CHECKER_WIDGET_KEY: &str = "pi-comment-checker";

pub enum CommentCheckerUiStatus { Idle, Loading, Missing, Clean, Warning, Error }
pub struct CommentCheckerWarning { pub file_path: String, pub message: String }
pub struct CommentCheckerUiState {
    pub status: CommentCheckerUiStatus,
    pub checked_files: Vec<String>,
    pub warnings: Vec<CommentCheckerWarning>,
    pub error_message: Option<String>,
}
pub fn get_comment_checker_widget_lines(_state: &CommentCheckerUiState) -> Option<Vec<String>> { None }

pub fn sync_comment_checker_widget(ui: &dyn ExtensionUi) {
    ui.set_widget(COMMENT_CHECKER_WIDGET_KEY, None, Default::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(status: CommentCheckerUiStatus) -> CommentCheckerUiState { CommentCheckerUiState { status, checked_files: Vec::new(), warnings: Vec::new(), error_message: None } }
    #[test] fn loading_hidden() { assert!(get_comment_checker_widget_lines(&state(CommentCheckerUiStatus::Loading)).is_none()); assert_eq!(COMMENT_CHECKER_WIDGET_KEY, "pi-comment-checker"); }
    #[test] fn missing_hidden() { assert!(get_comment_checker_widget_lines(&state(CommentCheckerUiStatus::Missing)).is_none()); }
    #[test] fn warning_hidden() { let mut state = state(CommentCheckerUiStatus::Warning); state.warnings.push(CommentCheckerWarning { file_path: "a.ts".into(), message: "warning".into() }); assert!(get_comment_checker_widget_lines(&state).is_none()); }
    #[test] fn clean_hidden() { assert!(get_comment_checker_widget_lines(&state(CommentCheckerUiStatus::Clean)).is_none()); }
    #[test] fn error_hidden() { let mut state = state(CommentCheckerUiStatus::Error); state.error_message = Some("failed".into()); assert!(get_comment_checker_widget_lines(&state).is_none()); }
}
