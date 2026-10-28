//! Per-run transcript refusals. The first failure is reported once; refused messages
//! are removed before another prompt so live context agrees with restoration.
use maho_agent::types::AgentMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFailure {
    pub error: String,
}
#[derive(Default)]
pub struct TranscriptWriteFailures {
    run_failure: Option<WriteFailure>,
    reported: bool,
    refused: Vec<(AgentMessage, WriteFailure)>,
}
impl TranscriptWriteFailures {
    pub fn start_run(&mut self) {
        self.run_failure = None;
        self.reported = false;
    }
    pub fn record(&mut self, message: AgentMessage, error: String) {
        let failure = WriteFailure { error };
        if let Some(existing) = self.refused.iter_mut().find(|(m, _)| m == &message) {
            existing.1 = failure.clone();
        } else {
            self.refused.push((message, failure.clone()));
        }
        self.run_failure.get_or_insert(failure);
    }
    pub fn refusal(&self, message: &AgentMessage) -> Option<&WriteFailure> {
        self.refused
            .iter()
            .find(|(m, _)| m == message)
            .map(|(_, error)| error)
    }
    pub fn take_report(&mut self) -> Option<WriteFailure> {
        if self.reported {
            return None;
        }
        let failure = self.run_failure.clone()?;
        self.reported = true;
        Some(failure)
    }
    pub fn take_refused_out(&mut self, messages: &[AgentMessage]) -> Option<Vec<AgentMessage>> {
        if self.refused.is_empty() {
            return None;
        }
        let kept: Vec<_> = messages
            .iter()
            .filter(|message| self.refusal(message).is_none())
            .cloned()
            .collect();
        self.refused.clear();
        (kept.len() != messages.len()).then_some(kept)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn message(text: &str) -> AgentMessage {
        serde_json::from_value(serde_json::json!({"role":"user","content":text,"timestamp":1}))
            .unwrap()
    }
    #[test]
    fn reports_first_error_once_but_keeps_refusals_across_runs() {
        let mut failures = TranscriptWriteFailures::default();
        let a = message("a");
        let b = message("b");
        failures.start_run();
        failures.record(a.clone(), "first".into());
        failures.record(b.clone(), "second".into());
        assert_eq!(failures.take_report().unwrap().error, "first");
        assert!(failures.take_report().is_none());
        failures.start_run();
        assert!(failures.take_report().is_none());
        assert_eq!(failures.take_refused_out(&[a, b]), Some(vec![]));
        assert!(failures.take_refused_out(&[]).is_none());
    }
    #[test]
    fn absent_refusals_leave_context_unchanged_and_are_cleared() {
        let mut failures = TranscriptWriteFailures::default();
        failures.record(message("a"), "error".into());
        assert!(failures.take_refused_out(&[message("b")]).is_none());
        assert!(failures.refusal(&message("a")).is_none());
    }
}
