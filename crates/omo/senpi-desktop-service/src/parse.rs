//! Response and notification parsing at the engine trust boundary.
//! Matches upstream `src/service/parse.ts`.

use senpi_desktop_core::error::{ErrorCode, PermissionDeniedData, TccPermission};
use senpi_desktop_core::protocol::MethodRejection;
use senpi_desktop_protocol::{
    AuditEvent, DesktopCapabilities, ENGINE_METHODS, EngineErrorData, JsonRpcErrorData,
    MethodRejectionData, StopPathStatus, UNKNOWN_AUDIT_VALUE,
};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloHandshake {
    pub abi: String,
    pub protocol_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionOpened {
    pub capabilities: DesktopCapabilities,
    pub resume_token: String,
}

pub fn is_record(value: &Value) -> bool {
    value.is_object()
}

pub fn is_stop_path_status(value: &Value) -> bool {
    serde_json::from_value::<StopPathStatus>(value.clone()).is_ok()
}

/// `kind?` from upstream `matches`: absent, `null`, or the given JSON kind.
fn optional_field(value: &Value, key: &str, kind: fn(&Value) -> bool) -> bool {
    value
        .get(key)
        .is_none_or(|field| field.is_null() || kind(field))
}

pub fn is_desktop_capabilities(value: &Value) -> bool {
    // Optional so this host still opens an engine from before or after their release (macOS
    // shared-screen parity); checked here as well because the core struct may not carry them.
    optional_field(value, "interventionWatch", Value::is_string)
        && optional_field(value, "displayAsleep", Value::is_boolean)
        && serde_json::from_value::<DesktopCapabilities>(value.clone()).is_ok()
}

/// An `audit` payload, or `None` when it is malformed. An action or code this host does not know
/// (sent by a newer engine) reads as `"unknown"`: the event is kept, never dropped, so the audit
/// trail stays complete.
pub fn parse_audit_event(value: &Value) -> Option<AuditEvent> {
    let mut shaped = value.as_object()?.clone();
    if !shaped.get("action").is_some_and(Value::is_string)
        || !optional_field(value, "code", Value::is_string)
    {
        return None;
    }
    if !shaped
        .get("action")
        .and_then(Value::as_str)
        .is_some_and(|action| ENGINE_METHODS.contains(&action))
    {
        shaped.insert("action".into(), Value::from(UNKNOWN_AUDIT_VALUE));
    }
    let unknown_code = shaped
        .get("code")
        .filter(|code| code.is_string())
        .is_some_and(|code| serde_json::from_value::<ErrorCode>(code.clone()).is_err());
    if unknown_code {
        shaped.insert("code".into(), Value::from(UNKNOWN_AUDIT_VALUE));
    }
    serde_json::from_value::<AuditEvent>(Value::Object(shaped)).ok()
}

/// The contract fields of an `engine.hello` result, or `None` when malformed.
pub fn parse_hello(value: &Value) -> Option<HelloHandshake> {
    let obj = value.as_object()?;
    let abi = obj.get("abi")?.as_str()?.to_string();
    let protocol_version = obj.get("protocolVersion")?.as_str()?.to_string();
    Some(HelloHandshake {
        abi,
        protocol_version,
    })
}

pub fn parse_session_opened(value: &Value) -> Option<SessionOpened> {
    let obj = value.as_object()?;
    let resume_token = obj.get("resumeToken")?.as_str()?.to_string();
    let caps_val = obj.get("capabilities")?;
    if !is_desktop_capabilities(caps_val) {
        return None;
    }
    let capabilities = serde_json::from_value::<DesktopCapabilities>(caps_val.clone()).ok()?;
    Some(SessionOpened {
        capabilities,
        resume_token,
    })
}

/// `data` of an engine error: an `ErrorCode` failure, a `-32601` rejection, or `None`.
pub fn parse_error_data(value: &Value) -> Option<JsonRpcErrorData> {
    if !value.is_object() {
        return None;
    }
    if let Ok(data) = serde_json::from_value::<JsonRpcErrorData>(value.clone()) {
        return Some(data);
    }
    let obj = value.as_object()?;
    if let Some(code_val) = obj.get("code")
        && let Ok(code) = serde_json::from_value::<ErrorCode>(code_val.clone())
    {
        let hint = obj
            .get("hint")
            .and_then(|h| h.as_str().map(|s| s.to_string()));
        let permission = obj.get("permission").and_then(parse_permission);
        return Some(JsonRpcErrorData::Engine(EngineErrorData {
            code,
            hint,
            permission,
        }));
    }
    if let Some(reason_val) = obj.get("reason")
        && let Ok(reason) = serde_json::from_value::<MethodRejection>(reason_val.clone())
    {
        return Some(JsonRpcErrorData::Rejected(MethodRejectionData { reason }));
    }
    None
}

pub fn parse_permission(value: &Value) -> Option<PermissionDeniedData> {
    let obj = value.as_object()?;
    let perm_str = obj.get("permission")?.as_str()?;
    let permission = match perm_str {
        "screen_recording" => TccPermission::ScreenRecording,
        "accessibility" => TccPermission::Accessibility,
        _ => return None,
    };
    let settings_url = obj.get("settingsUrl")?.as_str()?.to_string();
    let app = obj.get("app")?.as_str()?.to_string();
    let relaunch_required = obj.get("relaunchRequired")?.as_bool()?;
    Some(PermissionDeniedData {
        permission,
        settings_url,
        app,
        relaunch_required,
    })
}
