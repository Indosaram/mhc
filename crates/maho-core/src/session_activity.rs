//! Port of senpi packages/coding-agent/src/core/session-activity.ts.
//!
//! The session activity contract every occupancy decision shares. Busy means work the session OWNS
//! that must outlive an idle-eviction decision - not merely a streaming turn.

use std::collections::BTreeMap;

use serde_json::Value;

/// Live counts of extension-published wake sources, keyed by source name.
#[derive(Debug, Clone, Default)]
pub struct WakeSourceTracker {
    counts: BTreeMap<String, (f64, bool)>,
}

impl WakeSourceTracker {
    /// Folds one wake_source_state payload in. Unknown shapes are ignored: a malformed publisher
    /// must never flip a session to idle by accident.
    pub fn observe(&mut self, data: &Value) {
        let Some(object) = data.as_object() else {
            return;
        };
        let Some(source) = object
            .get("source")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        let Some(active_count) = object
            .get("activeCount")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
        else {
            return;
        };
        if active_count > 0.0 {
            self.counts.insert(
                source.to_owned(),
                (
                    active_count,
                    object.get("kind").and_then(Value::as_str) == Some("children"),
                ),
            );
        } else {
            self.counts.remove(source);
        }
    }

    pub fn active_counts(&self) -> ActiveCounts {
        let mut counts = ActiveCounts::default();
        for &(count, children) in self.counts.values() {
            if children {
                counts.children += count;
            } else {
                counts.background += count;
            }
        }
        counts
    }

    pub fn has_active(&self) -> bool {
        !self.counts.is_empty()
    }

    pub fn active_sources(&self) -> Vec<&str> {
        self.counts.keys().map(String::as_str).collect()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ActiveCounts {
    pub children: f64,
    pub background: f64,
}

/// In-session activity signals. Every field means work that must not be evicted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionActivitySnapshot {
    pub is_streaming: bool,
    pub is_bash_running: bool,
    pub is_compacting: bool,
    pub has_session_work: bool,
    pub has_active_wake_source: bool,
}

/// The complete session-owned activity predicate.
pub fn is_session_busy_snapshot(snapshot: SessionActivitySnapshot) -> bool {
    snapshot.is_streaming
        || snapshot.is_bash_running
        || snapshot.is_compacting
        || snapshot.has_session_work
        || snapshot.has_active_wake_source
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_published_source_marks_the_session_busy() {
        let mut tracker = WakeSourceTracker::default();
        assert!(!tracker.has_active());
        tracker.observe(&json!({ "source": "terminal", "activeCount": 2 }));
        assert!(tracker.has_active());
        assert_eq!(tracker.active_sources(), vec!["terminal"]);
    }

    #[test]
    fn a_zero_count_clears_the_source() {
        let mut tracker = WakeSourceTracker::default();
        tracker.observe(&json!({ "source": "terminal", "activeCount": 1 }));
        tracker.observe(&json!({ "source": "terminal", "activeCount": 0 }));
        assert!(!tracker.has_active());
    }

    #[test]
    fn malformed_payloads_are_ignored() {
        let mut tracker = WakeSourceTracker::default();
        tracker.observe(&json!(null));
        tracker.observe(&json!("nope"));
        tracker.observe(&json!({ "source": "", "activeCount": 1 }));
        tracker.observe(&json!({ "source": "x" }));
        tracker.observe(&json!({ "source": "x", "activeCount": "1" }));
        assert!(!tracker.has_active());
    }

    #[test]
    fn counts_preserve_kinds_and_fractional_publishers() {
        let mut tracker = WakeSourceTracker::default();
        tracker.observe(&json!({"source":"child", "activeCount":2, "kind":"children"}));
        tracker.observe(&json!({"source":"monitor", "activeCount":1.5}));
        assert_eq!(
            tracker.active_counts(),
            ActiveCounts {
                children: 2.0,
                background: 1.5
            }
        );
        tracker.observe(&json!({"source":"child", "activeCount":3, "kind":"unknown"}));
        assert_eq!(
            tracker.active_counts(),
            ActiveCounts {
                children: 0.0,
                background: 4.5
            }
        );
        tracker.observe(&json!({"source":"monitor", "activeCount":0}));
        assert_eq!(tracker.active_counts().background, 3.0);
    }

    #[test]
    fn any_single_field_makes_the_snapshot_busy() {
        assert!(!is_session_busy_snapshot(SessionActivitySnapshot::default()));
        for snapshot in [
            SessionActivitySnapshot {
                is_streaming: true,
                ..Default::default()
            },
            SessionActivitySnapshot {
                is_bash_running: true,
                ..Default::default()
            },
            SessionActivitySnapshot {
                is_compacting: true,
                ..Default::default()
            },
            SessionActivitySnapshot {
                has_session_work: true,
                ..Default::default()
            },
            SessionActivitySnapshot {
                has_active_wake_source: true,
                ..Default::default()
            },
        ] {
            assert!(is_session_busy_snapshot(snapshot));
        }
    }
}
