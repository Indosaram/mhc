//! senpi `modes/rpc/compact-current-components.ts`: run state, the latest lifecycle
//! description, and open dialogs of one epoch (S03b-3b, RULING-26 Q4).

use crate::compact_component_roots::{CompactComponentRoots, ComponentRoot};
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::CompactError;
use crate::compact_values::{CompactValues, ValueRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactRunState {
    Idle,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogAudience {
    Broadcast,
    Targeted,
}

/// One open dialog as a cut holds it. A broadcast question keeps its request; a targeted dialog
/// keeps only its identity, method and deadline, so no reader of a cut can present it (RULING-25 Q4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactDialog {
    pub id: String,
    pub audience: DialogAudience,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogCounts {
    pub broadcast: usize,
    pub targeted: usize,
}

#[derive(Debug, Clone)]
pub struct FrozenComponents {
    pub run_state: CompactRunState,
    pub lifecycle: Option<ValueRef>,
    pub dialogs: Option<ValueRef>,
    pub dialog_counts: DialogCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentComponentsView {
    pub run_state: CompactRunState,
    pub lifecycle: Option<Value>,
    pub dialogs: Vec<CompactDialog>,
    pub dialog_counts: DialogCounts,
}

pub const COMPONENT_RECORDS: [&str; 7] = [
    "agent_start",
    "agent_end",
    "session_lifecycle",
    "extension_ui_request",
    "question_updated",
    "question_resolved",
    "ui_request_settled",
];

pub fn is_component_record(record_type: &str) -> bool {
    COMPONENT_RECORDS.contains(&record_type)
}

fn is_targeted_method(method: &str) -> bool {
    matches!(method, "select" | "confirm" | "input" | "editor")
}

struct OpenDialogEntry {
    order: String,
    dialog: CompactDialog,
}

pub struct CompactCurrentComponents {
    run_state: CompactRunState,
    lifecycle: Option<ValueRef>,
    dialogs: ComponentRoot<CompactDialog>,
    open: HashMap<String, OpenDialogEntry>,
    opened: u64,
    store: CompactContentStore,
    values: CompactValues,
    dialog_roots: CompactComponentRoots<CompactDialog>,
}

impl CompactCurrentComponents {
    pub fn new(store: CompactContentStore) -> Self {
        Self {
            run_state: CompactRunState::Idle,
            lifecycle: None,
            dialogs: ComponentRoot::default(),
            open: HashMap::new(),
            opened: 0,
            values: CompactValues::new(store.clone()),
            dialog_roots: CompactComponentRoots::new(store.clone(), |_| Vec::new()),
            store,
        }
    }

    pub fn open_broadcast(&self) -> usize {
        self.open
            .values()
            .filter(|entry| entry.dialog.audience == DialogAudience::Broadcast)
            .count()
    }

    pub fn apply(&mut self, record: &Value) -> Result<(), CompactError> {
        let kind = record.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "agent_start" => {
                self.run_state = CompactRunState::Running;
                Ok(())
            }
            "agent_end" => {
                self.run_state = CompactRunState::Idle;
                Ok(())
            }
            "session_lifecycle" => {
                let next = self.values.own(record)?;
                if let Some(prev) = self.lifecycle.replace(next) {
                    self.store.adjust(prev.owner, -1, 0)?;
                }
                Ok(())
            }
            "extension_ui_request" => {
                let Some(id) = record.get("id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let method = record
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("undefined");
                if (method != "question" && !is_targeted_method(method))
                    || self.open.contains_key(id)
                {
                    return Ok(());
                }
                let broadcast = method == "question";
                let deadline_at_ms = record.get("deadlineAtMs").and_then(Value::as_u64);
                let dialog = CompactDialog {
                    id: id.to_owned(),
                    audience: if broadcast {
                        DialogAudience::Broadcast
                    } else {
                        DialogAudience::Targeted
                    },
                    method: method.to_owned(),
                    deadline_at_ms,
                    request: broadcast.then(|| record.clone()),
                };
                let order = format!("{:016}", self.opened);
                self.opened += 1;
                self.put(id, order, dialog)
            }
            "question_updated" => {
                let Some(id) = record.get("id").and_then(Value::as_str) else {
                    return Ok(());
                };
                let Some(existing) = self.open.get(id) else {
                    return Ok(());
                };
                let order = existing.order.clone();
                let mut updated = existing.dialog.clone();
                if let Some(dl) = record.get("deadlineAtMs").and_then(Value::as_u64) {
                    updated.deadline_at_ms = Some(dl);
                }
                self.put(id, order, updated)
            }
            _ => {
                let id_opt = record
                    .get("id")
                    .or_else(|| record.get("uiRequestId"))
                    .and_then(Value::as_str);
                let Some(id) = id_opt else {
                    return Ok(());
                };
                let Some(open) = self.open.remove(id) else {
                    return Ok(());
                };
                self.dialogs = self.dialog_roots.set(&self.dialogs, &open.order, None)?;
                Ok(())
            }
        }
    }

    pub fn freeze(&self) -> Result<FrozenComponents, CompactError> {
        if let Some(ref lifecycle) = self.lifecycle {
            self.store.adjust(lifecycle.owner, 0, 1)?;
        }
        if let Some(ref dialogs) = self.dialogs.reference {
            self.store.adjust(dialogs.owner, 0, 1)?;
        }
        let broadcast = self.open_broadcast();
        Ok(FrozenComponents {
            run_state: self.run_state,
            lifecycle: self.lifecycle.clone(),
            dialogs: self.dialogs.reference.clone(),
            dialog_counts: DialogCounts {
                broadcast,
                targeted: self.open.len() - broadcast,
            },
        })
    }

    pub fn release(&self, frozen: &FrozenComponents) -> Result<(), CompactError> {
        if let Some(ref lifecycle) = frozen.lifecycle {
            self.store.adjust(lifecycle.owner, 0, -1)?;
        }
        if let Some(ref dialogs) = frozen.dialogs {
            self.store.adjust(dialogs.owner, 0, -1)?;
        }
        Ok(())
    }

    pub async fn read(
        &self,
        frozen: &FrozenComponents,
        valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    ) -> Result<CurrentComponentsView, CompactError> {
        valid()?;
        let lifecycle = match &frozen.lifecycle {
            Some(ref_val) => Some(self.values.read(ref_val).await?),
            None => None,
        };
        valid()?;
        let entries = self
            .dialog_roots
            .entries(frozen.dialogs.as_ref(), valid)
            .await?;
        let dialogs = entries.into_iter().map(|(_, d)| d).collect();
        Ok(CurrentComponentsView {
            run_state: frozen.run_state,
            lifecycle,
            dialogs,
            dialog_counts: frozen.dialog_counts,
        })
    }

    fn put(&mut self, id: &str, order: String, dialog: CompactDialog) -> Result<(), CompactError> {
        self.dialogs = self
            .dialog_roots
            .set(&self.dialogs, &order, Some(dialog.clone()))?;
        self.open
            .insert(id.to_owned(), OpenDialogEntry { order, dialog });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    fn ok() -> Result<(), CompactError> {
        Ok(())
    }

    #[tokio::test]
    async fn tracks_run_state_lifecycle_and_broadcast_vs_targeted_dialogs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut comp = CompactCurrentComponents::new(store.clone());

        comp.apply(&json!({"type":"agent_start"})).expect("start");
        comp.apply(&json!({"type":"session_lifecycle","state":"ready"}))
            .expect("lifecycle");
        comp.apply(&json!({"type":"extension_ui_request","id":"q1","method":"question","deadlineAtMs":1000}))
            .expect("q1");
        comp.apply(&json!({"type":"extension_ui_request","id":"c1","method":"confirm","deadlineAtMs":2000}))
            .expect("c1");
        comp.apply(&json!({"type":"question_updated","id":"q1","deadlineAtMs":1500}))
            .expect("q1 update");

        let frozen = comp.freeze().expect("freeze");
        assert_eq!(frozen.dialog_counts.broadcast, 1);
        assert_eq!(frozen.dialog_counts.targeted, 1);

        let view = comp.read(&frozen, &ok).await.expect("read");
        assert_eq!(view.run_state, CompactRunState::Running);
        assert_eq!(view.lifecycle.expect("lc")["state"], "ready");
        assert_eq!(view.dialogs.len(), 2);
        assert_eq!(view.dialogs[0].deadline_at_ms, Some(1500));
        assert!(view.dialogs[0].request.is_some());
        assert!(view.dialogs[1].request.is_none());

        comp.apply(&json!({"type":"ui_request_settled","uiRequestId":"c1"}))
            .expect("settle c1");
        let frozen2 = comp.freeze().expect("freeze 2");
        let view2 = comp.read(&frozen2, &ok).await.expect("read 2");
        assert_eq!(view2.dialogs.len(), 1);

        comp.release(&frozen).expect("rel1");
        comp.release(&frozen2).expect("rel2");
        store.idle().await;
        store.retire().await.expect("retire");
    }
}
