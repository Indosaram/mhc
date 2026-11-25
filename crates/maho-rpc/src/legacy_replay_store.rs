//! senpi `modes/rpc/legacy-replay-store.ts`: retained replay source for a session that owns no
//! acceptance lane (RULING-25 Q5).

use crate::jsonl::serialize_json_line;
use crate::legacy_view_cursor::{FrozenFrames, FrozenFramesCursor};
use crate::lifecycle_delivery::observe_lifecycle_delivery;
use crate::media_placeholders::{carries_inline_media, omit_inline_media};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
struct SnapshotRecord {
    line: String,
    placeholder_line: Option<String>,
    has_message: bool,
    may_carry_media: bool,
}

impl SnapshotRecord {
    fn placeholder_line(&mut self) -> String {
        if let Some(cached) = &self.placeholder_line {
            return cached.clone();
        }
        if !self.may_carry_media {
            return self.line.clone();
        }
        let Ok(source) = serde_json::from_str::<Value>(&self.line) else {
            return self.line.clone();
        };
        let redacted = omit_inline_media(&source);
        let derived = match redacted {
            std::borrow::Cow::Borrowed(_) => self.line.clone(),
            std::borrow::Cow::Owned(value) => {
                serialize_json_line(&value).unwrap_or_else(|_| self.line.clone())
            }
        };
        self.placeholder_line = Some(derived.clone());
        derived
    }
}

/// The legacy frames a cut is read in: lifecycle, the active message's records, then the open
/// questions.
pub struct FrozenAttachView {
    pub placeholders: bool,
    pub now: i64,
    pub on_release: Box<dyn FnOnce() + Send>,
}

/// Today's retained replay source, kept only for a session that owns no acceptance lane.
#[derive(Default)]
pub struct LegacyReplayStore {
    session_snapshots: HashMap<String, Vec<SnapshotRecord>>,
    lifecycle_snapshots: HashMap<String, SnapshotRecord>,
    pending_questions: HashMap<String, HashMap<String, Map<String, Value>>>,
    cursors: Arc<AtomicUsize>,
}

