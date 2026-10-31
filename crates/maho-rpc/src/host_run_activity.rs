//! Which sessions have an agent run in progress, from the host child's events
//! (senpi `rpc/host-run-activity.ts`). A run emits `agent_start` on every loop iteration but
//! `agent_settled` once, so this tracks runs, not starts. A session that closes is dropped even if its
//! settle never arrives (#2713).
use std::collections::HashSet;

#[derive(Default)]
pub struct SessionRunActivity {
    running: HashSet<String>,
}

impl SessionRunActivity {
    /// Applies one host event line; true when it is a run or session event the idle decision must re-read.
    pub fn observe(&mut self, line: &str) -> bool {
        let Some((event_type, session_id)) = parse_host_event(line) else {
            return false;
        };
        match event_type.as_deref() {
            Some("agent_start") => {
                self.running.insert(session_id);
            }
            Some("agent_settled" | "session_closed") => {
                self.running.remove(&session_id);
            }
            _ => return false,
        }
        true
    }

    pub fn busy_sessions(&self) -> usize {
        self.running.len()
    }
}

fn parse_host_event(line: &str) -> Option<(Option<String>, String)> {
    let event: serde_json::Value = serde_json::from_str(line).ok()?;
    let object = event.as_object()?;
    let session_id = object.get("sessionId")?.as_str()?.to_owned();
    let event_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some((event_type, session_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_starts_are_one_run_and_close_drops_it() {
        let mut activity = SessionRunActivity::default();
        assert!(activity.observe(r#"{"type":"agent_start","sessionId":"a"}"#));
        assert!(activity.observe(r#"{"type":"agent_start","sessionId":"a"}"#));
        assert!(activity.observe(r#"{"type":"agent_start","sessionId":"b"}"#));
        assert_eq!(activity.busy_sessions(), 2);
        assert!(activity.observe(r#"{"type":"agent_settled","sessionId":"a"}"#));
        assert!(activity.observe(r#"{"type":"session_closed","sessionId":"b"}"#));
        assert_eq!(activity.busy_sessions(), 0);
    }

    #[test]
    fn other_lines_change_nothing() {
        let mut activity = SessionRunActivity::default();
        assert!(!activity.observe("not json"));
        assert!(!activity.observe(r#"{"type":"agent_start"}"#));
        assert!(!activity.observe(r#"{"type":"message_update","sessionId":"a"}"#));
        assert_eq!(activity.busy_sessions(), 0);
    }
}
