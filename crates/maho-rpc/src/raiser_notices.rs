//! Port of senpi `packages/coding-agent/src/modes/rpc/raiser-notices.ts`.
//!
//! Broadcasts `raised` / `settled` secret or grant-select notices to attached connections that
//! opted into `raiser_notice_v1` (excluding the presenter and sealed holders).

use crate::jsonl::serialize_json_line;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub const RAISER_NOTICE_CAPABILITY: &str = "raiser_notice_v1";

pub trait RaiserNoticeTarget {
    fn attached_connections(&self, session_id: &str) -> Vec<String>;
    fn connection_has(&self, connection_id: &str, capability: &str) -> bool;
    fn enqueue_line(&self, connection_id: &str, line: String);
    fn flush(&self);
}

#[derive(Default)]
pub struct RaiserNotices {
    pending: HashMap<String, HashMap<String, Vec<String>>>,
}

impl RaiserNotices {
    pub fn raised(
        &mut self,
        target: &dyn RaiserNoticeTarget,
        session_id: &str,
        ui_request_id: &str,
        excluded: &[&str],
        notice: &Map<String, Value>,
    ) {
        let session = self.pending.entry(session_id.to_owned()).or_default();
        if session.contains_key(ui_request_id) {
            return;
        }
        let recipients: Vec<String> = target
            .attached_connections(session_id)
            .into_iter()
            .filter(|id| {
                !excluded.contains(&id.as_str())
                    && target.connection_has(id, RAISER_NOTICE_CAPABILITY)
            })
            .collect();
        if recipients.is_empty() {
            if session.is_empty() {
                self.pending.remove(session_id);
            }
            return;
        }
        session.insert(ui_request_id.to_owned(), recipients.clone());
        let mut payload = notice.clone();
        payload.insert("sessionId".to_owned(), json!(session_id));
        payload.insert("uiRequestId".to_owned(), json!(ui_request_id));
        if let Ok(line) = serialize_json_line(&Value::Object(payload)) {
            for id in &recipients {
                target.enqueue_line(id, line.clone());
            }
            target.flush();
        }
    }

    pub fn settled(
        &mut self,
        target: &dyn RaiserNoticeTarget,
        session_id: &str,
        ui_request_id: &str,
        record: &Map<String, Value>,
    ) {
        let Some(session) = self.pending.get_mut(session_id) else {
            return;
        };
        let Some(recipients) = session.remove(ui_request_id) else {
            return;
        };
        if session.is_empty() {
            self.pending.remove(session_id);
        }
        let mut payload = record.clone();
        payload.insert("sessionId".to_owned(), json!(session_id));
        payload.insert("uiRequestId".to_owned(), json!(ui_request_id));
        if let Ok(line) = serialize_json_line(&Value::Object(payload)) {
            for id in &recipients {
                target.enqueue_line(id, line.clone());
            }
            target.flush();
        }
    }

    pub fn drop_session(&mut self, session_id: &str) {
        self.pending.remove(session_id);
    }
}
