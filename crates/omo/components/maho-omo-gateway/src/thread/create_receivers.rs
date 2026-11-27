//! Receiving-side create hooks (`thread/create-receivers.ts`).

use std::path::Path;

use serde_json::Value;

use super::worktree::{WorktreeRpcAnswer, answer_worktree_request};

pub async fn handle_worktree_rpc(data: &Value, worktree_root: &Path) -> WorktreeRpcAnswer {
    answer_worktree_request(data, worktree_root, None).await
}
