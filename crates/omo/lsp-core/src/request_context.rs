//! Port of TS `request-context.ts`. TS used `AsyncLocalStorage`; Rust installs the context
//! for a synchronous closure ([`run_with_request_context`]) via a thread-local stack, or for a
//! future ([`scope_request_context`]) via a tokio task-local. [`lsp_request_context`] checks
//! the task-local first, then the thread-local stack.

use serde::Serialize;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspRequestCapabilities {
    pub install_decision_tool: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LspRequestContext {
    pub cwd: String,
    pub project_config_paths: Vec<String>,
    pub user_config_path: String,
    pub install_decisions_path: String,
    pub capabilities: LspRequestCapabilities,
}

pub type RequestContext = LspRequestContext;

#[derive(Debug, Clone, Default)]
pub struct StandaloneMcpRequestContextInput {
    pub cwd: Option<String>,
    /// Environment overrides; `None` reads the process environment.
    pub env: Option<HashMap<String, String>>,
    pub home_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LspRequestContextParseError {
    pub code: String,
    pub message: String,
}

impl LspRequestContextParseError {
    fn new(code: &str, message: String) -> Self {
        Self {
            code: code.to_string(),
            message,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "LSP request context is required. Standalone MCP startup must install one with runWithRequestContext(createStandaloneMcpRequestContext())."
)]
pub struct LspRequestContextUnavailableError;

thread_local! {
    static CONTEXT_STACK: RefCell<Vec<LspRequestContext>> = const { RefCell::new(Vec::new()) };
}

tokio::task_local! {
    static TASK_CONTEXT: LspRequestContext;
}

struct StackGuard;

impl Drop for StackGuard {
    fn drop(&mut self) {
        CONTEXT_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

/// TS `runWithRequestContext` for synchronous work.
pub fn run_with_request_context<T>(context: LspRequestContext, f: impl FnOnce() -> T) -> T {
    CONTEXT_STACK.with(|stack| stack.borrow_mut().push(context));
    let _guard = StackGuard;
    f()
}

/// TS `runWithRequestContext` for async work: the context follows the future across awaits.
pub async fn scope_request_context<F: Future>(context: LspRequestContext, future: F) -> F::Output {
    TASK_CONTEXT.scope(context, future).await
}

/// TS `lspRequestContext`.
pub fn lsp_request_context() -> Result<LspRequestContext, LspRequestContextUnavailableError> {
    if let Ok(context) = TASK_CONTEXT.try_with(Clone::clone) {
        return Ok(context);
    }
    CONTEXT_STACK
        .with(|stack| stack.borrow().last().cloned())
        .ok_or(LspRequestContextUnavailableError)
}

/// TS `contextCwd`.
pub fn context_cwd() -> Result<String, LspRequestContextUnavailableError> {
    lsp_request_context().map(|context| context.cwd)
}

/// TS `contextEnv`.
pub fn context_env(key: &str) -> Result<Option<String>, LspRequestContextUnavailableError> {
    let context = lsp_request_context()?;
    Ok(match key {
        "LSP_TOOLS_MCP_PROJECT_CONFIG" => Some(context.project_config_paths.join(DELIMITER)),
        "LSP_TOOLS_MCP_USER_CONFIG" => Some(context.user_config_path),
        "LSP_TOOLS_MCP_INSTALL_DECISIONS" => Some(context.install_decisions_path),
        _ => None,
    })
}

/// Node `path.delimiter`.
pub const DELIMITER: &str = if cfg!(windows) { ";" } else { ":" };

/// TS `createStandaloneMcpRequestContext`.
pub fn create_standalone_mcp_request_context(
    input: StandaloneMcpRequestContextInput,
) -> Result<LspRequestContext, LspRequestContextParseError> {
    let env_value = |key: &str| -> Option<String> {
        match &input.env {
            Some(env) => env.get(key).cloned(),
            None => std::env::var(key).ok(),
        }
    };
    let cwd_input = match input.cwd.clone() {
        Some(cwd) => cwd,
        None => std::env::current_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string()),
    };
    let cwd = canonical_cwd(&cwd_input)?;
    let home = input
        .home_dir
        .clone()
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_default();
    let project_config_paths =
        translate_project_config_env(env_value("LSP_TOOLS_MCP_PROJECT_CONFIG"), &cwd);
    let user_config_path = translate_home_config_env(
        env_value("LSP_TOOLS_MCP_USER_CONFIG"),
        &home,
        ".codex/lsp-client.json",
    );
    let install_decisions_path = translate_home_config_env(
        env_value("LSP_TOOLS_MCP_INSTALL_DECISIONS"),
        &home,
        ".codex/lsp-install-decisions.json",
    );
    parse_lsp_request_context(&serde_json::json!({
        "cwd": cwd,
        "projectConfigPaths": project_config_paths,
        "userConfigPath": user_config_path,
        "installDecisionsPath": install_decisions_path,
        "capabilities": { "installDecisionTool": true },
    }))
}

const CONTEXT_FIELDS: [&str; 5] = [
    "cwd",
    "projectConfigPaths",
    "userConfigPath",
    "installDecisionsPath",
    "capabilities",
];

/// TS `parseLspRequestContext`.
pub fn parse_lsp_request_context(
    value: &Value,
) -> Result<LspRequestContext, LspRequestContextParseError> {
    let Some(record) = value.as_object() else {
        return Err(LspRequestContextParseError::new(
            "invalid_context",
            "LSP request context must be an object.".to_string(),
        ));
    };
    reject_unknown_fields(record, &CONTEXT_FIELDS, "context")?;
    let cwd = string_field(record, "cwd")?;
    let project_config_paths = string_array_field(record, "projectConfigPaths")?;
    let user_config_path = string_field(record, "userConfigPath")?;
    let install_decisions_path = string_field(record, "installDecisionsPath")?;
    let capabilities = capabilities_field(record.get("capabilities"))?;
    let canonical = canonical_cwd(&cwd)?;

    for path in &project_config_paths {
        require_absolute_path(path, "projectConfigPaths")?;
        let project_path = canonicalize_existing_or_nearest_ancestor(path).map_err(|error| {
            LspRequestContextParseError::new("invalid_field", error.to_string())
        })?;
        if !is_path_inside(&canonical, &project_path) {
            return Err(LspRequestContextParseError::new(
                "project_config_outside_cwd",
                format!("Project LSP config path must be inside cwd: {path}"),
            ));
        }
    }
    require_absolute_path(&user_config_path, "userConfigPath")?;
    require_absolute_path(&install_decisions_path, "installDecisionsPath")?;

    let mut canonical_project_paths = Vec::with_capacity(project_config_paths.len());
    for path in &project_config_paths {
        canonical_project_paths.push(canonicalize_existing_or_nearest_ancestor(path).map_err(
            |error| LspRequestContextParseError::new("invalid_field", error.to_string()),
        )?);
    }
    Ok(LspRequestContext {
        cwd: canonical,
        project_config_paths: canonical_project_paths,
        user_config_path,
        install_decisions_path,
        capabilities,
    })
}

fn translate_project_config_env(value: Option<String>, cwd: &str) -> Vec<String> {
    match value.filter(|value| !value.is_empty()) {
        None => vec![join(cwd, &[".codex", "lsp-client.json"])],
        Some(value) => value
            .split(DELIMITER)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                if Path::new(entry).is_absolute() {
                    entry.to_string()
                } else {
                    join(cwd, &[entry])
                }
            })
            .collect(),
    }
}

fn translate_home_config_env(value: Option<String>, home: &str, fallback: &str) -> String {
    match value.filter(|value| !value.is_empty()) {
        None => join(home, &[fallback]),
        Some(value) if Path::new(&value).is_absolute() => value,
        Some(value) => join(home, &[&value]),
    }
}

fn canonical_cwd(cwd: &str) -> Result<String, LspRequestContextParseError> {
    let resolved = resolve(cwd);
    let invalid = || {
        LspRequestContextParseError::new(
            "invalid_cwd",
            format!("LSP request cwd must be an existing directory: {cwd}"),
        )
    };
    if !Path::new(&resolved).is_dir() {
        return Err(invalid());
    }
    std::fs::canonicalize(&resolved)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|_| invalid())
}

/// TS `canonicalizeExistingOrNearestAncestor`: realpath of the nearest existing ancestor
/// with the missing suffix re-appended.
pub fn canonicalize_existing_or_nearest_ancestor(path: &str) -> std::io::Result<String> {
    let mut current = PathBuf::from(resolve(path));
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&current) {
            Ok(mut existing) => {
                for part in suffix.iter().rev() {
                    existing.push(part);
                }
                return Ok(existing.to_string_lossy().into_owned());
            }
            Err(error) => {
                let missing = error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ENOTDIR);
                if !missing {
                    return Err(error);
                }
                let Some(name) = current.file_name().map(std::ffi::OsStr::to_os_string) else {
                    return Err(error);
                };
                suffix.push(name);
                if !current.pop() {
                    return Err(error);
                }
            }
        }
    }
}

