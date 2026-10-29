//! The v1.1 attachment fence around session-scoped mutations.
//!
//! Upstream counterpart: `thread/attachment-fence.ts`. Only a session-scoped mutation is fenced, as
//! the host gates only commands that name a session; a terminal's session-less wake is sent as it
//! always was.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::adapter::BoxFuture;

const MUTATIONS: [&str; 8] = ["prompt", "interrupt", "set_session_name", "set_model", "set_thinking_level", "wake", "release_session", "extension_ui_response"];

/// Commands whose reply union carries the host's refusal: their callers branch on the refusal code,
/// so an attach refusal is answered as that command's own refusal frame instead of a throw.
const TYPED_REFUSAL: [&str; 2] = ["release_session", "extension_ui_response"];

/// One request on an authenticated connection.
pub trait RpcTransport: Send + Sync {
    fn request(&self, socket: &str, command: Value, durable_session_id: Option<String>) -> BoxFuture<Result<Value, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachRefusal {
    pub code: String,
    pub error_data: Option<Value>,
    pub frame: Value,
}

/// What the fence answers with when the mutation cannot run.
#[derive(Debug, Clone, PartialEq)]
pub enum FenceOutcome {
    /// The command ran (or the fence did not apply).
    Ran(Value),
    /// The command's own typed refusal frame.
    Refused(Value),
    /// A refusal that is an error for every other command.
    Failed(String),
}

fn error_code(frame: &Value) -> Option<&str> {
    frame.get("errorCode").and_then(Value::as_str).or_else(|| frame.get("error").and_then(Value::as_str))
}

fn error_reason(frame: &Value) -> Option<&str> {
    frame.get("errorData")?.get("reason")?.as_str()
}

/// Attaches the selected session on this connection; none once attached, else the refusal.
async fn attach_selected_session(transport: &dyn RpcTransport, socket: &str, session_id: &Value, durable_session_id: &str) -> Result<Option<AttachRefusal>, String> {
    let frame = transport
        .request(socket, serde_json::json!({ "type": "attach_session", "sessionId": session_id, "durableSessionId": durable_session_id }), Some(durable_session_id.to_owned()))
        .await?;
    if frame.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let code = error_code(&frame).unwrap_or("attach_refused").to_owned();
    if code == "auth_required" {
        return Err(format!("auth_required:{socket}"));
    }
    if code == "session_busy_lifecycle" && error_reason(&frame) == Some("host_draining") {
        return Ok(Some(AttachRefusal { code: "host_draining".to_owned(), error_data: frame.get("errorData").cloned(), frame }));
    }
    Ok(Some(AttachRefusal { code, error_data: frame.get("errorData").cloned(), frame }))
}

/// Releases the hold after the mutation already took effect: no outcome here may turn that success
/// into an error, so a failure is only logged.
async fn release_attachment(transport: &dyn RpcTransport, socket: &str, command: &Map<String, Value>) -> Option<String> {
    let session_id = command.get("sessionId").cloned().unwrap_or(Value::Null);
    match transport.request(socket, serde_json::json!({ "type": "release_attachment", "sessionId": session_id }), None).await {
        Ok(frame) => {
            let code = error_code(&frame);
            let released = frame.get("success").and_then(Value::as_bool) == Some(true);
            if released || code == Some("session_not_attached") || (command.get("type").and_then(Value::as_str) == Some("release_session") && code == Some("unknown_session")) {
                return None;
            }
            Some(frame.get("error").map_or_else(|| frame.to_string(), Value::to_string))
        }
        Err(error) => Some(error),
    }
}

fn attach_refusal_error(refusal: &AttachRefusal, socket: &str, session_id: &Value) -> String {
    match refusal.code.as_str() {
        "host_draining" => format!("host_draining:{socket}"),
        "session_not_attached" => format!("session_not_attached:{}", session_id.as_str().unwrap_or_default()),
        _ => format!("thread RPC request failed: {}", refusal.frame.get("error").map_or_else(|| refusal.frame.to_string(), Value::to_string)),
    }
}

/// Runs one command on an authenticated connection under the attachment fence.
pub async fn fenced_command(
    transport: &dyn RpcTransport,
    socket: &str,
    command: Value,
    capabilities: &BTreeSet<String>,
    durable_session_id: Option<&str>,
    send: &(dyn Fn() -> BoxFuture<Result<Value, String>> + Send + Sync),
) -> Result<FenceOutcome, String> {
    let Some(object) = command.as_object() else {
        return Ok(FenceOutcome::Ran(send().await?));
    };
    let command_type = object.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
    let attach = MUTATIONS.contains(&command_type.as_str())
        && object.get("sessionId").is_some()
        && capabilities.contains("attach_session")
        && (command_type != "release_session" || capabilities.contains("release_caller_aware"));
    if !attach {
        return Ok(FenceOutcome::Ran(send().await?));
    }
    let Some(durable_session_id) = durable_session_id else {
        return Err("session_identity_mismatch:missing selected durable session id".to_owned());
    };
    let session_id = object.get("sessionId").cloned().unwrap_or(Value::Null);
    if let Some(refusal) = attach_selected_session(transport, socket, &session_id, durable_session_id).await? {
        if !TYPED_REFUSAL.contains(&command_type.as_str()) {
            return Err(attach_refusal_error(&refusal, socket, &session_id));
        }
        let mut frame = Map::new();
        frame.insert("id".into(), object.get("id").cloned().unwrap_or(Value::Null));
        frame.insert("type".into(), Value::String("response".into()));
        frame.insert("command".into(), Value::String(command_type.clone()));
        frame.insert("success".into(), Value::Bool(false));
        frame.insert("error".into(), Value::String(refusal.code.clone()));
        frame.insert("errorCode".into(), Value::String(refusal.code.clone()));
        if let Some(error_data) = refusal.error_data {
            frame.insert("errorData".into(), error_data);
        }
        return Ok(FenceOutcome::Refused(Value::Object(frame)));
    }
    let result = send().await?;
    if capabilities.contains("release_attachment") {
        let _ = release_attachment(transport, socket, object).await;
    }
    Ok(FenceOutcome::Ran(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Recorder {
        commands: Arc<Mutex<Vec<Value>>>,
        attach: Value,
    }

    impl RpcTransport for Recorder {
        fn request(&self, _socket: &str, command: Value, _durable: Option<String>) -> BoxFuture<Result<Value, String>> {
            let commands = self.commands.clone();
            let attach = self.attach.clone();
            let kind = command.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
            commands.lock().expect("commands").push(command);
            Box::pin(async move { Ok(if kind == "attach_session" { attach } else { serde_json::json!({ "success": true }) }) })
        }
    }

    fn capabilities(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn send_ok() -> BoxFuture<Result<Value, String>> {
        Box::pin(async { Ok(serde_json::json!({ "success": true, "data": {} })) })
    }

    #[test]
    fn a_session_scoped_mutation_attaches_first_and_releases_after() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let transport = Recorder { commands: commands.clone(), attach: serde_json::json!({ "success": true }) };
        let outcome = futures::executor::block_on(fenced_command(
            &transport,
            "/run/host.sock",
            serde_json::json!({ "id": 1, "type": "prompt", "sessionId": "r-1" }),
            &capabilities(&["attach_session", "release_attachment"]),
            Some("d-1"),
            &send_ok,
        ))
        .expect("fenced");
        assert!(matches!(outcome, FenceOutcome::Ran(_)));
        let seen: Vec<String> = commands.lock().expect("commands").iter().filter_map(|command| command.get("type").and_then(Value::as_str).map(str::to_owned)).collect();
        assert_eq!(seen, vec!["attach_session", "release_attachment"]);
    }

    #[test]
    fn a_read_only_listing_is_not_fenced() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let transport = Recorder { commands: commands.clone(), attach: serde_json::json!({ "success": true }) };
        let outcome = futures::executor::block_on(fenced_command(
            &transport,
            "/run/host.sock",
            serde_json::json!({ "id": 1, "type": "list_sessions" }),
            &capabilities(&["attach_session"]),
            None,
            &send_ok,
        ))
        .expect("fenced");
        assert!(matches!(outcome, FenceOutcome::Ran(_)));
        assert!(commands.lock().expect("commands").is_empty());
    }

    #[test]
    fn a_draining_host_answers_a_typed_refusal_frame() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let transport = Recorder {
            commands,
            attach: serde_json::json!({ "success": false, "errorCode": "session_busy_lifecycle", "errorData": { "reason": "host_draining" } }),
        };
        let outcome = futures::executor::block_on(fenced_command(
            &transport,
            "/run/host.sock",
            serde_json::json!({ "id": 7, "type": "release_session", "sessionId": "r-1" }),
            &capabilities(&["attach_session", "release_caller_aware"]),
            Some("d-1"),
            &send_ok,
        ))
        .expect("fenced");
        match outcome {
            FenceOutcome::Refused(frame) => {
                assert_eq!(frame["error"], "host_draining");
                assert_eq!(frame["success"], false);
            }
            other => panic!("expected a typed refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_prompt_attach_refusal_is_an_error() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let transport = Recorder { commands, attach: serde_json::json!({ "success": false, "errorCode": "session_not_attached" }) };
        let error = futures::executor::block_on(fenced_command(
            &transport,
            "/run/host.sock",
            serde_json::json!({ "id": 2, "type": "prompt", "sessionId": "r-1" }),
            &capabilities(&["attach_session"]),
            Some("d-1"),
            &send_ok,
        ))
        .expect_err("refused");
        assert!(error.starts_with("session_not_attached:"));
    }

    #[test]
    fn a_mutation_without_a_durable_id_is_refused() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let transport = Recorder { commands, attach: serde_json::json!({ "success": true }) };
        let error = futures::executor::block_on(fenced_command(
            &transport,
            "/run/host.sock",
            serde_json::json!({ "id": 3, "type": "wake", "sessionId": "r-1" }),
            &capabilities(&["attach_session"]),
            None,
            &send_ok,
        ))
        .expect_err("refused");
        assert_eq!(error, "session_identity_mismatch:missing selected durable session id");
    }
}