impl LegacyReplayStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Legacy views of either source not yet released: zero once every barrier ended.
    pub fn open_cursors(&self) -> usize {
        self.cursors.load(Ordering::SeqCst)
    }

    pub fn pending_question_count(&self, session_id: &str) -> usize {
        self.pending_questions
            .get(session_id)
            .map_or(0, HashMap::len)
    }

    pub fn remember(
        &mut self,
        session_id: &str,
        value: &Value,
        line: &str,
        placeholder_line: Option<String>,
    ) {
        let record = SnapshotRecord {
            line: line.to_owned(),
            placeholder_line,
            has_message: value.get("message").is_some_and(|m| !m.is_null()),
            may_carry_media: carries_inline_media(value),
        };
        let kind = value.get("type").and_then(Value::as_str);
        if kind == Some("session_lifecycle") {
            self.lifecycle_snapshots
                .insert(session_id.to_owned(), record);
            return;
        }
        if kind == Some("extension_ui_request")
            && value.get("method").and_then(Value::as_str) == Some("question")
            && let Some(id) = value.get("id").and_then(Value::as_str)
            && let Some(obj) = value.as_object()
        {
            self.pending_questions
                .entry(session_id.to_owned())
                .or_default()
                .insert(id.to_owned(), obj.clone());
            return;
        }
        if kind == Some("question_resolved")
            && let Some(id) = value.get("id").and_then(Value::as_str)
        {
            if let Some(pending) = self.pending_questions.get_mut(session_id) {
                pending.remove(id);
            }
            return;
        }
        if kind == Some("question_updated")
            && let Some(id) = value.get("id").and_then(Value::as_str)
        {
            if let Some(frame) = self
                .pending_questions
                .get_mut(session_id)
                .and_then(|pending| pending.get_mut(id))
            {
                if let Some(deadline) = value.get("deadlineAtMs") {
                    frame.insert("deadlineAtMs".into(), deadline.clone());
                }
                if let Some(remaining) = value.get("remainingMs") {
                    frame.insert("remainingMs".into(), remaining.clone());
                }
            }
            return;
        }
        let event_type = value
            .get("assistantMessageEvent")
            .and_then(|event| event.get("type"))
            .and_then(Value::as_str);
        if kind == Some("message_start")
            || (kind == Some("message_update") && event_type == Some("text_start"))
        {
            self.session_snapshots
                .insert(session_id.to_owned(), vec![record]);
        } else if let Some(records) = self.session_snapshots.get_mut(session_id) {
            records.push(record);
        }
        if kind == Some("message_end") {
            self.session_snapshots.remove(session_id);
        }
    }

    pub fn forget(&mut self, session_id: &str) {
        self.pending_questions.remove(session_id);
        self.session_snapshots.remove(session_id);
        self.lifecycle_snapshots.remove(session_id);
    }

    /// A late peer's baseline, frozen now: the records snapshot and the question frames at this
    /// clock.
    pub fn freeze_attach(
        &mut self,
        session_id: &str,
        view: FrozenAttachView,
    ) -> FrozenFramesCursor {
        let lifecycle = self.lifecycle_snapshots.get(session_id).cloned();
        let mut records = self
            .session_snapshots
            .get(session_id)
            .cloned()
            .unwrap_or_default();
        let length = records.len();
        let head = usize::from(lifecycle.is_some());
        let questions: Vec<String> = self
            .pending_questions
            .get(session_id)
            .map(|pending| {
                pending
                    .values()
                    .filter_map(|frame| {
                        let mut frame = frame.clone();
                        let remaining = frame
                            .get("deadlineAtMs")
                            .and_then(Value::as_i64)
                            .map_or(0, |deadline| (deadline - view.now).max(0));
                        frame.insert("remainingMs".into(), Value::from(remaining));
                        serialize_json_line(&Value::Object(frame)).ok()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let placeholders = view.placeholders;
        let record_lines: Vec<String> = records
            .iter_mut()
            .map(|record| {
                if placeholders {
                    record.placeholder_line()
                } else {
                    record.line.clone()
                }
            })
            .collect();
        let count = head + length + questions.len();
        let frames = FrozenFrames {
            count,
            frame: Box::new(move |index| {
                if let Some(lifecycle) = &lifecycle
                    && index == 0
                {
                    if let Ok(mut parsed) = serde_json::from_str::<Value>(&lifecycle.line) {
                        observe_lifecycle_delivery(&mut parsed);
                        if let Ok(line) = serialize_json_line(&parsed) {
                            return line;
                        }
                    }
                    return lifecycle.line.clone();
                }
                let at = index - head;
                if at < length {
                    return record_lines[at].clone();
                }
                questions[at - length].clone()
            }),
        };
        let release = self.track(view.on_release);
        FrozenFramesCursor::new(move || frames, release)
    }

    /// The message slot of a resync frame, read when the cut is read: the latest retained record
    /// carrying a message.
    pub fn latest_message(&self, session_id: &str) -> Value {
        let Some(records) = self.session_snapshots.get(session_id) else {
            return Value::Null;
        };
        let Some(record) = records.iter().rev().find(|record| record.has_message) else {
            return Value::Null;
        };
        serde_json::from_str::<Value>(&record.line)
            .ok()
            .and_then(|frozen| frozen.get("message").cloned())
            .unwrap_or(Value::Null)
    }

    /// Counts a legacy view of either source open until its release runs once.
    pub fn track(&self, on_release: Box<dyn FnOnce() + Send>) -> Box<dyn FnOnce() + Send> {
        self.cursors.fetch_add(1, Ordering::SeqCst);
        let cursors = Arc::clone(&self.cursors);
        Box::new(move || {
            cursors.fetch_sub(1, Ordering::SeqCst);
            on_release();
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy_view_cursor::LegacyViewCursor;
    use serde_json::json;

    #[tokio::test]
    async fn replays_lifecycle_active_message_with_placeholders_and_questions() {
        let mut store = LegacyReplayStore::new();
        let lc = json!({"type":"session_lifecycle","lifecycle":{"seq":1}});
        store.remember("s", &lc, &serialize_json_line(&lc).expect("lc"), None);
        let start = json!({"type":"message_start","message":{"role":"toolResult","toolCallId":"c1","content":[{"type":"image","data":"YQ==","mimeType":"image/png"}]}});
        store.remember(
            "s",
            &start,
            &serialize_json_line(&start).expect("start"),
            None,
        );
        let q = json!({"type":"extension_ui_request","method":"question","id":"q1","deadlineAtMs":1050});
        store.remember("s", &q, &serialize_json_line(&q).expect("q"), None);
        assert_eq!(store.pending_question_count("s"), 1);
        assert_eq!(store.latest_message("s")["toolCallId"], "c1");

        let mut cursor = store.freeze_attach(
            "s",
            FrozenAttachView {
                placeholders: true,
                now: 1000,
                on_release: Box::new(|| {}),
            },
        );
        assert_eq!(store.open_cursors(), 1);
        let f0: Value =
            serde_json::from_str(cursor.next().await.expect("f0").trim_end()).expect("json");
        assert!(f0["lifecycle"]["observedAt"].is_string());
        let f1: Value =
            serde_json::from_str(cursor.next().await.expect("f1").trim_end()).expect("json");
        assert_eq!(f1["message"]["content"][0]["type"], "image_ref");
        let f2: Value =
            serde_json::from_str(cursor.next().await.expect("f2").trim_end()).expect("json");
        assert_eq!(f2["remainingMs"], 50);
        assert_eq!(cursor.next().await, None);
        cursor.release();
        assert_eq!(store.open_cursors(), 0);
    }
}
