//! Port of interactive/startup-tools.ts.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartupToolPaths {
    pub fd_path: Option<String>,
}

static STARTUP_FD_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
/// senpi interactive-mode.ts `this.fdPath`: the fd resolved by the startup `ensureTool` probe, fed to autocomplete.
pub fn startup_fd_path() -> Option<String> { STARTUP_FD_PATH.get().cloned() }
pub fn record_startup_fd_path(path: Option<String>) { if let Some(path) = path { drop(STARTUP_FD_PATH.set(path)); } }

pub fn resolve_startup_tool_paths(mut get_path: impl FnMut(&str) -> Option<String>) -> StartupToolPaths {
    StartupToolPaths { fd_path: get_path("fd") }
}
