//! senpi `modes/rpc/compact-settled-rows.ts`: row types and bounded reader interface for
//! settled messages and durable owner joins.

use crate::compact_content_store::{CompactContentStore, LeafId};
use crate::compact_read_lease::CompactError;
use crate::compact_values::{CompactValues, ValueRef};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Pending,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalError {
    pub error_message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRow {
    pub message_id: String,
    pub turn_id: Option<String>,
    pub terminal_sequence: u64,
    pub role: String,
    /// A toolResult row carries the key of the call it retired (RULING-13 Q7); every other row has
    /// none.
    pub runtime_item_key: Option<String>,
    /// The retired call's `callUnknown` mark (#5348): this carrier was joined to that call by
    /// order.
    #[serde(default, skip_serializing_if = "is_false")]
    pub call_unknown: bool,
    pub entry_id: Option<String>,
    pub status: TerminalStatus,
    pub error: Option<TerminalError>,
    pub payload: Vec<LeafId>,
    /// The retired call's projection as paged value pages (RULING-60 Q3).
    pub call: Option<ValueRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerRow {
    pub message_id: String,
    pub entry_id: String,
}

/// A join as stored: its row and the epoch-local ordinal it was joined at, the identity member's
/// stable id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredOwner {
    pub message_id: String,
    pub entry_id: String,
    pub ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum SettledRow {
    Terminal(TerminalRow),
    Owner(StoredOwner),
}

/// One row of a cut as the catalog enumerates it: a terminal with its paged call, or one
/// message's entry join.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum SettledMember {
    Terminal(TerminalRow),
    Owner(StoredOwner),
}

/// The bounded reads of one cut of the settled store (S10b): its rows a page at a time, its
/// leaves, values, joins.
#[async_trait]
pub trait SettledReader: Send + Sync {
    fn store(&self) -> &CompactContentStore;
    fn values(&self) -> &CompactValues;
    async fn members(
        &self,
        valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    ) -> Result<Vec<SettledMember>, CompactError>;
    async fn message_of(&self, entry_id: &str) -> Result<Option<String>, CompactError>;
}

pub fn is_projection(value: &Value) -> bool {
    value.get("runtimeItemKey").is_some_and(Value::is_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_projection_values() {
        assert!(is_projection(&json!({"runtimeItemKey": "e:1.0"})));
        assert!(!is_projection(&json!({"messageId": "m1"})));
        assert!(!is_projection(&Value::Null));
    }
}
