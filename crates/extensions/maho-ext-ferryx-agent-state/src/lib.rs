use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
mod extension;
pub use extension::FerryxAgentState;

/// `wake_source_state` sources that mean the agent is still waiting on its own background work
/// (monitors, background shells, senpi tasks, DAG runs, detached eval cells).
pub const BACKGROUND_WAKE_SOURCES: [&str; 5] = [
    "terminal-monitors",
    "terminal-background-sessions",
    "senpi-task",
    "omo-dag",
    "senpi-codemode",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
}
impl AgentState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Default)]
pub struct State {
    pub agent_active: bool,
    pub root_session: bool,
    blocking_questions: Vec<(String, Option<String>)>,
    non_blocking_ids: HashSet<String>,
    active_wake_sources: BTreeSet<String>,
    last_state: Option<AgentState>,
    last_provider_session_id: Option<String>,
}
impl State {
    pub fn desired_state(&self) -> AgentState {
        if !self.blocking_questions.is_empty() {
            AgentState::Blocked
        } else if self.agent_active || self.waiting_on_background() {
            AgentState::Working
        } else {
            AgentState::Idle
        }
    }
    pub fn waiting_on_background(&self) -> bool {
        !self.active_wake_sources.is_empty()
    }
    /// Tracks background wake sources. Returns true only when waiting starts: a drain is published
    /// by the turn that delivers the completion, so it never flaps `idle` before that turn starts.
    pub fn wake_source(&mut self, data: &Value) -> bool {
        let Some(source) = data
            .get("source")
            .and_then(Value::as_str)
            .filter(|source| BACKGROUND_WAKE_SOURCES.contains(source))
        else {
            return false;
        };
        let Some(count) = data
            .get("activeCount")
            .and_then(Value::as_f64)
            .filter(|count| count.is_finite())
        else {
            return false;
        };
        let was_waiting = self.waiting_on_background();
        if count > 0.0 {
            self.active_wake_sources.insert(source.to_owned());
        } else {
            self.active_wake_sources.remove(source);
        }
        self.root_session && !was_waiting && self.waiting_on_background()
    }
    pub fn blocked_detail(&self) -> Option<&str> {
        self.blocking_questions
            .iter()
            .filter_map(|(_, label)| label.as_deref())
            .find(|label| !label.is_empty())
    }
    pub fn asked(&mut self, data: &Value) -> bool {
        if !self.root_session {
            return false;
        }
        let Some(request) = data.get("request") else {
            return false;
        };
        let Some(id) = request.get("requestId").and_then(Value::as_str) else {
            return false;
        };
        if request.get("waitForAnswer") == Some(&Value::Bool(false)) {
            self.non_blocking_ids.insert(id.to_owned());
            return false;
        }
        let label = question_label(request);
        self.set_blocking(id, label);
        true
    }
    /// `Map.prototype.set`: a re-asked id keeps its position and takes the new label.
    fn set_blocking(&mut self, id: &str, label: Option<String>) {
        if let Some((_, existing)) = self
            .blocking_questions
            .iter_mut()
            .find(|(key, _)| key == id)
        {
            *existing = label;
        } else {
            self.blocking_questions.push((id.to_owned(), label));
        }
    }
    /// `tool_approval_requested`: omp's `Allow tool: <name>` dialog waits on the user without the
    /// question tool, so it is tracked under its own `tool-approval:<callId>` key.
    pub fn tool_approval_requested(&mut self, data: &Value) -> bool {
        if !self.root_session {
            return false;
        }
        let Some(call_id) = data.get("toolCallId").and_then(Value::as_str) else {
            return false;
        };
        let label = data
            .get("toolName")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| format!("Allow tool: {name}"));
        self.set_blocking(&format!("tool-approval:{call_id}"), label);
        true
    }
    /// `tool_approval_resolved`: true only when the approval was still blocking.
    pub fn tool_approval_resolved(&mut self, data: &Value) -> bool {
        if !self.root_session {
            return false;
        }
        let Some(call_id) = data.get("toolCallId").and_then(Value::as_str) else {
            return false;
        };
        let key = format!("tool-approval:{call_id}");
        let count = self.blocking_questions.len();
        self.blocking_questions.retain(|(id, _)| *id != key);
        count != self.blocking_questions.len()
    }
    pub fn blocked(&mut self, data: &Value) -> bool {
        if !self.root_session {
            return false;
        }
        let Some(id) = data.get("id").and_then(Value::as_str) else {
            return false;
        };
        if data.get("active").is_some_and(json_truthy) {
            if self.non_blocking_ids.contains(id)
                || self.blocking_questions.iter().any(|(key, _)| key == id)
            {
                return false;
            }
            self.blocking_questions.push((
                id.to_owned(),
                data.get("label").and_then(Value::as_str).map(str::to_owned),
            ));
            return true;
        }
        let count = self.blocking_questions.len();
        self.blocking_questions.retain(|(key, _)| key != id);
        self.non_blocking_ids.remove(id);
        count != self.blocking_questions.len()
    }
    pub fn publish(&mut self, force: bool, provider_id: Option<&str>) -> Option<AgentState> {
        let next = self.desired_state();
        let rotated =
            provider_id.is_some_and(|id| self.last_provider_session_id.as_deref() != Some(id));
        if !force && !rotated && self.last_state == Some(next) {
            return None;
        }
        self.last_state = Some(next);
        if let Some(id) = provider_id {
            self.last_provider_session_id = Some(id.to_owned());
        }
        Some(next)
    }
}

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

