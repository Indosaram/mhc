//! Port of senpi packages/coding-agent/src/core/session-closed.ts.

use serde::{Deserialize, Serialize};

/// The `session-closed` custom entry type.
pub const SESSION_CLOSED_ENTRY: &str = "session-closed";

/// The engine-turn hold a draining graceful close keeps while the running turn ends.
pub const CLOSING_HOLD_KEY: &str = "closing";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClosedReturned {
    pub deliveries: Vec<String>,
    pub user_messages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClosedStopped {
    pub wake_sources: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClosedRecord {
    pub graceful: bool,
    pub turn: u64,
    pub waited_ms: u64,
    pub returned: SessionClosedReturned,
    pub stopped: SessionClosedStopped,
    pub at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_session_closed_record() {
        let record = SessionClosedRecord {
            graceful: true,
            turn: 3,
            waited_ms: 42,
            returned: SessionClosedReturned {
                deliveries: vec!["d1".into()],
                user_messages: vec!["hello".into()],
            },
            stopped: SessionClosedStopped {
                wake_sources: vec!["monitor".into()],
            },
            at: "2026-11-24T00:00:00.000Z".into(),
        };
        let encoded = serde_json::to_value(&record).expect("serialize");
        let decoded: SessionClosedRecord = serde_json::from_value(encoded).expect("deserialize");
        assert_eq!(decoded, record);
    }
}
