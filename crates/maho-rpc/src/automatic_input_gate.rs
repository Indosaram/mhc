use maho_core::agent_session::AgentSession;
pub fn hold_automatic_input(session: &AgentSession, message: &str, interactive_user: bool) -> bool {
    !interactive_user && session.note_automatic_input(message)
}
