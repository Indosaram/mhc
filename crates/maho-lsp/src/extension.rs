//! LSP tools, `/lsp` command and post-edit diagnostics on the maho extension API.
use crate::config::{ConfigPaths, merged_servers};
use crate::hooks::{HookChain, Mutation};
use crate::manager::{DaemonMode, LspManager, LspManagerOptions};
use crate::post_edit::{LspDiagnosticsHook, MUTATION_TOOLS, extract_mutated_file_paths};
use crate::servers::{BUILTIN_SERVERS, INSTALL_RECIPES};
use crate::tools::{LspTools, ToolOutput};
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, NotificationType, ToolCall,
    ToolContent, ToolDefinition, ToolError, ToolExecutionMode, ToolResult, ToolResultEventResult,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const STATUS_KEY: &str = "lsp";

static SHARED: Mutex<Option<Arc<LspManager>>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Process-wide manager shared by every session's LSP extension (created on first use).
pub fn shared_manager(options: &LspManagerOptions) -> Arc<LspManager> {
    let mut shared = lock(&SHARED);
    if let Some(manager) = shared.as_ref() {
        return Arc::clone(manager);
    }
    let manager = LspManager::new(options.clone());
    *shared = Some(Arc::clone(&manager));
    manager
}

/// Stops and forgets the shared manager (session shutdown); the next use starts a fresh one.
pub async fn dispose_shared_manager() {
    let manager = lock(&SHARED).take();
    if let Some(manager) = manager {
        manager.stop_all().await;
    }
}

/// The LSP extension.
#[derive(Clone, Debug, Default)]
pub struct LspExtension {
    /// User config directory holding `lsp-client.json` (mhc's agent dir).
    pub agent_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub daemon: DaemonMode,
    /// Run `lsp_diagnostics` after `write`/`edit`/`apply_patch` and append errors.
    pub post_edit_diagnostics: bool,
}

impl LspExtension {
    /// Settings from the environment: daemon links when the `mhc-lsp` sibling exists and
    /// `MHC_LSP_DAEMON` does not disable them.
    pub fn from_env(agent_dir: Option<PathBuf>) -> Self {
        let home =
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
        Self {
            agent_dir,
            home,
            daemon: default_daemon_mode(),
            post_edit_diagnostics: true,
        }
    }
}

#[cfg(unix)]
fn default_daemon_mode() -> DaemonMode {
    match crate::daemon::daemon_binary() {
        Some(binary) => DaemonMode::Auto {
            binary,
            socket: crate::daemon::default_socket_path(),
        },
        None => DaemonMode::Off,
    }
}

#[cfg(not(unix))]
fn default_daemon_mode() -> DaemonMode {
    DaemonMode::Off
}

fn result(output: ToolOutput) -> ToolResult {
    ToolResult {
        content: vec![ToolContent::text(output.text)],
        details: Some(output.details),
        structured_content: None,
        is_error: None,
    }
}

fn string_param(params: &Value, key: &str) -> Result<String, ToolError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolError::Message(format!("Missing required parameter: {key}")))
}

fn number_param(params: &Value, key: &str) -> Result<u32, ToolError> {
    params
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value as u32)
        .ok_or_else(|| ToolError::Message(format!("Missing required parameter: {key}")))
}

type Operation = Arc<
    dyn Fn(
            LspTools,
            PathBuf,
            Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ToolOutput, ToolError>> + Send>,
        > + Send
        + Sync,
>;

fn define(
    tools: &LspTools,
    fallback_cwd: &Path,
    name: &str,
    label: &str,
    description: &str,
    parameters: Value,
    operation: Operation,
) -> ToolDefinition {
    let tools = tools.clone();
    let fallback_cwd = fallback_cwd.to_path_buf();
    let mut definition = ToolDefinition::new(
        name,
        description,
        parameters,
        Arc::new(move |call: ToolCall<'_>| {
            let tools = tools.clone();
            let cwd = call.context.map_or_else(
                || fallback_cwd.clone(),
                |context| context.cwd().to_path_buf(),
            );
            let params = call.params.clone();
            let operation = Arc::clone(&operation);
            let signal = call.signal.clone();
            Box::pin(async move {
                tokio::select! {
                    output = operation(tools, cwd, params) => output.map(result),
                    () = signal.cancelled() => Err(ToolError::Aborted),
                }
            })
        }),
    );
    definition.label = label.to_owned();
    definition
}

fn lsp_error(error: crate::error::LspError) -> ToolError {
    ToolError::Message(error.to_string())
}

