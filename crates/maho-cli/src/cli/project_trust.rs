use maho_core::project_trust::{AppMode, ProjectTrustContext};
pub fn extension_mode(mode: AppMode) -> &'static str {
    match mode { AppMode::Interactive => "tui", AppMode::Print | AppMode::AppServer => "print", AppMode::Json => "json", AppMode::Rpc => "rpc" }
}
pub type Selector = dyn Fn(&str, &[String]) -> Option<String> + Send + Sync;
pub struct CliProjectTrustContext {
    pub cwd: String,
    pub mode: AppMode,
    pub ui_available: bool,
    pub selector: Box<Selector>,
}
impl ProjectTrustContext for CliProjectTrustContext {
    fn has_ui(&self) -> bool { self.ui_available }
    fn select(&self, title: &str, options: &[String]) -> Option<String> {
        if !self.ui_available || self.mode != AppMode::Interactive { return None; }
        (self.selector)(title, options)
    }
}
