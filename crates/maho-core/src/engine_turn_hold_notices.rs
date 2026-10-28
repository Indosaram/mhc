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
    items: Vec<(String, Value)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeldMessage {
    pub source: String,
    pub notice_ids: Vec<String>,
    pub message: Value,
}

#[derive(Debug, Clone)]
pub struct HeldItem {
    pub source: String,
    pub message: Value,
    pub item_id: String,
    pub run_ref: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldItemReceipt {
    pub item_id: String,
    pub notice_id: String,
    pub entry_id: String,
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
                Some("session-notice") => {
                    if let Some(notices) = data.get("notices").and_then(Value::as_array) {
                        for notice in notices {
                            this.remember(notice);
                        }
                    } else {
                        this.remember(&data);
                    }
                }
                Some("session-notices-consumed") => {
                    let ids: Vec<&str> = data
                        .get("noticeIds")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    for (_, wake) in this.wakes.iter_mut() {
                        wake.notice_ids.retain(|id| !ids.contains(&id.as_str()));
                        wake.items.retain(|(id, _)| !ids.contains(&id.as_str()));
                        if !wake
                            .notice_ids
                            .iter()
                            .any(|id| !wake.items.iter().any(|(item, _)| item == id))
                        {
                            wake.message = None;
                        }
                    }
                    this.wakes.retain(|(_, wake)| !wake.notice_ids.is_empty());
                }
                _ => {}
            }
        }
        this
    }

    pub fn notice_ids(&self, source: &str) -> Vec<String> {
        self.wakes
            .iter()
            .find(|(s, _)| s == source)
            .map(|(_, wake)| wake.notice_ids.clone())
            .unwrap_or_default()
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

    pub fn stopped_notice_text(count: usize) -> String {
        format!(
            "Paused: {count} {} arrived while paused. Send a message or /resume to continue.",
            if count == 1 { "update" } else { "updates" }
        )
    }

    pub fn note(&mut self, source: &str, pause_seq: u64, message: Option<Value>) -> String {
        self.note_with_stop(source, pause_seq, message, false)
    }

    pub fn note_with_stop(
        &mut self,
        source: &str,
        pause_seq: u64,
        message: Option<Value>,
        stopped: bool,
    ) -> String {
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
            json!(if stopped {
                Self::stopped_notice_text(count)
            } else {
                self.notice_text_for(count, Some(source))
            }),
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
        let id = data["noticeId"].as_str().unwrap_or_default().to_owned();
        self.remember(&data);
        id
    }

    pub fn note_items(
        &mut self,
        pause_seq: u64,
        items: &[HeldItem],
        stopped: bool,
        append: impl FnOnce(Value) -> Result<String, String>,
    ) -> Result<Vec<HeldItemReceipt>, String> {
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let notices: Vec<Value> = items.iter().enumerate().map(|(index, item)| {
            let count = self.notice_count() + index + 1;
            let mut notice = json!({"noticeId": uuid::Uuid::new_v4().to_string(), "producer": item.source, "pauseSeq": pause_seq, "at": at, "heldCount": count, "text": if stopped { Self::stopped_notice_text(count) } else { self.notice_text_for(count, Some(&item.source)) }, "action": "resume_session", "message": item.message, "itemId": item.item_id});
            if let Some(run_ref) = &item.run_ref { notice["runRef"] = run_ref.clone(); }
            notice
        }).collect();
        let entry_id = append(json!({"notices": notices}))?;
        let receipts = items
            .iter()
            .zip(&notices)
            .map(|(item, notice)| HeldItemReceipt {
                item_id: item.item_id.clone(),
                notice_id: notice["noticeId"].as_str().unwrap_or_default().to_owned(),
                entry_id: entry_id.clone(),
            })
            .collect();
        for notice in &notices {
            self.remember(notice);
        }
        Ok(receipts)
    }

    pub fn messages(&self) -> Vec<HeldMessage> {
        let mut out = Vec::new();
        for (source, wake) in &self.wakes {
            let notice_ids: Vec<String> = wake
                .notice_ids
                .iter()
                .filter(|id| !wake.items.iter().any(|(item, _)| item == *id))
                .cloned()
                .collect();
            if source != "goal" && !notice_ids.is_empty()
                && let Some(message) = &wake.message {
                    let mut message = message.clone();
                    if source == "monitor" {
                        let prefix = format!(
                            "{} automatic monitor wakes held. Latest event:\n",
                            notice_ids.len()
                        );
                        if let Some(object) = message.as_object_mut() {
                            let content = match object.get("content") {
                                Some(Value::String(text)) => {
                                    Value::String(format!("{prefix}{text}"))
                                }
                                Some(Value::Array(blocks)) => {
                                    let mut next = vec![json!({"type": "text", "text": prefix})];
                                    next.extend(blocks.iter().cloned());
                                    Value::Array(next)
                                }
                                _ => Value::Array(vec![json!({"type": "text", "text": prefix})]),
                            };
                            object.insert("content".into(), content);
                        }
                    }
                    out.push(HeldMessage {
                        source: source.clone(),
                        notice_ids,
                        message,
                    });
                }
            out.extend(wake.items.iter().map(|(id, message)| HeldMessage {
                source: source.clone(),
                notice_ids: vec![id.clone()],
                message: message.clone(),
            }));
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
        wake.items.retain(|(id, _)| !notice_ids.contains(id));
        if !wake
            .notice_ids
            .iter()
            .any(|id| !wake.items.iter().any(|(item, _)| item == id))
        {
            wake.message = None;
        }
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
            let ids: Vec<String> = self
                .wakes
                .iter()
                .find(|(s, _)| s == &source)
                .map(|(_, wake)| {
                    wake.notice_ids
                        .iter()
                        .filter(|id| !wake.items.iter().any(|(item, _)| item == *id))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            self.consume(&source, pause_seq, Some(&ids));
        }
    }

    fn remember(&mut self, notice: &Value) {
        let producer = notice.get("producer").and_then(Value::as_str).unwrap_or("unknown");
        let Some(id) = notice.get("noticeId").and_then(Value::as_str) else {
            return;
        };
        let index = match self.wakes.iter().position(|(s, _)| s == producer) {
            Some(index) => index,
            None => {
                self.wakes.push((producer.to_owned(), HeldWake::default()));
                self.wakes.len() - 1
            }
        };
        let wake = &mut self.wakes[index].1;
        wake.notice_ids.push(id.to_string());
        if let Some(message) = notice.get("message").filter(|m| !m.is_null()) {
            if notice.get("itemId").is_some() {
                wake.items.push((id.to_owned(), message.clone()));
            } else {
                wake.message = Some(message.clone());
            }
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
    fn itemized_payloads_survive_replay_and_plain_wakes() {
        let mut notices =
            EngineTurnHoldNotices::new(&[], Box::new(|_, _| Ok(())), Box::new(|_| {}));
        let items = vec![
            HeldItem {
                source: "monitor".into(),
                message: json!({"content": "first"}),
                item_id: "one".into(),
                run_ref: None,
            },
            HeldItem {
                source: "monitor".into(),
                message: json!({"content": "second"}),
                item_id: "two".into(),
                run_ref: Some(json!({"runId":"run"})),
            },
        ];
        assert!(
            notices
                .note_items(1, &items, true, |_| Err("disk full".into()))
                .is_err()
        );
        assert_eq!(notices.notice_count(), 0);
        let batch = std::sync::Mutex::new(Value::Null);
        let receipts = notices
            .note_items(1, &items, true, |data| {
                *batch.lock().expect("batch") = data;
                Ok("entry".into())
            })
            .expect("written");
        notices.note("monitor", 1, Some(json!({"content": "latest"})));
        assert_eq!(notices.messages().len(), 3);
        let entries = vec![
            json!({"type":"custom", "customType":"session-notice", "data":batch.into_inner().expect("batch")}),
        ];
        let mut replay =
            EngineTurnHoldNotices::new(&entries, Box::new(|_, _| Ok(())), Box::new(|_| {}));
        assert_eq!(replay.messages()[0].message["content"], "first");
        assert_eq!(replay.messages()[1].message["content"], "second");
        replay.consume_notices(1);
        assert_eq!(replay.notice_count(), 2);
        assert!(replay.consume("monitor", 1, Some(&[receipts[0].notice_id.clone()])));
        assert_eq!(replay.messages()[0].message["content"], "second");
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

#[cfg(test)]
mod itemized_tests {
    use super::*;
    #[test]
    fn failed_batch_has_no_custody_and_successful_items_keep_own_payloads() {
        let notices = vec![
            HeldItem { source: "task".into(), item_id: "a".into(), message: json!({"content":"one"}), run_ref: None },
            HeldItem { source: "task".into(), item_id: "b".into(), message: json!({"content":"two"}), run_ref: None },
        ];
        let mut failed = EngineTurnHoldNotices::new(
            &[],
            Box::new(|_, _| Err("disk full".into())),
            Box::new(|_| {}),
        );
        assert!(failed.note_items(1, &notices, false, |_| Err("disk full".into())).is_err());
        assert_eq!(failed.notice_count(), 0);
        let mut held = EngineTurnHoldNotices::new(&[], Box::new(|_, _| Ok(())), Box::new(|_| {}));
        held.note_items(1, &notices, false, |_| Ok("entry".into())).expect("persist");
        assert_eq!(held.notice_count(), 2);
        let messages = held.messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message["content"], "one");
        assert_eq!(messages[1].message["content"], "two");
    }
}
