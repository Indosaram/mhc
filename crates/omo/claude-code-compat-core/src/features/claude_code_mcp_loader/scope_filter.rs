use std::{io, path::Path};
use utils::contains_path::contains_path;
use super::types::{ClaudeCodeMcpServer, McpScope};

pub fn should_load_mcp_server(server: &ClaudeCodeMcpServer, cwd: Option<&Path>) -> io::Result<bool> {
    if server.scope != Some(McpScope::Local) { return Ok(true); }
    let Some(project_path) = server.project_path.as_ref().filter(|path| !path.as_os_str().is_empty()) else { return Ok(false); };
    let cwd = match cwd { Some(cwd) => cwd.to_path_buf(), None => std::env::current_dir()? };
    Ok(contains_path(project_path, cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_scope_requires_contained_project_path() {
        // Given local metadata and exact, child, unrelated, and parent paths.
        let root = tempfile::tempdir().expect("fixture directory");
        let project = root.path().join("repo");
        let server = ClaudeCodeMcpServer { scope: Some(McpScope::Local), project_path: Some(project.clone()), ..Default::default() };
        // When scope filtering each candidate, then only descendants load.
        for (cwd, expected) in [(project.clone(), true), (project.join("packages/app"), true), (root.path().join("other"), false), (root.path().to_path_buf(), false)] {
            assert_eq!(should_load_mcp_server(&server, Some(&cwd)).expect("scope"), expected);
        }
        assert!(!should_load_mcp_server(&ClaudeCodeMcpServer { project_path: None, ..server }, Some(root.path())).expect("scope"));
    }
}
