//! Port of senpi `packages/agent/src/execution/plane-model.ts`.

use std::sync::Arc;

use maho_ai::types::{AssistantMessage, BoxFuture, Context, Message, ThinkingLevel, Tool};
use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::plane::{NestedRequestFn, PlaneHandler, PlaneRefusedError};

pub const MODEL_PLANE: &str = "model";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPlaneRef {
    pub provider: String,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPlaneOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ThinkingLevel>,
}

#[derive(Debug, Clone)]
pub struct ModelPlaneRequest {
    pub model: Option<ModelPlaneRef>,
    pub context: Context,
    pub options: Option<ModelPlaneOptions>,
}

fn refuse(detail: &str) -> PlaneRefusedError {
    PlaneRefusedError::new(format!("malformed_request: {detail}"))
}

fn decode_model(value: Option<&Value>) -> Result<Option<ModelPlaneRef>, PlaneRefusedError> {
    let Some(val) = value else {
        return Ok(None);
    };
    let Some(obj) = val.as_object() else {
        return Err(refuse("model"));
    };
    let provider = obj
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| refuse("model"))?
        .to_owned();
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| refuse("model"))?
        .to_owned();
    Ok(Some(ModelPlaneRef { provider, id }))
}

fn decode_context(value: Option<&Value>) -> Result<Context, PlaneRefusedError> {
    let obj = value
        .and_then(Value::as_object)
        .ok_or_else(|| refuse("context"))?;
    let msgs_val = obj
        .get("messages")
        .filter(|v| v.is_array())
        .ok_or_else(|| refuse("context"))?;
    let system_prompt = match obj.get("systemPrompt") {
        None => None,
        Some(v) => Some(v.as_str().ok_or_else(|| refuse("systemPrompt"))?.to_owned()),
    };
    let tools: Option<Vec<Tool>> = match obj.get("tools") {
        None => None,
        Some(v) => {
            if !v.is_array() {
                return Err(refuse("tools"));
            }
            Some(serde_json::from_value(v.clone()).map_err(|_| refuse("tools"))?)
        }
    };
    let messages: Vec<Message> =
        serde_json::from_value(msgs_val.clone()).map_err(|_| refuse("context"))?;
    Ok(Context {
        system_prompt,
        messages,
        tools,
        active_tool_names: None,
    })
}

fn decode_options(value: Option<&Value>) -> Result<Option<ModelPlaneOptions>, PlaneRefusedError> {
    let Some(val) = value else {
        return Ok(None);
    };
    let obj = val.as_object().ok_or_else(|| refuse("options"))?;
    let max_tokens = match obj.get("maxTokens") {
        None => None,
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| refuse("maxTokens"))?;
            if n == 0 || n > u64::from(u32::MAX) {
                return Err(refuse("maxTokens"));
            }
            Some(n as u32)
        }
    };
    let temperature = match obj.get("temperature") {
        None => None,
        Some(v) => {
            let t = v.as_f64().ok_or_else(|| refuse("temperature"))?;
            if !t.is_finite() {
                return Err(refuse("temperature"));
            }
            Some(t)
        }
    };
    let reasoning = match obj.get("reasoning") {
        None => None,
        Some(v) => {
            let parsed: ThinkingLevel =
                serde_json::from_value(v.clone()).map_err(|_| refuse("reasoning"))?;
            Some(parsed)
        }
    };
    Ok(Some(ModelPlaneOptions {
        max_tokens,
        temperature,
        reasoning,
    }))
}

pub fn decode_model_plane_request(body: &Value) -> Result<ModelPlaneRequest, PlaneRefusedError> {
    let obj = body.as_object().ok_or_else(|| refuse("body"))?;
    let model = decode_model(obj.get("model"))?;
    let options = decode_options(obj.get("options"))?;
    let context = decode_context(obj.get("context"))?;
    Ok(ModelPlaneRequest {
        model,
        context,
        options,
    })
}

pub type ModelCompleteFn = Arc<
    dyn Fn(ModelPlaneRequest, AbortSignal) -> BoxFuture<'static, Result<AssistantMessage, String>>
        + Send
        + Sync,
>;

pub fn serve_model_plane(complete: ModelCompleteFn) -> PlaneHandler {
    Arc::new(move |body, signal| {
        let complete = Arc::clone(&complete);
        Box::pin(async move {
            let req = decode_model_plane_request(&body).map_err(|e| e.to_string())?;
            let message = complete(req, signal).await?;
            serde_json::to_value(message).map_err(|e| e.to_string())
        })
    })
}

#[derive(Clone)]
pub struct ModelPlaneClient {
    nested: NestedRequestFn,
}

impl ModelPlaneClient {
    pub async fn complete(
        &self,
        request: ModelPlaneRequest,
        signal: Option<AbortSignal>,
    ) -> Result<AssistantMessage, PlaneRefusedError> {
        let mut map = serde_json::Map::new();
        if let Some(model) = request.model
            && let Ok(v) = serde_json::to_value(model)
        {
            map.insert("model".to_owned(), v);
        }
        let ctx_val = serde_json::to_value(request.context)
            .map_err(|e| PlaneRefusedError::new(e.to_string()))?;
        map.insert("context".to_owned(), ctx_val);
        if let Some(opts) = request.options
            && let Ok(v) = serde_json::to_value(opts)
        {
            map.insert("options".to_owned(), v);
        }
        let reply = (self.nested)(MODEL_PLANE, Value::Object(map), signal).await?;
        let obj = reply
            .as_object()
            .ok_or_else(|| PlaneRefusedError::new("malformed_reply"))?;
        if obj.get("role").and_then(Value::as_str) != Some("assistant")
            || !obj.get("content").is_some_and(Value::is_array)
        {
            return Err(PlaneRefusedError::new("malformed_reply"));
        }
        serde_json::from_value(reply).map_err(|_| PlaneRefusedError::new("malformed_reply"))
    }
}

pub fn create_model_plane_client(nested: NestedRequestFn) -> ModelPlaneClient {
    ModelPlaneClient { nested }
}
