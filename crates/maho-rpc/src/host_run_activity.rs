//! Which sessions have an agent run in progress, from the host child's events
//! (senpi `rpc/host-run-activity.ts`). A run emits `agent_start` on every loop iteration but
//! `agent_settled` once, so this tracks runs, not starts. A session that closes is dropped even if its
//! settle never arrives (#2713). A settled run holds idle exit until `agent_idle`, because its
//! extension `agent_settled` handlers may still be running, but only for a bounded hold (omo#11419).
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

pub type SettleHoldExpiredCallback = Box<dyn Fn(&str, u64) + Send + Sync>;

pub struct SessionRunActivityOptions {
    pub settle_hold_ms: Option<u64>,
    pub now: Option<Box<dyn Fn() -> u64 + Send + Sync>>,
    pub on_settle_hold_expired: Option<SettleHoldExpiredCallback>,
}

pub struct SessionRunActivity {
    running: HashSet<String>,
    settling: HashMap<String, u64>,
    settle_hold_ms: u64,
    now: Box<dyn Fn() -> u64 + Send + Sync>,
    on_settle_hold_expired: Option<SettleHoldExpiredCallback>,
}

fn default_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Default for SessionRunActivity {
    fn default() -> Self {
        Self::new(SessionRunActivityOptions {
            settle_hold_ms: None,
            now: None,
            on_settle_hold_expired: None,
        })
    }
}

impl SessionRunActivity {
    pub fn new(options: SessionRunActivityOptions) -> Self {
        Self {
            running: HashSet::new(),
            settling: HashMap::new(),
            settle_hold_ms: options.settle_hold_ms.unwrap_or(u64::MAX),
            now: options.now.unwrap_or_else(|| Box::new(default_now_ms)),
            on_settle_hold_expired: options.on_settle_hold_expired,
        }
    }

    /// Applies one host event line; true when it is a run or session event the idle decision must re-read.
    pub fn observe(&mut self, line: &str) -> bool {
        let Some((event_type, session_id, reason)) = parse_host_event(line) else {
            return false;
        };
        match event_type.as_deref() {
            Some("agent_start") => {
                self.running.insert(session_id.clone());
                self.settling.remove(&session_id);
            }
            Some("agent_settled") if reason.as_deref() != Some("session_closed") => {
                self.running.remove(&session_id);
                self.settling.insert(session_id, (self.now)());
            }
            Some("agent_settled" | "agent_idle" | "session_closed") => {
                self.running.remove(&session_id);
                self.settling.remove(&session_id);
            }
            _ => return false,
        }
        true
    }

    pub fn busy_sessions(&self) -> usize {
        self.running.len()
    }

    pub fn settling_sessions(&mut self) -> usize {
        let now = (self.now)();
        let mut expired = Vec::new();
        for (session_id, settled_at) in &self.settling {
            let held_ms = now.saturating_sub(*settled_at);
            if held_ms >= self.settle_hold_ms {
                expired.push((session_id.clone(), held_ms));
            }
        }
        for (session_id, held_ms) in expired {
            self.settling.remove(&session_id);
            if let Some(cb) = &self.on_settle_hold_expired {
                cb(&session_id, held_ms);
            }
        }
        self.settling.len()
    }
}

/// An idle window that ran out still waits while a settled session's handlers are within their hold (omo#11419).
pub fn hold_idle_exit_for_settling<'a>(
    decision: &'a str,
    runs: &mut SessionRunActivity,
) -> &'a str {
    if decision == "exit" && runs.settling_sessions() > 0 {
        "idle"
    } else {
        decision
    }
}

fn parse_host_event(line: &str) -> Option<(Option<String>, String, Option<String>)> {
    let event: serde_json::Value = serde_json::from_str(line).ok()?;
    let object = event.as_object()?;
    let session_id = object.get("sessionId")?.as_str()?.to_owned();
    let event_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let reason = object
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some((event_type, session_id, reason))
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
