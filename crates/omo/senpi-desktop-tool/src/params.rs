//! Computer tool action parameters and validation matching upstream `src/params.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::action_schema::ComputerArgumentsError;

pub const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
pub const MAX_TIMEOUT_SECONDS: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallStep {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ComputerToolParams {
    #[serde(rename = "call")]
    Call {
        chain: Vec<CallStep>,
    },
    #[serde(rename = "run")]
    Run {
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_only: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
    },
    #[serde(rename = "capabilities")]
    Capabilities,
    #[serde(rename = "close")]
    Close,
}

pub fn computer_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["call", "run", "capabilities", "close"],
                "description": "call: one desktop helper chain; run: a JavaScript body; capabilities: host report; close: end the session"
            },
            "chain": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "method": {
                            "type": "string",
                            "description": "desktop helper name, e.g. screenshot, window, click, clipboard.read"
                        },
                        "args": {
                            "type": "array",
                            "description": "positional helper arguments"
                        }
                    },
                    "required": ["method"],
                    "additionalProperties": false
                },
                "description": "call (required): one desktop helper call, optionally followed by one call on the window/element it returns"
            },
            "code": {
                "type": "string",
                "description": "run (required): JavaScript async function body; desktop, wait, assert, tool in scope; return sets the value"
            },
            "read_only": {
                "type": "boolean",
                "description": "run: true = inspection only: screenshots and AX reads, input/mutation blocked"
            },
            "timeout": {
                "type": "number",
                "minimum": 1,
                "maximum": MAX_TIMEOUT_SECONDS,
                "description": format!("run: budget in seconds (default {DEFAULT_TIMEOUT_SECONDS})")
            }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

/// The flat published arguments narrowed to one action's exact shape; `ComputerArgumentsError` otherwise.
pub fn parse_computer_params(input: &Value) -> Result<ComputerToolParams, ComputerArgumentsError> {
    let obj = match input.as_object() {
        Some(o) => o,
        None => return Err(ComputerArgumentsError::new("computer must be an object")),
    };

    let action = match obj.get("action").and_then(Value::as_str) {
        Some(a) => a,
        None => {
            return Err(ComputerArgumentsError::new(
                r#"computer: "action" must be one of call, run, capabilities, close"#,
            ))
        }
    };

    let allowed_keys: &[&str] = match action {
        "call" => &["action", "chain"],
        "run" => &["action", "code", "read_only", "timeout"],
        "capabilities" => &["action"],
        "close" => &["action"],
        other => {
            return Err(ComputerArgumentsError::new(format!(
                r#"computer: unknown action "{other}"; expected one of call, run, capabilities, close"#
            )))
        }
    };

    let extra: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed_keys.contains(k))
        .collect();
    if !extra.is_empty() {
        return Err(ComputerArgumentsError::new(format!(
            r#"computer: action "{action}" does not take {}"#,
            extra.join(", ")
        )));
    }

    match action {
        "call" => {
            let chain_val = match obj.get("chain") {
                Some(c) => c,
                None => {
                    return Err(ComputerArgumentsError::new(
                        r#"computer: action "call": must have required properties chain"#,
                    ))
                }
            };
            let chain_arr = match chain_val.as_array() {
                Some(arr) => arr,
                None => {
                    return Err(ComputerArgumentsError::new(
                        r#"computer: action "call" at /chain: must be array"#,
                    ))
                }
            };
            let mut steps = Vec::new();
            for (idx, item) in chain_arr.iter().enumerate() {
                let item_obj = match item.as_object() {
                    Some(o) => o,
                    None => {
                        return Err(ComputerArgumentsError::new(format!(
                            r#"computer: action "call" at /chain/{idx}: must be object"#
                        )))
                    }
                };
                for k in item_obj.keys() {
                    if k != "method" && k != "args" {
                        return Err(ComputerArgumentsError::new(format!(
                            r#"computer: action "call" at /chain/{idx}: unexpected property '{k}'"#
                        )));
                    }
                }
                let method = match item_obj.get("method").and_then(Value::as_str) {
                    Some(m) => m.to_string(),
                    None => {
                        return Err(ComputerArgumentsError::new(format!(
                            r#"computer: action "call" at /chain/{idx}: missing method"#
                        )))
                    }
                };
                let args = match item_obj.get("args") {
                    Some(Value::Array(a)) => Some(a.clone()),
                    Some(_) => {
                        return Err(ComputerArgumentsError::new(format!(
                            r#"computer: action "call" at /chain/{idx}/args: must be array"#
                        )))
                    }
                    None => None,
                };
                steps.push(CallStep { method, args });
            }
            Ok(ComputerToolParams::Call { chain: steps })
        }
        "run" => {
            let code = match obj.get("code").and_then(Value::as_str) {
                Some(c) => c.to_string(),
                None => {
                    return Err(ComputerArgumentsError::new(
                        r#"computer: action "run": must have required properties code"#,
                    ))
                }
            };
            let read_only = match obj.get("read_only") {
                Some(Value::Bool(b)) => Some(*b),
                Some(_) => {
                    return Err(ComputerArgumentsError::new(
                        r#"computer: action "run" at /read_only: must be boolean"#,
                    ))
                }
                None => None,
            };
            let timeout = match obj.get("timeout") {
                Some(Value::Number(n)) => {
                    let val = n.as_f64().unwrap_or(0.0);
                    if val < 1.0 {
                        return Err(ComputerArgumentsError::new(
                            r#"computer: action "run" at /timeout: must be >= 1"#,
                        ));
                    }
                    if val > MAX_TIMEOUT_SECONDS as f64 {
                        return Err(ComputerArgumentsError::new(format!(
                            r#"computer: action "run" at /timeout: must be <= {MAX_TIMEOUT_SECONDS}"#
                        )));
                    }
                    Some(val)
                }
                Some(_) => {
                    return Err(ComputerArgumentsError::new(
                        r#"computer: action "run" at /timeout: must be number"#,
                    ))
                }
                None => None,
            };
            Ok(ComputerToolParams::Run {
                code,
                read_only,
                timeout,
            })
        }
        "capabilities" => Ok(ComputerToolParams::Capabilities),
        "close" => Ok(ComputerToolParams::Close),
        _ => unreachable!(),
    }
}