fn position_schema(extra: Value, required: &[&str]) -> Value {
    let mut properties = json!({
        "filePath": {"type": "string", "description": "Path to the source file"},
        "line": {"type": "number", "description": "1-based line of the symbol"},
        "character": {"type": "number", "description": "0-based column of the symbol on that line"}
    });
    if let (Some(target), Some(extra)) = (properties.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    json!({"type": "object", "properties": properties, "required": required})
}

/// The six tool definitions over `tools`.
pub fn tool_definitions(tools: &LspTools, cwd: &Path) -> Vec<ToolDefinition> {
    let diagnostics = define(
        tools,
        cwd,
        "lsp_diagnostics",
        "LSP Diagnostics",
        "Get errors, warnings, and hints from the language server BEFORE running build. Works for both single files and directories - file extension is auto-detected for directories.",
        json!({"type": "object", "properties": {
            "filePath": {"type": "string", "description": "File or directory path to check diagnostics for"},
            "severity": {"type": "string", "enum": ["error", "warning", "information", "hint", "all"], "description": "Filter by severity level"}
        }, "required": ["filePath"]}),
        Arc::new(|tools, cwd, params| {
            Box::pin(async move {
                let file = string_param(&params, "filePath")?;
                tools
                    .diagnostics(&cwd, &file, params.get("severity").and_then(Value::as_str))
                    .await
                    .map_err(lsp_error)
            })
        }),
    );
    let definition = define(
        tools,
        cwd,
        "lsp_goto_definition",
        "LSP Goto Definition",
        "Jump to symbol definition. Find WHERE something is defined.",
        position_schema(json!({}), &["filePath", "line", "character"]),
        Arc::new(|tools, cwd, params| {
            Box::pin(async move {
                let file = string_param(&params, "filePath")?;
                tools
                    .goto_definition(
                        &cwd,
                        &file,
                        number_param(&params, "line")?,
                        number_param(&params, "character")?,
                    )
                    .await
                    .map_err(lsp_error)
            })
        }),
    );
    let references = define(
        tools,
        cwd,
        "lsp_find_references",
        "LSP Find References",
        "Find ALL usages/references of a symbol across the entire workspace.",
        position_schema(
            json!({"includeDeclaration": {"type": "boolean", "description": "Include the declaration itself (default: true)"}}),
            &["filePath", "line", "character"],
        ),
        Arc::new(|tools, cwd, params| {
            Box::pin(async move {
                let file = string_param(&params, "filePath")?;
                let include = params
                    .get("includeDeclaration")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                tools
                    .find_references(
                        &cwd,
                        &file,
                        number_param(&params, "line")?,
                        number_param(&params, "character")?,
                        include,
                    )
                    .await
                    .map_err(lsp_error)
            })
        }),
    );
    let symbols = define(
        tools,
        cwd,
        "lsp_symbols",
        "LSP Symbols",
        "Get symbols from a file (document) or search across the workspace. Use scope='document' for a file outline, scope='workspace' for project-wide symbol search.",
        json!({"type": "object", "properties": {
            "filePath": {"type": "string", "description": "File path used as LSP context"},
            "scope": {"type": "string", "enum": ["document", "workspace"], "description": "'document' for file symbols, 'workspace' for project-wide search"},
            "query": {"type": "string", "description": "Symbol name to search (required for workspace scope)"},
            "limit": {"type": "number", "description": "Max results (default: 200)"}
        }, "required": ["filePath", "scope"]}),
        Arc::new(|tools, cwd, params| {
            Box::pin(async move {
                let file = string_param(&params, "filePath")?;
                let scope = string_param(&params, "scope")?;
                let limit = params
                    .get("limit")
                    .and_then(Value::as_f64)
                    .filter(|limit| *limit >= 0.0)
                    .map(|limit| limit as usize);
                tools
                    .symbols(
                        &cwd,
                        &file,
                        &scope,
                        params.get("query").and_then(Value::as_str),
                        limit,
                    )
                    .await
                    .map_err(lsp_error)
            })
        }),
    );
    let prepare = define(
        tools,
        cwd,
        "lsp_prepare_rename",
        "LSP Prepare Rename",
        "Check if rename is valid at a given position. Use BEFORE lsp_rename.",
        position_schema(json!({}), &["filePath", "line", "character"]),
        Arc::new(|tools, cwd, params| {
            Box::pin(async move {
                let file = string_param(&params, "filePath")?;
                tools
                    .prepare_rename(
                        &cwd,
                        &file,
                        number_param(&params, "line")?,
                        number_param(&params, "character")?,
                    )
                    .await
                    .map_err(lsp_error)
            })
        }),
    );
    let mut rename = define(
        tools,
        cwd,
        "lsp_rename",
        "LSP Rename",
        "Rename symbol across the entire workspace. APPLIES changes to all files.",
        position_schema(
            json!({"newName": {"type": "string", "description": "New symbol name"}}),
            &["filePath", "line", "character", "newName"],
        ),
        Arc::new(|tools, cwd, params| {
            Box::pin(async move {
                let file = string_param(&params, "filePath")?;
                let new_name = string_param(&params, "newName")?;
                tools
                    .rename(
                        &cwd,
                        &file,
                        number_param(&params, "line")?,
                        number_param(&params, "character")?,
                        &new_name,
                    )
                    .await
                    .map_err(lsp_error)
            })
        }),
    );
    rename.execution_mode = Some(ToolExecutionMode::Sequential);
    vec![
        diagnostics,
        definition,
        references,
        symbols,
        prepare,
        rename,
    ]
}

fn status_text(manager: &LspManager) -> Option<String> {
    let rows = manager.snapshot();
    if rows.is_empty() {
        return None;
    }
    let alive = rows
        .iter()
        .filter(|row| row.alive && !row.initializing)
        .count();
    let initializing = rows.iter().filter(|row| row.initializing).count();
    let mut parts = Vec::new();
    if alive > 0 {
        parts.push(format!("LSP {alive}"));
    }
    if initializing > 0 {
        parts.push(format!("init {initializing}"));
    }
    Some(parts.join(" "))
}

impl Extension for LspExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let options = LspManagerOptions {
            daemon: self.daemon.clone(),
            ..LspManagerOptions::default()
        };
        let config =
            ConfigPaths::standard(&api.cwd, self.agent_dir.as_deref(), self.home.as_deref());
        let tools = LspTools::new(shared_manager(&options), config.clone());
        for definition in tool_definitions(&tools, &api.cwd) {
            if let Err(error) = api.try_register_tool(definition) {
                maho_ext_api::diagnostic(&format!("lsp: {error}"));
            }
        }
        let status = |options: LspManagerOptions| -> maho_ext_api::ExtensionHandler {
            Arc::new(move |_event, ctx| {
                let manager = shared_manager(&options);
                Box::pin(async move {
                    ctx.ui
                        .set_status(STATUS_KEY, status_text(&manager).as_deref());
                    Ok(EventResult::None)
                })
            })
        };
        api.on(EventKind::SessionStart, status(options.clone()));
        api.on(EventKind::TurnEnd, status(options.clone()));
        if self.post_edit_diagnostics {
            let hook_tools = tools.clone();
            api.on(
                EventKind::ToolResult,
                Arc::new(move |event, ctx| {
                    let chain = HookChain::default().with_hook(Arc::new(LspDiagnosticsHook {
                        tools: hook_tools.clone(),
                    }));
                    Box::pin(async move {
                        let ExtensionEvent::ToolResult(event) = event else {
                            return Ok(EventResult::None);
                        };
                        if event.is_error || !MUTATION_TOOLS.contains(&event.tool_name.as_str()) {
                            return Ok(EventResult::None);
                        }
                        let files = extract_mutated_file_paths(&event.input);
                        if files.is_empty() {
                            return Ok(EventResult::None);
                        }
                        let mutation = Mutation {
                            tool_name: event.tool_name.clone(),
                            tool_call_id: event.tool_call_id.clone(),
                            cwd: ctx.cwd.clone(),
                            files,
                        };
                        let texts = chain.run(&mutation).await;
                        if texts.is_empty() {
                            return Ok(EventResult::None);
                        }
                        let mut content = event.content.clone();
                        content.extend(texts.into_iter().map(ToolContent::text));
                        Ok(EventResult::ToolResult(ToolResultEventResult {
                            content: Some(content),
                            details: None,
                            is_error: None,
                            usage: None,
                        }))
                    })
                }),
            );
        }
        api.on(
            EventKind::SessionShutdown,
            Arc::new(|_event, ctx| {
                Box::pin(async move {
                    ctx.ui.set_status(STATUS_KEY, None);
                    dispose_shared_manager().await;
                    Ok(EventResult::None)
                })
            }),
        );
        let command_options = options;
        api.register_command(
            "lsp",
            Some("Inspect active LSP servers, install or warm up servers".to_owned()),
            Some("[status|install <id>|warmup <id>]".to_owned()),
            Arc::new(move |raw, ctx| {
                let manager = shared_manager(&command_options);
                let config = config.clone();
                let args = raw.trim().to_owned();
                Box::pin(async move {
                    if let Some(id) = args.strip_prefix("install") {
                        let id = id.trim();
                        if id.is_empty() {
                            ctx.ui.notify(
                                "Usage: /lsp install <serverId>",
                                NotificationType::Warning,
                            );
                        } else {
                            run_install(id, ctx).await;
                        }
                        return Ok(());
                    }
                    if let Some(id) = args.strip_prefix("warmup") {
                        let id = id.trim();
                        if id.is_empty() {
                            ctx.ui
                                .notify("Usage: /lsp warmup <serverId>", NotificationType::Warning);
                            return Ok(());
                        }
                        match merged_servers(&config)
                            .into_iter()
                            .find(|merged| merged.server.id == id)
                        {
                            Some(merged) => {
                                manager.warmup_client(&ctx.cwd, &merged.server);
                                ctx.ui.notify(
                                    &format!("Warming up '{id}' in background"),
                                    NotificationType::Info,
                                );
                            }
                            None => ctx.ui.notify(
                                &format!("Unknown server id '{id}'."),
                                NotificationType::Error,
                            ),
                        }
                        return Ok(());
                    }
                    if args == "status" {
                        let mut seen = Vec::new();
                        for merged in merged_servers(&config) {
                            if !seen.contains(&merged.server.id)
                                && crate::config::is_server_installed(
                                    &merged.server.command,
                                    &ctx.cwd,
                                )
                            {
                                seen.push(merged.server.id);
                            }
                        }
                        let summary = if seen.is_empty() {
                            "No installed LSP servers".to_owned()
                        } else {
                            format!("{} installed server(s): {}", seen.len(), seen.join(", "))
                        };
                        ctx.ui.notify(&summary, NotificationType::Info);
                        return Ok(());
                    }
                    let rows = manager.snapshot();
                    let summary = if rows.is_empty() {
                        "No active LSP servers".to_owned()
                    } else {
                        let lines: Vec<String> = rows
                            .iter()
                            .map(|row| {
                                let state = if row.initializing {
                                    "initializing"
                                } else if row.alive {
                                    "alive"
                                } else {
                                    "dead"
                                };
                                let via = if row.via_daemon { " via mhc-lsp" } else { "" };
                                format!(
                                    "{} {} ({state}{via}, refs {}, idle {}s) {}",
                                    row.server_id,
                                    row.root.display(),
                                    row.ref_count,
                                    row.idle_for.as_secs(),
                                    row.command.join(" ")
                                )
                            })
                            .collect();
                        format!("{} active:\n{}", rows.len(), lines.join("\n"))
                    };
                    ctx.ui.notify(&summary, NotificationType::Info);
                    Ok(())
                })
            }),
        );
    }
}

