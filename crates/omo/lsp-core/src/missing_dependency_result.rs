use crate::lsp::errors::LspError;
use crate::lsp::types::FailedServerLookupResult;
use crate::lsp::utils::handle_missing_dependency_error;
use crate::request_context::lsp_request_context;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

/// TS `ToolExecutionResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionResult {
    pub content: Vec<ToolTextContent>,
    pub details: Map<String, Value>,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTextContent {
    pub text: String,
}

impl ToolExecutionResult {
    pub fn text(text: impl Into<String>, details: Map<String, Value>) -> Self {
        Self {
            content: vec![ToolTextContent { text: text.into() }],
            details,
            is_error: false,
        }
    }

    /// TS `text(text, details, true)`.
    pub fn error_text(text: impl Into<String>, details: Map<String, Value>) -> Self {
        Self {
            is_error: true,
            ..Self::text(text, details)
        }
    }

    pub fn first_text(&self) -> &str {
        self.content
            .first()
            .map_or("", |content| content.text.as_str())
    }

    /// MCP `tools/call` result shape: `{ content: [{ type: "text", text }], ... }`.
    pub fn content_json(&self) -> Value {
        Value::Array(
            self.content
                .iter()
                .map(|content| json!({ "type": "text", "text": content.text }))
                .collect(),
        )
    }
}

/// TS `missingDependencyResult`.
pub fn missing_dependency_result(
    error: &LspError,
    details: Map<String, Value>,
) -> Option<ToolExecutionResult> {
    let message = handle_missing_dependency_error(error)?;
    let mut merged = details;
    merged.insert("error".to_string(), Value::String(message.clone()));
    merged.insert(
        "errorKind".to_string(),
        Value::String("missing_dependency".to_string()),
    );
    if let Some(availability) = missing_dependency_availability(error) {
        merged.insert("availability".to_string(), availability);
    }
    Some(ToolExecutionResult::text(message, merged))
}

fn missing_dependency_availability(error: &LspError) -> Option<Value> {
    let LspError::ServerLookup {
        lookup: Some(lookup),
        ..
    } = error
    else {
        return None;
    };
    let context = lsp_request_context().ok()?;
    Some(match lookup {
        FailedServerLookupResult::NotConfigured {
            extension,
            available_servers,
        } => json!({
            "kind": "not_configured",
            "extension": extension,
            "availableServers": available_servers,
            "projectConfigPaths": context.project_config_paths,
            "userConfigPath": context.user_config_path,
            "installDecisionTool": context.capabilities.install_decision_tool,
        }),
        FailedServerLookupResult::NotInstalled {
            server,
            install_hint,
        } => json!({
            "kind": "not_installed",
            "serverId": server.id,
            "command": server.command,
            "extensions": server.extensions,
            "installHint": install_hint,
            "installDecisionTool": context.capabilities.install_decision_tool,
            "installDecisionsPath": context.install_decisions_path,
        }),
    })
}

#[cfg(test)]
#[path = "missing_dependency_result_tests.rs"]
mod tests;
