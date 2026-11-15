//! Port of omo `packages/omo-senpi/src/components/task/host-prewarm-intent.ts`.

use serde_json::Value;

use maho_rpc::host_endpoints::{ShardKind, parse_shard_socket};
use senpi_task::runners::rpc_host::own_endpoint::read_own_host_socket;
use senpi_task::tools::task::tool::TASK_TOOL_NAME;

const DELEGATION_TOOLS: [&str; 2] = [TASK_TOOL_NAME, "task_send"];

/// A Desktop per-thread host is an `i-*` endpoint, so a session behind one warms its child shard
/// when the model begins a delegating call rather than on its first prompt.
pub fn runs_in_desktop_thread_host(pi: &Value) -> bool {
    let Some(socket) = read_own_host_socket(pi) else {
        return false;
    };
    matches!(parse_shard_socket(&socket), Some((ShardKind::I, _)))
}

/// Whether a `message_update` opens a `task` or `task_send` call: the `toolcall_start` event, which
/// the engine streams with the call's name before any argument arrives. Every other update is
/// rejected on its event type first, so the per-token cost stays one read.
pub fn starts_delegation(payload: &Value) -> bool {
    let Some(event) = payload.get("assistantMessageEvent") else {
        return false;
    };
    if event.get("type").and_then(Value::as_str) != Some("toolcall_start") {
        return false;
    }
    let Some(index) = event.get("contentIndex").and_then(Value::as_u64) else {
        return false;
    };
    let Some(block) = event
        .get("partial")
        .and_then(|partial| partial.get("content"))
        .and_then(|content| content.as_array())
        .and_then(|content| content.get(usize::try_from(index).ok()?))
    else {
        return false;
    };
    block.get("type").and_then(Value::as_str) == Some("toolCall")
        && block
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| DELEGATION_TOOLS.contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(name: &str) -> Value {
        serde_json::json!({
            "assistantMessageEvent": {
                "type": "toolcall_start",
                "contentIndex": 0,
                "partial": {"content": [{"type": "toolCall", "name": name}]},
            }
        })
    }

    #[test]
    fn a_task_call_start_is_a_delegation() {
        assert!(starts_delegation(&partial("task")));
        assert!(starts_delegation(&partial("task_send")));
    }

    #[test]
    fn any_other_call_or_event_is_not_a_delegation() {
        assert!(!starts_delegation(&partial("read")));
        assert!(!starts_delegation(&serde_json::json!({
            "assistantMessageEvent": {
                "type": "text_delta",
                "contentIndex": 0,
                "partial": {"content": [{"type": "toolCall", "name": "task"}]},
            }
        })));
        assert!(!starts_delegation(&Value::Null));
    }

    #[test]
    fn an_out_of_range_content_index_is_not_a_delegation() {
        assert!(!starts_delegation(&serde_json::json!({
            "assistantMessageEvent": {
                "type": "toolcall_start",
                "contentIndex": 3,
                "partial": {"content": [{"type": "toolCall", "name": "task"}]},
            }
        })));
    }

    #[test]
    fn only_an_interactive_shard_makes_a_desktop_thread_host() {
        let interactive = serde_json::json!({
            "sessionContext": {"host_socket": "/rpc/shards/i-0123456789abcdef.sock"}
        });
        assert!(runs_in_desktop_thread_host(&interactive));
        let parent = serde_json::json!({
            "sessionContext": {"host_socket": "/rpc/shards/p-0123456789abcdef.sock"}
        });
        assert!(!runs_in_desktop_thread_host(&parent));
        assert!(!runs_in_desktop_thread_host(&Value::Null));
    }
}
