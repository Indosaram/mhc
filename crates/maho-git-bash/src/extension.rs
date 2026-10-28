//! Declares the `git_bash` stdio MCP server on the MCP client (`maho-ext-mcp`) when mhc runs on
//! native Windows and the `mhc-git-bash` sibling sits beside the executable. A user or project
//! `mcp.json` entry with the same name replaces it.

use maho_ext_api::{
    Extension, ExtensionApi, McpExposure, McpLifecycle, McpServerDeclaration, McpTransport,
};
use std::path::{Path, PathBuf};

/// MCP server name the tools are exposed under (`mcp_git_bash_<tool>`).
pub const SERVER_NAME: &str = "git_bash";
/// File name of the sibling binary.
pub const BINARY_NAME: &str = if cfg!(windows) {
    "mhc-git-bash.exe"
} else {
    "mhc-git-bash"
};

/// The sibling binary next to `exe`, when it exists.
pub fn sibling_binary(exe: &Path) -> Option<PathBuf> {
    let candidate = exe.parent()?.join(BINARY_NAME);
    candidate.is_file().then_some(candidate)
}

/// The declaration for `platform` (Node-style name) and the located sibling binary. Nothing is
/// declared off Windows (the server would exit before `initialize`) or without the binary.
pub fn declaration(platform: &str, binary: Option<&Path>) -> Option<McpServerDeclaration> {
    if platform != "win32" {
        return None;
    }
    let binary = binary?;
    Some(McpServerDeclaration {
        transport: Some(McpTransport::Stdio),
        command: Some(binary.to_string_lossy().into_owned()),
        args: Some(vec!["mcp".to_owned()]),
        lifecycle: Some(McpLifecycle::Lazy),
        exposure: Some(McpExposure::Auto),
        ..Default::default()
    })
}

/// The `<builtin:git-bash>` extension.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitBashMcpExtension;

impl Extension for GitBashMcpExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let binary = std::env::current_exe()
            .ok()
            .and_then(|exe| sibling_binary(&exe));
        let Some(declaration) = declaration(&crate::host::node_platform(), binary.as_deref())
        else {
            return;
        };
        if let Err(error) = api.try_register_mcp_server(SERVER_NAME, declaration) {
            maho_ext_api::diagnostic(&format!("git-bash: {}", error.message));
        }
    }
}
