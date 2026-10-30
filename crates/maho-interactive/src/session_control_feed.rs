//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-feed.ts (v2026.10.27-2).
//!
//! The cursor-based event feed a control client `subscribe`s to: state changes, assistant reports,
//! question changes and turn completions, numbered by one sequence. A client that reconnects with the
//! last `seq` it saw is replayed what the bounded ring still holds after it.
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlFeedKind {
    State,
    Report,
    Question,
    Completion,
}

impl ControlFeedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Report => "report",
            Self::Question => "question",
            Self::Completion => "completion",
        }
    }
}

/// `{type:"session_control_event", seq, kind, data}`.
pub type ControlFeedEvent = Value;
pub type FeedSend = Arc<dyn Fn(&ControlFeedEvent) + Send + Sync>;

const FEED_RING_SIZE: usize = 256;

#[derive(Default)]
pub struct ControlFeed {
    seq: u64,
    state_version: u64,
    ring: VecDeque<ControlFeedEvent>,
    subscribers: BTreeMap<u64, FeedSend>,
}

impl ControlFeed {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state_version(&self) -> u64 {
        self.state_version
    }

    pub fn publish(&mut self, kind: ControlFeedKind, data: Map<String, Value>) {
        let mut data = data;
        if kind == ControlFeedKind::State {
            self.state_version += 1;
            data.insert("state_version".into(), self.state_version.into());
        }
        self.seq += 1;
        let event = json!({
            "type": "session_control_event",
            "seq": self.seq,
            "kind": kind.as_str(),
            "data": data,
        });
        self.ring.push_back(event.clone());
        if self.ring.len() > FEED_RING_SIZE {
            self.ring.pop_front();
        }
        for send in self.subscribers.values() {
            send(&event);
        }
    }

    pub fn subscribe(&mut self, connection: u64, cursor: Option<u64>, send: FeedSend) -> u64 {
        if let Some(cursor) = cursor {
            for event in &self.ring {
                if event["seq"].as_u64().is_some_and(|seq| seq > cursor) {
                    send(event);
                }
            }
        }
        self.subscribers.insert(connection, send);
        self.seq
    }

    pub fn unsubscribe(&mut self, connection: u64) {
        self.subscribers.remove(&connection);
    }

    pub fn clear(&mut self) {
        self.subscribers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn collector() -> (FeedSend, Arc<Mutex<Vec<Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (
            Arc::new(move |event: &Value| {
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(event.clone());
            }),
            seen,
        )
    }

    #[test]
    fn state_events_carry_a_rising_state_version_and_reconnects_replay_after_the_cursor() {
        let mut feed = ControlFeed::new();
        feed.publish(ControlFeedKind::State, Map::new());
        feed.publish(ControlFeedKind::Report, Map::new());
        feed.publish(ControlFeedKind::State, Map::new());
        assert_eq!(feed.state_version(), 2);
        let (send, seen) = collector();
        assert_eq!(feed.subscribe(1, Some(1), send), 3);
        let seen = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(
            seen.iter()
                .map(|event| event["seq"].clone())
                .collect::<Vec<_>>(),
            vec![json!(2), json!(3)]
        );
        assert_eq!(seen[1]["data"]["state_version"], 2);
        assert_eq!(seen[0]["kind"], "report");
    }

    #[test]
    fn the_ring_is_bounded_and_unsubscribed_connections_stop_receiving() {
        let mut feed = ControlFeed::new();
        for _ in 0..(FEED_RING_SIZE + 4) {
            feed.publish(ControlFeedKind::Completion, Map::new());
        }
        let (send, seen) = collector();
        feed.subscribe(7, Some(0), send);
        assert_eq!(
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            FEED_RING_SIZE
        );
        feed.unsubscribe(7);
        feed.publish(ControlFeedKind::Question, Map::new());
        assert_eq!(
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            FEED_RING_SIZE
        );
    }
}
