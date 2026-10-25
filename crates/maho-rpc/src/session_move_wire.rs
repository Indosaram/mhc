//! Move-session wire constants and typed command operations.
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const MOVE_SESSION_CAPABILITY: &str = "move_session";
pub const MOVE_SESSION_REVOKE_CAPABILITY: &str = "move_session_revoke";
pub const RPC_ERROR_INVALID_MOVE_REQUEST: &str = "invalid_move_request";
pub const RPC_ERROR_SESSION_OPEN_HERE: &str = "session_open_here";
pub const RPC_ERROR_MOVE_NOT_RELEASED: &str = "move_not_released";
pub const RPC_ERROR_MOVE_FAILED: &str = "move_failed";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum RpcMoveSessionCommand {
    Prepare {
        move_id: String,
    },
    Cancel {
        move_id: String,
    },
    Status {
        move_id: String,
        session_path: Option<String>,
    },
    Export {
        session_path: String,
        move_id: String,
        target: String,
        #[serde(default)]
        dropped_user_messages: Vec<String>,
    },
    Import {
        bundle_dir: String,
        cwd: String,
        from: Option<String>,
    },
    Finish {
        session_path: String,
        move_id: String,
        answer: Value,
    },
    Revoke {
        session_path: String,
        move_id: String,
    },
}
