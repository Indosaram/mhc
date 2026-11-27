//! Port of `packages/coding-agent/src/modes/rpc/machine-op/wire.ts`.
//!
//! The `machine_op` record family (`senpi#6752`): the local relay host hands the engine the `run`
//! step of one device operation and gets back its `OpOutcome`, the relay host's own result shape.

use crate::delegation_wire::{DelegationResponse, refused};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Advertised in `get_protocol_info` by a host that answers these records.
pub const MACHINE_OP_CAPABILITY: &str = "machine_op_v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineOpCommand {
    MachineOp,
    MachineOpResult,
    MachineOpOwner,
    MachineOpHealth,
    MachineOpCancel,
}

impl MachineOpCommand {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MachineOp => "machine_op",
            Self::MachineOpResult => "machine_op_result",
            Self::MachineOpOwner => "machine_op_owner",
            Self::MachineOpHealth => "machine_op_health",
            Self::MachineOpCancel => "machine_op_cancel",
        }
    }

    pub fn from_str_name(name: &str) -> Option<Self> {
        match name {
            "machine_op" => Some(Self::MachineOp),
            "machine_op_result" => Some(Self::MachineOpResult),
            "machine_op_owner" => Some(Self::MachineOpOwner),
            "machine_op_health" => Some(Self::MachineOpHealth),
            "machine_op_cancel" => Some(Self::MachineOpCancel),
            _ => None,
        }
    }
}

impl fmt::Display for MachineOpCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn is_machine_op_command(record_type: Option<&str>) -> bool {
    record_type
        .and_then(MachineOpCommand::from_str_name)
        .is_some()
}

pub type Args = Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpState {
    Applied,
    Failed,
    Refused,
    Cancelled,
    TimedOut,
}

/// The relay host's `OpOutcome`, answered unchanged as a `machine_op` response's `data`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpOutcome {
    pub state: OpState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OpError>,
}

impl OpOutcome {
    pub fn applied(response: Map<String, Value>) -> Self {
        Self {
            state: OpState::Applied,
            response: Some(response),
            error: None,
        }
    }

