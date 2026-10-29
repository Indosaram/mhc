use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use senpi_desktop_protocol::DesktopCapabilities;
use senpi_desktop_tool::permission::computer_permission_parser;
use senpi_desktop_tool::cua_definition::computer_actions_permission_parser;
use senpi_desktop_tool::tool_definition::COMPUTER_TOOL_NAME;
use senpi_desktop_tool::cua_definition::COMPUTER_ACTIONS_TOOL_NAME;
use serde_json::Value;

pub const COMPUTER_USE_ACTIVATION_SOURCES: [&str; 3] = ["command_on", "command_off", "tool_call"];
pub const COMPUTER_USE_BACKENDS: [&str; 6] = ["atspi", "fake", "macos", "wayland", "win32", "x11"];
pub const COMPUTER_USE_ENGINE_ERROR_CODES: [&str; 15] = [
    "native-unavailable",
    "quarantined",
    "abi-mismatch",
    "handshake-failed",
    "timeout",
    "PermissionDenied",
    "MethodNotFound",
    "InvalidParams",
    "InternalError",
    "ParseError",
    "InvalidRequest",
    "DesktopSessionNotOpen",
    "DesktopSessionAlreadyOpen",
    "DesktopSessionClosed",
    "other",
];
pub const COMPUTER_USE_PERMISSIONS: [&str; 5] = ["capture", "input", "ax", "read", "exec"];
pub const COMPUTER_USE_PERMISSION_SCOPES: [&str; 2] = ["os", "tier"];
pub const COMPUTER_USE_PLATFORMS: [&str; 4] = ["darwin", "linux", "win32", "other"];

pub type ComputerUseActivationSource = String;
pub type ComputerUseBackend = String;
pub type ComputerUseEngineErrorCode = String;
pub type ComputerUsePermission = String;
pub type ComputerUsePermissionScope = String;
pub type ComputerUsePlatform = String;

