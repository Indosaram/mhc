//! OpenAI CUA action definitions and script generation matching upstream `src/cua-actions.ts`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::action_schema::ComputerArgumentsError;

pub const ACTION_NAMES: &[&str] = &[
    "screenshot", "click", "double_click", "move", "drag", "scroll", "type", "keypress", "wait",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotBounds {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ComputerAction {
    Screenshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
    Click {
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
    DoubleClick {
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
    Move {
        x: f64,
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
    Drag {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
    Scroll {
        x: f64,
        y: f64,
        scroll_x: f64,
        scroll_y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
    Type {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
    Keypress {
        keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
    Wait {
        ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ComputerActionsInput {
    Single(ComputerAction),
    Batch {
        action: String,
        actions: Vec<ComputerAction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_screenshot: Option<bool>,
    },
}

impl ComputerActionsInput {
    pub fn timeout(&self) -> Option<f64> {
        match self {
            Self::Single(a) => match a {
                ComputerAction::Screenshot { timeout, .. }
                | ComputerAction::Click { timeout, .. }
                | ComputerAction::DoubleClick { timeout, .. }
                | ComputerAction::Move { timeout, .. }
                | ComputerAction::Drag { timeout, .. }
                | ComputerAction::Scroll { timeout, .. }
                | ComputerAction::Type { timeout, .. }
                | ComputerAction::Keypress { timeout, .. }
                | ComputerAction::Wait { timeout, .. } => *timeout,
            },
            Self::Batch { timeout, .. } => *timeout,
        }
    }
}

pub fn actions_of(input: &ComputerActionsInput) -> Vec<ComputerAction> {
    match input {
        ComputerActionsInput::Single(a) => vec![a.clone()],
        ComputerActionsInput::Batch { actions, .. } => actions.clone(),
    }
}

pub fn is_read_only_actions(input: &Value) -> bool {
    let obj = match input.as_object() {
        Some(o) => o,
        None => return false,
    };
    match obj.get("action").and_then(Value::as_str) {
        Some("batch") => {
            let actions = match obj.get("actions").and_then(Value::as_array) {
                Some(arr) if !arr.is_empty() => arr,
                _ => return false,
            };
            actions.iter().all(|a| {
                let act = a.get("action").and_then(Value::as_str).unwrap_or("");
                act == "screenshot" || act == "wait"
            })
        }
        Some(single) => single == "screenshot" || single == "wait",
        _ => false,
    }
}

fn parse_single_action(
    obj: &serde_json::Map<String, Value>,
    label: &str,
) -> Result<ComputerAction, ComputerArgumentsError> {
    let action = match obj.get("action").and_then(Value::as_str) {
        Some(a) => a,
        None => {
            return Err(ComputerArgumentsError::new(format!(
                r#"{label}: "action" must be one of screenshot, click, double_click, move, drag, scroll, type, keypress, wait"#
            )))
        }
    };

    let allowed: &[&str] = match action {
        "screenshot" => &["action", "timeout", "include_screenshot"],
        "click" | "double_click" | "move" => {
            &["action", "x", "y", "button", "timeout", "include_screenshot"]
        }
        "drag" => &[
            "action",
            "x",
            "y",
            "to_x",
            "to_y",
            "button",
            "timeout",
            "include_screenshot",
        ],
        "scroll" => &[
            "action",
            "x",
            "y",
            "scroll_x",
            "scroll_y",
            "timeout",
            "include_screenshot",
        ],
        "type" => &["action", "text", "timeout", "include_screenshot"],
        "keypress" => &["action", "keys", "timeout", "include_screenshot"],
        "wait" => &["action", "ms", "timeout", "include_screenshot"],
        other => {
            return Err(ComputerArgumentsError::new(format!(
                r#"{label}: unknown action "{other}""#
            )))
        }
    };

    let extra: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    if !extra.is_empty() {
        return Err(ComputerArgumentsError::new(format!(
            r#"{label}: action "{action}" does not take {}"#,
            extra.join(", ")
        )));
    }

    let timeout = obj.get("timeout").and_then(Value::as_f64);
    let include_screenshot = obj.get("include_screenshot").and_then(Value::as_bool);

    match action {
        "screenshot" => Ok(ComputerAction::Screenshot {
            timeout,
            include_screenshot,
        }),
        "click" | "double_click" | "move" => {
            let x = match obj.get("x").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "{action}": must have required properties x"#
                    )))
                }
            };
            let y = match obj.get("y").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "{action}": must have required properties y"#
                    )))
                }
            };
            let button = obj.get("button").and_then(Value::as_str).map(String::from);
            match action {
                "click" => Ok(ComputerAction::Click {
                    x,
                    y,
                    button,
                    timeout,
                    include_screenshot,
                }),
                "double_click" => Ok(ComputerAction::DoubleClick {
                    x,
                    y,
                    button,
                    timeout,
                    include_screenshot,
                }),
                "move" => Ok(ComputerAction::Move {
                    x,
                    y,
                    button,
                    timeout,
                    include_screenshot,
                }),
                _ => unreachable!(),
            }
        }
        "drag" => {
            let x = match obj.get("x").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "drag": must have required properties x"#
                    )))
                }
            };
            let y = match obj.get("y").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "drag": must have required properties y"#
                    )))
                }
            };
            let to_x = match obj.get("to_x").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "drag": must have required properties to_x"#
                    )))
                }
            };
            let to_y = match obj.get("to_y").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "drag": must have required properties to_y"#
                    )))
                }
            };
            let button = obj.get("button").and_then(Value::as_str).map(String::from);
            Ok(ComputerAction::Drag {
                x,
                y,
                to_x,
                to_y,
                button,
                timeout,
                include_screenshot,
            })
        }
        "scroll" => {
            let x = match obj.get("x").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "scroll": must have required properties x"#
                    )))
                }
            };
            let y = match obj.get("y").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "scroll": must have required properties y"#
                    )))
                }
            };
            let scroll_x = match obj.get("scroll_x").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "scroll": must have required properties scroll_x"#
                    )))
                }
            };
            let scroll_y = match obj.get("scroll_y").and_then(Value::as_f64) {
                Some(v) => v,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "scroll": must have required properties scroll_y"#
                    )))
                }
            };
            Ok(ComputerAction::Scroll {
                x,
                y,
                scroll_x,
                scroll_y,
                timeout,
                include_screenshot,
            })
        }
        "type" => {
            let text = match obj.get("text").and_then(Value::as_str) {
                Some(t) => t.to_string(),
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "type": must have required properties text"#
                    )))
                }
            };
            Ok(ComputerAction::Type {
                text,
                timeout,
                include_screenshot,
            })
        }
        "keypress" => {
            let keys_arr = match obj.get("keys").and_then(Value::as_array) {
                Some(arr) if !arr.is_empty() => arr,
                _ => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "keypress": must have non-empty array keys"#
                    )))
                }
            };
            let keys: Vec<String> = keys_arr
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect();
            Ok(ComputerAction::Keypress {
                keys,
                timeout,
                include_screenshot,
            })
        }
        "wait" => {
            let ms = match obj.get("ms").and_then(Value::as_u64) {
                Some(m) => m,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        r#"{label}: action "wait": must have required properties ms"#
                    )))
                }
            };
            Ok(ComputerAction::Wait {
                ms,
                timeout,
                include_screenshot,
            })
        }
        _ => unreachable!(),
    }
}

