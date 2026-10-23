//! Port of senpi packages/coding-agent/src/core/engine-turn-hold-notices.ts.
//!
//! Durable notice custody, retaining only the latest model payload for each source. Session entries are
//! untyped JSON, matching session_manager.

use serde_json::{Map, Value, json};

pub type AppendEntry = Box<dyn Fn(&str, Value) -> Result<(), String> + Send + Sync>;
pub type WarnFn = Box<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone, Default)]
struct HeldWake {
    notice_ids: Vec<String>,
    message: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeldMessage {
    pub source: String,
    pub notice_ids: Vec<String>,
    pub message: Value,
}

pub struct EngineTurnHoldNotices {
    /// Insertion-ordered like a JS object.
    wakes: Vec<(String, HeldWake)>,
    append: AppendEntry,
    warn: WarnFn,
}

impl EngineTurnHoldNotices {
    pub fn new(entries: &[Value], append: AppendEntry, warn: WarnFn) -> Self {
        let mut this = Self {
            wakes: Vec::new(),
            append,
            warn,
        };
        for entry in entries {
            if entry.get("type").and_then(Value::as_str) != Some("custom") {
                continue;
            }
            let data = entry.get("data").cloned().unwrap_or(Value::Null);
            match entry.get("customType").and_then(Value::as_str) {
                Some("session-notice") => this.remember(&data),
                Some("session-notices-consumed") => {
                    let ids: Vec<&str> = data
                        .get("noticeIds")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    for (_, wake) in this.wakes.iter_mut() {
                        wake.notice_ids.retain(|id| !ids.contains(&id.as_str()));
                    }
                    this.wakes.retain(|(_, wake)| !wake.notice_ids.is_empty());
                }
                _ => {}
            }
        }
        this
    }

    pub fn sources(&self) -> Vec<(String, usize)> {
        self.wakes
            .iter()
            .map(|(s, w)| (s.clone(), w.notice_ids.len()))
            .collect()
    }

    pub fn notice_count(&self) -> usize {
        self.wakes.iter().map(|(_, w)| w.notice_ids.len()).sum()
    }

    pub fn notice_text(&self) -> String {
        self.notice_text_for(self.notice_count(), None)
    }

    pub fn notice_text_for(&self, count: usize, source: Option<&str>) -> String {
        let mut sources: Vec<&str> = self.wakes.iter().map(|(s, _)| s.as_str()).collect();
        if let Some(source) = source
            && !sources.contains(&source)
        {
            sources.push(source);
        }
        let labels: Vec<String> = sources
            .iter()
            .map(|key| {
                if matches!(*key, "goal" | "monitor" | "codemode") {
                    (*key).to_string()
                } else {
                    format!("other ({key})")
                }
            })
            .collect();
        let joined = if labels.is_empty() {
            "other".to_string()
        } else {
            labels.join(", ")
        };
        format!(
            "Paused while no one was here ({joined}): {count} automatic wakes held. Resume or send a message to continue."
        )
    }

