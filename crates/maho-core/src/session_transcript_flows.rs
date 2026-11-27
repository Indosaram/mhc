//! Port of senpi packages/coding-agent/src/core/session-transcript-flows.ts.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledFlowMessage {
    pub id: String,
    pub message: Value,
}

/// A committed change that removes, replaces or reinstalls whole messages of the active path,
/// named by entry id (RULING-33 condition 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TranscriptFlow {
    Edit {
        target_id: String,
        message: Value,
    },
    ContextEditRemoval {
        target_id: String,
    },
    Compaction {
        removed_ids: Vec<String>,
    },
    Branch {
        removed_ids: Vec<String>,
        installed: Vec<InstalledFlowMessage>,
    },
}

#[derive(Clone, Debug)]
pub struct ProjectedFlowEntry {
    pub source_id: String,
    pub source_type: String,
    pub messages: Vec<Value>,
}

fn is_flow_role(role: &str) -> bool {
    matches!(role, "user" | "assistant" | "toolResult" | "custom")
}

/// Only the roles compact state holds; owned JSON.
pub fn owned_flow_message(message: Option<&Value>) -> Option<Value> {
    let msg = message?;
    let role = msg.get("role")?.as_str()?;
    is_flow_role(role).then(|| msg.clone())
}

fn context_message(entry: &ProjectedFlowEntry) -> Option<&Value> {
    let message = entry.messages.first()?;
    let is_msg = entry.source_type == "message" || entry.source_type == "custom_message";
    let role = message.get("role")?.as_str()?;
    (is_msg && is_flow_role(role)).then_some(message)
}

fn left_ids(from: &[ProjectedFlowEntry], to: &[ProjectedFlowEntry]) -> Vec<String> {
    let kept: HashSet<&str> = to.iter().map(|e| e.source_id.as_str()).collect();
    from.iter()
        .filter(|e| !kept.contains(e.source_id.as_str()) && context_message(e).is_some())
        .map(|e| e.source_id.clone())
        .collect()
}

pub fn compaction_flow(from: &[ProjectedFlowEntry], to: &[ProjectedFlowEntry]) -> TranscriptFlow {
    TranscriptFlow::Compaction {
        removed_ids: left_ids(from, to),
    }
}

pub fn branch_flow(from: &[ProjectedFlowEntry], to: &[ProjectedFlowEntry]) -> TranscriptFlow {
    let before: HashSet<&str> = from.iter().map(|e| e.source_id.as_str()).collect();
    let installed = to
        .iter()
        .filter_map(|e| {
            if before.contains(e.source_id.as_str()) {
                return None;
            }
            let message = owned_flow_message(context_message(e))?;
            Some(InstalledFlowMessage {
                id: e.source_id.clone(),
                message,
            })
        })
        .collect();
    TranscriptFlow::Branch {
        removed_ids: left_ids(from, to),
        installed,
    }
}

pub fn context_edit_flow(
    target_id: &str,
    replacement_is_null: bool,
    edited: Option<Value>,
) -> Option<TranscriptFlow> {
    if replacement_is_null {
        return Some(TranscriptFlow::ContextEditRemoval {
            target_id: target_id.to_owned(),
        });
    }
    edited.map(|message| TranscriptFlow::Edit {
        target_id: target_id.to_owned(),
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_compaction_and_branch_flows() {
        let e1 = ProjectedFlowEntry {
            source_id: "1".into(),
            source_type: "message".into(),
            messages: vec![json!({"role": "user", "content": "hi"})],
        };
        let e2 = ProjectedFlowEntry {
            source_id: "2".into(),
            source_type: "message".into(),
            messages: vec![json!({"role": "assistant", "content": "yo"})],
        };
        assert_eq!(
            compaction_flow(&[e1.clone(), e2.clone()], std::slice::from_ref(&e2)),
            TranscriptFlow::Compaction {
                removed_ids: vec!["1".into()]
            }
        );
        assert_eq!(
            branch_flow(&[e1], &[e2]),
            TranscriptFlow::Branch {
                removed_ids: vec!["1".into()],
                installed: vec![InstalledFlowMessage {
                    id: "2".into(),
                    message: json!({"role": "assistant", "content": "yo"})
                }],
            }
        );
    }
}