pub fn parse_computer_actions(input: &Value) -> Result<ComputerActionsInput, ComputerArgumentsError> {
    let obj = match input.as_object() {
        Some(o) => o,
        None => return Err(ComputerArgumentsError::new("computer_actions must be an object")),
    };

    let action = match obj.get("action").and_then(Value::as_str) {
        Some(a) => a,
        None => {
            return Err(ComputerArgumentsError::new(
                r#"computer_actions: "action" must be one of screenshot, click, double_click, move, drag, scroll, type, keypress, wait, batch"#
            ))
        }
    };

    if action == "batch" {
        let allowed = &["action", "actions", "timeout", "include_screenshot"];
        let extra: Vec<&str> = obj
            .keys()
            .map(String::as_str)
            .filter(|k| !allowed.contains(k))
            .collect();
        if !extra.is_empty() {
            return Err(ComputerArgumentsError::new(format!(
                r#"computer_actions: action "batch" does not take {}"#,
                extra.join(", ")
            )));
        }

        let actions_arr = match obj.get("actions").and_then(Value::as_array) {
            Some(a) if !a.is_empty() => a,
            _ => {
                return Err(ComputerArgumentsError::new(
                    r#"computer_actions: action "batch": must have required non-empty array actions"#
                ))
            }
        };

        let mut actions = Vec::new();
        for (idx, item) in actions_arr.iter().enumerate() {
            let item_obj = match item.as_object() {
                Some(o) => o,
                None => {
                    return Err(ComputerArgumentsError::new(format!(
                        "computer_actions actions[{idx}] must be an object"
                    )))
                }
            };
            let single = parse_single_action(item_obj, &format!("computer_actions actions[{idx}]"))?;
            actions.push(single);
        }

        let timeout = obj.get("timeout").and_then(Value::as_f64);
        let include_screenshot = obj.get("include_screenshot").and_then(Value::as_bool);

        Ok(ComputerActionsInput::Batch {
            action: "batch".to_string(),
            actions,
            timeout,
            include_screenshot,
        })
    } else {
        let single = parse_single_action(obj, "computer_actions")?;
        Ok(ComputerActionsInput::Single(single))
    }
}

