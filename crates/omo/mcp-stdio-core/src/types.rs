use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Number;
use serde_json::Value;

/// JSON-RPC id: the reference implementation accepts only `string | number | null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(Number),
    String(String),
    Null,
}

impl JsonRpcId {
    pub fn as_text(&self) -> Option<String> {
        match self {
            JsonRpcId::Number(number) => Some(number.to_string()),
            JsonRpcId::String(text) => Some(text.clone()),
            JsonRpcId::Null => None,
        }
    }
}

impl std::fmt::Display for JsonRpcId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonRpcId::Number(number) => write!(formatter, "{number}"),
            JsonRpcId::String(text) => formatter.write_str(text),
            JsonRpcId::Null => formatter.write_str("null"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum JsonRpcVersion {
    #[default]
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Open record, not a closed struct: the reference implementation permits arbitrary keys.
pub type JsonRpcResult = Map<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<JsonRpcResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: JsonRpcId, result: JsonRpcResult) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: JsonRpcId, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    pub fn error_with_data(
        id: JsonRpcId,
        code: i64,
        message: impl Into<String>,
        data: Value,
    ) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: Some(data),
            }),
        }
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// Lifecycle fields: restricted to JSON scalars (`boolean | number | string | null`).
pub type McpLogFields = Map<String, Value>;

pub trait McpLifecycleLog {
    fn log(&self, event: &str, fields: Option<&McpLogFields>);
}

impl<F> McpLifecycleLog for F
where
    F: Fn(&str, Option<&McpLogFields>),
{
    fn log(&self, event: &str, fields: Option<&McpLogFields>) {
        self(event, fields)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopLog;

impl McpLifecycleLog for NoopLog {
    fn log(&self, _event: &str, _fields: Option<&McpLogFields>) {}
}
