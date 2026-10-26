use maho_ext_api::{
    AgentToolResult, ContentBlock, Extension, ExtensionApi, ExtensionToolExecutor, ToolDefinition,
    ToolExecutionMode, ToolExposure,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;
pub const DESKTOP_TOOLKIT_TOOL_NAME: &str = "omo_desktop_toolkit";
pub const TOOLKIT_REQUEST_EVENT: &str = "omo.toolkit.request";
pub const TOOLKIT_GRANT_RPC: &str = "omo.toolkit.grant";
pub const TOOLKITS_CONTEXT_KEY: &str = "omo_toolkits";
fn reply(details: Value) -> AgentToolResult {
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
pub fn parse_grant(value: &Value) -> Option<Value> {
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
    Some(json!({"endpoint":endpoint,"tokenFile":token_file}))
}
#[derive(Clone)]
pub struct DesktopToolkitComponent {
    pub timeout_ms: u64,
}
impl Default for DesktopToolkitComponent {
    fn default() -> Self {
        Self { timeout_ms: 15_000 }
    }
}
pub fn create_desktop_toolkit_component(timeout_ms: Option<u64>) -> DesktopToolkitComponent {
    DesktopToolkitComponent {
        timeout_ms: timeout_ms.unwrap_or(15_000),
    }
}
impl Extension for DesktopToolkitComponent {
    fn register(&self, api: &mut ExtensionApi) {
        type Pending = Arc<Mutex<BTreeMap<String, oneshot::Sender<Value>>>>;
        let pending: Pending = Arc::new(Mutex::new(BTreeMap::new()));
        let offered = offered_toolkits(&api.profile.session_context);
        let events = api.events.clone();
        let timeout_ms = self.timeout_ms;
        let requests = pending.clone();
        let mut tool = ToolDefinition::new(
            DESKTOP_TOOLKIT_TOOL_NAME,
            "Internal channel for skills: ask the app for a toolkit it hosts for this thread, such as its in-app browser. Not for direct use.",
            json!({"type":"object","properties":{"op":{"type":"string","enum":["acquire"]},"toolkit":{"type":"string"}},"required":["op","toolkit"],"additionalProperties":false}),
            Arc::new(|_| Box::pin(async { panic!("legacy executor must not run") })),
        );
        tool.label = "Desktop toolkit".into();
        tool.exposure = Some(ToolExposure::Eval);
        tool.execution_mode = Some(ToolExecutionMode::Parallel);
        tool.permission_parser = Some(Arc::new(|_, _| Vec::new()));
        let execute: ExtensionToolExecutor = Arc::new(
            move |_id, params, _signal, _update, _context| {
                let pending = requests.clone();
                let offered = offered.clone();
                let events = events.clone();
                Box::pin(async move {
                    let toolkit = params.get("toolkit").and_then(Value::as_str).unwrap_or("");
                    if params.get("op").and_then(Value::as_str) != Some("acquire")
                        || toolkit.is_empty()
                    {
                        return Ok(reply(json!({"ok":false,"reason":"invalid_request"})));
                    }
                    if !offered.contains(toolkit) {
                        return Ok(reply(json!({"ok":false,"reason":"unavailable"})));
                    }
                    let id = uuid::Uuid::new_v4().to_string();
                    let (send, receive) = oneshot::channel();
                    pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(id.clone(), send);
                    events.emit("senpi:extension-rpc-event", &json!({"name":TOOLKIT_REQUEST_EVENT,"data":{"requestId":id,"toolkit":toolkit}}));
                    let settled = match tokio::time::timeout(
                        Duration::from_millis(timeout_ms),
                        receive,
                    )
                    .await
                    {
                        Ok(Ok(value)) => value,
                        _ => json!({"ok":false,"reason":"timeout"}),
                    };
                    pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&id);
                    Ok(reply(settled))
                })
            },
        );
        if let Err(error) = api.register_tool_with_extension_context(tool, execute) {
            std::panic::panic_any(error);
        }
        if let Err(error) = api.rpc_handle(TOOLKIT_GRANT_RPC, Arc::new(move |message| {
            let pending = pending.clone();
            Box::pin(async move {
                let sender = message.get("requestId").and_then(Value::as_str).and_then(|id| pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id));
                let Some(sender) = sender else { return Ok(json!({"accepted":false})); };
                let settled = if let Some(grant) = message.get("grant").and_then(parse_grant) { json!({"ok":true,"grant":grant}) } else { json!({"ok":false,"reason":message.pointer("/refusal/reason").and_then(Value::as_str).unwrap_or("invalid_grant")}) };
                let _ = sender.send(settled);
                Ok(json!({"accepted":true}))
            })
        })) { std::panic::panic_any(error); }
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
