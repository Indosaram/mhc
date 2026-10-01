use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState { Idle, Working, Blocked }
impl AgentState {
    pub const fn as_str(self) -> &'static str { match self { Self::Idle => "idle", Self::Working => "working", Self::Blocked => "blocked" } }
}

#[derive(Default)]
pub struct State {
    pub agent_active: bool,
    pub root_session: bool,
    blocking_questions: Vec<(String, Option<String>)>,
    non_blocking_ids: HashSet<String>,
    last_state: Option<AgentState>,
    last_provider_session_id: Option<String>,
}
impl State {
    pub fn desired_state(&self) -> AgentState {
        if !self.blocking_questions.is_empty() { AgentState::Blocked }
        else if self.agent_active { AgentState::Working }
        else { AgentState::Idle }
    }
    pub fn blocked_detail(&self) -> Option<&str> { self.blocking_questions.iter().filter_map(|(_, label)| label.as_deref()).find(|label| !label.is_empty()) }
    pub fn asked(&mut self, data: &Value) -> bool {
        if !self.root_session { return false; }
        let Some(request) = data.get("request") else { return false; };
        let Some(id) = request.get("requestId").and_then(Value::as_str) else { return false; };
        if request.get("waitForAnswer") == Some(&Value::Bool(false)) { self.non_blocking_ids.insert(id.to_owned()); return false; }
        let label = question_label(request);
        if let Some((_, existing)) = self.blocking_questions.iter_mut().find(|(key, _)| key == id) { *existing = label; }
        else { self.blocking_questions.push((id.to_owned(), label)); }
        true
    }
    pub fn blocked(&mut self, data: &Value) -> bool {
        if !self.root_session { return false; }
        let Some(id) = data.get("id").and_then(Value::as_str) else { return false; };
        if data.get("active").is_some_and(json_truthy) {
            if self.non_blocking_ids.contains(id) || self.blocking_questions.iter().any(|(key, _)| key == id) { return false; }
            self.blocking_questions.push((id.to_owned(), data.get("label").and_then(Value::as_str).map(str::to_owned)));
            return true;
        }
        let count = self.blocking_questions.len();
        self.blocking_questions.retain(|(key, _)| key != id);
        self.non_blocking_ids.remove(id);
        count != self.blocking_questions.len()
    }
    pub fn publish(&mut self, force: bool, provider_id: Option<&str>) -> Option<AgentState> {
        let next = self.desired_state();
        let rotated = provider_id.is_some_and(|id| self.last_provider_session_id.as_deref() != Some(id));
        if !force && !rotated && self.last_state == Some(next) { return None; }
        self.last_state = Some(next);
        if let Some(id) = provider_id { self.last_provider_session_id = Some(id.to_owned()); }
        Some(next)
    }
}

fn json_truthy(value: &Value) -> bool {
    match value { Value::Null => false, Value::Bool(value) => *value, Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0), Value::String(value) => !value.is_empty(), Value::Array(_) | Value::Object(_) => true }
}

pub fn question_label(request: &Value) -> Option<String> {
    let first = request.get("questions").and_then(Value::as_array).and_then(|questions| questions.first());
    let header = first.and_then(|value| value.get("header")).and_then(Value::as_str).unwrap_or("").trim();
    let question = first.and_then(|value| value.get("question")).and_then(Value::as_str).unwrap_or("").trim();
    match (header.is_empty(), question.is_empty()) {
        (false, false) => Some(format!("{header} — {question}")),
        (true, false) => Some(question.to_owned()),
        (false, true) => Some(header.to_owned()),
        (true, true) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn working_when_agent_active() { let value = State { agent_active: true, ..Default::default() }; assert_eq!(value.desired_state(), AgentState::Working); }
    #[test] fn blocked_when_waiting_question() { let mut value = State { root_session: true, agent_active: true, ..Default::default() }; value.asked(&json!({"request":{"requestId":"a","waitForAnswer":true}})); assert_eq!(value.desired_state(), AgentState::Blocked); }
    #[test] fn working_when_nonblocking_question() { let mut value = State { root_session: true, agent_active: true, ..Default::default() }; value.asked(&json!({"request":{"requestId":"a","waitForAnswer":false}})); value.blocked(&json!({"active":true,"id":"a"})); assert_eq!(value.desired_state(), AgentState::Working); }
    #[test] fn idle_when_question_released() { let mut value = State { root_session: true, ..Default::default() }; value.blocked(&json!({"active":true,"id":"a"})); value.blocked(&json!({"active":false,"id":"a"})); assert_eq!(value.desired_state(), AgentState::Idle); }
    #[test] fn published_when_session_rotates() { let mut value = State::default(); value.publish(true, Some("a")); assert_eq!(value.publish(false, Some("b")), Some(AgentState::Idle)); }
    #[test] fn suppressed_when_no_change() { let mut value = State::default(); value.publish(true, None); assert_eq!(value.publish(false, None), None); }
    #[test] fn detail_when_first_label_present() { let mut value = State { root_session: true, ..Default::default() }; value.blocked(&json!({"active":true,"id":"a","label":"Dialog"})); assert_eq!(value.blocked_detail(), Some("Dialog")); }
    #[test] fn label_when_header_and_question() { assert_eq!(question_label(&json!({"questions":[{"header":" H ","question":" Q "}]})), Some("H — Q".into())); }
}