fn capabilities_field(
    value: Option<&Value>,
) -> Result<LspRequestCapabilities, LspRequestContextParseError> {
    let Some(record) = value.and_then(Value::as_object) else {
        return Err(LspRequestContextParseError::new(
            "invalid_capabilities",
            "LSP request capabilities must be an object.".to_string(),
        ));
    };
    reject_unknown_fields(record, &["installDecisionTool"], "capabilities")?;
    match record.get("installDecisionTool").and_then(Value::as_bool) {
        Some(install_decision_tool) => Ok(LspRequestCapabilities {
            install_decision_tool,
        }),
        None => Err(LspRequestContextParseError::new(
            "invalid_install_decision_capability",
            "LSP request capabilities.installDecisionTool must be a boolean.".to_string(),
        )),
    }
}

fn string_field(
    record: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<String, LspRequestContextParseError> {
    match record.get(field).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(LspRequestContextParseError::new(
            "invalid_field",
            format!("LSP request context.{field} must be a non-empty string."),
        )),
    }
}

fn string_array_field(
    record: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Vec<String>, LspRequestContextParseError> {
    let invalid = || {
        LspRequestContextParseError::new(
            "invalid_field",
            format!("LSP request context.{field} must be a non-empty string array."),
        )
    };
    let items = record
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    items
        .iter()
        .map(|item| match item.as_str() {
            Some(value) if !value.is_empty() => Ok(value.to_string()),
            _ => Err(invalid()),
        })
        .collect()
}

fn require_absolute_path(path: &str, field: &str) -> Result<(), LspRequestContextParseError> {
    if Path::new(path).is_absolute() {
        Ok(())
    } else {
        Err(LspRequestContextParseError::new(
            "relative_path",
            format!("LSP request context.{field} must be absolute: {path}"),
        ))
    }
}

fn reject_unknown_fields(
    record: &serde_json::Map<String, Value>,
    allowed: &[&str],
    scope: &str,
) -> Result<(), LspRequestContextParseError> {
    let unknown: Vec<&str> = record
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(LspRequestContextParseError::new(
            "unknown_field",
            format!("Unknown LSP request {scope} field: {}", unknown.join(", ")),
        ))
    }
}

