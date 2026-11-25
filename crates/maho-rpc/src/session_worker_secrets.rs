//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-secrets.ts`
//! and `packages/coding-agent/src/modes/rpc/session-worker-secrets.test.ts`.
//!
//! The secret ids one worker holds on the host, and the frames each may still send (senpi#5192).

use crate::session_worker_protocol::SESSION_WORKER_LIMITS;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Secret frames ride the control exchange outside `outputBytes`, bounded at 64 KiB.
pub const WORKER_SECRET_FRAME_BYTES: usize = 64 * 1024;
/// Unsettled secret inputs per worker: one per waiting tool call or command (`SESSION_WORKER_LIMITS.requests`).
pub const WORKER_SECRET_INPUTS: usize = SESSION_WORKER_LIMITS.requests;
/// Every secret id tracked per worker: 1024 settled receipts + `WORKER_SECRET_INPUTS` open ones.
pub const WORKER_SECRET_IDS: usize = 1024 + WORKER_SECRET_INPUTS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretFrameType {
    ExtensionUiRequest,
    SecretInputSettled,
    SecretSealedOffer,
}

impl SecretFrameType {
    pub fn from_record(record: &Value) -> Option<Self> {
        match record.get("type").and_then(Value::as_str)? {
            "extension_ui_request" => Some(Self::ExtensionUiRequest),
            "secret_input_settled" => Some(Self::SecretInputSettled),
            "secret_sealed_offer" => Some(Self::SecretSealedOffer),
            _ => None,
        }
    }
}

/// A worker's offer of a sealed request to the host's holders (D11 secret phase 5.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedOfferRecord {
    #[serde(rename = "type")]
    pub record_type: String,
    pub hpke_pk: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    pub deadline_at_ms: i64,
}

pub fn is_sealed_offer(record: &Value) -> Option<SealedOfferRecord> {
    let obj = record.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("secret_sealed_offer") {
        return None;
    }
    let hpke_pk = obj.get("hpkePk").and_then(Value::as_str)?;
    if hpke_pk.len() != 43
        || !hpke_pk
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let title = obj.get("title").and_then(Value::as_str)?;
    let placeholder = match obj.get("placeholder") {
        None => None,
        Some(v) => Some(v.as_str()?.to_owned()),
    };
    let deadline_at_ms = obj.get("deadlineAtMs").and_then(Value::as_i64)?;
    const MAX_SAFE_INT: i64 = (1i64 << 53) - 1;
    if deadline_at_ms.abs() > MAX_SAFE_INT {
        return None;
    }
    Some(SealedOfferRecord {
        record_type: "secret_sealed_offer".to_owned(),
        hpke_pk: hpke_pk.to_owned(),
        title: title.to_owned(),
        placeholder,
        deadline_at_ms,
    })
}

#[derive(Default)]
pub struct WorkerSecretIds {
    sent: HashMap<String, HashSet<SecretFrameType>>,
}

impl WorkerSecretIds {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has(&self, id: &str) -> bool {
        self.sent.contains_key(id)
    }

    pub fn ids(&self) -> Vec<String> {
        self.sent.keys().cloned().collect()
    }

    pub fn admits_new(&self) -> bool {
        if self.sent.len() >= WORKER_SECRET_IDS {
            return false;
        }
        let open = self
            .sent
            .values()
            .filter(|sent| !sent.contains(&SecretFrameType::SecretInputSettled))
            .count();
        open < WORKER_SECRET_INPUTS
    }

    pub fn add(&mut self, id: &str) {
        self.sent.entry(id.to_owned()).or_default();
    }