#[derive(Clone, Debug, PartialEq)]
pub struct ComputerUseRuntimeInfo {
    pub backend: ComputerUseBackend,
    pub platform: ComputerUsePlatform,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ComputerUseTelemetryObservation {
    Activation {
        runtime: ComputerUseRuntimeInfo,
        active: bool,
        source: ComputerUseActivationSource,
    },
    PermissionDenied {
        runtime: ComputerUseRuntimeInfo,
        permission: ComputerUsePermission,
        scope: ComputerUsePermissionScope,
    },
    EngineError {
        runtime: ComputerUseRuntimeInfo,
        code: ComputerUseEngineErrorCode,
    },
}

pub type ComputerUseTelemetryObserver = Arc<dyn Fn(ComputerUseTelemetryObservation) + Send + Sync>;

pub trait ComputerUseTelemetryObservers: Send + Sync {
    fn publish(&self, observation: ComputerUseTelemetryObservation);
    fn subscribe(&self, observer: ComputerUseTelemetryObserver) -> Box<dyn FnOnce() + Send + Sync>;
}

#[derive(Default)]
pub struct SharedComputerUseTelemetryObservers {
    observers: Arc<Mutex<Vec<ComputerUseTelemetryObserver>>>,
}

impl SharedComputerUseTelemetryObservers {
    pub fn new() -> Self {
        Self {
            observers: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ComputerUseTelemetryObservers for SharedComputerUseTelemetryObservers {
    fn publish(&self, observation: ComputerUseTelemetryObservation) {
        let list = self.observers.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for obs in list {
            obs(observation.clone());
        }
    }

    fn subscribe(&self, observer: ComputerUseTelemetryObserver) -> Box<dyn FnOnce() + Send + Sync> {
        let mut list = self.observers.lock().unwrap_or_else(|e| e.into_inner());
        list.push(observer.clone());
        let list_arc = Arc::clone(&self.observers);
        Box::new(move || {
            let mut l = list_arc.lock().unwrap_or_else(|e| e.into_inner());
            l.retain(|item| !Arc::ptr_eq(item, &observer));
        })
    }
}

pub fn shared_computer_use_telemetry_observers() -> Arc<dyn ComputerUseTelemetryObservers> {
    Arc::new(SharedComputerUseTelemetryObservers::new())
}

pub fn computer_use_platform(value: &str) -> ComputerUsePlatform {
    match value {
        "darwin" | "linux" | "win32" => value.to_string(),
        _ => "other".to_string(),
    }
}

pub fn computer_use_backend(value: &Value) -> ComputerUseBackend {
    if let Some(s) = value.as_str()
        && COMPUTER_USE_BACKENDS.contains(&s) {
        return s.to_string();
    }
    "other".to_string()
}

pub fn computer_use_session_id(context: &Value) -> Option<String> {
    if let Value::Object(map) = context {
        if let Some(Value::String(id)) = map.get("sessionId").or_else(|| map.get("session_id"))
            && !id.is_empty() {
            return Some(id.clone());
        }
        if let Some(Value::Object(sm)) = map.get("sessionManager").or_else(|| map.get("session_manager"))
            && let Some(Value::String(id)) = sm.get("sessionId").or_else(|| sm.get("session_id"))
            && !id.is_empty() {
            return Some(id.clone());
        }
    }
    None
}

pub struct ComputerUseTelemetry {
    platform: ComputerUsePlatform,
    observers: Arc<dyn ComputerUseTelemetryObservers>,
    pending_permissions: Mutex<HashMap<String, ComputerUsePermission>>,
}

pub fn create_computer_use_telemetry(
    platform: &str,
    observers: Option<Arc<dyn ComputerUseTelemetryObservers>>,
) -> ComputerUseTelemetry {
    ComputerUseTelemetry::new(platform, observers)
}

impl ComputerUseTelemetry {
    pub fn new(
        platform: &str,
        observers: Option<Arc<dyn ComputerUseTelemetryObservers>>,
    ) -> Self {
        Self {
            platform: computer_use_platform(platform),
            observers: observers.unwrap_or_else(shared_computer_use_telemetry_observers),
            pending_permissions: Mutex::new(HashMap::new()),
        }
    }

    pub fn activation(
        &self,
        session_id: &str,
        active: bool,
        source: &str,
        backend: &Value,
    ) {
        if session_id.is_empty() {
            return;
        }
        self.observers.publish(ComputerUseTelemetryObservation::Activation {
            runtime: ComputerUseRuntimeInfo {
                session_id: session_id.to_string(),
                platform: self.platform.clone(),
                backend: computer_use_backend(backend),
            },
            active,
            source: source.to_string(),
        });
    }

    pub fn engine_error(
        &self,
        session_id: &str,
        code: &str,
        backend: &Value,
    ) {
        if session_id.is_empty() {
            return;
        }
        self.observers.publish(ComputerUseTelemetryObservation::EngineError {
            runtime: ComputerUseRuntimeInfo {
                session_id: session_id.to_string(),
                platform: self.platform.clone(),
                backend: computer_use_backend(backend),
            },
            code: code.to_string(),
        });
    }

    pub fn os_permissions(&self, session_id: &str, capabilities: &DesktopCapabilities) {
        if session_id.is_empty() {
            return;
        }
        let runtime = ComputerUseRuntimeInfo {
            session_id: session_id.to_string(),
            platform: self.platform.clone(),
            backend: computer_use_backend(&Value::String(capabilities.backend.clone())),
        };
        if !capabilities.capture {
            self.observers.publish(ComputerUseTelemetryObservation::PermissionDenied {
                runtime: runtime.clone(),
                permission: "capture".to_string(),
                scope: "os".to_string(),
            });
        }
        if !capabilities.input {
            self.observers.publish(ComputerUseTelemetryObservation::PermissionDenied {
                runtime: runtime.clone(),
                permission: "input".to_string(),
                scope: "os".to_string(),
            });
        }
        if !capabilities.ax {
            self.observers.publish(ComputerUseTelemetryObservation::PermissionDenied {
                runtime,
                permission: "ax".to_string(),
                scope: "os".to_string(),
            });
        }
    }

    pub fn tool_execution_started(&self, payload: &Value) -> bool {
        if let Some(req) = permission_request(payload) {
            let mut pending = self.pending_permissions.lock().unwrap_or_else(|e| e.into_inner());
            pending.insert(req.tool_call_id, req.permission);
            true
        } else {
            false
        }
    }

    /// Whether any computer-use permission request is still awaiting its tool result.
    pub fn has_pending(&self) -> bool {
        !self.pending_permissions.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    pub fn permission_tier_denied(&self, payload: &Value, session_id: &str, backend: &Value) {
        let Some(event) = denied_tool_execution(payload) else {
            return;
        };
        let permission = {
            let mut pending = self.pending_permissions.lock().unwrap_or_else(|e| e.into_inner());
            if event.denied || event.final_result {
                pending.remove(&event.tool_call_id)
            } else {
                pending.get(&event.tool_call_id).cloned()
            }
        };

        if !event.denied || permission.is_none() || session_id.is_empty() {
            return;
        }

        self.observers.publish(ComputerUseTelemetryObservation::PermissionDenied {
            runtime: ComputerUseRuntimeInfo {
                session_id: session_id.to_string(),
                platform: self.platform.clone(),
                backend: computer_use_backend(backend),
            },
            permission: permission.unwrap_or_default(),
            scope: "tier".to_string(),
        });
    }
}

struct ParsedPermissionRequest {
    tool_call_id: String,
    #[allow(dead_code)]
    tool_name: String,
    permission: ComputerUsePermission,
}

fn permission_request(value: &Value) -> Option<ParsedPermissionRequest> {
    let Value::Object(map) = value else { return None; };
    let t = map.get("type")?.as_str()?;
    if t != "tool_execution_start" {
        return None;
    }
    let tool_call_id = map.get("toolCallId").or_else(|| map.get("tool_call_id"))?.as_str()?.to_string();
    let tool_name = map.get("toolName").or_else(|| map.get("tool_name"))?.as_str()?.to_string();
    let args = map.get("args")?;
    let permission = permission_for_tool(&tool_name, args)?;
    Some(ParsedPermissionRequest {
        tool_call_id,
        tool_name,
        permission,
    })
}

fn permission_from_requests(requests: &[maho_tools::definition::ToolPermissionRequest]) -> Option<ComputerUsePermission> {
    let first = requests.first()?;
    let tier = first.patterns.first()?.as_str();
    if tier == "read" || tier == "exec" {
        Some(tier.to_string())
    } else {
        None
    }
}

fn permission_for_tool(tool_name: &str, input: &Value) -> Option<ComputerUsePermission> {
    let dummy_path = Path::new("");
    if matches_tool_name(tool_name, COMPUTER_TOOL_NAME) {
        let requests: Vec<maho_tools::definition::ToolPermissionRequest> =
            computer_permission_parser(COMPUTER_TOOL_NAME, input, dummy_path)
                .into_iter()
                .map(Into::into)
                .collect();
        return permission_from_requests(&requests);
    }
    if matches_tool_name(tool_name, COMPUTER_ACTIONS_TOOL_NAME) {
        let requests = computer_actions_permission_parser(COMPUTER_ACTIONS_TOOL_NAME, input, dummy_path);
        return permission_from_requests(&requests);
    }
    None
}

fn matches_tool_name(tool_name: &str, expected: &str) -> bool {
    let normalized = tool_name.trim().to_lowercase().replace('-', "_");
    normalized == expected || normalized.ends_with(&format!("_{expected}")) || normalized.ends_with(&format!(":{expected}"))
}

struct DeniedExecutionEvent {
    tool_call_id: String,
    #[allow(dead_code)]
    tool_name: String,
    denied: bool,
    final_result: bool,
}

fn denied_tool_execution(value: &Value) -> Option<DeniedExecutionEvent> {
    let Value::Object(map) = value else { return None; };
    let event_type = map.get("type").and_then(Value::as_str);

    if event_type == Some("message_end")
        && let Some(Value::Object(msg)) = map.get("message") {
        let role = msg.get("role").and_then(Value::as_str);
        let tool_call_id = msg.get("toolCallId").or_else(|| msg.get("tool_call_id")).and_then(Value::as_str)?;
        let tool_name = msg.get("toolName").or_else(|| msg.get("tool_name")).and_then(Value::as_str)?;
        if role != Some("toolResult") {
            return None;
        }
        let is_error = msg.get("isError").or_else(|| msg.get("is_error")).and_then(Value::as_bool).unwrap_or(false);
        let denied = is_error && is_permission_policy_failure(Value::Object(msg.clone()));
        return Some(DeniedExecutionEvent {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            denied,
            final_result: true,
        });
    }

    if event_type == Some("tool_execution_end") {
        let tool_call_id = map.get("toolCallId").or_else(|| map.get("tool_call_id")).and_then(Value::as_str)?;
        let tool_name = map.get("toolName").or_else(|| map.get("tool_name")).and_then(Value::as_str)?;
        let result = map.get("result");
        let is_error = map.get("isError").or_else(|| map.get("is_error")).and_then(Value::as_bool).unwrap_or(false);
        let denied = is_error && result.is_some_and(|r| is_permission_policy_failure(r.clone()));
        return Some(DeniedExecutionEvent {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            denied,
            final_result: false,
        });
    }

    None
}

fn is_permission_policy_failure(result: Value) -> bool {
    let Value::Object(map) = result else { return false; };
    if let Some(Value::Object(details)) = map.get("details")
        && let Some(tag) = details.get("_tag").and_then(Value::as_str)
        && (tag == "PermissionDeniedError" || tag == "PermissionRejectedError" || tag == "PermissionCorrectedError") {
        return true;
    }
    if let Some(Value::Array(content)) = map.get("content") {
        for part in content {
            if let Value::Object(part_map) = part
                && let Some(text) = part_map.get("text").and_then(Value::as_str)
                && (text == "The user rejected permission to use this specific tool call."
                    || text.starts_with("The user rejected permission to use this specific tool call with the following feedback:")
                    || text == "The user has specified a rule which prevents you from using this specific tool call.")
            {
                return true;
            }
        }
    }
    false
}
