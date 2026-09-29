use super::parameters::js_number;
use super::parameters::number_value;
use super::parameters::optional_boolean;
use super::parameters::optional_number;
use super::parameters::optional_string;
use super::parameters::position_arg;
use super::parameters::require_number;
use super::parameters::require_string;
use super::parameters::severity_filter;
use crate::abort::AbortSignal;
use crate::lsp::client::LspClient;
use crate::lsp::client_wrapper::WithLspClientOptions;
use crate::lsp::client_wrapper::is_directory_path;
use crate::lsp::client_wrapper::resolve_readable_path_inside_context;
use crate::lsp::client_wrapper::with_lsp_client;
use crate::lsp::config_loader::get_merged_servers;
use crate::lsp::constants::DEFAULT_MAX_DIAGNOSTICS;
use crate::lsp::constants::DEFAULT_MAX_REFERENCES;
use crate::lsp::constants::DEFAULT_MAX_SYMBOLS;
use crate::lsp::directory_diagnostics::DirectoryDiagnosticsOptions;
use crate::lsp::directory_diagnostics::aggregate_diagnostics_for_directory;
use crate::lsp::errors::LspError;
use crate::lsp::formatters::filter_diagnostics_by_severity;
use crate::lsp::formatters::format_apply_result;
use crate::lsp::formatters::format_diagnostic;
use crate::lsp::formatters::format_document_symbol;
use crate::lsp::formatters::format_location;
use crate::lsp::formatters::format_prepare_rename_result;
use crate::lsp::formatters::format_symbol_info;
use crate::lsp::infer_extension::infer_extension_from_directory;
use crate::lsp::manager::SharedClient;
use crate::lsp::manager::get_lsp_manager;
use crate::lsp::server_install_state::InstallDecision;
use crate::lsp::server_install_state::record_install_decision;
use crate::lsp::server_resolution::get_all_servers;
use crate::lsp::types::DocumentSymbol;
use crate::lsp::types::SymbolInfo;
use crate::missing_dependency_result::ToolExecutionResult;
use crate::missing_dependency_result::missing_dependency_result;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::path::Path;

fn details(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Map::new()
        }
    }
}

fn client_options(signal: Option<&AbortSignal>) -> WithLspClientOptions {
    WithLspClientOptions {
        signal: signal.cloned(),
        manager: None,
    }
}

fn lsp_client(client: &SharedClient) -> Result<&LspClient, LspError> {
    client
        .as_lsp_client()
        .ok_or_else(|| LspError::other("LSP client does not support this request"))
}

fn recover(error: LspError, fallback: Value) -> Result<ToolExecutionResult, LspError> {
    missing_dependency_result(&error, details(fallback)).ok_or(error)
}

