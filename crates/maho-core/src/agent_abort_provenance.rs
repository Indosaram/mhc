//! Port of senpi packages/coding-agent/src/core/agent-abort-provenance.ts.

use maho_agent::types::AgentMessage;
use maho_ext_api::AbortSource;

/// The agent_end event this module owns the abort provenance for. senpi declares AgentEndEvent in
/// extensions/types.ts; maho-ext-api models it as an enum variant, so the mutable event object is
/// declared here.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentEndEvent {
    pub messages: Vec<AgentMessage>,
    pub will_retry: bool,
    pub aborted: bool,
    pub abort_source: Option<AbortSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinedAbort {
    pub abort_current_agent: bool,
    pub user_owned: bool,
}

#[derive(Debug, Default)]
pub struct AgentAbortProvenance {
    source: Option<AbortSource>,
    agent_end_event: Option<AgentEndEvent>,
    settling_agent_end_event: Option<AgentEndEvent>,
    agent_end_boundary_open: bool,
    late_user_join: bool,
    late_user_join_delivered: bool,
}

impl AgentAbortProvenance {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_open_agent_end_boundary(&self) -> bool {
        self.agent_end_boundary_open || self.agent_end_event.is_some()
    }

    pub fn current_source(&self) -> Option<AbortSource> {
        self.source
            .or_else(|| self.agent_end_event.as_ref().and_then(|event| event.abort_source))
            .or_else(|| self.settling_agent_end_event.as_ref().and_then(|event| event.abort_source))
    }

    pub fn begin(&mut self, source: AbortSource) -> bool {
        self.source = Some(source);
        self.settling_agent_end_event = None;
        self.agent_end_boundary_open = false;
        self.late_user_join = false;
        self.late_user_join_delivered = false;
        source == AbortSource::User
    }

    pub fn join(&mut self, source: AbortSource, is_streaming: bool) -> JoinedAbort {
        if source == AbortSource::User && (self.agent_end_event.is_some() || self.agent_end_boundary_open) {
            self.source = Some(AbortSource::User);
            if !self.late_user_join_delivered {
                self.late_user_join = true;
            }
            if let Some(event) = self.agent_end_event.as_mut().or(self.settling_agent_end_event.as_mut()) {
                event.aborted = true;
                event.abort_source = Some(AbortSource::User);
            }
            return JoinedAbort { abort_current_agent: false, user_owned: true };
        }
        if source == AbortSource::User && self.source.is_some() {
            self.source = Some(AbortSource::User);
            return JoinedAbort { abort_current_agent: false, user_owned: true };
        }
        if self.source.is_none() {
            if !is_streaming {
                return JoinedAbort { abort_current_agent: false, user_owned: false };
            }
            self.source = Some(source);
            return JoinedAbort { abort_current_agent: true, user_owned: source == AbortSource::User };
        }
        JoinedAbort { abort_current_agent: false, user_owned: false }
    }

    fn find_provider_abort_source(&self, messages: &[AgentMessage]) -> Option<AbortSource> {
        match messages.last() {
            Some(AgentMessage::Llm(maho_ai::types::Message::Assistant(message)))
                if message.abort_source == Some(maho_ai::types::AbortSource::Provider) =>
            {
                Some(AbortSource::Provider)
            }
            _ => None,
        }
    }

    pub fn begin_agent_end(&mut self, messages: Vec<AgentMessage>, will_retry: bool, aborted_without_source: bool) -> AgentEndEvent {
        let aborted = self.source.is_some() || aborted_without_source;
        let abort_source = self.source.or_else(|| self.find_provider_abort_source(&messages));
        let event = AgentEndEvent { messages, will_retry, aborted, abort_source };
        self.agent_end_event = Some(event.clone());
        self.settling_agent_end_event = None;
        self.agent_end_boundary_open = false;
        self.late_user_join = false;
        self.late_user_join_delivered = false;
        event
    }

    pub fn end_agent_end(&mut self, event: &AgentEndEvent) {
        if self.agent_end_event.as_ref() == Some(event) {
            self.agent_end_event = None;
            self.settling_agent_end_event = Some(event.clone());
            self.agent_end_boundary_open = true;
        }
        self.source = None;
    }

    pub fn take_late_user_join(&mut self) -> bool {
        let late_user_join = self.late_user_join;
        self.late_user_join = false;
        if late_user_join {
            self.late_user_join_delivered = true;
        }
        late_user_join
    }

    pub fn close_agent_end_boundary(&mut self) {
        self.agent_end_boundary_open = false;
        self.settling_agent_end_event = None;
    }

    pub fn join_open_boundary(&mut self, source: AbortSource) -> Option<JoinedAbort> {
        if !self.agent_end_boundary_open && self.agent_end_event.is_none() {
            return None;
        }
        Some(self.join(source, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beginning_a_user_abort_is_user_owned() {
        let mut provenance = AgentAbortProvenance::new();
        assert!(provenance.begin(AbortSource::User));
        assert_eq!(provenance.current_source(), Some(AbortSource::User));
        assert!(!provenance.begin(AbortSource::System));
    }

    #[test]
    fn joining_a_system_abort_while_streaming_aborts_the_agent() {
        let mut provenance = AgentAbortProvenance::new();
        let joined = provenance.join(AbortSource::System, true);
        assert!(joined.abort_current_agent);
        assert!(!joined.user_owned);
    }

    #[test]
    fn joining_while_idle_does_nothing() {
        let mut provenance = AgentAbortProvenance::new();
        let joined = provenance.join(AbortSource::User, false);
        assert!(!joined.abort_current_agent);
    }

    #[test]
    fn a_late_user_join_marks_the_settling_event() {
        let mut provenance = AgentAbortProvenance::new();
        let event = provenance.begin_agent_end(Vec::new(), false, false);
        provenance.end_agent_end(&event);
        let joined = provenance.join(AbortSource::User, false);
        assert!(joined.user_owned);
        assert!(provenance.take_late_user_join());
        assert!(!provenance.take_late_user_join());
    }

    #[test]
    fn a_provider_abort_source_is_read_from_the_last_assistant_message() {
        let mut provenance = AgentAbortProvenance::new();
        let message: AgentMessage = serde_json::from_value(serde_json::json!({
            "role": "assistant", "content": [], "api": "faux", "provider": "faux", "model": "faux-1",
            "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
            "stopReason": "aborted", "timestamp": 0, "abortSource": "provider"
        }))
        .expect("assistant");
        let event = provenance.begin_agent_end(vec![message], false, false);
        assert_eq!(event.abort_source, Some(AbortSource::Provider));
    }
}
