use maho_ext_api::{
    AgentToolResult, ContentBlock, Extension, ExtensionApi, ExtensionToolExecutor, ToolDefinition,
    ToolExecutionMode, ToolExposure,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};
pub mod runtime;
pub mod thread_tools;
pub const DEFAULT_ACQUIRE_TIMEOUT_MS: u64 = 15_000;
pub const DESKTOP_TOOLKIT_TOOL_NAME: &str = "omo_desktop_toolkit";
pub const TOOLKIT_REQUEST_EVENT: &str = "omo.toolkit.request";
pub const TOOLKIT_GRANT_RPC: &str = "omo.toolkit.grant";
pub const TOOLKITS_CONTEXT_KEY: &str = "omo_toolkits";
pub(crate) fn reply(details: Value) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::text(details.to_string())],
        details,
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}
pub fn offered_toolkits(context: &BTreeMap<String, String>) -> BTreeSet<String> {
    context
        .get(TOOLKITS_CONTEXT_KEY)
        .into_iter()
        .flat_map(|raw| raw.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
/// What the app hands one session for one toolkit. The token stays in the file, never in a tool result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolkitGrant {
    pub endpoint: String,
    pub token_file: String,
}
impl ToolkitGrant {
    pub fn to_value(&self) -> Value {
        json!({"endpoint":self.endpoint,"tokenFile":self.token_file})
    }
}
pub fn parse_toolkit_grant(value: &Value) -> Option<ToolkitGrant> {
    let endpoint = value.get("endpoint")?.as_str()?;
    let token_file = value.get("tokenFile")?.as_str()?;
    if !Path::new(token_file).has_root() {
        return None;
    }
    let url = url::Url::parse(endpoint).ok()?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some_and(|query| !query.is_empty())
    {
        return None;
    }
    Some(ToolkitGrant {
        endpoint: endpoint.to_owned(),
        token_file: token_file.to_owned(),
    })
}
pub fn parse_grant(value: &Value) -> Option<Value> {
    parse_toolkit_grant(value).map(|grant| grant.to_value())
}
#[derive(Clone)]
pub struct DesktopToolkitComponent {
    pub timeout_ms: u64,
}
impl Default for DesktopToolkitComponent {
    fn default() -> Self {
        Self {
            timeout_ms: DEFAULT_ACQUIRE_TIMEOUT_MS,
        }
    }
}
pub fn create_desktop_toolkit_component(timeout_ms: Option<u64>) -> DesktopToolkitComponent {
    DesktopToolkitComponent {
        timeout_ms: timeout_ms.unwrap_or(DEFAULT_ACQUIRE_TIMEOUT_MS),
    }
}
impl Extension for DesktopToolkitComponent {
    /// The lazy runtime (acquire tool, grant rpc, thread discovery) is registered only when the app
    /// offers at least one toolkit; otherwise a stub tool answers `unavailable` at once.
    fn register(&self, api: &mut ExtensionApi) {
        let offered = api
            .profile
            .session_context
            .get(TOOLKITS_CONTEXT_KEY)
            .is_some_and(|raw| !raw.trim().is_empty());
        if offered {
            runtime::register_runtime(api, self.timeout_ms);
            return;
        }
        let mut tool = ToolDefinition::new(
            DESKTOP_TOOLKIT_TOOL_NAME,
            "Internal channel for skills: ask the app for a toolkit it hosts for this thread. Not for direct use.",
            runtime::acquire_parameters(),
            Arc::new(|_| Box::pin(async { panic!("legacy executor must not run") })),
        );
        tool.label = "Desktop toolkit".into();
        tool.exposure = Some(ToolExposure::Eval);
        tool.execution_mode = Some(ToolExecutionMode::Parallel);
        tool.permission_parser = Some(Arc::new(|_, _| Vec::new()));
        let execute: ExtensionToolExecutor =
            Arc::new(move |_id, params, _signal, _update, _context| {
                Box::pin(async move {
                    let toolkit = params.get("toolkit").and_then(Value::as_str).unwrap_or("");
                    let reason = if params.get("op").and_then(Value::as_str) == Some("acquire")
                        && !toolkit.is_empty()
                    {
                        "unavailable"
                    } else {
                        "invalid_request"
                    };
                    Ok(reply(json!({"ok":false,"reason":reason})))
                })
            });
        if let Err(error) = api.register_tool_with_extension_context(tool, execute) {
            std::panic::panic_any(error);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_grants_and_session_offers() {
        assert!(
            parse_grant(
                &json!({"endpoint":"http://127.0.0.1:3000/toolkit","tokenFile":"/tmp/token"})
            )
            .is_some()
        );
        for endpoint in [
            "https://127.0.0.1:3000",
            "http://localhost:3000",
            "http://u:p@127.0.0.1:3000",
            "http://127.0.0.1:3000/?token=secret",
        ] {
            assert!(parse_grant(&json!({"endpoint":endpoint,"tokenFile":"/tmp/token"})).is_none());
        }
        assert!(
            parse_grant(&json!({"endpoint":"http://127.0.0.1:3000","tokenFile":"token"})).is_none()
        );
        assert_eq!(
            offered_toolkits(&BTreeMap::from([(
                TOOLKITS_CONTEXT_KEY.into(),
                " browser, , desktop,browser ".into()
            )])),
            BTreeSet::from(["browser".into(), "desktop".into()])
        );
    }
}
