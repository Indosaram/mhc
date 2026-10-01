//! Port of senpi packages/agent/src/harness/session/values.ts.

use serde::{Deserialize, Serialize};

use super::types::JsonValue;

/// Address of one durable scalar value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Value {
    pub namespace: String,
    pub key: String,
}

/// Address of one durable append-only list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueList {
    pub namespace: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredValue {
    pub address: Value,
    pub value: JsonValue,
    pub seq: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListElement {
    pub seq: i64,
    pub value: JsonValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListCursor {
    pub seq: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListReadOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ListCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<ListOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedListReadOptions {
    pub cursor: Option<ListCursor>,
    pub order: ListOrder,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueSetWrite {
    pub namespace: String,
    pub key: String,
    pub value: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueDeleteWrite {
    pub namespace: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAppendWrite {
    pub namespace: String,
    pub key: String,
    pub value: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDeleteWrite {
    pub namespace: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ValueWrite {
    Set(ValueSetWrite),
    Delete(ValueDeleteWrite),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ListWrite {
    Append(ListAppendWrite),
    Delete(ListDeleteWrite),
}

impl ValueWrite {
    pub fn namespace(&self) -> &str {
        match self {
            ValueWrite::Set(write) => &write.namespace,
            ValueWrite::Delete(write) => &write.namespace,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            ValueWrite::Set(write) => &write.key,
            ValueWrite::Delete(write) => &write.key,
        }
    }
}

impl ListWrite {
    pub fn namespace(&self) -> &str {
        match self {
            ListWrite::Append(write) => &write.namespace,
            ListWrite::Delete(write) => &write.namespace,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            ListWrite::Append(write) => &write.key,
            ListWrite::Delete(write) => &write.key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressError {
    pub message: String,
}

impl std::fmt::Display for AddressError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AddressError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListReadOptionsError {
    pub message: String,
}

impl std::fmt::Display for ListReadOptionsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ListReadOptionsError {}

pub const NUL: &str = "\u{0}";

fn validate_address(namespace: &str, key: &str) -> Result<(), AddressError> {
    if namespace.is_empty() {
        return Err(AddressError {
            message: "Value namespace must not be empty".to_owned(),
        });
    }
    if namespace.contains(NUL) {
        return Err(AddressError {
            message: "Value namespace must not contain \\u0000".to_owned(),
        });
    }
    if key.contains(NUL) {
        return Err(AddressError {
            message: "Value key must not contain \\u0000".to_owned(),
        });
    }
    Ok(())
}

pub fn value(namespace: impl Into<String>, key: impl Into<String>) -> Result<Value, AddressError> {
    let namespace = namespace.into();
    let key = key.into();
    validate_address(&namespace, &key)?;
    Ok(Value { namespace, key })
}

pub fn list(namespace: impl Into<String>, key: impl Into<String>) -> Result<ValueList, AddressError> {
    let namespace = namespace.into();
    let key = key.into();
    validate_address(&namespace, &key)?;
    Ok(ValueList { namespace, key })
}

fn bound_value(namespace: &str, key: impl Into<String>) -> Value {
    value(namespace, key).expect("built-in durable address must be valid")
}

fn bound_list(namespace: &str, key: impl Into<String>) -> ValueList {
    list(namespace, key).expect("built-in durable list address must be valid")
}

pub fn set_value(address: &Value, next: JsonValue) -> ValueWrite {
    ValueWrite::Set(ValueSetWrite {
        namespace: address.namespace.clone(),
        key: address.key.clone(),
        value: next,
    })
}

pub fn delete_value(address: &Value) -> ValueWrite {
    ValueWrite::Delete(ValueDeleteWrite {
        namespace: address.namespace.clone(),
        key: address.key.clone(),
    })
}

pub fn append_list(address: &ValueList, element: JsonValue) -> ListWrite {
    ListWrite::Append(ListAppendWrite {
        namespace: address.namespace.clone(),
        key: address.key.clone(),
        value: element,
    })
}

pub fn delete_list(address: &ValueList) -> ListWrite {
    ListWrite::Delete(ListDeleteWrite {
        namespace: address.namespace.clone(),
        key: address.key.clone(),
    })
}

pub fn resolve_list_read_options(
    options: Option<ListReadOptions>,
) -> Result<ResolvedListReadOptions, ListReadOptionsError> {
    let options = options.unwrap_or_default();
    let requested_limit = options.limit.unwrap_or(1_000);
    if requested_limit == 0 {
        return Err(ListReadOptionsError {
            message: "List read limit must be a positive safe integer".to_owned(),
        });
    }
    Ok(ResolvedListReadOptions {
        cursor: options.cursor,
        order: options.order.unwrap_or(ListOrder::Asc),
        limit: requested_limit.min(10_000),
    })
}

pub fn physical_key(namespace: &str, key: &str) -> String {
    format!("{namespace}{NUL}{key}")
}

pub fn branch_tip(branch: &str) -> Value {
    bound_value("pi.branch.tip", branch)
}

pub fn branch_tip_inventory_prefix() -> Value {
    bound_value("pi.branch.tip", "")
}

pub fn lane_config(lane: &str) -> Value {
    bound_value("pi.lane.config", lane)
}

pub fn lane_state(lane: &str) -> Value {
    bound_value("pi.lane.state", lane)
}

pub fn operation_result(operation_id: &str) -> Value {
    bound_value("pi.result", operation_id)
}

pub fn operation_meta(operation_id: &str) -> Value {
    bound_value("pi.op.meta", operation_id)
}

pub fn operation_state(operation_id: &str) -> Value {
    bound_value("pi.op.state", operation_id)
}

pub fn operation_tool_args(operation_id: &str, step_id: &str, source_index: usize) -> Value {
    bound_value("pi.op.tool_args", format!("{operation_id}:{step_id}:{source_index}"))
}

pub fn operation_tool_memo(operation_id: &str, invocation_id: &str, name: &str) -> Value {
    bound_value("pi.op.tool_memo", format!("{operation_id}:{invocation_id}:{name}"))
}

pub fn operation_preparation(operation_id: &str, task_id: &str) -> Value {
    bound_value("pi.op.preparation", format!("{operation_id}:{task_id}"))
}

pub fn operation_tool_args_prefix(operation_id: &str, step_id: Option<&str>) -> Value {
    let key = match step_id {
        Some(step_id) => format!("{operation_id}:{step_id}:"),
        None => format!("{operation_id}:"),
    };
    bound_value("pi.op.tool_args", key)
}

pub fn operation_tool_memo_prefix(operation_id: &str, invocation_id: Option<&str>) -> Value {
    let key = match invocation_id {
        Some(invocation_id) => format!("{operation_id}:{invocation_id}:"),
        None => format!("{operation_id}:"),
    };
    bound_value("pi.op.tool_memo", key)
}

pub fn operation_preparation_prefix(operation_id: &str) -> Value {
    bound_value("pi.op.preparation", format!("{operation_id}:"))
}

pub fn pending_entry(entry_id: &str) -> Value {
    bound_value("pi.pending.entry", entry_id)
}

pub fn pending_tool_output(operation_id: &str, invocation_id: &str) -> Value {
    bound_value("pi.pending.tool_output", format!("{operation_id}:{invocation_id}"))
}

pub fn pending_assistant_frames(operation_id: &str, response_entry_id: &str) -> ValueList {
    bound_list(
        "pi.pending.assistant_frame",
        format!("{operation_id}:{response_entry_id}"),
    )
}

pub fn pending_tool_output_prefix(operation_id: &str) -> Value {
    bound_value("pi.pending.tool_output", format!("{operation_id}:"))
}

pub fn session_name() -> Value {
    bound_value("pi.session.name", "")
}

pub fn entry_label(entry_id: &str) -> Value {
    bound_value("pi.entry.label", entry_id)
}
