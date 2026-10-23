use crate::manager::TerminalManager;
use maho_ai::types::{TextAudience, TextContent};
use maho_pty::PtyExit;
use serde_json::{Map, Value};
/// What a background exit notice reads: on the tool child it is a summary the child sent, not the live session.
pub struct TerminalExitView {
    pub exit_result: Option<PtyExit>,
    full_output: String,
}
impl TerminalExitView {
    pub fn new(exit_result: Option<PtyExit>, full_output: String) -> Self {
        Self {
            exit_result,
            full_output,
        }
    }
    pub fn full_output(&self) -> &str {
        &self.full_output
    }
    /// Snapshot of a live runtime session (upstream passes the session itself, which satisfies the view).
    pub fn of_runtime(runtime: &crate::runtime_session::TerminalRuntimeSession) -> Self {
        Self::new(
            runtime.exit_result().ok().flatten(),
            runtime.full_output().unwrap_or_default(),
        )
    }
}
/// The tool child's execution barrier for terminal launches: `wrap` rewrites the launch behind a go-line
/// shim, and `release` records the started session with the server and only then lets the command run.
pub trait TerminalLaunchBarrier: Send + Sync {
    fn wrap(&self, command: &str, args: &[String]) -> (String, Vec<String>);
    fn release(
        &self,
        pid: Option<u32>,
        write: &mut dyn FnMut(&str) -> std::io::Result<()>,
    ) -> Result<(), String>;
}
#[derive(Debug)]
pub struct TerminalToolResult {
    pub content: Vec<TextContent>,
    pub details: Option<Map<String, Value>>,
    pub is_error: Option<bool>,
}
pub fn text_result(text: impl Into<String>) -> TerminalToolResult {
    TerminalToolResult {
        content: vec![TextContent {
            text: text.into(),
            audience: None,
            text_signature: None,
        }],
        details: None,
        is_error: None,
    }
}
pub fn error_result(text: impl Into<String>) -> TerminalToolResult {
    let mut result = text_result(text);
    result.is_error = Some(true);
    result
}
pub fn noticed_result(text: &str, notices: &[Option<&str>]) -> TerminalToolResult {
    TerminalToolResult {
        content: crate::output_format::split_model_only_notices(text, notices),
        details: None,
        is_error: None,
    }
}
pub fn model_only_part(text: impl Into<String>) -> TextContent {
    TextContent {
        text: text.into(),
        audience: Some(TextAudience::Model),
        text_signature: None,
    }
}
pub fn resolve_terminal_id(manager: &TerminalManager, id: &str) -> String {
    manager.resolve_id(id).unwrap_or_else(|| id.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolve_keeps_runtime_ids_and_falls_back_for_unseen_monitor_ids() {
        let mut manager = TerminalManager::new(1);
        manager.bind_monitor_id("mon_1", "bash_9");
        assert_eq!(resolve_terminal_id(&manager, "mon_1"), "bash_9");
        assert_eq!(resolve_terminal_id(&manager, "bash_9"), "bash_9");
        assert_eq!(resolve_terminal_id(&manager, "mon_missing"), "mon_missing");
    }
}