/// TS `executeLspDiagnostics`.
pub async fn execute_lsp_diagnostics(
    params: &Map<String, Value>,
    signal: Option<&AbortSignal>,
) -> Result<ToolExecutionResult, LspError> {
    let file_path = require_string(params, "filePath")?;
    let severity = severity_filter(params);
    let severity_text = severity.as_str();
    let outcome = async {
        let abs_path = resolve_readable_path_inside_context(&file_path)?;
        if is_directory_path(&abs_path) {
            let Some(extension) = infer_extension_from_directory(Path::new(&abs_path)) else {
                let message = format!("No supported source files found in directory: {abs_path}");
                return Ok(ToolExecutionResult::text(
                    message.clone(),
                    details(json!({
                        "filePath": file_path, "severity": severity_text, "mode": "directory",
                        "diagnostics": [], "totalDiagnostics": 0, "truncated": false,
                        "error": message, "errorKind": "no_files",
                    })),
                ));
            };
            let output = aggregate_diagnostics_for_directory(
                &abs_path,
                &extension,
                Some(severity),
                None,
                DirectoryDiagnosticsOptions {
                    signal: signal.cloned(),
                    ..DirectoryDiagnosticsOptions::default()
                },
            )
            .await?;
            let failures: Vec<Value> = output
                .file_failures
                .iter()
                .map(|failure| json!({ "file": failure.file, "error": failure.error }))
                .collect();
            return Ok(ToolExecutionResult::text(
                output.output,
                details(json!({
                    "filePath": file_path, "severity": severity_text, "mode": "directory",
                    "diagnostics": [], "totalDiagnostics": output.total_diagnostics,
                    "truncated": false, "fileFailures": failures,
                })),
            ));
        }

        let result = with_lsp_client(
            &file_path,
            |client, _root, resolved| async move { client.diagnostics(&resolved, signal).await },
            "diagnostics",
            client_options(signal),
        )
        .await?;
        if let Some(transient) = result.transient_error {
            return Ok(ToolExecutionResult::error_text(
                transient.message.clone(),
                details(json!({
                    "filePath": file_path, "severity": severity_text, "mode": "file",
                    "diagnostics": [], "totalDiagnostics": 0, "truncated": false,
                    "error": transient.message, "errorKind": transient.kind,
                })),
            ));
        }
        let diagnostics = filter_diagnostics_by_severity(result.items, Some(severity));
        let total = diagnostics.len();
        let truncated = total > DEFAULT_MAX_DIAGNOSTICS;
        let output = if total == 0 {
            "No diagnostics found".to_string()
        } else {
            let mut lines = Vec::new();
            if truncated {
                lines.push(format!(
                    "Found {total} diagnostics (showing first {DEFAULT_MAX_DIAGNOSTICS}):"
                ));
            }
            lines.extend(
                diagnostics
                    .iter()
                    .take(DEFAULT_MAX_DIAGNOSTICS)
                    .map(format_diagnostic),
            );
            lines.join("\n")
        };
        let entries: Vec<Value> = diagnostics
            .iter()
            .map(|diagnostic| json!({ "file": abs_path, "diagnostic": diagnostic }))
            .collect();
        Ok(ToolExecutionResult::text(
            output,
            details(json!({
                "filePath": file_path, "severity": severity_text, "mode": "file",
                "diagnostics": entries, "totalDiagnostics": total, "truncated": truncated,
            })),
        ))
    }
    .await;
    outcome.or_else(|error| {
        recover(
            error,
            json!({
                "filePath": file_path, "severity": severity_text, "mode": "file",
                "diagnostics": [], "totalDiagnostics": 0, "truncated": false,
            }),
        )
    })
}

struct PositionParams {
    file_path: String,
    line: f64,
    character: f64,
}

impl PositionParams {
    fn read(params: &Map<String, Value>) -> Result<Self, LspError> {
        Ok(Self {
            file_path: require_string(params, "filePath")?,
            line: require_number(params, "line")?,
            character: require_number(params, "character")?,
        })
    }

    fn base(&self) -> Map<String, Value> {
        details(json!({
            "filePath": self.file_path,
            "line": number_value(self.line),
            "character": number_value(self.character),
        }))
    }

    fn with(&self, extra: Value) -> Map<String, Value> {
        let mut map = self.base();
        map.extend(details(extra));
        map
    }
}

