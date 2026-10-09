//! Permission parsing and tier classification matching upstream `src/permission.ts`.

use maho_tools::ToolPermissionRequest;
use senpi_desktop_protocol::{
    is_read_only_computer_call, ComputerCallPolicy, ComputerCallStep,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const COMPUTER_PERMISSION: &str = "computer";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub permission: String,
    pub patterns: Vec<String>,
    pub always: Vec<String>,
}

impl From<PermissionRequest> for ToolPermissionRequest {
    fn from(r: PermissionRequest) -> Self {
        ToolPermissionRequest::new(r.permission, r.patterns, r.always)
    }
}

pub fn computer_tier(input: &Value) -> ComputerCallPolicy {
    let obj = match input.as_object() {
        Some(o) => o,
        None => return ComputerCallPolicy::Exec,
    };

    match obj.get("action").and_then(Value::as_str) {
        Some("capabilities") => ComputerCallPolicy::Read,
        Some("run") => {
            if obj.get("read_only").and_then(Value::as_bool) == Some(true) {
                ComputerCallPolicy::Read
            } else {
                ComputerCallPolicy::Exec
            }
        }
        Some("call") => {
            let chain = match obj.get("chain").and_then(Value::as_array) {
                Some(c) => c,
                None => return ComputerCallPolicy::Exec,
            };
            let mut steps = Vec::new();
            for item in chain {
                let item_obj = match item.as_object() {
                    Some(o) => o,
                    None => return ComputerCallPolicy::Exec,
                };
                let method = match item_obj.get("method").and_then(Value::as_str) {
                    Some(m) => m.to_string(),
                    None => return ComputerCallPolicy::Exec,
                };
                let args = item_obj.get("args").and_then(Value::as_array).cloned();
                steps.push(ComputerCallStep { method, args });
            }
            if steps.is_empty() {
                return ComputerCallPolicy::Exec;
            }
            match is_read_only_computer_call(&steps) {
                Ok(true) => ComputerCallPolicy::Read,
                Ok(false) => ComputerCallPolicy::Exec,
                Err(_) => ComputerCallPolicy::Exec,
            }
        }
        _ => ComputerCallPolicy::Exec,
    }
}

pub fn computer_permission_parser(
    _tool_name: &str,
    input: &Value,
    _cwd: &std::path::Path,
) -> Vec<PermissionRequest> {
    let tier = match computer_tier(input) {
        ComputerCallPolicy::Read => "read",
        ComputerCallPolicy::Exec => "exec",
    };
    vec![PermissionRequest {
        permission: COMPUTER_PERMISSION.to_string(),
        patterns: vec![tier.to_string()],
        always: vec![tier.to_string()],
    }]
}