    pub fn note(&mut self, source: &str, pause_seq: u64, message: Option<Value>) {
        let count = self.notice_count() + 1;
        let mut data = Map::new();
        data.insert("noticeId".into(), json!(uuid::Uuid::new_v4().to_string()));
        data.insert("producer".into(), json!(source));
        data.insert("pauseSeq".into(), json!(pause_seq));
        data.insert(
            "at".into(),
            json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
        data.insert("heldCount".into(), json!(count));
        data.insert(
            "text".into(),
            json!(self.notice_text_for(count, Some(source))),
        );
        data.insert("action".into(), json!("resume_session"));
        if source != "goal"
            && let Some(message) = message
        {
            data.insert("message".into(), message);
        }
        let data = Value::Object(data);
        if let Err(error) = (self.append)("session-notice", data.clone()) {
            (self.warn)(&error);
        }
        self.remember(&data);
    }

    pub fn messages(&self) -> Vec<HeldMessage> {
        let mut out = Vec::new();
        for (source, wake) in &self.wakes {
            let Some(message) = wake.message.as_ref() else {
                continue;
            };
            if source == "goal" {
                continue;
            }
            let notice_ids = wake.notice_ids.clone();
            if source != "monitor" {
                out.push(HeldMessage {
                    source: source.clone(),
                    notice_ids,
                    message: message.clone(),
                });
                continue;
            }
            let prefix = format!(
                "{} automatic monitor wakes held. Latest event:\n",
                wake.notice_ids.len()
            );
            let mut message = message.clone();
            if let Some(object) = message.as_object_mut() {
                let content = match object.get("content") {
                    Some(Value::String(text)) => Value::String(format!("{prefix}{text}")),
                    Some(Value::Array(blocks)) => {
                        let mut next = vec![json!({"type": "text", "text": prefix})];
                        next.extend(blocks.iter().cloned());
                        Value::Array(next)
                    }
                    _ => Value::Array(vec![json!({"type": "text", "text": prefix})]),
                };
                object.insert("content".into(), content);
            }
            out.push(HeldMessage {
                source: source.clone(),
                notice_ids,
                message,
            });
        }
        out
    }

    /// The single held -> released transition. Append before removal; failure retains custody.
    pub fn consume(
        &mut self,
        source: &str,
        pause_seq: u64,
        delivered_ids: Option<&[String]>,
    ) -> bool {
        let Some(index) = self.wakes.iter().position(|(s, _)| s == source) else {
            return false;
        };
        let wake = &self.wakes[index].1;
        let notice_ids: Vec<String> = wake
            .notice_ids
            .iter()
            .filter(|id| delivered_ids.is_none_or(|d| d.contains(id)))
            .cloned()
            .collect();
        if notice_ids.is_empty() {
            return false;
        }
        if let Err(error) = (self.append)(
            "session-notices-consumed",
            json!({"noticeIds": notice_ids, "pauseSeq": pause_seq}),
        ) {
            (self.warn)(&error);
            return false;
        }
        let wake = &mut self.wakes[index].1;
        wake.notice_ids.retain(|id| !notice_ids.contains(id));
        if wake.notice_ids.is_empty() {
            self.wakes.remove(index);
        }
        true
    }

    pub fn consume_notices(&mut self, pause_seq: u64) {
        let sources: Vec<String> = self
            .wakes
            .iter()
            .filter(|(_, w)| w.message.is_none())
            .map(|(s, _)| s.clone())
            .collect();
        for source in sources {
            self.consume(&source, pause_seq, None);
        }
    }

    fn remember(&mut self, notice: &Value) {
        let Some(producer) = notice.get("producer").and_then(Value::as_str) else {
            return;
        };
        let Some(id) = notice.get("noticeId").and_then(Value::as_str) else {
            return;
        };
        let index = match self.wakes.iter().position(|(s, _)| s == producer) {
            Some(index) => index,
            None => {
                self.wakes.push((producer.to_string(), HeldWake::default()));
                self.wakes.len() - 1
            }
        };
        let wake = &mut self.wakes[index].1;
        wake.notice_ids.push(id.to_string());
        if let Some(message) = notice.get("message").filter(|m| !m.is_null()) {
            wake.message = Some(message.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Recorded = Arc<Mutex<Vec<(String, Value)>>>;

    fn recorder() -> (Recorded, AppendEntry) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let append: AppendEntry = Box::new(move |t, d| {
            if let Ok(mut l) = sink.lock() {
                l.push((t.to_string(), d));
            }
            Ok(())
        });
        (log, append)
    }

    #[test]
    fn notes_text_and_goal_has_no_message() {
        let (log, append) = recorder();
        let mut n = EngineTurnHoldNotices::new(&[], append, Box::new(|_| {}));
        n.note("goal", 1, Some(json!({"content": "x"})));
        n.note("monitor", 1, Some(json!({"content": "ev"})));
        assert_eq!(n.notice_count(), 2);
        assert_eq!(
            n.notice_text(),
            "Paused while no one was here (goal, monitor): 2 automatic wakes held. Resume or send a message to continue."
        );
        let messages = n.messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].message["content"],
            "1 automatic monitor wakes held. Latest event:\nev"
        );
        let log = log.lock().map(|l| l.clone()).unwrap_or_default();
        assert!(log[0].1.get("message").is_none());
    }

    #[test]
    fn replay_and_selective_consume() {
        let (log, append) = recorder();
        let mut n = EngineTurnHoldNotices::new(&[], append, Box::new(|_| {}));
        n.note("codemode", 1, Some(json!({"content": "a"})));
        n.note("codemode", 1, Some(json!({"content": "b"})));
        let entries: Vec<Value> = log
            .lock()
            .map(|l| {
                l.iter()
                    .map(|(t, d)| json!({"type": "custom", "customType": t, "data": d}))
                    .collect()
            })
            .unwrap_or_default();
        let (_, append2) = recorder();
        let mut replay = EngineTurnHoldNotices::new(&entries, append2, Box::new(|_| {}));
        assert_eq!(replay.sources(), vec![("codemode".to_string(), 2)]);
        let first = replay.messages()[0].notice_ids[0].clone();
        assert!(replay.consume("codemode", 1, Some(&[first])));
        assert_eq!(replay.notice_count(), 1);
        assert!(replay.consume("codemode", 1, None));
        assert!(replay.messages().is_empty());
    }
}
