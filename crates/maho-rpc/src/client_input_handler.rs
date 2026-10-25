//! Client input entry point shares the connection's actual admission path.
use maho_core::agent_session::AgentSession;
use serde_json::Value;
pub async fn handle_client_input(
    session: &AgentSession,
    command: &Value,
    sink: Option<crate::connection_handler::RpcRecordSink>,
) -> Result<Option<String>, serde_json::Error> {
    crate::connection_handler::handle_input_line_with_sink(
        session,
        &serde_json::to_string(command)?,
        sink,
    )
    .await
}
