//! senpi `modes/rpc/compact-catalog-source.ts`: the `source` metadata family (R4.B2, RULING-61 Q2).

use crate::accepted_transcript::TranscriptRoot;
use crate::compact_catalog_codec::protocol_id;
use crate::compact_catalog_registry::{
    CatalogFamily, FamilyProducer, FamilyProducers, MemberOrdinal, MemberPrefix, entry_family,
    family_id, member_id,
};
use crate::compact_catalog_steps::{
    CatalogReader, CatalogStep, ContainerKind, StepSink, ValueStep, literal_steps,
};
use crate::compact_catalog_transcript::{AcceptedEntryMeta, TailHold, base_row, text_component};
use crate::compact_catalog_writer::{CatalogComponentSink, empty_component, steps_component};
use crate::compact_read_lease::CompactError;
use crate::compact_settled_rows::SettledReader;
use serde_json::Value;

async fn push_head_fields(
    entry: &AcceptedEntryMeta,
    message_id: Option<&str>,
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    let fields: [(&str, Value); 5] = [
        ("index", Value::from(entry.index)),
        ("id", Value::String(entry.id.clone())),
        (
            "parentId",
            entry
                .parent_id
                .as_deref()
                .map_or(Value::Null, |p| Value::String(p.to_owned())),
        ),
        ("kind", Value::String(entry.kind.clone())),
        (
            "messageId",
            message_id.map_or(Value::Null, |m| Value::String(m.to_owned())),
        ),
    ];
    for (name, val) in fields {
        sink.push(CatalogStep::Value(ValueStep::String {
            text: name.to_owned(),
            more: false,
        }))
        .await?;
        literal_steps(&val, sink).await?;
    }
    Ok(())
}

async fn push_raw_chunks(chunks: Vec<String>, sink: &mut dyn StepSink) -> Result<(), CompactError> {
    if chunks.is_empty() {
        return sink
            .push(CatalogStep::Raw {
                text: String::new(),
                more: false,
            })
            .await;
    }
    let last = chunks.len() - 1;
    for (index, text) in chunks.into_iter().enumerate() {
        sink.push(CatalogStep::Raw {
            text,
            more: index < last,
        })
        .await?;
    }
    Ok(())
}

/// The source-metadata family (R4.B2, RULING-61 Q2).
pub async fn source_components(
    reader: &CatalogReader<'_>,
    transcript: &TranscriptRoot,
    accepted: &[AcceptedEntryMeta],
    hold: &mut dyn TailHold,
    producers: &FamilyProducers,
    settled: &dyn SettledReader,
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let result =
        source_components_inner(reader, transcript, accepted, hold, producers, settled, sink).await;
    hold.release();
    result
}

async fn source_components_inner(
    reader: &CatalogReader<'_>,
    transcript: &TranscriptRoot,
    accepted: &[AcceptedEntryMeta],
    hold: &dyn TailHold,
    producers: &FamilyProducers,
    settled: &dyn SettledReader,
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let source_id = family_id(CatalogFamily::Source);
    let base_id = protocol_id("base");
    let mut count = 0usize;
    let slice = if transcript.unavailable.is_none() {
        accepted
    } else {
        &[]
    };
    for entry in slice {
        (reader.valid)()?;
        if entry_family(&entry.kind) != CatalogFamily::Source {
            continue;
        }
        let message_id = settled.message_of(&entry.id).await?;
        let chunks = if hold.has(entry.index) {
            Some(hold.chunks(entry.index, reader.valid).await?)
        } else {
            None
        };
        let member = member_id(
            MemberPrefix::Entry,
            MemberOrdinal::Number(u64::try_from(entry.index).unwrap_or(u64::MAX)),
        );
        steps_component(
            reader.store,
            source_id.clone(),
            member,
            sink,
            |mut encoder| async move {
                encoder
                    .push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
                    .await?;
                push_head_fields(entry, message_id.as_deref(), &mut encoder).await?;
                encoder
                    .push(CatalogStep::Value(ValueStep::String {
                        text: "entry".into(),
                        more: false,
                    }))
                    .await?;
                match chunks {
                    Some(chunks) => push_raw_chunks(chunks, &mut encoder).await?,
                    None => literal_steps(&Value::Null, &mut encoder).await?,
                }
                encoder.push(CatalogStep::Value(ValueStep::Close)).await?;
                Ok(encoder)
            },
        )
        .await?;
        count += 1;
    }
    if let Some(base_text) = base_row(transcript) {
        text_component(reader.store, source_id, base_id, &base_text, sink).await?;
    } else if count == 0 && producers.source == FamilyProducer::Complete {
        sink.accept(empty_component(source_id)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accepted_transcript::AcceptedTranscript;
    use crate::compact_catalog_registry::FAMILY_PRODUCERS;
    use crate::compact_catalog_transcript::EmptyTailHold;
    use crate::compact_catalog_writer::CatalogComponent;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_settled_rows::SettledMember;
    use crate::compact_values::CompactValues;
    use async_trait::async_trait;
    use maho_core::session_transcript_journal::TranscriptTransition;

    struct FakeSettled {
        store: crate::compact_content_store::CompactContentStore,
        values: CompactValues,
    }
    #[async_trait]
    impl SettledReader for FakeSettled {
        fn store(&self) -> &crate::compact_content_store::CompactContentStore {
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
        async fn message_of(&self, entry_id: &str) -> Result<Option<String>, CompactError> {
            Ok((entry_id == "e1").then(|| "m1".to_owned()))
        }
    }

    #[tokio::test]
    async fn emits_source_members_with_entry_join_or_explicit_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            crate::compact_content_store::CompactContentStore::new(CompactContentStoreOptions {
                dir: dir.path().join("epoch").display().to_string(),
                ..Default::default()
            });
        let values = CompactValues::new(store.clone());
        let settled = FakeSettled {
            store: store.clone(),
            values: values.clone(),
        };
        let mut transcript = AcceptedTranscript::default();
        transcript.apply(&TranscriptTransition::Replace { base: None });
        let root = transcript.freeze();
        let ok = || -> Result<(), CompactError> { Ok(()) };
        let reader = CatalogReader {
            store: &store,
            values: &values,
            valid: &ok,
        };

        struct Capture(
            crate::compact_content_store::CompactContentStore,
            Vec<String>,
        );
        #[async_trait]
        impl CatalogComponentSink for Capture {
            async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError> {
                let mut text = String::new();
                for leaf in &component.leaves {
                    text.push_str(&self.0.read_leaf(*leaf).await?);
                }
                self.1.push(text);
                Ok(())
            }
        }
        let mut sink = Capture(store.clone(), Vec::new());
        let mut hold = EmptyTailHold::default();
        let accepted = vec![AcceptedEntryMeta {
            index: 1,
            id: "e1".into(),
            kind: "model_change".into(),
            parent_id: Some("e0".into()),
        }];
        source_components(
            &reader,
            &root,
            &accepted,
            &mut hold,
            &FAMILY_PRODUCERS,
            &settled,
            &mut sink,
        )
        .await
        .expect("source");
        let parsed: Value = serde_json::from_str(&sink.1[0]).expect("json");
        assert_eq!(parsed["kind"], "model_change");
        assert_eq!(parsed["messageId"], "m1");
        assert_eq!(parsed["entry"], Value::Null);
        store.retire().await.expect("retire");
    }
}