pub fn actions_script(actions: &[ComputerAction], bounds: Option<&ScreenshotBounds>) -> String {
    let actions_json = serde_json::to_string(actions).unwrap_or_else(|_| "[]".to_string());
    let bounds_json = match bounds {
        Some(b) => serde_json::to_string(b).unwrap_or_else(|_| "null".to_string()),
        None => "null".to_string(),
    };

    format!(
        r#"const actions = {actions_json};
let bounds = {bounds_json};
const steps = [];
const outside = (x, y) => bounds !== null && (x < 0 || y < 0 || x >= bounds.width || y >= bounds.height);
const shoot = async () => {{ const shot = await desktop.screenshot(); bounds = {{ width: shot.width, height: shot.height }}; }};
for (const [index, a] of actions.entries()) {{
	const points = a.action === "drag" ? [[a.x, a.y], [a.to_x, a.to_y]] : "x" in a ? [[a.x, a.y]] : [];
	const off = points.find(([x, y]) => outside(x, y));
	if (off !== undefined) {{
		const message = a.action + " coordinates (" + off[0] + "," + off[1] + ") are outside the latest screenshot bounds " + bounds.width + "x" + bounds.height + ".";
		steps.push({{ index, action: a.action, status: "error", code: "COMPUTER_COORD_INVALID", message }});
		return {{ steps, bounds, failedIndex: index }};
	}}
	try {{
		const button = a.button === undefined ? {{}} : {{ button: a.button }};
		switch (a.action) {{
			case "screenshot": await shoot(); break;
			case "click": await desktop.click(a.x, a.y, button); break;
			case "double_click": await desktop.doubleClick(a.x, a.y, button); break;
			case "move": await desktop.move(a.x, a.y); break;
			case "drag": await desktop.drag([[a.x, a.y], [a.to_x, a.to_y]], button); break;
			case "scroll": await desktop.scroll(a.x, a.y, {{ dx: a.scroll_x, dy: a.scroll_y }}); break;
			case "type": await desktop.type(a.text); break;
			case "keypress": await desktop.press(a.keys); break;
			case "wait": await wait(a.ms); break;
		}}
		if (a.include_screenshot === true && a.action !== "screenshot") await shoot();
		steps.push({{ index, action: a.action, status: "success" }});
	}} catch (error) {{
		const code = error?.data?.code ?? error?.reason ?? error?.name ?? "Error";
		steps.push({{ index, action: a.action, status: "error", code, message: String(error?.message ?? error), permission: error?.data?.permission }});
		return {{ steps, bounds, failedIndex: index }};
	}}
}}
return {{ steps, bounds, failedIndex: null }};"#
    )
}
