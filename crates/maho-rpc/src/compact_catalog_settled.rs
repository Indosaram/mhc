//! senpi `modes/rpc/compact-catalog-settled.ts`: the `settled` and `identity` families (R4.B2,
//! S05, C1.1).

use crate::compact_catalog_codec::BoundedId;
use crate::compact_catalog_registry::{
    CatalogFamily, MemberOrdinal, MemberPrefix, family_id, member_id,
};
use crate::compact_catalog_steps::{
    CatalogReader, CatalogStep, ContainerKind, StepSink, ValueStep, literal_steps, stored_steps,
    walked_steps,
};
use crate::compact_catalog_transcript::text_component;
use crate::compact_catalog_writer::{CatalogComponentSink, empty_component, steps_component};
use crate::compact_read_lease::CompactError;
use crate::compact_settled_rows::{SettledMember, SettledReader, TerminalRow};
use crate::session_acceptance_lane::{AcceptedCorrelation, FrozenAcceptedRoot};
use serde::Serialize;
use serde_json::Value;

/// The settled member of the terminal a message ended at: `message-<terminalSequence>`.
pub fn settled_member(terminal_sequence: u64) -> BoundedId {
    member_id(
        MemberPrefix::Message,
        MemberOrdinal::Number(terminal_sequence),
    )
}

fn join_member(ordinal: u64) -> BoundedId {
    member_id(MemberPrefix::Join, MemberOrdinal::Number(ordinal))
}

async fn push_field(name: &str, value: Value, sink: &mut dyn StepSink) -> Result<(), CompactError> {
    sink.push(CatalogStep::Value(ValueStep::String {
        text: name.to_owned(),
        more: false,
    }))
    .await?;
    literal_steps(&value, sink).await
}

async fn terminal_steps(
    settled: &dyn SettledReader,
    row: &TerminalRow,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
        .await?;
    push_field("messageId", Value::String(row.message_id.clone()), sink).await?;
    push_field(
        "turnId",
        row.turn_id
            .as_deref()
            .map_or(Value::Null, |t| Value::String(t.to_owned())),
        sink,
    )
    .await?;
    push_field("terminalSequence", Value::from(row.terminal_sequence), sink).await?;
    push_field("role", Value::String(row.role.clone()), sink).await?;
    push_field(
        "runtimeItemKey",
        row.runtime_item_key
            .as_deref()
            .map_or(Value::Null, |k| Value::String(k.to_owned())),
        sink,
    )
    .await?;
    if row.call_unknown {
        push_field("callUnknown", Value::Bool(true), sink).await?;
    }
    push_field(
        "entryId",
        row.entry_id
            .as_deref()
            .map_or(Value::Null, |e| Value::String(e.to_owned())),
        sink,
    )
    .await?;
    push_field("status", serde_json::to_value(row.status)?, sink).await?;
    push_field("error", serde_json::to_value(&row.error)?, sink).await?;
    sink.push(CatalogStep::Value(ValueStep::String {
        text: "payload".into(),
        more: false,
    }))
    .await?;
    stored_steps(settled.store(), &row.payload, sink).await?;
    sink.push(CatalogStep::Value(ValueStep::String {
        text: "call".into(),
        more: false,
    }))
    .await?;
    walked_steps(settled.values(), row.call.as_ref(), valid, sink).await?;
    sink.push(CatalogStep::Value(ValueStep::Close)).await
}

