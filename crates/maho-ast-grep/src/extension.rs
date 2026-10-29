//! Declares the `ast_grep` stdio MCP server on the MCP client (`maho-ext-mcp`) when the
//! `mhc-ast-grep` sibling sits beside the executable. A user or project `mcp.json` entry with the
//! same name replaces it.

use crate::server::PROJECT_CWD_ENV;
use maho_ext_api::{
    Extension, ExtensionApi, McpExposure, McpLifecycle, McpServerDeclaration, McpTransport,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// MCP server name the tools are exposed under (`mcp_ast_grep_<tool>`).
pub const SERVER_NAME: &str = "ast_grep";
/// File name of the sibling binary.
pub const BINARY_NAME: &str = if cfg!(windows) { "mhc-ast-grep.exe" } else { "mhc-ast-grep" };

/// The sibling binary next to `exe`, when it is a file.
pub fn sibling_binary(exe: &Path) -> Option<PathBuf> {
    let candidate = exe.parent()?.join(BINARY_NAME);
    candidate.is_file().then_some(candidate)
}

/// The declaration for the located binary and the session's working directory. An explicit
/// project-cwd value in `existing_env` wins and is passed through unchanged.
pub fn declaration(binary: &Path, cwd: &Path, existing_env: Option<&str>) -> McpServerDeclaration {
    let value = existing_env
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| cwd.to_string_lossy().into_owned(), str::to_owned);
    McpServerDeclaration {
        transport: Some(McpTransport::Stdio),
        command: Some(binary.to_string_lossy().into_owned()),
        args: Some(vec!["mcp".to_owned()]),
        env: Some(BTreeMap::from([(PROJECT_CWD_ENV.to_owned(), value)])),
        lifecycle: Some(McpLifecycle::Lazy),
        exposure: Some(McpExposure::Search),
        ..Default::default()
    }
}

/// The diagnostic line for a missing server binary.
pub fn missing_message(expected: &Path) -> String {
    format!(
        "ast-grep tools are not registered: the server binary is missing at {}.",
        expected.display()
    )
}

/// What registration does for `exe`: a declaration, or the diagnostic to emit.
pub fn plan(
    exe: &Path,
    cwd: &Path,
    existing_env: Option<&str>,
) -> Result<McpServerDeclaration, String> {
    match sibling_binary(exe) {
        Some(binary) => Ok(declaration(&binary, cwd, existing_env)),
        None => Err(missing_message(
            &exe.parent().unwrap_or(Path::new("")).join(BINARY_NAME),
        )),
    }
}

/// The `<builtin:ast-grep>` extension.
#[derive(Clone, Copy, Debug, Default)]
pub struct AstGrepExtension;

impl Extension for AstGrepExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let existing = std::env::var(PROJECT_CWD_ENV).ok();
        match plan(&exe, &api.cwd, existing.as_deref()) {
            Ok(declaration) => {
                if let Err(error) = api.try_register_mcp_server(SERVER_NAME, declaration) {
                    maho_ext_api::diagnostic(&format!("ast-grep: {}", error.message));
                }
            }
            Err(message) => maho_ext_api::diagnostic(&message),
        }
    }
}