    /// A live id's request, settled record and sealed offer (D11 secret phase 5.5), each at most
    /// once and within `WORKER_SECRET_FRAME_BYTES`; a refusal spends nothing.
    pub fn admit_frame(&mut self, id: &str, record: &Value) -> bool {
        let Some(sent) = self.sent.get_mut(id) else {
            return false;
        };
        let Some(frame_type) = SecretFrameType::from_record(record) else {
            return false;
        };
        if sent.contains(&frame_type) {
            return false;
        }
        let Ok(serialized) = serde_json::to_string(record) else {
            return false;
        };
        if serialized.len() > WORKER_SECRET_FRAME_BYTES {
            return false;
        }
        sent.insert(frame_type);
        true
    }

    pub fn forget(&mut self, id: &str) {
        self.sent.remove(id);
    }

    pub fn clear(&mut self) {
        self.sent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(id: &str, title: &str) -> Value {
        json!({
            "type": "extension_ui_request",
            "id": id,
            "method": "input",
            "secret": true,
            "title": title,
        })
    }

    fn settled(id: &str) -> Value {
        json!({
            "type": "secret_input_settled",
            "id": id,
            "outcome": "answered",
        })
    }

    fn request_of_bytes(id: &str, bytes: usize) -> Value {
        let base = serde_json::to_string(&request(id, ""))
            .expect("serialize")
            .len();
        request(id, &"x".repeat(bytes - base))
    }

    // 5 TS cases ported from session-worker-secrets.test.ts:

    #[test]
    fn admits_one_request_and_one_settled_frame_per_live_id_and_nothing_after_them() {
        let mut ids = WorkerSecretIds::new();
        ids.add("a");
        assert!(ids.admit_frame("a", &request("a", "Credential")));
        assert!(!ids.admit_frame("a", &request("a", "Credential")));
        assert!(ids.admit_frame("a", &settled("a")));
        assert!(!ids.admit_frame("a", &settled("a")));
        assert!(!ids.admit_frame("a", &request("a", "Credential")));
    }

    #[test]
    fn refuses_frames_for_unknown_or_forgotten_ids() {
        let mut ids = WorkerSecretIds::new();
        assert!(!ids.admit_frame("a", &request("a", "Credential")));
        ids.add("a");
        ids.forget("a");
        assert!(!ids.admit_frame("a", &settled("a")));
    }

    #[test]
    fn refuses_foreign_record_types_without_spending_a_phase() {
        let mut ids = WorkerSecretIds::new();
        ids.add("a");
        assert!(!ids.admit_frame(
            "a",
            &json!({ "type": "extension_ui_request_extra", "id": "a" })
        ));
        assert!(!ids.admit_frame("a", &json!({ "type": "secret_settled", "id": "a" })));
        assert!(!ids.admit_frame("a", &json!({ "id": "a" })));
        assert!(ids.admit_frame("a", &request("a", "Credential")));
    }

    #[test]
    fn bounds_the_serialized_frame_at_secret_frame_bytes_without_spending_a_phase_on_a_refusal() {
        let mut ids = WorkerSecretIds::new();
        ids.add("a");
        let limit = WORKER_SECRET_FRAME_BYTES;
        assert!(!ids.admit_frame("a", &request_of_bytes("a", limit + 1)));
        assert!(ids.admit_frame("a", &request_of_bytes("a", limit)));
    }

    #[test]
    fn caps_open_ids_and_settled_ids_only_against_the_total() {
        let mut ids = WorkerSecretIds::new();
        let open = WORKER_SECRET_INPUTS;
        for index in 0..open {
            assert!(ids.admits_new());
            ids.add(&format!("open-{index}"));
        }
        assert!(!ids.admits_new());
        assert!(ids.admit_frame("open-0", &settled("open-0")));
        assert!(ids.admits_new());

        for index in 1..open {
            ids.admit_frame(&format!("open-{index}"), &settled(&format!("open-{index}")));
        }
        let mut index = open;
        while ids.admits_new() {
            let id = format!("open-{index}");
            ids.add(&id);
            ids.admit_frame(&id, &settled(&id));
            index += 1;
        }
        assert_eq!(ids.ids().len(), WORKER_SECRET_IDS);
        ids.forget("open-0");
        assert!(ids.admits_new());
    }
}
