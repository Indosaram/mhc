//! Extension session-control surface, including host-provided relocation.
use futures::future::BoxFuture;
use maho_ext_api::AbortSignal;
use serde_json::{Value, json};
pub const SESSION_CONTROL_DELIVERY_TYPE: &str = "session_control_delivery";
pub fn is_session_control_delivery_details(v: &Value) -> bool {
    v["delivery_id"].is_string()
        && v["source"] == "session_control"
        && ["steer", "followUp"].contains(&v["deliverAs"].as_str().unwrap_or(""))
}
pub fn session_control_sender_of(details: &Value) -> Option<Value> {
    if !is_session_control_delivery_details(details) {
        return None;
    }
    let s = &details["sender"];
    let kind = s["kind"].as_str()?;
    let mut r = json!({"kind":kind});
    let optional = match kind {
        "agent" => {
            r["session_id"] = json!(s["session_id"].as_str()?);
            "name"
        }
        "command_line" => "user",
        "external" => {
            r["platform"] = json!(s["platform"].as_str()?);
            "author"
        }
        _ => return None,
    };
    if let Some(v) = s[optional].as_str() {
        r[optional] = json!(v);
    }
    Some(r)
}
pub fn relocation_not_offered() -> Value {
    json!({"status":"unavailable","reason":"not_offered","detail":"no app connected to this session offers moving it to another computer"})
}
pub trait SessionControlActions: Send + Sync {
    fn admission_gate(&self) -> Value;
    fn admit_external_message(&self, input: Value) -> Result<Value, String>;
    fn list_admitted_deliveries(&self) -> Value;
    fn persist_header_now(&self) -> BoxFuture<'_, Result<(), String>>;
    fn relocation_offered(&self) -> bool {
        false
    }
    fn relocation_targets(
        &self,
        _signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async { Ok(relocation_not_offered()) })
    }
    fn request_relocation(
        &self,
        _request: Value,
        _signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Value, String>> {
        Box::pin(async { Ok(relocation_not_offered()) })
    }
}