async fn run_install(id: &str, ctx: &maho_ext_api::ExtensionContext) {
    let Some((_, recipe)) = INSTALL_RECIPES.iter().find(|(key, _)| *key == id) else {
        if let Some(hint) = crate::config::install_hint(id) {
            ctx.ui.notify(
                &format!("No automated installer for '{id}'. Manual install: {hint}"),
                NotificationType::Warning,
            );
        } else if BUILTIN_SERVERS.iter().any(|(key, _, _)| *key == id) {
            ctx.ui.notify(
                &format!("No automated installer or hint for '{id}'."),
                NotificationType::Warning,
            );
        } else {
            ctx.ui.notify(
                &format!("Unknown server id '{id}'."),
                NotificationType::Error,
            );
        }
        return;
    };
    let Some((program, args)) = recipe.split_first() else {
        ctx.ui.notify(
            &format!("Invalid install recipe for '{id}'"),
            NotificationType::Error,
        );
        return;
    };
    ctx.ui
        .set_status(STATUS_KEY, Some(&format!("Installing {id}...")));
    let output = tokio::process::Command::new(program)
        .args(args.iter())
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    ctx.ui.set_status(STATUS_KEY, None);
    match output {
        Err(error) => ctx
            .ui
            .notify(&format!("Install failed: {error}"), NotificationType::Error),
        Ok(output) if output.status.success() => ctx.ui.notify(
            &format!("Installed '{id}' successfully"),
            NotificationType::Info,
        ),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let lines: Vec<&str> = stderr.trim().lines().collect();
            let tail = lines[lines.len().saturating_sub(3)..].join("\n");
            let code = output
                .status
                .code()
                .map_or_else(|| "null".to_owned(), |code| code.to_string());
            ctx.ui.notify(
                &format!("Install '{id}' failed (exit {code})\n{tail}"),
                NotificationType::Error,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_tools_with_upstream_names() {
        let tools = LspTools::new(
            LspManager::new(LspManagerOptions::default()),
            ConfigPaths::default(),
        );
        let definitions = tool_definitions(&tools, Path::new("/"));
        let names: Vec<&str> = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "lsp_diagnostics",
                "lsp_goto_definition",
                "lsp_find_references",
                "lsp_symbols",
                "lsp_prepare_rename",
                "lsp_rename"
            ]
        );
        assert_eq!(
            definitions[5].execution_mode,
            Some(ToolExecutionMode::Sequential)
        );
        assert_eq!(definitions[0].parameters["required"], json!(["filePath"]));
    }
}
