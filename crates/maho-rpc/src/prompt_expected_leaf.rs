//! Port of senpi `packages/coding-agent/src/modes/rpc/prompt-expected-leaf.ts`.
//!
//! senpi#6922: a prompt naming the leaf it was decided at is refused before admission when the
//! session moved.

use maho_core::agent_session::AgentSession;
use maho_core::edited_assistant_message::assert_expected_leaf;
use maho_core::lifecycle_edit_leaf::lifecycle_edit_leaf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptLeafGuard<'a> {
    pub expected_leaf_id: Option<&'a str>,
    pub has_streaming_behavior: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptLeafRefusal {
    pub message: String,
    pub error_code: &'static str,
}

pub fn refuse_prompt_at_moved_leaf(
    session: &AgentSession,
    guard: PromptLeafGuard<'_>,
    held_as_notice: bool,
) -> Option<PromptLeafRefusal> {
    let expected = guard.expected_leaf_id?;
    if guard.has_streaming_behavior {
        return Some(PromptLeafRefusal {
            message:
                "expectedLeafId cannot be combined with streamingBehavior: a queued prompt runs at a later leaf."
                    .to_owned(),
            error_code: crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT,
        });
    }
    if held_as_notice {
        return Some(PromptLeafRefusal {
            message:
                "expectedLeafId cannot be honoured while a Stop or run hold keeps input as a notice for a later turn."
                    .to_owned(),
            error_code: crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT,
        });
    }
    let current = session.with_session_manager(|sm| lifecycle_edit_leaf(sm, Some(expected)));
    if let Err(err) = assert_expected_leaf(Some(expected), current.as_deref()) {
        return Some(PromptLeafRefusal {
            message: err.to_string(),
            error_code: crate::rpc_types::RPC_ERROR_STALE_LEAF,
        });
    }
    None
}
