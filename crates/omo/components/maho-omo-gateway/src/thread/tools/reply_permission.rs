//! Auto-approved reply permission checks (`thread/tools/reply-permission.ts`).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::addressing::normalize_thread_name;
use super::local_permission::{ThreadPermissionRequest, thread_tool_permission};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplySender {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

fn is_uuid_shaped(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (idx, b) in bytes.iter().enumerate() {
        match idx {
            8 | 13 | 18 | 23 => {
                if *b != b'-' {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

fn senders_named_by(reply_senders: &[ReplySender], target: &str) -> Vec<ReplySender> {
    let wanted = normalize_thread_name(target);
    reply_senders
        .iter()
        .filter(|sender| {
            sender.session_id == target
                || sender
                    .name
                    .as_deref()
                    .is_some_and(|n| !wanted.is_empty() && normalize_thread_name(n) == wanted)
        })
        .cloned()
        .collect()
}

pub fn is_reply_to_consumed_sender(
    reply_targets: &[String],
    reply_senders: &[ReplySender],
    target: &str,
) -> bool {
    if is_uuid_shaped(target) && reply_targets.iter().any(|t| t == target) {
        return true;
    }
    !senders_named_by(reply_senders, target).is_empty()
}

pub fn consumed_senders_of(
    reply_targets: &[String],
    reply_senders: &[ReplySender],
    target: &str,
) -> Vec<ReplySender> {
    if !is_reply_to_consumed_sender(reply_targets, reply_senders, target) {
        return Vec::new();
    }
    let named = senders_named_by(reply_senders, target);
    if named.is_empty() {
        vec![ReplySender {
            session_id: target.to_owned(),
            name: None,
            device: None,
        }]
    } else {
        named
    }
}

pub fn thread_send_permission(
    input: &Value,
    reply_targets: &[String],
    reply_senders: &[ReplySender],
    sessions_dir: Option<&Path>,
) -> Vec<ThreadPermissionRequest> {
    if let Some(target) = input.get("thread").and_then(Value::as_str)
        && is_reply_to_consumed_sender(reply_targets, reply_senders, target)
    {
        return vec![ThreadPermissionRequest {
            permission: "thread_send".to_owned(),
            patterns: vec![target.to_owned()],
            always: vec![target.to_owned()],
            auto_approve_ask: true,
        }];
    }
    thread_tool_permission("thread_send", input, sessions_dir)
}
