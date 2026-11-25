//! Port of senpi `packages/coding-agent/src/modes/rpc/handlers-model.ts`.
//!
//! Model-command dispatch (`set_model`, `set_favorite_models`, `set_scoped_models`,
//! `cycle_model`, `get_available_models`) supporting `scope: "session"` vs default global scope.

use async_trait::async_trait;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelScope {
    Global,
    Session,
}

impl ModelScope {
    pub fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("session") => Self::Session,
            _ => Self::Global,
        }
    }
}

#[async_trait]
pub trait ModelCommandSession: Send + Sync {
    async fn set_model(
        &self,
        provider: &str,
        model_id: &str,
        scope: ModelScope,
        effect_id: Option<&str>,
    ) -> Result<Value, (String, &'static str)>;
    fn set_favorite_models(&self, models: &[Value]) -> Result<(), String>;
    fn set_scoped_models(&self, models: &[Value]) -> Result<(), String>;
    async fn cycle_model(&self, direction: Option<&str>) -> Result<Value, String>;
    async fn available_models(&self) -> Result<Value, String>;
}

fn success(id: Option<&Value>, command: &str, data: Option<Value>) -> Value {
    let mut res = json!({
        "id": id,
        "type": "response",
        "command": command,
        "success": true,
    });
    if let Some(d) = data {
        res["data"] = d;
    }
    res
}

fn refusal(id: Option<&Value>, command: &str, message: &str, code: &str) -> Value {
    json!({
        "id": id,
        "type": "response",
        "command": command,
        "success": false,
        "error": message,
        "errorCode": code,
    })
}

pub async fn handle_model_command(session: &dyn ModelCommandSession, command: &Value) -> Value {
    let id = command.get("id");
    let kind = command
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "set_model" => {
            let (Some(provider), Some(model_id)) = (
                command.get("provider").and_then(Value::as_str),
                command.get("modelId").and_then(Value::as_str),
            ) else {
                return refusal(
                    id,
                    "set_model",
                    "set_model requires provider and modelId",
                    crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT,
                );
            };
            let scope = ModelScope::from_wire(command.get("scope").and_then(Value::as_str));
            let effect_id = command.get("effectId").and_then(Value::as_str);
            match session
                .set_model(provider, model_id, scope, effect_id)
                .await
            {
                Ok(data) => success(id, "set_model", Some(data)),
                Err((msg, code)) => refusal(id, "set_model", &msg, code),
            }
        }
        "set_favorite_models" => {
            let models = command
                .get("models")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            match session.set_favorite_models(models) {
                Ok(()) => success(id, "set_favorite_models", None),
                Err(msg) => refusal(
                    id,
                    "set_favorite_models",
                    &msg,
                    crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT,
                ),
            }
        }
        "set_scoped_models" => {
            let models = command
                .get("models")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            match session.set_scoped_models(models) {
                Ok(()) => success(id, "set_scoped_models", None),
                Err(msg) => refusal(
                    id,
                    "set_scoped_models",
                    &msg,
                    crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT,
                ),
            }
        }
        "cycle_model" => {
            let direction = command.get("direction").and_then(Value::as_str);
            match session.cycle_model(direction).await {
                Ok(data) => success(id, "cycle_model", Some(data)),
                Err(msg) => refusal(
                    id,
                    "cycle_model",
                    &msg,
                    crate::rpc_types::RPC_ERROR_PROVIDER_UNAVAILABLE,
                ),
            }
        }
        "get_available_models" => match session.available_models().await {
            Ok(data) => success(id, "get_available_models", Some(data)),
            Err(msg) => refusal(
                id,
                "get_available_models",
                &msg,
                crate::rpc_types::RPC_ERROR_PROVIDER_UNAVAILABLE,
            ),
        },
        other => refusal(
            id,
            other,
            "unknown model command",
            crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT,
        ),
    }
}
