//! Per-session Cursor exec bridge state.

use std::sync::Arc;

use maho_agent::Agent;
use maho_ai::utils::abort::AbortSignal;

use crate::agent_session::AgentSession;
use crate::cursor_exec_bridge::{create_cursor_exec_bridge, CursorExecBridgeOptions};

pub fn create_session_cursor_exec_bridge(
    session: Arc<AgentSession>,
    agent: Agent,
    run_signal: Option<AbortSignal>,
) -> maho_ai::api::cursor_agent::types::CursorExecHandlers {
    let signal = run_signal.clone();
    let preflight_session = session.clone();
    let options = CursorExecBridgeOptions {
        get_tool: Arc::new(move |name| session.get_registered_tool(name)),
        emit_event: Arc::new(move |event, signal| { let agent = agent.clone(); Box::pin(async move { agent.emit_external_event(event, Some(signal)).await }) }),
        emit_tool_result: None,
        preflight_tool_call: Some(Arc::new(move |id, name, input| {
            let session = preflight_session.clone();
            Box::pin(async move {
                session.preflight_tool_call(&maho_agent::types::AgentToolCall {
                    id, name, arguments: input.as_object().cloned().unwrap_or_default(), ..Default::default()
                }, input).await.and_then(|(result, _)| result.block.and_then(|blocked| blocked.then(|| result.reason.unwrap_or_else(|| "Tool execution was blocked".to_owned()))))
            })
        })),
        get_abort_signal: Arc::new(move || signal.clone().filter(|signal| !signal.aborted())),
    };
    create_cursor_exec_bridge(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_bridge_factory_is_available() {
        let _factory: fn(Arc<AgentSession>, Agent, Option<AbortSignal>) -> maho_ai::api::cursor_agent::types::CursorExecHandlers = create_session_cursor_exec_bridge;
    }
}
