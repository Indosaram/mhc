//! senpi `modes/rpc/compact-settled-messages.ts`: one spillable collection owns payloads, end markers
//! and both directions of the durable join.
use crate::compact_content_index::IndexStatus;
use crate::compact_content_index::{CompactContentIndex, FrozenIndex};
use crate::compact_content_store::{
    COMPACT_LEAF_CODE_UNITS, CompactContentStore, CompactContentStoreOptions, CompactStorageStatus,
    LeafId, utf16_len, utf16_slice,
};
use crate::compact_read_lease::{CompactError, lock};
use maho_core::session_recovery_identity::RecoveryCarrier;
use maho_core::session_transcript_journal::TranscriptTransition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettledDiagnostic {
    UnknownMessage,
    DuplicateEnd,
    RetiredOwner,
    ConflictingOwner,
    UnsupportedRole,
}

impl SettledDiagnostic {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownMessage => "unknown_message",
            Self::DuplicateEnd => "duplicate_end",
            Self::RetiredOwner => "retired_owner",
            Self::ConflictingOwner => "conflicting_owner",
            Self::UnsupportedRole => "unsupported_role",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Row {
    Terminal {
        message_id: String,
        turn_id: Option<String>,
        terminal_sequence: u64,
        role: String,
        runtime_item_key: Option<String>,
        entry_id: Option<String>,
        status: String,
        error: Option<Value>,
        payload: Vec<LeafId>,
    },
    Owner {
        message_id: String,
        entry_id: String,
    },
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRow {
    pub message_id: String,
    pub entry_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettledMessage {
    pub message_id: String,
    pub turn_id: Option<String>,
    pub terminal_sequence: u64,
    pub role: String,
    pub status: String,
    pub error: Option<Value>,
    pub terminal: Value,
    pub correlation: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettledRead {
    pub settled_messages: Vec<SettledMessage>,
    pub owners: Vec<OwnerRow>,
}

pub type SettledRoot = FrozenIndex;

type Diagnose = Box<dyn Fn(SettledDiagnostic) + Send + Sync>;

pub struct CompactSettledMessages {
    store: CompactContentStore,
    rows: CompactContentIndex<Row>,
    /// Resident membership of the paged rows, the synchronous check acceptance needs.
    keys: Mutex<HashSet<String>>,
    epoch: String,
    diagnose: Diagnose,
}

impl CompactSettledMessages {
    pub fn new(
        epoch: String,
        options: CompactContentStoreOptions,
        diagnose: impl Fn(SettledDiagnostic) + Send + Sync + 'static,
    ) -> Self {
        let store = CompactContentStore::new(options);
        Self {
            rows: CompactContentIndex::new(
                store.clone(),
                |row: &Row| match row {
                    Row::Terminal { payload, .. } => payload.clone(),
                    _ => Vec::new(),
                },
                128,
            ),
            store,
            keys: Mutex::new(HashSet::new()),
            epoch,
            diagnose: Box::new(diagnose),
        }
    }

    pub fn valid<'a>(&self, carrier: Option<&'a RecoveryCarrier>) -> Option<&'a RecoveryCarrier> {
        carrier.filter(|carrier| carrier.epoch == self.epoch)
    }

    fn has(&self, key: &str) -> bool {
        lock(&self.keys).contains(key)
    }

    pub fn end(
        &self,
        record: &Value,
        carrier: Option<&RecoveryCarrier>,
        sequence: u64,
        turn_id: Option<String>,
    ) -> Result<(), CompactError> {
        let Some(carrier) = self.valid(carrier) else {
            (self.diagnose)(SettledDiagnostic::UnknownMessage);
            return Ok(());
        };
        let id = &carrier.message_id;
        if self.has(&format!("end:{id}")) {
            (self.diagnose)(SettledDiagnostic::DuplicateEnd);
            return Ok(());
        }
        if let Some(entry) = &carrier.durable_entry_id
            && !self.has(&format!("owner:{id}"))
        {
            if self.has(&format!("entry:{entry}")) {
                (self.diagnose)(SettledDiagnostic::ConflictingOwner);
                return Ok(());
            }
            self.join(id, entry)?;
        }
        self.put(&format!("end:{id}"), &Row::Ended)?;
        if self.has(&format!("owner:{id}")) {
            return Ok(());
        }
        let message = record.get("message").cloned().unwrap_or(Value::Null);
        let role = match message.get("role") {
            Some(Value::String(role)) => role.clone(),
            Some(other) => other.to_string(),
            None => "undefined".to_owned(),
        };
        let text = json!({
            "terminal": message,
            "correlation": {
                "clientMessageId": record.get("clientMessageId"),
                "clientTurnId": record.get("clientTurnId"),
                "clientMessages": record.get("clientMessages"),
            }
        })
        .to_string();
        let units = usize::try_from(utf16_len(&text)).unwrap_or(0);
        let mut payload = Vec::new();
        let mut offset = 0;
        while offset < units {
            let chunk = utf16_slice(&text, offset, offset + COMPACT_LEAF_CODE_UNITS);
            let chunk_units = usize::try_from(utf16_len(chunk)).unwrap_or(0).max(1);
            payload.push(self.store.create_leaf(chunk, false)?);
            offset += chunk_units;
        }
        let stored = self.put(
            &format!("terminal:{id}"),
            &Row::Terminal {
                message_id: id.clone(),
                turn_id,
                terminal_sequence: sequence,
                role: role.clone(),
                runtime_item_key: None,
                entry_id: None,
                status: "pending".into(),
                error: None,
                payload: payload.clone(),
            },
        );
        for leaf in payload {
            self.store.adjust(leaf, -1, 0)?;
        }
        stored?;
        if !["user", "assistant", "toolResult", "custom"].contains(&role.as_str()) {
            (self.diagnose)(SettledDiagnostic::UnsupportedRole);
        }
        Ok(())
    }

    pub fn fail(
        &self,
        carrier: Option<&RecoveryCarrier>,
        error_message: &str,
    ) -> Result<(), CompactError> {
        let Some(carrier) = self
            .valid(carrier)
            .filter(|carrier| self.has(&format!("terminal:{}", carrier.message_id)))
        else {
            (self.diagnose)(SettledDiagnostic::UnknownMessage);
            return Ok(());
        };
        self.rows.patch(
            &format!("terminal:{}", carrier.message_id),
            &[
                ("status".into(), json!("failed")),
                ("error".into(), json!({"errorMessage": error_message})),
            ],
        )
    }

    pub fn append(&self, transition: &TranscriptTransition) -> Result<(), CompactError> {
        let TranscriptTransition::Append {
            id: entry,
            owning_message_id: Some(message_id),
            recovery_epoch,
            ..
        } = transition
        else {
            return Ok(());
        };
        if recovery_epoch.as_deref() != Some(self.epoch.as_str()) {
            (self.diagnose)(SettledDiagnostic::RetiredOwner);
            return Ok(());
        }
        if self.has(&format!("owner:{message_id}")) || self.has(&format!("entry:{entry}")) {
            (self.diagnose)(SettledDiagnostic::ConflictingOwner);
            return Ok(());
        }
        self.join(message_id, entry)
    }

    fn join(&self, message_id: &str, entry_id: &str) -> Result<(), CompactError> {
        let owner = Row::Owner {
            message_id: message_id.to_owned(),
            entry_id: entry_id.to_owned(),
        };
        self.put(&format!("owner:{message_id}"), &owner)?;
        self.put(&format!("entry:{entry_id}"), &owner)?;
        let terminal = format!("terminal:{message_id}");
        self.rows.delete(&terminal)?;
        lock(&self.keys).remove(&terminal);
        Ok(())
    }

    fn put(&self, key: &str, row: &Row) -> Result<(), CompactError> {
        self.rows.set(key, row)?;
        lock(&self.keys).insert(key.to_owned());
        Ok(())
    }

    pub fn freeze(&self) -> Result<SettledRoot, CompactError> {
        self.rows.freeze()
    }

    pub fn release(&self, root: &SettledRoot) {
        self.rows.release(root);
    }

    pub async fn read(&self, root: &SettledRoot) -> Result<SettledRead, CompactError> {
        let mut settled_messages = Vec::new();
        let mut owners = Vec::new();
        for (key, row) in self.rows.entries(root).await? {
            match serde_json::from_value::<Row>(row)? {
                Row::Owner {
                    message_id,
                    entry_id,
                } if key.starts_with("owner:") => owners.push(OwnerRow {
                    message_id,
                    entry_id,
                }),
                Row::Terminal {
                    message_id,
                    turn_id,
                    terminal_sequence,
                    role,
                    status,
                    error,
                    payload,
                    ..
                } => {
                    let mut bytes = String::new();
                    for leaf in payload {
                        bytes.push_str(&self.store.read_leaf(leaf).await?);
                    }
                    let mut value: Value = serde_json::from_str(&bytes)?;
                    settled_messages.push(SettledMessage {
                        message_id,
                        turn_id,
                        terminal_sequence,
                        role,
                        status,
                        error,
                        terminal: value["terminal"].take(),
                        correlation: value["correlation"].take(),
                    });
                }
                _ => {}
            }
        }
        Ok(SettledRead {
            settled_messages,
            owners,
        })
    }

    pub fn storage_status(&self) -> (CompactStorageStatus, IndexStatus) {
        (self.store.status(), self.rows.status())
    }

    pub async fn idle(&self) {
        self.store.idle().await;
    }

    pub async fn retire(&self) -> Result<(), CompactError> {
        self.store.retire().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn carrier(id: &str, epoch: &str, entry: Option<&str>) -> RecoveryCarrier {
        RecoveryCarrier {
            message_id: id.into(),
            epoch: epoch.into(),
            durable_entry_id: entry.map(Into::into),
        }
    }

    // senpi 4272-terminal-joins / terminal-storage: terminals until joined, then durable owners.
    #[tokio::test]
    async fn keeps_terminals_until_their_durable_entry_joins() {
        let root = tempfile::tempdir().expect("tempdir");
        let seen: Arc<Mutex<Vec<SettledDiagnostic>>> = Arc::default();
        let log = seen.clone();
        let settled = CompactSettledMessages::new(
            "E".into(),
            CompactContentStoreOptions {
                dir: root.path().join("settled").display().to_string(),
                ..Default::default()
            },
            move |reason| lock(&log).push(reason),
        );
        let end = json!({"type":"message_end","message":{"role":"assistant","content":"hi"},"clientMessageId":"c1"});
        settled
            .end(&end, Some(&carrier("m1", "E", None)), 3, Some("t".into()))
            .expect("end");
        settled
            .end(&end, Some(&carrier("m1", "E", None)), 4, None)
            .expect("dup");
        settled
            .end(&end, Some(&carrier("m2", "OLD", None)), 5, None)
            .expect("stale");
        settled
            .fail(Some(&carrier("m1", "E", None)), "disk full")
            .expect("fail");
        settled
            .end(&end, Some(&carrier("m3", "E", Some("e3"))), 6, None)
            .expect("joined end");
        let cut = settled.freeze().expect("freeze");
        let read = settled.read(&cut).await.expect("read");
        assert_eq!(read.settled_messages.len(), 1);
        let first = &read.settled_messages[0];
        assert_eq!(first.status, "failed");
        assert_eq!(first.terminal["content"], "hi");
        assert_eq!(first.correlation["clientMessageId"], "c1");
        assert_eq!(
            read.owners,
            [OwnerRow {
                message_id: "m3".into(),
                entry_id: "e3".into()
            }]
        );
        settled.release(&cut);
        settled
            .append(&TranscriptTransition::Append {
                id: "e1".into(),
                entry: None,
                persistence: None,
                owning_message_id: Some("m1".into()),
                recovery_epoch: Some("E".into()),
            })
            .expect("join m1");
        let cut = settled.freeze().expect("freeze");
        let read = settled.read(&cut).await.expect("read");
        assert!(read.settled_messages.is_empty());
        assert_eq!(read.owners.len(), 2);
        settled.release(&cut);
        assert_eq!(
            *lock(&seen),
            [
                SettledDiagnostic::DuplicateEnd,
                SettledDiagnostic::UnknownMessage
            ]
        );
        settled.retire().await.expect("retire");
    }
}