pub fn question_label(request: &Value) -> Option<String> {
    let first = request
        .get("questions")
        .and_then(Value::as_array)
        .and_then(|questions| questions.first());
    // JS `String.prototype.trim`, not Rust `str::trim`: U+FEFF is trimmed, U+0085 is kept.
    let header = maho_ai::utils::js::trim(
        first
            .and_then(|value| value.get("header"))
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let question = maho_ai::utils::js::trim(
        first
            .and_then(|value| value.get("question"))
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
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
    #[test]
    fn working_when_agent_active() {
        let value = State {
            agent_active: true,
            ..Default::default()
        };
        assert_eq!(value.desired_state(), AgentState::Working);
    }
    #[test]
    fn blocked_when_waiting_question() {
        let mut value = State {
            root_session: true,
            agent_active: true,
            ..Default::default()
        };
        value.asked(&json!({"request":{"requestId":"a","waitForAnswer":true}}));
        assert_eq!(value.desired_state(), AgentState::Blocked);
    }
    #[test]
    fn working_when_nonblocking_question() {
        let mut value = State {
            root_session: true,
            agent_active: true,
            ..Default::default()
        };
        value.asked(&json!({"request":{"requestId":"a","waitForAnswer":false}}));
        value.blocked(&json!({"active":true,"id":"a"}));
        assert_eq!(value.desired_state(), AgentState::Working);
    }
    #[test]
    fn idle_when_question_released() {
        let mut value = State {
            root_session: true,
            ..Default::default()
        };
        value.blocked(&json!({"active":true,"id":"a"}));
        value.blocked(&json!({"active":false,"id":"a"}));
        assert_eq!(value.desired_state(), AgentState::Idle);
    }
    #[test]
    fn published_when_session_rotates() {
        let mut value = State::default();
        value.publish(true, Some("a"));
        assert_eq!(value.publish(false, Some("b")), Some(AgentState::Idle));
    }
    #[test]
    fn suppressed_when_no_change() {
        let mut value = State::default();
        value.publish(true, None);
        assert_eq!(value.publish(false, None), None);
    }
    #[test]
    fn working_while_background_work_waits() {
        let mut value = State {
            root_session: true,
            ..Default::default()
        };
        assert!(value.wake_source(&json!({"source":"senpi-task","activeCount":1})));
        assert_eq!(value.desired_state(), AgentState::Working);
        assert!(!value.wake_source(&json!({"source":"senpi-task","activeCount":0})));
        assert_eq!(value.desired_state(), AgentState::Idle);
    }
    #[test]
    fn unrelated_wake_source_ignored() {
        let mut value = State {
            root_session: true,
            ..Default::default()
        };
        assert!(!value.wake_source(&json!({"source":"memory-dream","activeCount":1})));
        assert!(!value.wake_source(&json!({"source":"ask-user","activeCount":1})));
        assert_eq!(value.desired_state(), AgentState::Idle);
    }
    #[test]
    fn idle_only_after_every_background_source_drains() {
        let mut value = State {
            root_session: true,
            ..Default::default()
        };
        value.wake_source(&json!({"source":"terminal-monitors","activeCount":2}));
        assert!(!value.wake_source(&json!({"source":"omo-dag","activeCount":1})));
        value.wake_source(&json!({"source":"terminal-monitors","activeCount":0}));
        assert_eq!(value.desired_state(), AgentState::Working);
        value.wake_source(&json!({"source":"omo-dag","activeCount":0}));
        assert_eq!(value.desired_state(), AgentState::Idle);
    }
    #[test]
    fn detail_when_first_label_present() {
        let mut value = State {
            root_session: true,
            ..Default::default()
        };
        value.blocked(&json!({"active":true,"id":"a","label":"Dialog"}));
        assert_eq!(value.blocked_detail(), Some("Dialog"));
    }
    #[test]
    fn label_when_header_and_question() {
        assert_eq!(
            question_label(&json!({"questions":[{"header":" H ","question":" Q "}]})),
            Some("H — Q".into())
        );
    }
    #[test]
    fn label_trims_like_javascript() {
        assert_eq!(
            question_label(
                &json!({"questions":[{"header":"\u{feff}","question":"\u{0085}Q\u{0085}"}]})
            ),
            Some("\u{0085}Q\u{0085}".into())
        );
    }
    #[test]
    fn blocked_while_tool_approval_pending() {
        let mut value = State {
            root_session: true,
            agent_active: true,
            ..Default::default()
        };
        assert!(value.tool_approval_requested(&json!({"toolCallId":"c1","toolName":"bash"})));
        assert_eq!(value.desired_state(), AgentState::Blocked);
        assert_eq!(value.blocked_detail(), Some("Allow tool: bash"));
        assert!(value.tool_approval_resolved(&json!({"toolCallId":"c1"})));
        assert!(!value.tool_approval_resolved(&json!({"toolCallId":"c1"})));
        assert_eq!(value.desired_state(), AgentState::Working);
    }
    #[test]
    fn tool_approval_ignored_without_call_id_or_root() {
        let mut value = State::default();
        assert!(!value.tool_approval_requested(&json!({"toolCallId":"c1","toolName":"bash"})));
        value.root_session = true;
        assert!(!value.tool_approval_requested(&json!({"toolName":"bash"})));
        assert!(value.tool_approval_requested(&json!({"toolCallId":"c2","toolName":""})));
        assert_eq!(value.blocked_detail(), None);
        assert_eq!(value.desired_state(), AgentState::Blocked);
    }
}
