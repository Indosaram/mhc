//! Port of TS `server-resolution.ts`.

use crate::lsp::config_loader::get_merged_servers_and_disabled_ids;
use crate::lsp::config_loader::get_merged_servers;
use crate::lsp::server_definitions::builtin_server;
use crate::lsp::server_definitions::install_hint;
use crate::lsp::server_installation::{is_server_installed_with_options, resolve_server_binary, ResolveServerBinaryOptions};
use crate::lsp::types::ResolvedServer;
use crate::lsp::types::ServerLookupInfo;
use crate::lsp::types::ServerLookupResult;
use crate::request_context::LspRequestContextUnavailableError;
use crate::request_context::context_cwd;

/// TS `findServerForExtension`.
pub fn find_server_for_extension(
    ext: &str,
) -> Result<ServerLookupResult, LspRequestContextUnavailableError> {
    let servers = get_merged_servers()?;
    let working_directory = context_cwd()?;
    let has_ext = |server: &ResolvedServer| server.extensions.iter().any(|e| e == ext);

    for entry in servers.iter().filter(|entry| has_ext(&entry.server)) {
        if let Some(binary) = resolve_server_binary(&entry.server.command, Some(&working_directory), &ResolveServerBinaryOptions::default()) {
            let mut server = entry.server.clone();
            server.command[0] = binary;
            return Ok(ServerLookupResult::Found { server });
        }
    }

    if let Some(configured) = servers.iter().find(|s| has_ext(&s.server)) {
        let server = &configured.server;
        let hint = install_hint(&server.id).map_or_else(
            || {
                let cmd = server.command.first().map_or("undefined", String::as_str);
                format!("Install '{cmd}' and ensure it's in your PATH")
            },
            str::to_string,
        );
        return Ok(ServerLookupResult::NotInstalled {
            server: ServerLookupInfo {
                id: server.id.clone(),
                command: server.command.clone(),
                extensions: server.extensions.clone(),
            },
            install_hint: hint,
        });
    }

    let mut available: Vec<String> = Vec::new();
    for server in &servers {
        if !available.contains(&server.server.id) {
            available.push(server.server.id.clone());
        }
    }
    Ok(ServerLookupResult::NotConfigured {
        extension: ext.to_string(),
        available_servers: available,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerStatus {
    pub id: String,
    pub installed: bool,
    pub extensions: Vec<String>,
    pub disabled: bool,
    pub source: String,
    pub priority: f64,
}

/// TS `getAllServers`.
pub fn get_all_servers() -> Result<Vec<ServerStatus>, LspRequestContextUnavailableError> {
    // PERF-DIVERGENCE(omo lsp config-loader): upstream reads the LSP config files twice per call (merged servers, then disabled ids); this port loads them once. Output is identical.
    let (servers, disabled) = get_merged_servers_and_disabled_ids()?;
    let working_directory = context_cwd()?;
    let options = ResolveServerBinaryOptions::default();
    let mut result: Vec<ServerStatus> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for entry in servers {
        if seen.contains(&entry.server.id) {
            continue;
        }
        seen.push(entry.server.id.clone());
        result.push(ServerStatus {
            installed: is_server_installed_with_options(&entry.server.command, Some(&working_directory), &options),
            id: entry.server.id,
            extensions: entry.server.extensions,
            disabled: false,
            source: entry.source.as_str().to_string(),
            priority: entry.server.priority,
        });
    }
    for id in disabled {
        if seen.contains(&id) {
            continue;
        }
        let builtin = builtin_server(&id);
        result.push(ServerStatus {
            installed: builtin.is_some_and(|b| is_server_installed_with_options(&b.command_vec(), Some(&working_directory), &options)),
            extensions: builtin.map(|b| b.extensions_vec()).unwrap_or_default(),
            id,
            disabled: true,
            source: "disabled".to_string(),
            priority: 0.0,
        });
    }
    Ok(result)
}
