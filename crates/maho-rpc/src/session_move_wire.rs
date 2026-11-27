//! Move-session wire constants and typed command operations (senpi `modes/rpc/session-move-wire.ts`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MOVE_SESSION_CAPABILITY: &str = "move_session";
pub const MOVE_SESSION_REVOKE_CAPABILITY: &str = "move_session_revoke";
pub const MOVE_SESSION_COPY_CAPABILITY: &str = "move_session_copy_v1";
pub const MOVE_SESSION_EXPORT_INLINE_CAPABILITY: &str = "move_session_export_inline_v1";
pub const MOVE_SESSION_IMPORT_OPEN_CAPABILITY: &str = "move_session_import_open_v1";
pub const MOVE_SESSION_IMPORT_FILES_CAPABILITY: &str = "move_session_import_files_v1";
pub const MOVE_SESSION_IMPORT_SESSION_DIR_CAPABILITY: &str = "move_session_import_session_dir_v1";
pub const MOVE_SESSION_PREPARE_REPOSITORY_CAPABILITY: &str = "move_session_prepare_repository_v1";
pub const MOVE_SESSION_IDENTITY_CAPABILITY: &str = "move_session_identity_v1";
pub const MOVE_SESSION_STATUS_LANDING_CAPABILITY: &str = "move_session_status_landing_v1";
pub const MOVE_SESSION_STATUS_LANDED_PATH_CAPABILITY: &str = "move_session_status_landed_path_v1";
pub const MOVE_SESSION_STATUS_WRITABLE_CAPABILITY: &str = "move_session_status_writable_v1";
pub const MOVE_INLINE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MOVE_INLINE_MAX_FILES: usize = 256;
pub const MOVE_SESSION_INLINE_MAX_BYTES_CAPABILITY: &str = "move_session_inline_max_bytes:8388608";

pub const RPC_ERROR_INVALID_MOVE_REQUEST: &str = "invalid_move_request";
pub const RPC_ERROR_SESSION_OPEN_HERE: &str = "session_open_here";
pub const RPC_ERROR_MOVE_NOT_RELEASED: &str = "move_not_released";
pub const RPC_ERROR_MOVE_FAILED: &str = "move_failed";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcMoveInlineFile {
    pub path: String,
    pub sha256: String,
    pub base64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcMoveStatusLanding {
    pub cwd: String,
    pub session_file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcMoveOpenError {
    pub error: String,
    #[serde(rename = "errorCode", skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(rename = "errorData", skip_serializing_if = "Option::is_none")]
    pub error_data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcMoveSealedBy {
    pub move_id: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum RpcMoveSessionCommand {
    Prepare {
        move_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<Value>,
    },
    Cancel {
        move_id: String,
    },
    Identity {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Status {
        move_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        landing: Option<RpcMoveStatusLanding>,
    },
    Export {
        session_path: String,
        move_id: String,
        target: String,
        #[serde(default)]
        dropped_user_messages: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inline: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inline_max_bytes: Option<usize>,
    },
    Import {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bundle_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        files: Option<Vec<RpcMoveInlineFile>>,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open: Option<Value>,
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
    Copy {
        session_path: String,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_dir: Option<String>,
    },
}
