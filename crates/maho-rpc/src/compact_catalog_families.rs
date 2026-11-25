//! senpi `modes/rpc/compact-catalog-families.ts`: emits every component of one frozen root in
//! schema order (R4.B2) and enforces the completeness gate (`assert_coverage`).

use crate::compact_call_model::FrozenCalls;
use crate::compact_catalog_calls::calls_components;
use crate::compact_catalog_codec::{BoundedId, protocol_id};
use crate::compact_catalog_messages::active_components;
use crate::compact_catalog_registry::{
    CATALOG_FAMILIES, CatalogFamily, FamilyRegistry, MemberOrdinal, MemberPrefix, assert_coverage,
    family_id, family_producers, family_registry, member_id,
};
use crate::compact_catalog_settled::{IdentitySnapshot, identity_components, settled_components};
use crate::compact_catalog_source::source_components;
use crate::compact_catalog_steps::{CatalogReader, walk_value_steps};
use crate::compact_catalog_transcript::{
    AcceptedEntryMeta, TailHold, text_component, transcript_components,
};
use crate::compact_catalog_writer::{
    CatalogComponent, CatalogComponentSink, empty_component, steps_component,
};
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_settled_rows::SettledReader;
use crate::compact_values::{CompactValues, ValueRef};
use crate::session_acceptance_lane::FrozenAcceptedRoot;
use async_trait::async_trait;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactRunState {
    #[default]
    Idle,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogCounts {
    pub broadcast: usize,
    pub targeted: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactDialog {
    pub id: String,
    pub audience: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrozenComponents {
    pub run_state: CompactRunState,
    pub lifecycle: Option<ValueRef>,
    pub dialogs: Option<ValueRef>,
    pub dialog_counts: DialogCounts,
}

/// One frozen cut as its catalog reads it.
pub struct CatalogCut<'a> {
    pub store: &'a CompactContentStore,
    pub values: &'a CompactValues,
    pub root: &'a FrozenAcceptedRoot,
    pub active: Option<&'a ValueRef>,
    pub components: &'a FrozenComponents,
    pub calls: &'a FrozenCalls,
    pub accepted: &'a [AcceptedEntryMeta],
    pub hold: &'a mut dyn TailHold,
    pub refused_correlations: usize,
    pub settled: &'a dyn SettledReader,
}

#[derive(Deserialize)]
struct DialogPage {
    key: ValueRef,
    value: ValueRef,
    left: Option<ValueRef>,
    right: Option<ValueRef>,
}

fn collect_dialog_rows<'a>(
    reader: &'a CatalogReader<'a>,
    reference: Option<&'a ValueRef>,
    out: &'a mut Vec<(String, ValueRef)>,
) -> BoxFuture<'a, Result<(), CompactError>> {
    Box::pin(async move {
        let Some(reference) = reference else {
            return Ok(());
        };
        (reader.valid)()?;
        let page: DialogPage =
            serde_json::from_str(&reader.store.read_leaf(reference.node).await?)?;
        (reader.valid)()?;
        collect_dialog_rows(reader, page.left.as_ref(), out).await?;
        let Value::String(key) = reader.values.read(&page.key).await? else {
            return Err(CompactError::read(CompactReadReason::Ownership));
        };
        (reader.valid)()?;
        out.push((key, page.value));
        collect_dialog_rows(reader, page.right.as_ref(), out).await?;
        Ok(())
    })
}

async fn dialog_components(
    reader: &CatalogReader<'_>,
    reference: Option<&ValueRef>,
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let dialogs_id = family_id(CatalogFamily::Dialogs);
    let mut rows = Vec::new();
    collect_dialog_rows(reader, reference, &mut rows).await?;
    if rows.is_empty() {
        return sink.accept(empty_component(dialogs_id)).await;
    }
    for (order, value_ref) in rows {
        let ordinal: u64 = order.parse().unwrap_or(0);
        let member = member_id(MemberPrefix::Dialog, MemberOrdinal::Number(ordinal));
        steps_component(
            reader.store,
            dialogs_id.clone(),
            member,
            sink,
            |mut encoder| async move {
                walk_value_steps(reader.values, &value_ref, reader.valid, &mut encoder).await?;
                Ok(encoder)
            },
        )
        .await?;
    }
    Ok(())
}

struct CoverageSink<'a> {
    inner: &'a mut dyn CatalogComponentSink,
    emitted: &'a mut HashSet<BoundedId>,
}

