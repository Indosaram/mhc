use crate::lsp::effective_extension::effective_extension;
use crate::lsp::errors::LspError;
use crate::lsp::errors::is_lsp_dead_connection_error;
use crate::lsp::manager::LspManager;
use crate::lsp::manager::SharedClient;
use crate::lsp::manager::get_lsp_manager;
use crate::lsp::server_install_state::InstallDecision;
use crate::lsp::server_install_state::load_install_decision;
use crate::lsp::server_resolution::find_server_for_extension;
use crate::lsp::types::FailedServerLookupResult;
use crate::lsp::types::ServerLookupResult;
use crate::request_context::canonicalize_existing_or_nearest_ancestor;
use crate::request_context::context_cwd;
use crate::request_context::dirname;
use crate::request_context::is_path_inside;
use crate::request_context::lsp_request_context;
use crate::request_context::resolve_from;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

const WORKSPACE_MARKERS: [&str; 7] = [
    ".git",
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
];

const READ_ONLY_TOOLS: [&str; 6] = [
    "diagnostics",
    "definition",
    "references",
    "documentSymbols",
    "workspaceSymbols",
    "prepareRename",
];

fn outside_cwd_error(file_path: &str) -> LspError {
    LspError::InvalidPath(format!(
        "LSP file path must be inside request cwd: {file_path}"
    ))
}

