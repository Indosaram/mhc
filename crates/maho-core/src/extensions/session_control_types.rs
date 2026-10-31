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
    /// Interactive sessions only; any mode without an endpoint host answers `unsupported` and registers nothing.
    fn register_control_endpoint(
        &self,
        _options: RegisterControlEndpointOptions,
    ) -> BoxFuture<'_, SessionControlRegistration> {
        Box::pin(async {
            SessionControlRegistration::Unsupported {
                reason: ControlEndpointUnsupported::UnsupportedMode,
            }
        })
    }
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

/// One `session_control_wake` pass's drain; resolves to the pass's `SessionControlDrainResult`, if any.
pub type SessionControlDrain =
    std::sync::Arc<dyn Fn(Value) -> BoxFuture<'static, Option<Value>> + Send + Sync>;
/// Asked once at clean exit: whether anything outside the transcript still names this session.
pub type IsSessionReferenced = std::sync::Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;
/// Releases a registered endpoint.
pub type DisposeControlEndpoint =
    std::sync::Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// senpi `RegisterControlEndpointOptions`.
#[derive(Clone)]
pub struct RegisterControlEndpointOptions {
    /// Directory whose entry creations/deletions wake the drain (`reason: "inbox"`). Created 0700 when missing.
    pub inbox_dir: String,
    pub drain: SessionControlDrain,
    pub is_session_referenced: Option<IsSessionReferenced>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlEndpointUnsupported {
    UnsupportedPlatform,
    UnsupportedMode,
}

impl ControlEndpointUnsupported {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::UnsupportedMode => "unsupported_mode",
        }
    }
}

/// senpi `SessionControlRegistration`.
#[derive(Clone)]
pub enum SessionControlRegistration {
    Registered {
        socket: String,
        dispose: DisposeControlEndpoint,
    },
    Unsupported {
        reason: ControlEndpointUnsupported,
    },
    Failed {
        reason: String,
    },
}

impl SessionControlRegistration {
    /// The wire shape (`dispose` is not serializable and is omitted).
    pub fn to_json(&self) -> Value {
        match self {
            Self::Registered { socket, .. } => json!({"status":"registered","socket":socket}),
            Self::Unsupported { reason } => {
                json!({"status":"unsupported","reason":reason.as_str()})
            }
            Self::Failed { reason } => json!({"status":"failed","reason":reason}),
        }
    }
}
