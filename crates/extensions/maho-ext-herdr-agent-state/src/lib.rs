#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState { Working, Blocked, Idle }
impl AgentState {
    pub const fn as_str(self) -> &'static str { match self { Self::Working => "working", Self::Blocked => "blocked", Self::Idle => "idle" } }
}

#[derive(Default)]
pub struct State {
    pub agent_active: bool,
    pub root_session: bool,
    blocked_count: usize,
    blocked_message: Option<String>,
    last: Option<(AgentState, Option<String>)>,
}
impl State {
    pub fn desired_state(&self) -> (AgentState, Option<&str>) {
        if self.blocked_count > 0 { (AgentState::Blocked, self.blocked_message.as_deref()) }
        else if self.agent_active { (AgentState::Working, None) }
        else { (AgentState::Idle, None) }
    }
    pub fn blocked(&mut self, active: bool, label: Option<&str>) {
        if !self.root_session { return; }
        if active {
            self.blocked_count = self.blocked_count.saturating_add(1);
            self.blocked_message = label.map(str::to_owned);
        } else {
            self.blocked_count = self.blocked_count.saturating_sub(1);
            if self.blocked_count == 0 { self.blocked_message = None; }
        }
    }
    pub fn publish(&mut self, force: bool) -> Option<(AgentState, Option<String>)> {
        let (state, message) = self.desired_state();
        let next = (state, message.map(str::to_owned));
        if !force && self.last.as_ref() == Some(&next) { return None; }
        self.last = Some(next.clone());
        Some(next)
    }
}

pub fn session_reference(path: Option<&str>, id: Option<&str>) -> Option<(&'static str, String)> {
    if let Some(path) = path.filter(|path| path.starts_with('/')) { return Some(("agent_session_path", path.to_owned())); }
    id.filter(|id| !id.is_empty()).map(|id| ("agent_session_id", id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn working_when_agent_active() { assert_eq!(State { agent_active: true, ..Default::default() }.desired_state(), (AgentState::Working, None)); }
    #[test] fn blocked_when_dialog_active() { let mut value = State { root_session: true, agent_active: true, ..Default::default() }; value.blocked(true, Some("Dialog")); assert_eq!(value.desired_state(), (AgentState::Blocked, Some("Dialog"))); }
    #[test] fn blocked_when_one_dialog_remains() { let mut value = State { root_session: true, ..Default::default() }; value.blocked(true, Some("a")); value.blocked(true, Some("b")); value.blocked(false, None); assert_eq!(value.desired_state(), (AgentState::Blocked, Some("b"))); }
    #[test] fn idle_when_all_dialogs_released() { let mut value = State { root_session: true, ..Default::default() }; value.blocked(true, Some("a")); value.blocked(false, None); value.blocked(false, None); assert_eq!(value.desired_state(), (AgentState::Idle, None)); }
    #[test] fn ignored_when_not_root() { let mut value = State::default(); value.blocked(true, Some("a")); assert_eq!(value.desired_state(), (AgentState::Idle, None)); }
    #[test] fn suppressed_when_unchanged() { let mut value = State::default(); value.publish(true); assert_eq!(value.publish(false), None); }
    #[test] fn path_when_absolute() { assert_eq!(session_reference(Some("/session"), Some("id")), Some(("agent_session_path", "/session".into()))); }
    #[test] fn id_when_path_relative() { assert_eq!(session_reference(Some("relative"), Some("id")), Some(("agent_session_id", "id".into()))); }
}