fn realpath(path: &str) -> Option<String> {
    std::fs::canonicalize(path)
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

pub fn is_directory_path(file_path: &str) -> bool {
    std::fs::metadata(file_path).is_ok_and(|metadata| metadata.is_dir())
}

/// TS `findWorkspaceRoot`: nearest marker directory inside the request cwd.
pub fn find_workspace_root(file_path: &str) -> Result<String, LspError> {
    let cwd = context_cwd()?;
    let abs = resolve_readable_path_inside_context(file_path)?;
    let mut dir = if is_directory_path(&abs) {
        abs
    } else {
        dirname(&abs)
    };
    let fallback_root =
        nearest_existing_directory_inside_context(&dir, &cwd).unwrap_or_else(|| cwd.clone());
    while is_path_inside(&cwd, &dir) {
        if let Some(canonical_dir) = existing_directory_inside_context(&dir, &cwd)
            && WORKSPACE_MARKERS
                .iter()
                .any(|marker| Path::new(&dir).join(marker).exists())
        {
            return Ok(canonical_dir);
        }
        if dir == cwd {
            break;
        }
        let parent = dirname(&dir);
        if parent == dir {
            break;
        }
        dir = parent;
    }
    Ok(fallback_root)
}

/// TS `resolveReadablePathInsideContext`: keeps the lexical path when it is inside cwd.
pub fn resolve_readable_path_inside_context(file_path: &str) -> Result<String, LspError> {
    let cwd = context_cwd()?;
    let abs = resolve_from(&cwd, file_path);
    if is_path_inside(&cwd, &abs) {
        return Ok(abs);
    }
    if let Some(rebased) = rebase_through_canonical_ancestor(&abs, &cwd) {
        return Ok(rebased);
    }
    let canonical = canonicalize_existing_or_nearest_ancestor(&abs)?;
    if is_path_inside(&cwd, &canonical) {
        return Ok(canonical);
    }
    Err(outside_cwd_error(file_path))
}

/// TS `resolvePathInsideContext`: like the readable variant, but the canonical target
/// must also stay inside cwd (used for mutating tools).
pub fn resolve_path_inside_context(file_path: &str) -> Result<String, LspError> {
    let cwd = context_cwd()?;
    let abs = resolve_readable_path_inside_context(file_path)?;
    let canonical = canonicalize_existing_or_nearest_ancestor(&abs)?;
    if !is_path_inside(&cwd, &canonical) {
        return Err(outside_cwd_error(file_path));
    }
    Ok(canonical)
}

fn rebase_through_canonical_ancestor(path: &str, cwd: &str) -> Option<String> {
    let mut current = path.to_string();
    let mut suffix: Vec<String> = Vec::new();
    loop {
        if Path::new(&current).exists()
            && let Some(canonical) = realpath(&current)
            && is_path_inside(cwd, &canonical)
        {
            if suffix.is_empty() {
                return Some(canonical);
            }
            let mut rebased = std::path::PathBuf::from(canonical);
            rebased.extend(suffix.iter());
            return Some(rebased.to_string_lossy().into_owned());
        }
        let parent = dirname(&current);
        if parent == current {
            return None;
        }
        let name = Path::new(&current)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        suffix.insert(0, name);
        current = parent;
    }
}

fn existing_directory_inside_context(directory: &str, cwd: &str) -> Option<String> {
    let canonical = realpath(directory)?;
    if !is_directory_path(&canonical) {
        return None;
    }
    is_path_inside(cwd, &canonical).then_some(canonical)
}

fn nearest_existing_directory_inside_context(directory: &str, cwd: &str) -> Option<String> {
    let mut current = directory.to_string();
    while is_path_inside(cwd, &current) {
        if let Some(canonical) = existing_directory_inside_context(&current, cwd) {
            return Some(canonical);
        }
        if current == cwd {
            return None;
        }
        let parent = dirname(&current);
        if parent == current {
            return None;
        }
        current = parent;
    }
    None
}

/// TS `formatServerLookupError`.
pub fn format_server_lookup_error(result: &FailedServerLookupResult) -> Result<String, LspError> {
    let context = lsp_request_context()?;
    match result {
        FailedServerLookupResult::NotConfigured {
            extension,
            available_servers,
        } => {
            let first_project_config_path = context
                .project_config_paths
                .first()
                .cloned()
                .unwrap_or_else(|| "<project lsp config>".to_string());
            let listed = available_servers
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let ellipsis = if available_servers.len() > 10 {
                "..."
            } else {
                ""
            };
            Ok([
                format!("No LSP server configured for extension: {extension}"),
                String::new(),
                format!("Available servers: {listed}{ellipsis}"),
                String::new(),
                format!(
                    "Configure a custom server in '{first_project_config_path}' or '{}':",
                    context.user_config_path
                ),
                "  {".to_string(),
                "    \"lsp\": {".to_string(),
                "      \"my-server\": {".to_string(),
                "        \"command\": [\"my-lsp\", \"--stdio\"],".to_string(),
                format!("        \"extensions\": [\"{extension}\"]"),
                "      }".to_string(),
                "    }".to_string(),
                "  }".to_string(),
            ]
            .join("\n"))
        }
        FailedServerLookupResult::NotInstalled {
            server,
            install_hint,
        } => {
            let extensions = server.extensions.join(", ");
            let decision = load_install_decision(&server.id)?.map(|record| record.decision);
            if decision == Some(InstallDecision::Declined) {
                return Ok(format!(
                    "LSP server '{}' ({extensions}) is NOT INSTALLED; user previously declined installation \u{2014} proceed without LSP.",
                    server.id
                ));
            }
            let mut lines = vec![
                format!(
                    "LSP server '{}' for {extensions} is NOT INSTALLED.",
                    server.id
                ),
                String::new(),
                format!(
                    "Command not found: {}",
                    server.command.first().cloned().unwrap_or_default()
                ),
                String::new(),
            ];
            if decision == Some(InstallDecision::Allowed) {
                lines.push(
                    "The user has pre-authorized LSP installation. Run the install command, then retry this tool:"
                        .to_string(),
                );
                lines.push(format!("  {install_hint}"));
                return Ok(lines.join("\n"));
            }
            lines.push("To install, run:".to_string());
            lines.push(format!("  {install_hint}"));
            lines.push(String::new());
            lines.push(
                "ACTION REQUIRED \u{2014} ASK THE USER whether to install this LSP server."
                    .to_string(),
            );
            if !context.capabilities.install_decision_tool {
                lines.push(
                    "Install-decision recording is unavailable in this harness; proceed without LSP if the user declines."
                        .to_string(),
                );
                return Ok(lines.join("\n"));
            }
            lines.push(
                "- If the user agrees: run the install command above, then retry this tool."
                    .to_string(),
            );
            lines.push(
                "- If the user declines, OR has NOT explicitly asked for LSP installation:"
                    .to_string(),
            );
            lines.push(format!(
                "    call lsp_install_decision {{ server_id: \"{}\", decision: \"declined\" }},",
                server.id
            ));
            lines.push(format!(
                "    which writes to {},",
                context.install_decisions_path
            ));
            lines.push("    then ignore this message and proceed WITHOUT LSP.".to_string());
            Ok(lines.join("\n"))
        }
    }
}

/// Converts a lookup result into the resolved server or the TS `LspServerLookupError`.
pub fn require_found_server(
    result: ServerLookupResult,
) -> Result<crate::lsp::types::ResolvedServer, LspError> {
    let failed = match result {
        ServerLookupResult::Found { server } => return Ok(server),
        ServerLookupResult::NotConfigured {
            extension,
            available_servers,
        } => FailedServerLookupResult::NotConfigured {
            extension,
            available_servers,
        },
        ServerLookupResult::NotInstalled {
            server,
            install_hint,
        } => FailedServerLookupResult::NotInstalled {
            server,
            install_hint,
        },
    };
    Err(LspError::ServerLookup {
        message: format_server_lookup_error(&failed)?,
        lookup: Some(failed),
    })
}

/// TS `WithLspClientOptions`.
#[derive(Debug, Clone, Default)]
pub struct WithLspClientOptions {
    pub signal: Option<crate::abort::AbortSignal>,
    pub manager: Option<Arc<LspManager>>,
}

/// TS `withLspClient`: resolves and confines the path, acquires a pooled client, runs
/// `f`, retries once on a dead connection for read-only tools, and always releases.
pub async fn with_lsp_client<T, F, Fut>(
    file_path: &str,
    f: F,
    tool_name: &str,
    options: WithLspClientOptions,
) -> Result<T, LspError>
where
    F: Fn(SharedClient, String, String) -> Fut,
    Fut: Future<Output = Result<T, LspError>>,
{
    let read_only = READ_ONLY_TOOLS.contains(&tool_name);
    let abs_path = if read_only {
        resolve_readable_path_inside_context(file_path)?
    } else {
        resolve_path_inside_context(file_path)?
    };
    if is_directory_path(&abs_path) {
        return Err(LspError::InvalidPath(
            "Directory paths are not supported by this LSP tool. Use lsp.diagnostics with a directory path for directory diagnostics."
                .to_string(),
        ));
    }
    let server = require_found_server(find_server_for_extension(&effective_extension(&abs_path))?)?;
    let root = find_workspace_root(&abs_path)?;
    let manager = options.manager.clone().unwrap_or_else(get_lsp_manager);
    let signal = options.signal.as_ref();

    let mut allow_retry = true;
    loop {
        let client = manager.get_client(&root, &server, signal).await?;
        let result = f(client.clone(), root.clone(), abs_path.clone()).await;
        manager.release_client(&root, &server.id);
        match result {
            Ok(value) => return Ok(value),
            Err(error) if allow_retry && read_only && is_lsp_dead_connection_error(&error) => {
                manager.invalidate_client(&root, &server.id, Some(&client));
                allow_retry = false;
            }
            Err(error @ LspError::RequestTimeout { .. })
                if manager.is_server_initializing(&root, &server.id) =>
            {
                return Err(error.into_server_initializing());
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
#[path = "client_wrapper_tests.rs"]
mod tests;
