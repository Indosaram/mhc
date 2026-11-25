//! Port of senpi `packages/coding-agent/src/modes/rpc/session-move-router.ts`.
//!
//! `move_session` as the multi-session router runs it (senpi#3676, senpi#6267).

use crate::session_move_commands::{MoveSessionPort, move_session_with_port};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::Path;

#[async_trait]
pub trait MoveRouterHost: Send + Sync {
    fn agent_dir(&self) -> Option<&Path>;
    fn hosted_paths(&self) -> Vec<String>;
    fn is_draining(&self) -> bool;
    async fn open_imported(&self, open_command: Value) -> Result<Option<Value>, Value>;
}

struct RouterMovePort<'a> {
    host: &'a dyn MoveRouterHost,
    agent_dir: &'a Path,
}

#[async_trait]
impl MoveSessionPort for RouterMovePort<'_> {
    fn agent_dir(&self) -> &Path {
        self.agent_dir
    }

    fn hosted_paths(&self) -> Vec<String> {
        self.host.hosted_paths()
    }

    async fn open_landed(&self, open_command: Value) -> Result<Option<Value>, Value> {
        if self.host.is_draining() {
            return Ok(Some(json!({
                "error": "host_draining",
                "errorCode": "host_draining",
            })));
        }
        self.host.open_imported(open_command).await
    }
}

pub async fn route_move_session(host: &dyn MoveRouterHost, command: &Value) -> Value {
    let Some(agent_dir) = host.agent_dir() else {
        return json!({
            "id": command.get("id"),
            "type": "response",
            "command": "move_session",
            "success": false,
            "error": "move_unsupported: this host keeps no agent directory to move sessions in",
            "errorCode": "move_unsupported",
        });
    };
    let port = RouterMovePort { host, agent_dir };
    move_session_with_port(&port, command).await
}