/// TS `isPathInside` (lexical, Node `path.relative` semantics).
pub fn is_path_inside(parent: &str, child: &str) -> bool {
    let parent = PathBuf::from(resolve(parent));
    let child = PathBuf::from(resolve(child));
    child.starts_with(&parent)
}

/// Node `path.resolve`: absolute, lexically normalized (no filesystem access).
pub fn resolve(path: &str) -> String {
    let base = if Path::new(path).is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    };
    normalize(&base.join(path))
}

/// Node `path.resolve(base, path)`.
pub fn resolve_from(base: &str, path: &str) -> String {
    if Path::new(path).is_absolute() {
        return resolve(path);
    }
    resolve(&Path::new(base).join(path).to_string_lossy())
}

/// Node `path.join(base, ...parts)` (normalized).
pub fn join(base: &str, parts: &[&str]) -> String {
    let mut path = PathBuf::from(base);
    for part in parts {
        path.push(part.trim_start_matches('/'));
    }
    normalize(&path)
}

/// Node `path.dirname` for absolute normalized paths.
pub fn dirname(path: &str) -> String {
    Path::new(path).parent().map_or_else(
        || path.to_string(),
        |parent| parent.to_string_lossy().into_owned(),
    )
}

fn normalize(path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        return ".".to_string();
    }
    out.to_string_lossy().into_owned()
}

#[cfg(test)]
#[path = "request_context_tests.rs"]
mod tests;