/// The settled family (R4.B2, S05, RULING-13 Q2).
pub async fn settled_components(
    reader: &CatalogReader<'_>,
    settled: &dyn SettledReader,
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let settled_id = family_id(CatalogFamily::Settled);
    let mut count = 0usize;
    for member in settled.members(reader.valid).await? {
        let SettledMember::Terminal(row) = member else {
            continue;
        };
        let member_name = settled_member(row.terminal_sequence);
        steps_component(
            reader.store,
            settled_id.clone(),
            member_name,
            sink,
            |mut encoder| async move {
                terminal_steps(settled, &row, reader.valid, &mut encoder).await?;
                Ok(encoder)
            },
        )
        .await?;
        count += 1;
    }
    if count == 0 {
        sink.accept(empty_component(settled_id)).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCorrelation {
    pub sequence: u64,
    pub client_message_id: Option<Value>,
    pub client_turn_id: Option<Value>,
    pub client_messages: Option<Value>,
}

impl From<&AcceptedCorrelation> for IdentityCorrelation {
    fn from(c: &AcceptedCorrelation) -> Self {
        Self {
            sequence: c.sequence,
            client_message_id: c.client_message_id.clone(),
            client_turn_id: c.client_turn_id.clone(),
            client_messages: c.client_messages.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySnapshot {
    pub session_epoch: String,
    pub sequence: u64,
    pub active_message_id: Option<String>,
    pub turn_id: Option<String>,
    pub correlation: Option<IdentityCorrelation>,
    pub refused_correlations: usize,
}

impl IdentitySnapshot {
    pub fn from_root(root: &FrozenAcceptedRoot, refused_correlations: usize) -> Self {
        Self {
            session_epoch: root.session_epoch.clone(),
            sequence: root.sequence,
            active_message_id: root.active_message_id.clone(),
            turn_id: root.turn_id.clone(),
            correlation: root.correlation.as_ref().map(IdentityCorrelation::from),
            refused_correlations,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JoinPayload<'a> {
    message_id: &'a str,
    entry_id: &'a str,
}

/// The identity family (R4.B2, C1.1).
pub async fn identity_components(
    reader: &CatalogReader<'_>,
    identity: &IdentitySnapshot,
    settled: &dyn SettledReader,
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let identity_id = family_id(CatalogFamily::Identity);
    text_component(
        reader.store,
        identity_id.clone(),
        identity_id.clone(),
        &serde_json::to_string(identity)?,
        sink,
    )
    .await?;
    for member in settled.members(reader.valid).await? {
        let SettledMember::Owner(owner) = member else {
            continue;
        };
        let join = serde_json::to_string(&JoinPayload {
            message_id: &owner.message_id,
            entry_id: &owner.entry_id,
        })?;
        text_component(
            reader.store,
            identity_id.clone(),
            join_member(owner.ordinal),
            &join,
            sink,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_catalog_writer::CatalogComponent;
    use crate::compact_content_store::{CompactContentStore, CompactContentStoreOptions};
    use crate::compact_settled_rows::{StoredOwner, TerminalStatus};
    use crate::compact_values::CompactValues;
    use async_trait::async_trait;

    #[tokio::test]
    async fn emits_settled_terminals_and_identity_joins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let values = CompactValues::new(store.clone());
        let payload_leaf = store
            .create_leaf("{\"terminal\":{\"role\":\"assistant\"}}", true)
            .expect("leaf");

        struct FakeSettled {
            store: CompactContentStore,
            values: CompactValues,
            members: Vec<SettledMember>,
        }
        #[async_trait]
        impl SettledReader for FakeSettled {
            fn store(&self) -> &CompactContentStore {
                &self.store
            }
            fn values(&self) -> &CompactValues {
                &self.values
            }
            async fn members(
                &self,
                _valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
            ) -> Result<Vec<SettledMember>, CompactError> {
                Ok(self.members.clone())
            }
            async fn message_of(&self, _entry_id: &str) -> Result<Option<String>, CompactError> {
                Ok(None)
            }
        }

        let settled = FakeSettled {
            store: store.clone(),
            values: values.clone(),
            members: vec![
                SettledMember::Terminal(TerminalRow {
                    message_id: "m1".into(),
                    turn_id: Some("t1".into()),
                    terminal_sequence: 7,
                    role: "assistant".into(),
                    runtime_item_key: None,
                    call_unknown: false,
                    entry_id: None,
                    status: TerminalStatus::Pending,
                    error: None,
                    payload: vec![payload_leaf],
                    call: None,
                }),
                SettledMember::Owner(StoredOwner {
                    message_id: "m2".into(),
                    entry_id: "e2".into(),
                    ordinal: 3,
                }),
            ],
        };
        let ok = || -> Result<(), CompactError> { Ok(()) };
        let reader = CatalogReader {
            store: &store,
            values: &values,
            valid: &ok,
        };

        struct Capture(CompactContentStore, Vec<(String, String, String)>);
        #[async_trait]
        impl CatalogComponentSink for Capture {
            async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError> {
                let mut text = String::new();
                for leaf in &component.leaves {
                    text.push_str(&self.0.read_leaf(*leaf).await?);
                }
                self.1.push((
                    component.kind.as_str().to_owned(),
                    component.id.as_str().to_owned(),
                    text,
                ));
                Ok(())
            }
        }
        let mut sink = Capture(store.clone(), Vec::new());
        settled_components(&reader, &settled, &mut sink)
            .await
            .expect("settled");
        identity_components(
            &reader,
            &IdentitySnapshot {
                session_epoch: "E".into(),
                sequence: 7,
                active_message_id: None,
                turn_id: Some("t1".into()),
                correlation: None,
                refused_correlations: 0,
            },
            &settled,
            &mut sink,
        )
        .await
        .expect("identity");
        assert_eq!(sink.1.len(), 3);
        assert_eq!(sink.1[0].1, "message-0000000000000007");
        let term: Value = serde_json::from_str(&sink.1[0].2).expect("term json");
        assert_eq!(term["payload"]["terminal"]["role"], "assistant");
        assert_eq!(sink.1[2].1, "join-0000000000000003");

        store.adjust(payload_leaf, -1, 0).expect("release");
        store.retire().await.expect("retire");
    }
}
