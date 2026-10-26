//! Human origin is an entry-point contract, never inferred from message role.
use crate::agent_session::{AgentSession, HumanInputAdmittedEvent};
use std::sync::Arc;
pub fn bind_lifecycle_human_input(
    session: &AgentSession,
    listener: impl Fn(&HumanInputAdmittedEvent) + Send + Sync + 'static,
) -> impl FnOnce() + Send + 'static {
    session.on_human_input_admitted(Arc::new(listener))
}
