use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};

pub type PermissionCheckFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
pub type PermissionCheckHandler = Arc<dyn Fn(Value) -> PermissionCheckFuture + Send + Sync>;

static CHECKS: LazyLock<Mutex<HashMap<String, PermissionCheckHandler>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct PermissionCheckRegistration {
    token: String,
    handler: PermissionCheckHandler,
}

impl Drop for PermissionCheckRegistration {
    fn drop(&mut self) {
        let mut guard = CHECKS.lock().expect("permission checks lock");
        if guard
            .get(&self.token)
            .is_some_and(|current| Arc::ptr_eq(current, &self.handler))
        {
            guard.remove(&self.token);
        }
    }
}

/// Only the host registers invocation capabilities. The ordinary bridge bearer is not one.
pub fn register_permission_check(
    token: impl Into<String>,
    check: PermissionCheckHandler,
) -> PermissionCheckRegistration {
    let token = token.into();
    CHECKS
        .lock()
        .expect("permission checks lock")
        .insert(token.clone(), Arc::clone(&check));
    PermissionCheckRegistration {
        token,
        handler: check,
    }
}

pub async fn permission_check(body: &Value) -> Result<Value, String> {
    let Some(object) = body.as_object() else {
        return Err("Invalid permission request".into());
    };
    let (Some(token), Some(nonce), Some(call)) = (
        object.get("token").and_then(Value::as_str),
        object.get("nonce").and_then(Value::as_str),
        object.get("call").and_then(Value::as_object),
    ) else {
        return Err("Invalid permission request".into());
    };
    let (Some(tool), Some(input)) = (
        call.get("tool").and_then(Value::as_str),
        call.get("input").and_then(Value::as_object),
    ) else {
        return Err("Invalid permission operation".into());
    };
    if !matches!(tool, "bash" | "write" | "read" | "ls") {
        return Err("Invalid permission tool".into());
    }
    let valid_input = if tool == "bash" {
        input.get("command").is_some_and(Value::is_string)
    } else {
        input.get("path").is_some_and(Value::is_string)
    };
    if !valid_input {
        return Err("Invalid permission input".into());
    }
    if call.get("cwd").is_some_and(|cwd| !cwd.is_string()) {
        return Err("Invalid execution cwd".into());
    }
    let handler = CHECKS
        .lock()
        .expect("permission checks lock")
        .get(token)
        .cloned()
        .ok_or_else(|| "Permission authority unavailable".to_owned())?;

    let mut clean_input = serde_json::Map::new();
    for key in ["command", "path", "executedPath", "unresolvable"] {
        if let Some(val) = input.get(key).filter(|v| v.is_string()) {
            clean_input.insert(key.to_owned(), val.clone());
        }
    }
    if let Some(canonical) = input.get("canonical").and_then(Value::as_object)
        && let (Some(v), Some(path), Some(fold_case)) = (
            canonical.get("v").filter(|v| v.is_number()),
            canonical.get("path").filter(|v| v.is_string()),
            canonical.get("foldCase").filter(|v| v.is_boolean()),
        )
    {
        clean_input.insert(
            "canonical".to_owned(),
            json!({ "v": v, "path": path, "foldCase": fold_case }),
        );
    }

    let mut operation = json!({
        "tool": tool,
        "input": Value::Object(clean_input),
    });
    if let Some(cwd) = call.get("cwd").filter(|v| v.is_string()) {
        operation["cwd"] = cwd.clone();
    }
    let decision = handler(operation).await?;
    Ok(json!({ "nonce": nonce, "decision": decision }))
}