    pub fn failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            state: OpState::Failed,
            response: None,
            error: Some(OpError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    pub fn refused(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            state: OpState::Refused,
            response: None,
            error: Some(OpError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    pub fn cancelled(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            state: OpState::Cancelled,
            response: None,
            error: Some(OpError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    pub fn timed_out(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            state: OpState::TimedOut,
            response: None,
            error: Some(OpError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpScope {
    pub root: String,
    pub confined: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MachineOpRequest {
    pub op: String,
    pub args: Args,
    pub op_id: String,
    pub deadline_ms: Option<u64>,
    pub scope: OpScope,
    pub sender: String,
    pub env_overlay: BTreeMap<String, String>,
    pub cross_owner: bool,
}

/// `machine_op_result` / `machine_op_cancel`: whose op it is asking about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedOpLookup {
    pub op_id: String,
    pub sender: String,
    pub cross_owner: bool,
}

pub fn machine_op_refusal(id: Option<&str>, command: MachineOpCommand) -> DelegationResponse {
    refused(
        id.map(str::to_owned),
        command.as_str(),
        "machine_op_not_authorized",
        &format!(
            "{command} needs a connection that proved the endpoint's launch key and declared no delegation"
        ),
        None,
    )
}

pub fn machine_op_malformed(
    id: Option<&str>,
    command: MachineOpCommand,
    problem: &str,
) -> DelegationResponse {
    refused(
        id.map(str::to_owned),
        command.as_str(),
        "machine_op_malformed",
        problem,
        None,
    )
}

pub fn machine_op_conflict(id: Option<&str>, command: MachineOpCommand) -> DelegationResponse {
    refused(
        id.map(str::to_owned),
        command.as_str(),
        "machine_op_conflict",
        "op_id is bound to another sender",
        None,
    )
}

/// Retryable: the engine could not run the op now (its state dir, or a host that is ending); nothing ran or is cached.
pub fn machine_op_unavailable(id: Option<&str>, detail: &str) -> DelegationResponse {
    refused(
        id.map(str::to_owned),
        "machine_op",
        "machine_op_unavailable",
        detail,
        None,
    )
}

pub fn machine_op_answer(
    id: Option<&str>,
    command: MachineOpCommand,
    data: Value,
) -> DelegationResponse {
    DelegationResponse::success(id.map(str::to_owned), command.as_str(), data)
}

pub fn is_args(value: &Value) -> bool {
    value.is_object()
}

pub fn non_empty_text<'a>(record: &'a Args, key: &str) -> Option<&'a str> {
    record
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn cross_owner_of(record: &Args) -> Result<bool, &'static str> {
    match record.get("cross_owner") {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err("cross_owner must be a boolean"),
    }
}

fn positive_safe_int(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return (n > 0 && n <= MAX_SAFE_INTEGER).then_some(n);
    }
    if let Some(f) = value.as_f64()
        && f.is_finite()
        && f.fract() == 0.0
        && f >= 1.0
        && f <= MAX_SAFE_INTEGER as f64
    {
        return Some(f as u64);
    }
    None
}

/// The op a `machine_op_result` / `machine_op_cancel` names and who asks, or the first problem with them.
pub fn parse_owned_op_lookup(record: &Args) -> Result<OwnedOpLookup, String> {
    let op_id = non_empty_text(record, "op_id")
        .ok_or_else(|| "op_id must be a non-empty string".to_owned())?;
    let sender = non_empty_text(record, "sender")
        .ok_or_else(|| "sender must be a non-empty string".to_owned())?;
    let cross_owner = cross_owner_of(record).map_err(str::to_owned)?;
    Ok(OwnedOpLookup {
        op_id: op_id.to_owned(),
        sender: sender.to_owned(),
        cross_owner,
    })
}

/// The `machine_op` record's fields, or the first problem with them.
pub fn parse_machine_op(record: &Args) -> Result<MachineOpRequest, String> {
    let op =
        non_empty_text(record, "op").ok_or_else(|| "op must be a non-empty string".to_owned())?;
    let args = record
        .get("args")
        .and_then(Value::as_object)
        .ok_or_else(|| "args must be a map".to_owned())?
        .clone();
    let op_id = non_empty_text(record, "op_id")
        .ok_or_else(|| "op_id must be a non-empty string".to_owned())?;
    let deadline_ms = match record.get("deadline_ms") {
        None => None,
        Some(value) => Some(
            positive_safe_int(value)
                .ok_or_else(|| "deadline_ms must be a positive epoch-ms integer".to_owned())?,
        ),
    };
    let scope_obj = record
        .get("scope")
        .and_then(Value::as_object)
        .ok_or_else(|| "scope must be { root: <absolute path>, confined: <boolean> }".to_owned())?;
    let root = scope_obj
        .get("root")
        .and_then(Value::as_str)
        .filter(|s| s.starts_with('/'))
        .ok_or_else(|| "scope must be { root: <absolute path>, confined: <boolean> }".to_owned())?;
    let confined = scope_obj
        .get("confined")
        .and_then(Value::as_bool)
        .ok_or_else(|| "scope must be { root: <absolute path>, confined: <boolean> }".to_owned())?;
    let sender = non_empty_text(record, "sender")
        .ok_or_else(|| "sender must be a non-empty string".to_owned())?;

    let mut env_overlay = BTreeMap::new();
    if let Some(overlay_val) = record.get("env_overlay")
        && !overlay_val.is_null()
    {
        let overlay = overlay_val
            .as_object()
            .ok_or_else(|| "env_overlay must be a map of strings".to_owned())?;
        for (key, value) in overlay {
            let text = value
                .as_str()
                .ok_or_else(|| "env_overlay must be a map of strings".to_owned())?;
            env_overlay.insert(key.clone(), text.to_owned());
        }
    }

    let cross_owner = cross_owner_of(record).map_err(str::to_owned)?;
    Ok(MachineOpRequest {
        op: op.to_owned(),
        args,
        op_id: op_id.to_owned(),
        deadline_ms,
        scope: OpScope {
            root: root.to_owned(),
            confined,
        },
        sender: sender.to_owned(),
        env_overlay,
        cross_owner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognizes_machine_op_commands_and_parses_request() {
        assert!(is_machine_op_command(Some("machine_op")));
        assert!(is_machine_op_command(Some("machine_op_health")));
        assert!(!is_machine_op_command(Some("prompt")));

        let req = json!({
            "op": "fs.read",
            "args": { "path": "a.txt" },
            "op_id": "op-1",
            "deadline_ms": 1_700_000_000_000_u64,
            "scope": { "root": "/tmp/work", "confined": true },
            "sender": "dev-1",
            "env_overlay": { "FOO": "bar" },
            "cross_owner": true,
        });
        let parsed = parse_machine_op(req.as_object().expect("obj")).expect("valid");
        assert_eq!(parsed.op, "fs.read");
        assert_eq!(parsed.op_id, "op-1");
        assert_eq!(parsed.deadline_ms, Some(1_700_000_000_000));
        assert!(parsed.scope.confined);
        assert_eq!(
            parsed.env_overlay.get("FOO").map(String::as_str),
            Some("bar")
        );
        assert!(parsed.cross_owner);
    }

    #[test]
    fn rejects_invalid_machine_op_fields() {
        let bad_deadline = json!({
            "op": "shell",
            "args": { "command": "pwd" },
            "op_id": "op-1",
            "deadline_ms": 0,
            "scope": { "root": "/tmp", "confined": false },
            "sender": "dev-1",
        });
        assert_eq!(
            parse_machine_op(bad_deadline.as_object().expect("obj")),
            Err("deadline_ms must be a positive epoch-ms integer".to_owned())
        );

        let bad_scope = json!({
            "op": "shell",
            "args": {},
            "op_id": "op-1",
            "scope": { "root": "relative/path", "confined": true },
            "sender": "dev-1",
        });
        assert_eq!(
            parse_machine_op(bad_scope.as_object().expect("obj")),
            Err("scope must be { root: <absolute path>, confined: <boolean> }".to_owned())
        );

        let bad_overlay = json!({
            "op": "shell",
            "args": {},
            "op_id": "op-1",
            "scope": { "root": "/tmp", "confined": false },
            "sender": "dev-1",
            "env_overlay": { "A": 123 },
        });
        assert_eq!(
            parse_machine_op(bad_overlay.as_object().expect("obj")),
            Err("env_overlay must be a map of strings".to_owned())
        );
    }

    #[test]
    fn parses_owned_op_lookup_and_cross_owner() {
        let ok = json!({ "op_id": "x", "sender": "s" });
        assert_eq!(
            parse_owned_op_lookup(ok.as_object().expect("obj")),
            Ok(OwnedOpLookup {
                op_id: "x".to_owned(),
                sender: "s".to_owned(),
                cross_owner: false,
            })
        );

        let bad_cross = json!({ "op_id": "x", "sender": "s", "cross_owner": "yes" });
        assert_eq!(
            parse_owned_op_lookup(bad_cross.as_object().expect("obj")),
            Err("cross_owner must be a boolean".to_owned())
        );
    }
}
