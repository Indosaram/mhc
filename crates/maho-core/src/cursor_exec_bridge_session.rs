//! Port of senpi `packages/coding-agent/src/core/cursor-exec-bridge-session.ts`.

use std::sync::Arc;

use maho_agent::Agent;
use maho_ai::utils::abort::AbortSignal;

use crate::agent_session::AgentSession;
use crate::cursor_exec_bridge::{create_cursor_exec_bridge, CursorExecBridgeOptions};

/// Late-bound session lookup (pinned `sessionRef.current`): the bridge lives inside the agent the
/// session owns, so it must not keep the session alive.
pub type SessionRef = Arc<dyn Fn() -> Option<AgentSession> + Send + Sync>;

/// Build the exec handlers for ONE Cursor run.
///
/// `run_signal` is the signal of the run that owns this stream, captured when the loop opens it.
/// Resolving ownership from the agent's live signal instead would let a straggler frame from a
/// stream whose run already ended adopt the replacement run's signal and execute a dead run's tool
/// inside the new run.
pub fn create_session_cursor_exec_bridge(
    session_ref: SessionRef,
    agent: Agent,
    run_signal: Option<AbortSignal>,
) -> maho_ai::api::cursor_agent::types::CursorExecHandlers {
    let tool_session = session_ref.clone();
    let preflight_session = session_ref.clone();
    let result_session = session_ref;
    let event_agent = agent.clone();
    let options = CursorExecBridgeOptions {
        get_tool: Arc::new(move |name| tool_session().and_then(|session| session.get_registered_tool(name))),
        emit_event: Arc::new(move |event, signal| {
            let agent = event_agent.clone();
            Box::pin(async move { agent.emit_external_event(event, Some(signal)).await })
        }),
        emit_tool_result: Some(Arc::new(move |tool_call_id, tool_name, args, result, is_error| {
            let session = result_session();
            Box::pin(async move {
                if let Some(session) = session {
                    session.emit_exec_bridge_tool_result(tool_name, tool_call_id, args, result, is_error).await;
                }
            })
        })),
        preflight_tool_call: Some(Arc::new(move |id, name, input| {
            let session = preflight_session();
            Box::pin(async move {
                let session = session?;
                session.preflight_tool_call(&maho_agent::types::AgentToolCall {
                    id, name, arguments: input.as_object().cloned().unwrap_or_default(), ..Default::default()
                }, input).await.and_then(|(result, _)| result.block.and_then(|blocked| blocked.then(|| result.reason.unwrap_or_else(|| "Tool execution was blocked".to_owned()))))
            })
        })),
        get_abort_signal: Arc::new(move || {
            let run_signal = run_signal.as_ref().filter(|signal| !signal.aborted())?;
            agent.signal().filter(|active| active.same_signal(run_signal)).map(|_| run_signal.clone())
        }),
    };
    create_cursor_exec_bridge(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_bridge_factory_is_available() {
        let _factory: fn(SessionRef, Agent, Option<AbortSignal>) -> maho_ai::api::cursor_agent::types::CursorExecHandlers = create_session_cursor_exec_bridge;
    }

    #[test]
    fn bridge_rejects_stale_run_handler_invocation_without_execution_or_events() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let stream_fn: maho_agent::types::StreamFn =
            Arc::new(|_, _, _| maho_ai::types::AssistantMessageEventStream::assistant());
        let agent = Agent::new(maho_agent::AgentOptions { stream_fn: Some(stream_fn), ..Default::default() });
        let stale = maho_ai::utils::abort::AbortController::new();
        let session_lookup_count = Arc::new(AtomicUsize::new(0));
        let event_count = Arc::new(AtomicUsize::new(0));
        let lookup_count = session_lookup_count.clone();
        let observed_events = event_count.clone();
        agent.subscribe(Arc::new(move |_, _| {
            observed_events.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }));
        let session_ref: SessionRef = Arc::new(move || {
            lookup_count.fetch_add(1, Ordering::SeqCst);
            None
        });
        let handlers = create_session_cursor_exec_bridge(session_ref, agent, Some(stale.signal()));
        let handler = handlers.read.expect("read handler");
        let result = futures::executor::block_on(handler(maho_ai::api::cursor_agent::r#gen::agent_pb::ReadArgs {
            tool_call_id: "stale-call".into(), path: "file".into(), offset: None, limit: None, encoding_hint: None,
        }));
        let maho_ai::api::cursor_agent::types::CursorExecHandlerResult::ToolResult(result) = result else {
            panic!("expected ordinary handler result")
        };
        assert!(result.is_error);
        assert_eq!(session_lookup_count.load(Ordering::SeqCst), 0);
        assert_eq!(event_count.load(Ordering::SeqCst), 0);
    }
}