/// TS `executeLspGotoDefinition`.
pub async fn execute_lsp_goto_definition(
    params: &Map<String, Value>,
    signal: Option<&AbortSignal>,
) -> Result<ToolExecutionResult, LspError> {
    let position = PositionParams::read(params)?;
    let (line, character) = (
        position_arg(position.line),
        position_arg(position.character),
    );
    let result = with_lsp_client(
        &position.file_path,
        |client, _root, resolved| async move {
            lsp_client(&client)?
                .definition(&resolved, line, character, signal)
                .await
        },
        "definition",
        client_options(signal),
    )
    .await;
    match result {
        Ok(result) => {
            let locations = match result {
                Value::Null | Value::Bool(false) => Vec::new(),
                Value::Array(items) => items,
                other => vec![other],
            };
            let text = if locations.is_empty() {
                "No definition found".to_string()
            } else {
                locations
                    .iter()
                    .map(format_location)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(ToolExecutionResult::text(
                text,
                position.with(json!({ "locations": locations })),
            ))
        }
        Err(error) => recover(
            error,
            Value::Object(position.with(json!({ "locations": [] }))),
        ),
    }
}

/// TS `executeLspFindReferences`.
pub async fn execute_lsp_find_references(
    params: &Map<String, Value>,
    signal: Option<&AbortSignal>,
) -> Result<ToolExecutionResult, LspError> {
    let position = PositionParams::read(params)?;
    let include_declaration = optional_boolean(params, "includeDeclaration").unwrap_or(true);
    let (line, character) = (
        position_arg(position.line),
        position_arg(position.character),
    );
    let result = with_lsp_client(
        &position.file_path,
        |client, _root, resolved| async move {
            lsp_client(&client)?
                .references(&resolved, line, character, include_declaration, signal)
                .await
        },
        "references",
        client_options(signal),
    )
    .await;
    match result {
        Ok(result) => {
            let references = match result {
                Value::Array(items) => items,
                _ => Vec::new(),
            };
            let total = references.len();
            let truncated = total > DEFAULT_MAX_REFERENCES;
            let text = if total == 0 {
                "No references found".to_string()
            } else {
                let mut lines = Vec::new();
                if truncated {
                    lines.push(format!(
                        "Found {total} references (showing first {DEFAULT_MAX_REFERENCES}):"
                    ));
                }
                lines.extend(
                    references
                        .iter()
                        .take(DEFAULT_MAX_REFERENCES)
                        .map(format_location),
                );
                lines.join("\n")
            };
            Ok(ToolExecutionResult::text(
                text,
                position.with(json!({
                    "references": references, "totalReferences": total, "truncated": truncated,
                })),
            ))
        }
        Err(error) => recover(
            error,
            Value::Object(position.with(json!({
                "references": [], "totalReferences": 0, "truncated": false,
            }))),
        ),
    }
}

/// TS `executeLspSymbols`.
pub async fn execute_lsp_symbols(
    params: &Map<String, Value>,
    signal: Option<&AbortSignal>,
) -> Result<ToolExecutionResult, LspError> {
    let file_path = require_string(params, "filePath")?;
    let scope = match optional_string(params, "scope").as_deref() {
        Some("workspace") => "workspace",
        _ => "document",
    };
    let max = DEFAULT_MAX_SYMBOLS as f64;
    let limit = optional_number(params, "limit").unwrap_or(max).min(max);
    let query = optional_string(params, "query");
    let outcome = async {
        if scope == "workspace" {
            let Some(query) = query.clone().filter(|query| !query.is_empty()) else {
                let message = "Error: 'query' is required for workspace scope";
                return Ok(ToolExecutionResult::text(
                    message,
                    details(json!({
                        "filePath": file_path, "scope": scope, "symbols": [], "totalSymbols": 0,
                        "truncated": false, "error": message, "errorKind": "missing_query",
                    })),
                ));
            };
            let query_ref = query.as_str();
            let symbols = with_lsp_client(
                &file_path,
                |client, _root, _resolved| async move {
                    lsp_client(&client)?
                        .workspace_symbols(query_ref, signal)
                        .await
                },
                "workspaceSymbols",
                client_options(signal),
            )
            .await?;
            return Ok(format_symbols_result(
                &file_path,
                scope,
                symbols,
                limit,
                Some(&query),
            ));
        }
        let symbols = with_lsp_client(
            &file_path,
            |client, _root, resolved| async move {
                lsp_client(&client)?
                    .document_symbols(&resolved, signal)
                    .await
            },
            "documentSymbols",
            client_options(signal),
        )
        .await?;
        Ok(format_symbols_result(
            &file_path, scope, symbols, limit, None,
        ))
    }
    .await;
    outcome.or_else(|error| {
        let mut fallback = details(json!({
            "filePath": file_path, "scope": scope, "symbols": [], "totalSymbols": 0, "truncated": false,
        }));
        if let Some(query) = &query {
            fallback.insert("query".into(), Value::from(query.clone()));
        }
        recover(error, Value::Object(fallback))
    })
}

fn format_symbols_result(
    file_path: &str,
    scope: &str,
    symbols: Value,
    limit: f64,
    query: Option<&str>,
) -> ToolExecutionResult {
    let symbols = match symbols {
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    let total = symbols.len();
    let truncated = total as f64 > limit;
    let mut map = details(json!({
        "filePath": file_path, "scope": scope, "symbols": symbols,
        "totalSymbols": total, "truncated": truncated,
    }));
    if let Some(query) = query {
        map.insert("query".into(), Value::from(query));
    }
    if total == 0 {
        return ToolExecutionResult::text("No symbols found", map);
    }
    let take = if truncated {
        limit.max(0.0).ceil() as usize
    } else {
        total
    };
    let limited = &symbols[..take.min(total)];
    let mut lines = Vec::new();
    if truncated {
        lines.push(format!(
            "Found {total} symbols (showing first {}):",
            js_number(limit)
        ));
    }
    let is_document = |symbol: &Value| symbol.get("range").is_some();
    if limited.iter().all(is_document) {
        lines.extend(
            limited
                .iter()
                .filter_map(|symbol| serde_json::from_value::<DocumentSymbol>(symbol.clone()).ok())
                .map(|symbol| format_document_symbol(&symbol, 0)),
        );
    } else {
        lines.extend(
            limited
                .iter()
                .filter(|symbol| !is_document(symbol))
                .filter_map(|symbol| serde_json::from_value::<SymbolInfo>(symbol.clone()).ok())
                .map(|symbol| format_symbol_info(&symbol)),
        );
    }
    ToolExecutionResult::text(lines.join("\n"), map)
}

/// TS `executeLspPrepareRename`.
pub async fn execute_lsp_prepare_rename(
    params: &Map<String, Value>,
    signal: Option<&AbortSignal>,
) -> Result<ToolExecutionResult, LspError> {
    let position = PositionParams::read(params)?;
    let (line, character) = (
        position_arg(position.line),
        position_arg(position.character),
    );
    let result = with_lsp_client(
        &position.file_path,
        |client, _root, resolved| async move {
            lsp_client(&client)?
                .prepare_rename(&resolved, line, character, signal)
                .await
        },
        "prepareRename",
        client_options(signal),
    )
    .await;
    match result {
        Ok(result) => Ok(ToolExecutionResult::text(
            format_prepare_rename_result(&result),
            position.with(json!({ "result": result })),
        )),
        Err(error) => recover(
            error,
            Value::Object(position.with(json!({ "result": null }))),
        ),
    }
}

/// TS `executeLspRename`.
pub async fn execute_lsp_rename(
    params: &Map<String, Value>,
    signal: Option<&AbortSignal>,
) -> Result<ToolExecutionResult, LspError> {
    let position = PositionParams::read(params)?;
    let new_name = require_string(params, "newName")?;
    let (line, character) = (
        position_arg(position.line),
        position_arg(position.character),
    );
    let name_ref = new_name.as_str();
    let result = with_lsp_client(
        &position.file_path,
        |client, _root, resolved| async move {
            lsp_client(&client)?
                .rename(&resolved, line, character, name_ref, signal)
                .await
        },
        "rename",
        client_options(signal),
    )
    .await;
    match result {
        Ok(result) => {
            let text = format_apply_result(&result.apply);
            let details = position.with(json!({
                "newName": new_name, "apply": result.apply, "edit": result.edit,
            }));
            Ok(if result.apply.success {
                ToolExecutionResult::text(text, details)
            } else {
                ToolExecutionResult::error_text(text, details)
            })
        }
        Err(error) => recover(
            error,
            Value::Object(
                position.with(json!({ "newName": new_name, "apply": null, "edit": null })),
            ),
        ),
    }
}

/// TS `executeLspStatus`.
pub fn execute_lsp_status() -> Result<ToolExecutionResult, LspError> {
    let servers = get_all_servers()?;
    let snapshots = get_lsp_manager().get_snapshot();
    let installed = servers
        .iter()
        .filter(|server| server.installed && !server.disabled)
        .count();
    let mut lines = vec![
        format!("Configured LSP servers: {}", servers.len()),
        format!("Installed LSP servers: {installed}"),
        String::new(),
    ];
    lines.extend(servers.iter().map(|server| {
        let state = if server.disabled {
            "disabled"
        } else if server.installed {
            "installed"
        } else {
            "missing"
        };
        format!(
            "- {}: {state}; source={}; extensions={}",
            server.id,
            server.source,
            server.extensions.join(", ")
        )
    }));
    lines.push(String::new());
    lines.push(format!("Active LSP clients: {}", snapshots.len()));
    lines.extend(snapshots.iter().map(|snapshot| {
        let state = match (snapshot.alive, snapshot.is_initializing) {
            (false, _) => "dead",
            (true, true) => "initializing",
            (true, false) => "alive",
        };
        format!(
            "- {}: {state}; root={}; refs={}",
            snapshot.server_id, snapshot.root, snapshot.ref_count
        )
    }));
    let servers_json: Vec<Value> = servers
        .iter()
        .map(|server| {
            json!({
                "id": server.id, "installed": server.installed, "extensions": server.extensions,
                "disabled": server.disabled, "source": server.source,
                "priority": number_value(server.priority),
            })
        })
        .collect();
    let snapshots_json: Vec<Value> = snapshots
        .iter()
        .map(|snapshot| {
            json!({
                "root": snapshot.root, "serverId": snapshot.server_id,
                "refCount": snapshot.ref_count, "pendingWaiters": snapshot.pending_waiters,
                "lastUsedAt": number_value(snapshot.last_used_at),
                "isInitializing": snapshot.is_initializing, "alive": snapshot.alive,
                "command": snapshot.command,
            })
        })
        .collect();
    Ok(ToolExecutionResult::text(
        lines.join("\n"),
        details(json!({ "servers": servers_json, "snapshots": snapshots_json })),
    ))
}

/// TS `executeLspInstallDecision`.
pub fn execute_lsp_install_decision(
    params: &Map<String, Value>,
) -> Result<ToolExecutionResult, LspError> {
    let server_id = require_string(params, "server_id")?;
    let raw = params.get("decision");
    let Some(decision) = raw.and_then(Value::as_str).and_then(InstallDecision::parse) else {
        let shown = match raw {
            None => "undefined".to_string(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        };
        return Ok(ToolExecutionResult::error_text(
            format!("Invalid decision '{shown}'. Expected \"declined\" or \"allowed\"."),
            details(json!({ "serverId": server_id, "errorKind": "invalid_decision" })),
        ));
    };
    let mut server_ids: Vec<String> = Vec::new();
    for entry in get_merged_servers()? {
        if !server_ids.contains(&entry.server.id) {
            server_ids.push(entry.server.id);
        }
    }
    if !server_ids.contains(&server_id) {
        let preview = server_ids
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let more = if server_ids.len() > 20 { "..." } else { "" };
        return Ok(ToolExecutionResult::error_text(
            format!("Unknown LSP server '{server_id}'. Known servers: {preview}{more}"),
            details(json!({ "serverId": server_id, "errorKind": "unknown_server" })),
        ));
    }
    record_install_decision(&server_id, decision, None)
        .map_err(|error| LspError::other(error.to_string()))?;
    let follow_up = match decision {
        InstallDecision::Declined => {
            "Future LSP lookups for this server stay quiet; proceed without LSP."
        }
        InstallDecision::Allowed => {
            "Future LSP lookups keep install instructions without asking the user."
        }
    };
    Ok(ToolExecutionResult::text(
        format!(
            "Recorded install decision for '{server_id}': {}. {follow_up}",
            decision.as_str()
        ),
        details(json!({ "serverId": server_id, "decision": decision.as_str() })),
    ))
}
