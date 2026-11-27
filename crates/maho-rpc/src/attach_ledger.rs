//! Port of senpi `packages/coding-agent/src/modes/rpc/attach-ledger.ts`.
//!
//! The durable takeover record behind a headless engine's quiet takeover (R21; S37).

use maho_agent::durable::{AdmissionLine, DurableStore, StoreValue, StoreWrite};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub const ATTACH_TAKEOVER_RECORD: &str = "attach.takeover";
const HOLDER_PREFIX: &str = "attach.holder:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachHolder {
    pub takeover_id: String,
    pub session_id: String,
    pub holder: String,
    pub generation: u64,
}

fn holder_key(session_id: &str) -> String {
    let encoded = serde_json::to_string(session_id).unwrap_or_else(|_| format!("\"{session_id}\""));
    format!("{HOLDER_PREFIX}{encoded}")
}

fn decode_holder(value: &StoreValue) -> Result<AttachHolder, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "attach: a holder document is not an object".to_owned())?;
    let takeover_id = obj
        .get("takeoverId")
        .and_then(Value::as_str)
        .ok_or_else(|| "attach: a holder document is malformed".to_owned())?
        .to_owned();
    let session_id = obj
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "attach: a holder document is malformed".to_owned())?
        .to_owned();
    let holder = obj
        .get("holder")
        .and_then(Value::as_str)
        .ok_or_else(|| "attach: a holder document is malformed".to_owned())?
        .to_owned();
    let generation = obj
        .get("generation")
        .and_then(Value::as_u64)
        .filter(|&g| g >= 1)
        .ok_or_else(|| "attach: a holder document is malformed".to_owned())?;
    Ok(AttachHolder {
        takeover_id,
        session_id,
        holder,
        generation,
    })
}

fn encode_holder(record: &AttachHolder) -> StoreValue {
    json!({
        "takeoverId": record.takeover_id,
        "sessionId": record.session_id,
        "holder": record.holder,
        "generation": record.generation,
    })
}

pub struct AttachLedger {
    store: Arc<dyn DurableStore>,
    epoch: u64,
    admission: AdmissionLine,
}

impl AttachLedger {
    pub fn new(store: Arc<dyn DurableStore>, epoch: u64, admission: AdmissionLine) -> Self {
        Self {
            store,
            epoch,
            admission,
        }
    }

    pub async fn holder_of(&self, session_id: &str) -> Result<Option<AttachHolder>, String> {
        let value = self
            .store
            .get_document(&holder_key(session_id))
            .await
            .map_err(|e| e.to_string())?;
        match value {
            None => Ok(None),
            Some(v) => Ok(Some(decode_holder(&v)?)),
        }
    }

    pub async fn take_over(&self, session_id: &str, holder: &str) -> Result<AttachHolder, String> {
        let session_id = session_id.to_owned();
        let holder = holder.to_owned();
        let store = Arc::clone(&self.store);
        let epoch = self.epoch;
        self.admission
            .admit(|| async move {
                let existing = store
                    .get_document(&holder_key(&session_id))
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(val) = existing {
                    let current = decode_holder(&val)?;
                    if current.holder == holder {
                        return Ok(current);
                    }
                    let next = AttachHolder {
                        takeover_id: uuid::Uuid::new_v4().to_string(),
                        session_id: session_id.clone(),
                        holder: holder.clone(),
                        generation: current.generation + 1,
                    };
                    let data = encode_holder(&next);
                    store
                        .commit(
                            epoch,
                            vec![
                                StoreWrite::Append {
                                    id: format!("{ATTACH_TAKEOVER_RECORD}:{}", next.takeover_id),
                                    kind: ATTACH_TAKEOVER_RECORD.to_owned(),
                                    data: data.clone(),
                                },
                                StoreWrite::Put {
                                    key: holder_key(&session_id),
                                    data,
                                },
                            ],
                            Box::new(|_| {}),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(next)
                } else {
                    let next = AttachHolder {
                        takeover_id: uuid::Uuid::new_v4().to_string(),
                        session_id: session_id.clone(),
                        holder: holder.clone(),
                        generation: 1,
                    };
                    let data = encode_holder(&next);
                    store
                        .commit(
                            epoch,
                            vec![
                                StoreWrite::Append {
                                    id: format!("{ATTACH_TAKEOVER_RECORD}:{}", next.takeover_id),
                                    kind: ATTACH_TAKEOVER_RECORD.to_owned(),
                                    data: data.clone(),
                                },
                                StoreWrite::Put {
                                    key: holder_key(&session_id),
                                    data,
                                },
                            ],
                            Box::new(|_| {}),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(next)
                }
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::durable::{
        DurableStoreOptions, MemoryStoreMedium, create_admission_line, open_memory_store,
    };

    #[tokio::test]
    async fn takeover_is_idempotent_for_same_holder_and_bumps_generation_for_new() {
        let store: Arc<dyn DurableStore> = Arc::new(open_memory_store(
            MemoryStoreMedium::default(),
            DurableStoreOptions::default(),
        ));
        store.claim(0, 1).await.expect("claim");
        let ledger = AttachLedger::new(store, 1, create_admission_line());
        let first = ledger.take_over("s1", "client-a").await.expect("a");
        assert_eq!(first.generation, 1);
        let retry = ledger.take_over("s1", "client-a").await.expect("retry");
        assert_eq!(retry, first);
        let second = ledger.take_over("s1", "client-b").await.expect("b");
        assert_eq!(second.generation, 2);
        assert_eq!(second.holder, "client-b");
    }
}
