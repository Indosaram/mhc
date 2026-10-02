//! Port of interactive/startup-tools.ts.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartupToolPaths {
    pub fd_path: Option<String>,
}

pub fn resolve_startup_tool_paths(mut get_path: impl FnMut(&str) -> Option<String>) -> StartupToolPaths {
    StartupToolPaths { fd_path: get_path("fd") }
}
