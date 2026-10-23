//! The `onboarding_session_history` tool (upstream `session-history-tool.ts`).

use std::sync::Arc;

use maho_ext_api::{ToolDefinition, ToolError, ToolExecutionMode, ToolExposure, ToolResult};
use serde_json::{Value, json};

use super::tool_runtime::{
    OnboardingSessionHistoryParams, OnboardingSessionHistoryRun, execute_onboarding_session_history,
};
use super::types::SessionHistoryOptions;

pub const ONBOARDING_SESSION_HISTORY_TOOL_NAME: &str = "onboarding_session_history";

pub type NewScanId = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone)]
pub struct CreateOnboardingSessionHistoryToolOptions {
    /// Engine state dir; digests land in `<stateDir>/onboarding/scans/<scanId>/digests.jsonl`.
    pub state_dir: String,
    /// Test seam: fixture homes, fs, clock. Production uses the module's defaults.
    pub scan_options: Option<SessionHistoryOptions>,
    pub new_scan_id: Option<NewScanId>,
}

/// JSON schema of the tool parameters: optional `workspace_root` (min length 1) and `recent_days` (1..=365).
pub fn onboarding_session_history_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "workspace_root": {"type": "string", "minLength": 1},
            "recent_days": {"type": "integer", "minimum": 1, "maximum": 365},
        },
        "additionalProperties": false,
    })
}

/// Validates the call arguments against the schema above.
pub fn parse_params(params: &Value) -> Result<OnboardingSessionHistoryParams, String> {
    let Some(object) = params.as_object() else {
        return Err("params must be an object".to_owned());
    };
    if let Some(extra) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "workspace_root" | "recent_days"))
    {
        return Err(format!("unexpected property: {extra}"));
    }
    let workspace_root = match object.get("workspace_root") {
        None => None,
        Some(Value::String(root)) if !root.is_empty() => Some(root.clone()),
        Some(_) => return Err("workspace_root must be a non-empty string".to_owned()),
    };
    let recent_days = match object.get("recent_days") {
        None => None,
        Some(value) => match value.as_i64() {
            Some(days) if (1..=365).contains(&days) => Some(days),
            _ => return Err("recent_days must be an integer from 1 to 365".to_owned()),
        },
    };
    Ok(OnboardingSessionHistoryParams {
        workspace_root,
        recent_days,
    })
}

/// TUI consumer of the engine session-history module: drives the same scan stream the onboarding scan RPC
/// exposes for the `projects` and `sessions` sources. Read-only, no model call, no network.
pub fn create_onboarding_session_history_tool(
    options: CreateOnboardingSessionHistoryToolOptions,
) -> ToolDefinition {
    let new_scan_id: NewScanId = options
        .new_scan_id
        .clone()
        .unwrap_or_else(|| Arc::new(|| format!("tui-{}", uuid::Uuid::new_v4())));
    let state_dir = options.state_dir.clone();
    let scan_options = options.scan_options.clone();
    let mut tool = ToolDefinition::new(
        ONBOARDING_SESSION_HISTORY_TOOL_NAME,
        "Onboarding: lists this machine's coding-agent projects and recent sessions (Claude Code and other agents' local history) \
         with a per-session digest: first request, tools used, files changed, outcome. Programmatic and read-only; \
         digests stay on this machine. Use it instead of reading transcript files by hand.",
        onboarding_session_history_parameters(),
        Arc::new(move |call| {
            let params = call.params.clone();
            let state_dir = state_dir.clone();
            let scan_options = scan_options.clone();
            let scan_id = new_scan_id();
            Box::pin(async move {
                let params = parse_params(&params).map_err(ToolError::Message)?;
                let result = execute_onboarding_session_history(OnboardingSessionHistoryRun {
                    params,
                    state_dir,
                    scan_options,
                    scan_id,
                })
                .map_err(ToolError::Message)?;
                let mut out = ToolResult::text(result.text);
                out.details = Some(result.details.to_json());
                Ok(out)
            })
        }),
    );
    tool.label = "Scan agent session history".into();
    tool.exposure = Some(ToolExposure::Search);
    tool.search_group = Some("onboarding".into());
    tool.search_keywords = Some(
        [
            "onboarding",
            "session archaeology",
            "previous sessions",
            "agent history",
            "projects",
        ]
        .iter()
        .map(|k| (*k).to_owned())
        .collect(),
    );
    tool.allow_lazy_activation = Some(true);
    tool.execution_mode = Some(ToolExecutionMode::Sequential);
    tool
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_follow_the_schema() {
        assert!(parse_params(&json!({})).is_ok());
        assert_eq!(
            parse_params(&json!({"workspace_root": "/p", "recent_days": 7})).ok(),
            Some(OnboardingSessionHistoryParams {
                workspace_root: Some("/p".into()),
                recent_days: Some(7)
            })
        );
        for bad in [
            json!({"workspace_root": ""}),
            json!({"recent_days": 0}),
            json!({"recent_days": 366}),
            json!({"recent_days": 1.5}),
            json!({"extra": 1}),
            json!("x"),
        ] {
            assert!(parse_params(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn definition_is_a_lazy_sequential_search_tool() {
        let tool =
            create_onboarding_session_history_tool(CreateOnboardingSessionHistoryToolOptions {
                state_dir: "/s".into(),
                scan_options: None,
                new_scan_id: None,
            });
        assert_eq!(tool.name, "onboarding_session_history");
        assert_eq!(tool.label, "Scan agent session history");
        assert_eq!(tool.exposure, Some(ToolExposure::Search));
        assert_eq!(tool.search_group.as_deref(), Some("onboarding"));
        assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential));
        assert_eq!(tool.parameters["additionalProperties"], json!(false));
    }
}
