//! `state/host-liveness.ts`: a host-session child's liveness, overlaid on its record (not persisted).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum HostLivenessStatus {
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "unresponsive")]
    Unresponsive { since: i64 },
    #[serde(rename = "terminal-pending")]
    TerminalPending { since: i64 },
}

impl HostLivenessStatus {
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

fn iso(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

pub fn host_liveness_text(status: HostLivenessStatus) -> String {
    match status {
        HostLivenessStatus::Running => {
            "running: the original session is responding again".to_string()
        }
        HostLivenessStatus::Unresponsive { since } => format!(
            "unresponsive since {}, retrying; the session may still be running",
            iso(since)
        ),
        HostLivenessStatus::TerminalPending { since } => format!(
            "unresponsive since {}, close pending; retrying until the host confirms the session ended",
            iso(since)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_text_and_wire_shape() {
        assert_eq!(
            host_liveness_text(HostLivenessStatus::Unresponsive { since: 0 }),
            "unresponsive since 1970-01-01T00:00:00.000Z, retrying; the session may still be running"
        );
        assert!(
            host_liveness_text(HostLivenessStatus::TerminalPending { since: 0 })
                .contains("close pending")
        );
        assert_eq!(
            serde_json::to_value(HostLivenessStatus::TerminalPending { since: 5 }).expect("json"),
            serde_json::json!({"state": "terminal-pending", "since": 5})
        );
    }
}
