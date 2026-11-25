//! Port of senpi `packages/coding-agent/src/modes/rpc/mcp-presenter-host-surface.ts`.
//!
//! Host surface trait and `mcp_presenter_context` reply builder (senpi#4249).

use crate::mcp_form_types::McpFormBinding;
use crate::mcp_presenter_authority::{McpPresenterAuthority, McpPresenterConnection};
use serde_json::{Value, json};
use std::sync::Arc;

pub trait PresenterHostSurface: Send + Sync {
    fn authority(&self) -> &McpPresenterAuthority;
    fn shared_authority(&self) -> bool {
        false
    }
    fn get_mcp_form_binding(&self, session_id: &str) -> Option<Arc<McpFormBinding>>;
    fn find_mcp_presenter_connection(&self, nonce: &str) -> Option<McpPresenterConnection>;
    fn get_connection_capabilities(&self, id: &str) -> Option<Vec<String>>;
    fn advertise_mcp_presenter_control(&self, path: &str);
    fn withdraw_mcp_presenter_control(&self, path: &str);
}

pub fn presenter_context_reply(
    authority: Option<&McpPresenterAuthority>,
    sender: Option<&McpPresenterConnection>,
    request: &Value,
    advertised: Option<&str>,
) -> Value {
    let session_id = request.get("sessionId").and_then(Value::as_str);
    let context = match (authority, sender, session_id) {
        (Some(auth), Some(conn), Some(sid)) => auth.connection_context(conn, sid),
        _ => None,
    };
    let Some(context) = context else {
        return json!({ "error": "presenter_not_authorized" });
    };
    let discover = request.get("discover").and_then(Value::as_bool) == Some(true);
    let mut ctx_val = context.to_value();
    if discover
        && let Some(endpoint) = advertised
        && let Some(obj) = ctx_val.as_object_mut()
    {
        obj.insert("controlEndpoint".to_owned(), json!(endpoint));
    }
    json!({ "ok": true, "context": ctx_val })
}