#[async_trait]
impl CatalogComponentSink for CoverageSink<'_> {
    async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError> {
        self.emitted.insert(component.kind.clone());
        self.inner.accept(component).await
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunPayload {
    run_state: CompactRunState,
    dialog_counts: DialogCounts,
}

/// Every component of one frozen root, in schema order (R4.B2).
pub async fn checkpoint_components(
    cut: &mut CatalogCut<'_>,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    sink: &mut dyn CatalogComponentSink,
) -> Result<FamilyRegistry, CompactError> {
    let families_id = protocol_id("families");
    let run_id = family_id(CatalogFamily::Run);
    let lifecycle_id = family_id(CatalogFamily::Lifecycle);

    let reader = CatalogReader {
        store: cut.store,
        values: cut.values,
        valid,
    };
    let producers = family_producers(&cut.root.transcript, cut.refused_correlations);
    let registry = family_registry(&producers);
    text_component(
        cut.store,
        families_id.clone(),
        families_id,
        &serde_json::to_string(&registry)?,
        sink,
    )
    .await?;

    let mut emitted = HashSet::new();
    let mut tracked = CoverageSink {
        inner: sink,
        emitted: &mut emitted,
    };
    let identity = IdentitySnapshot::from_root(cut.root, cut.refused_correlations);

    for family in CATALOG_FAMILIES {
        match family {
            CatalogFamily::Transcript => {
                transcript_components(
                    cut.store,
                    &cut.root.transcript,
                    cut.accepted,
                    cut.hold,
                    &producers,
                    valid,
                    &mut tracked,
                )
                .await?;
            }
            CatalogFamily::Opaque => {
                // Emitted by `transcript_components` beside ancestry rows.
            }
            CatalogFamily::Run => {
                let run_json = serde_json::to_string(&RunPayload {
                    run_state: cut.components.run_state,
                    dialog_counts: cut.components.dialog_counts,
                })?;
                text_component(
                    cut.store,
                    run_id.clone(),
                    run_id.clone(),
                    &run_json,
                    &mut tracked,
                )
                .await?;
            }
            CatalogFamily::Lifecycle => {
                if let Some(reference) = &cut.components.lifecycle {
                    let values = cut.values;
                    steps_component(
                        cut.store,
                        lifecycle_id.clone(),
                        lifecycle_id.clone(),
                        &mut tracked,
                        |mut encoder| async move {
                            walk_value_steps(values, reference, valid, &mut encoder).await?;
                            Ok(encoder)
                        },
                    )
                    .await?;
                } else {
                    tracked
                        .accept(empty_component(lifecycle_id.clone()))
                        .await?;
                }
            }
            CatalogFamily::Dialogs => {
                dialog_components(&reader, cut.components.dialogs.as_ref(), &mut tracked).await?;
            }
            CatalogFamily::Active => {
                active_components(&reader, cut.active, &mut tracked).await?;
            }
            CatalogFamily::Calls => {
                calls_components(&reader, cut.calls, &mut tracked).await?;
            }
            CatalogFamily::Settled => {
                settled_components(&reader, cut.settled, &mut tracked).await?;
            }
            CatalogFamily::Identity => {
                identity_components(&reader, &identity, cut.settled, &mut tracked).await?;
            }
            CatalogFamily::Source => {
                source_components(
                    &reader,
                    &cut.root.transcript,
                    cut.accepted,
                    cut.hold,
                    &producers,
                    cut.settled,
                    &mut tracked,
                )
                .await?;
            }
        }
    }

    assert_coverage(&registry, &emitted)?;
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_call_model::CallCounters;
    use crate::compact_catalog_transcript::EmptyTailHold;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_settled_rows::SettledMember;
    use crate::session_acceptance_lane::{SessionAcceptanceLane, SessionBootstrap};

    #[tokio::test]
    async fn emits_all_ten_families_and_verifies_coverage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let values = CompactValues::new(store.clone());
        let mut lane = SessionAcceptanceLane::new(None, None);
        lane.bootstrap(SessionBootstrap::default(), Some("n1".into()));
        let root = lane.freeze();

        struct FakeSettled {
            store: CompactContentStore,
            values: CompactValues,
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
                Ok(Vec::new())
            }
            async fn message_of(&self, _entry_id: &str) -> Result<Option<String>, CompactError> {
                Ok(None)
            }
        }

        struct Recorder(Vec<(String, String)>);
        #[async_trait]
        impl CatalogComponentSink for Recorder {
            async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError> {
                self.0.push((
                    component.kind.as_str().to_owned(),
                    component.id.as_str().to_owned(),
                ));
                Ok(())
            }
        }

        let settled = FakeSettled {
            store: store.clone(),
            values: values.clone(),
        };
        let mut hold = EmptyTailHold::default();
        let components = FrozenComponents::default();
        let calls = FrozenCalls {
            sequence: 0,
            reference: None,
            counters: CallCounters::default(),
        };
        let mut cut = CatalogCut {
            store: &store,
            values: &values,
            root: &root,
            active: None,
            components: &components,
            calls: &calls,
            accepted: &[],
            hold: &mut hold,
            refused_correlations: 0,
            settled: &settled,
        };
        let ok = || -> Result<(), CompactError> { Ok(()) };
        let mut recorder = Recorder(Vec::new());
        let registry = checkpoint_components(&mut cut, &ok, &mut recorder)
            .await
            .expect("checkpoint");
        assert!(registry.is_complete());
        let kinds: HashSet<String> = recorder.0.iter().map(|(k, _)| k.clone()).collect();
        assert!(kinds.contains("families"));
        for f in CATALOG_FAMILIES {
            assert!(kinds.contains(f.as_str()), "missing family {}", f.as_str());
        }
        lane.release(&root);
        store.retire().await.expect("retire");
    }
}
