//! senpi `modes/rpc/compact-cold-flows.ts`: the four RULING-33 condition-2 flows that target
//! settled messages (edit, context_edit_removal, compaction, branch) mapped to ordered cold ops.

use crate::compact_cold_operations::CompactColdOperation;
use crate::compact_transition::CompactPersistence;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Seeded message payload for settled message replacement/installation (matches `SeededMessage`).
pub type SeededMessage = Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BranchInstalledEntry {
    pub message_id: String,
    pub message: SeededMessage,
    pub persistence: CompactPersistence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CompactColdFlow {
    Edit {
        message_id: String,
        message: SeededMessage,
    },
    ContextEditRemoval {
        message_id: String,
    },
    Compaction {
        removed: Vec<String>,
    },
    Branch {
        removed: Vec<String>,
        installed: Vec<BranchInstalledEntry>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompactColdFlowOperation {
    MessageRemove {
        message_id: String,
    },
    MessageReplace {
        message_id: String,
        message: SeededMessage,
    },
    MessageInstall {
        message_id: String,
        message: SeededMessage,
        persistence: CompactPersistence,
    },
}

impl CompactColdFlowOperation {
    pub fn as_compact_cold_operation(&self) -> Option<CompactColdOperation> {
        match self {
            Self::MessageRemove { message_id } => Some(CompactColdOperation::MessageRemove {
                message_id: message_id.clone(),
            }),
            Self::MessageReplace { .. } | Self::MessageInstall { .. } => None,
        }
    }
}

pub fn cold_flow_operations(flow: &CompactColdFlow) -> Vec<CompactColdFlowOperation> {
    let remove = |message_id: &str| CompactColdFlowOperation::MessageRemove {
        message_id: message_id.to_owned(),
    };
    match flow {
        CompactColdFlow::Edit {
            message_id,
            message,
        } => vec![CompactColdFlowOperation::MessageReplace {
            message_id: message_id.clone(),
            message: message.clone(),
        }],
        CompactColdFlow::ContextEditRemoval { message_id } => vec![remove(message_id)],
        CompactColdFlow::Compaction { removed } => removed.iter().map(|id| remove(id)).collect(),
        CompactColdFlow::Branch { removed, installed } => {
            let mut ops: Vec<CompactColdFlowOperation> =
                removed.iter().map(|id| remove(id)).collect();
            for entry in installed {
                ops.push(CompactColdFlowOperation::MessageInstall {
                    message_id: entry.message_id.clone(),
                    message: entry.message.clone(),
                    persistence: entry.persistence,
                });
            }
            ops
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_all_four_cold_flows_in_order() {
        let edit_ops = cold_flow_operations(&CompactColdFlow::Edit {
            message_id: "m1".into(),
            message: json!({"role":"user"}),
        });
        assert!(matches!(
            edit_ops.as_slice(),
            [CompactColdFlowOperation::MessageReplace { message_id, .. }] if message_id == "m1"
        ));

        let ctx_ops = cold_flow_operations(&CompactColdFlow::ContextEditRemoval {
            message_id: "m2".into(),
        });
        assert_eq!(
            ctx_ops[0].as_compact_cold_operation(),
            Some(CompactColdOperation::MessageRemove {
                message_id: "m2".into()
            })
        );

        let comp_ops = cold_flow_operations(&CompactColdFlow::Compaction {
            removed: vec!["a".into(), "b".into()],
        });
        assert_eq!(comp_ops.len(), 2);

        let branch_ops = cold_flow_operations(&CompactColdFlow::Branch {
            removed: vec!["old".into()],
            installed: vec![BranchInstalledEntry {
                message_id: "new".into(),
                message: json!({"role":"assistant"}),
                persistence: CompactPersistence::Appended,
            }],
        });
        assert_eq!(branch_ops.len(), 2);
        assert!(matches!(
            branch_ops[0],
            CompactColdFlowOperation::MessageRemove { .. }
        ));
        assert!(matches!(
            branch_ops[1],
            CompactColdFlowOperation::MessageInstall { .. }
        ));
    }
}
