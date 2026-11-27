//! Forwardable dialog parsing and response encoding (`runners/child-dialogs/forwardable-dialog.ts`).

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardableDialog {
    Select {
        title: String,
        options: Vec<String>,
        tool_call_id: Option<String>,
        parent_tool_call_id: Option<String>,
        timeout: Option<u64>,
    },
    Confirm {
        title: String,
        message: String,
        timeout: Option<u64>,
    },
    Input {
        title: String,
        placeholder: Option<String>,
        tool_call_id: Option<String>,
        parent_tool_call_id: Option<String>,
        timeout: Option<u64>,
    },
    Editor {
        title: String,
        prefill: Option<String>,
    },
}

impl ForwardableDialog {
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Select { .. } => "select",
            Self::Confirm { .. } => "confirm",
            Self::Input { .. } => "input",
            Self::Editor { .. } => "editor",
        }
    }

    #[must_use]
    pub fn title(&self) -> &str {
        match self {
            Self::Select { title, .. }
            | Self::Confirm { title, .. }
            | Self::Input { title, .. }
            | Self::Editor { title, .. } => title,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardedAnswer {
    Value(String),
    Confirmed(bool),
    Cancelled,
}

pub const CANCELLED_CHILD_DIALOG: ForwardedAnswer = ForwardedAnswer::Cancelled;

fn text(frame: &Value, key: &str) -> Option<String> {
    frame.get(key).and_then(Value::as_str).map(str::to_string)
}

fn positive_u64(frame: &Value, key: &str) -> Option<u64> {
    let val = frame.get(key)?;
    if let Some(n) = val.as_u64() {
        return (n > 0).then_some(n);
    }
    let f = val.as_f64()?;
    if f.is_finite() && f > 0.0 && f <= (u64::MAX as f64) {
        Some(f as u64)
    } else {
        None
    }
}

/// Parses a child's `extension_ui_request` frame; `None` for non-forwardable methods or
/// secret/grant-select dialogs (those stay with the raiser and keep the auto-cancel).
pub fn forwardable_dialog_of(frame: &Value) -> Option<ForwardableDialog> {
    if frame.get("secret").and_then(Value::as_bool) == Some(true)
        || frame.get("grantSelect").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let title = text(frame, "title")?;
    let timeout = positive_u64(frame, "timeout");
    let tool_call_id = text(frame, "toolCallId");
    let parent_tool_call_id = text(frame, "parentToolCallId");
    match frame.get("method").and_then(Value::as_str)? {
        "select" => {
            let arr = frame.get("options")?.as_array()?;
            let mut options = Vec::with_capacity(arr.len());
            for item in arr {
                options.push(item.as_str()?.to_string());
            }
            Some(ForwardableDialog::Select {
                title,
                options,
                tool_call_id,
                parent_tool_call_id,
                timeout,
            })
        }
        "confirm" => {
            let message = text(frame, "message")?;
            Some(ForwardableDialog::Confirm {
                title,
                message,
                timeout,
            })
        }
        "input" => Some(ForwardableDialog::Input {
            title,
            placeholder: text(frame, "placeholder"),
            tool_call_id,
            parent_tool_call_id,
            timeout,
        }),
        "editor" => Some(ForwardableDialog::Editor {
            title,
            prefill: text(frame, "prefill"),
        }),
        _ => None,
    }
}

pub fn forwarded_ui_response(id: &Value, answer: &ForwardedAnswer) -> Value {
    match answer {
        ForwardedAnswer::Value(value) => {
            json!({ "type": "extension_ui_response", "id": id, "value": value })
        }
        ForwardedAnswer::Confirmed(confirmed) => {
            json!({ "type": "extension_ui_response", "id": id, "confirmed": confirmed })
        }
        ForwardedAnswer::Cancelled => {
            json!({ "type": "extension_ui_response", "id": id, "cancelled": true })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwardable_dialog_parses_four_methods_and_rejects_secret_or_grant_select() {
        assert_eq!(
            forwardable_dialog_of(&json!({
                "method": "select",
                "title": "Allow?",
                "options": ["Allow", "Deny"],
                "toolCallId": "tc-1",
                "parentToolCallId": "ptc-1",
                "timeout": 5000
            })),
            Some(ForwardableDialog::Select {
                title: "Allow?".into(),
                options: vec!["Allow".into(), "Deny".into()],
                tool_call_id: Some("tc-1".into()),
                parent_tool_call_id: Some("ptc-1".into()),
                timeout: Some(5000),
            })
        );
        assert_eq!(
            forwardable_dialog_of(&json!({
                "method": "input",
                "title": "Token",
                "secret": true
            })),
            None
        );
        assert_eq!(
            forwardable_dialog_of(&json!({
                "method": "select",
                "title": "Grant",
                "options": ["Always"],
                "grantSelect": true
            })),
            None
        );
        assert_eq!(
            forwarded_ui_response(&json!("req-1"), &ForwardedAnswer::Confirmed(true)),
            json!({ "type": "extension_ui_response", "id": "req-1", "confirmed": true })
        );
    }
}
