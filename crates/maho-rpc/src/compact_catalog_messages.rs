//! senpi `modes/rpc/compact-catalog-messages.ts`: the `active` family (R4.B2, R3.C6.4).

use crate::compact_catalog_registry::{
    CatalogFamily, MemberOrdinal, MemberPrefix, family_id, member_id,
};
use crate::compact_catalog_steps::{
    CatalogReader, CatalogStep, ContainerKind, StepSink, ValueStep, field_steps, literal_steps,
    text_steps,
};
use crate::compact_catalog_writer::{CatalogComponentSink, empty_component, steps_component};
use crate::compact_component_roots::CompactComponentRoots;
use crate::compact_read_lease::CompactError;
use crate::compact_seeded_block::FrozenSeededBlock;
use crate::compact_seeded_draft::SeededFields;
use crate::compact_state_view::MessageRoot;
use crate::compact_text_view::FrozenBlock;
use crate::compact_values::ValueRef;
use serde_json::Value;

async fn push_field_label(name: &str, sink: &mut dyn StepSink) -> Result<(), CompactError> {
    sink.push(CatalogStep::Value(ValueStep::String {
        text: name.to_owned(),
        more: false,
    }))
    .await
}

/// One live block: its public descriptor, its full text, and optional seeded state.
pub async fn block_steps(
    reader: &CatalogReader<'_>,
    block: &FrozenBlock,
    seeded: Option<&FrozenSeededBlock>,
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
        .await?;
    push_field_label("contentIndex", sink).await?;
    literal_steps(&Value::from(block.content_index), sink).await?;
    push_field_label("kind", sink).await?;
    literal_steps(&serde_json::to_value(block.kind)?, sink).await?;
    push_field_label("text", sink).await?;
    let roots = [block.root, block.tail_root.unwrap_or(0)];
    text_steps(
        &reader.store.text(),
        &roots,
        reader.valid,
        &block.tail,
        sink,
    )
    .await?;
    push_field_label("seeded", sink).await?;
    if let Some(seeded) = seeded {
        sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
            .await?;
        push_field_label("type", sink).await?;
        literal_steps(&Value::String(seeded.block_type.clone()), sink).await?;
        push_field_label("phase", sink).await?;
        literal_steps(&serde_json::to_value(seeded.phase)?, sink).await?;
        push_field_label("parsedThrough", sink).await?;
        literal_steps(&Value::from(seeded.parsed_through), sink).await?;
        push_field_label("fields", sink).await?;
        field_steps(reader.values, &seeded.fields, reader.valid, sink).await?;
        push_field_label("signature", sink).await?;
        let sig_roots = [seeded.signature.root, seeded.signature.tail_root];
        text_steps(&reader.store.text(), &sig_roots, reader.valid, "", sink).await?;
        sink.push(CatalogStep::Value(ValueStep::Close)).await?;
    } else {
        literal_steps(&Value::Null, sink).await?;
    }
    sink.push(CatalogStep::Value(ValueStep::Close)).await
}

pub async fn message_steps(
    reader: &CatalogReader<'_>,
    row: &MessageRoot,
    fields: Option<&SeededFields>,
    correlation: Option<&SeededFields>,
    blocks: &CompactComponentRoots<FrozenBlock>,
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
        .await?;
    push_field_label("messageId", sink).await?;
    literal_steps(&Value::String(row.message_id.clone()), sink).await?;
    push_field_label("fields", sink).await?;
    match fields {
        Some(fields) => field_steps(reader.values, fields, reader.valid, sink).await?,
        None => literal_steps(&Value::Null, sink).await?,
    }
    push_field_label("correlation", sink).await?;
    match correlation {
        Some(correlation) => field_steps(reader.values, correlation, reader.valid, sink).await?,
        None => literal_steps(&Value::Null, sink).await?,
    }
    push_field_label("blocks", sink).await?;
    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Array)))
        .await?;
    for (_, block) in blocks.entries(row.blocks.as_ref(), reader.valid).await? {
        block_steps(reader, &block, None, sink).await?;
    }
    sink.push(CatalogStep::Value(ValueStep::Close)).await?;
    sink.push(CatalogStep::Value(ValueStep::Close)).await
}

/// The active family (R4.B2, R3.C6.4): one `message-<order>` member per live message of the cut,
/// or the explicit empty when none is live.
pub async fn active_components(
    reader: &CatalogReader<'_>,
    active: Option<&ValueRef>,
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let active_id = family_id(CatalogFamily::Active);
    let messages = CompactComponentRoots::<MessageRoot>::new(reader.store.clone(), |_| Vec::new());
    let blocks = CompactComponentRoots::<FrozenBlock>::new(reader.store.clone(), |_| Vec::new());
    let mut count = 0usize;
    for (order, row) in messages.entries(active, reader.valid).await? {
        let ordinal: u64 = order.parse().unwrap_or(0);
        let member = member_id(MemberPrefix::Message, MemberOrdinal::Number(ordinal));
        steps_component(
            reader.store,
            active_id.clone(),
            member,
            sink,
            |mut encoder| {
                let blocks_ref = &blocks;
                async move {
                    message_steps(reader, &row, None, None, blocks_ref, &mut encoder).await?;
                    Ok(encoder)
                }
            },
        )
        .await?;
        count += 1;
    }
    if count == 0 {
        sink.accept(empty_component(active_id)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_catalog_writer::CatalogComponent;
    use crate::compact_component_roots::ComponentRoot;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_transition::CompactBlockKind;
    use crate::compact_values::CompactValues;
    use async_trait::async_trait;

    #[tokio::test]
    async fn emits_active_message_or_explicit_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            crate::compact_content_store::CompactContentStore::new(CompactContentStoreOptions {
                dir: dir.path().join("epoch").display().to_string(),
                ..Default::default()
            });
        let values = CompactValues::new(store.clone());
        let ok = || -> Result<(), CompactError> { Ok(()) };
        let reader = CatalogReader {
            store: &store,
            values: &values,
            valid: &ok,
        };

        struct Capture(
            crate::compact_content_store::CompactContentStore,
            Vec<(String, String)>,
        );
        #[async_trait]
        impl CatalogComponentSink for Capture {
            async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError> {
                let mut text = String::new();
                for leaf in &component.leaves {
                    text.push_str(&self.0.read_leaf(*leaf).await?);
                }
                self.1.push((component.id.as_str().to_owned(), text));
                Ok(())
            }
        }

        let mut empty_sink = Capture(store.clone(), Vec::new());
        active_components(&reader, None, &mut empty_sink)
            .await
            .expect("empty");
        assert_eq!(empty_sink.1, vec![("active".into(), String::new())]);

        let block_roots = CompactComponentRoots::new(store.clone(), |b: &FrozenBlock| {
            vec![b.root, b.tail_root.unwrap_or(0)]
        });
        let message_roots = CompactComponentRoots::new(store.clone(), |m: &MessageRoot| {
            vec![m.blocks.as_ref().map_or(0, |b| b.owner)]
        });
        let br = block_roots
            .set(
                &ComponentRoot::default(),
                "0000000000000000",
                Some(FrozenBlock {
                    content_index: 0,
                    kind: CompactBlockKind::Text,
                    root: 0,
                    tail: "hi".into(),
                    tail_root: None,
                }),
            )
            .expect("br");
        let mr = message_roots
            .set(
                &ComponentRoot::default(),
                "0000000000000001",
                Some(MessageRoot {
                    message_id: "m1".into(),
                    blocks: br.reference.clone(),
                }),
            )
            .expect("mr");

        let mut sink = Capture(store.clone(), Vec::new());
        active_components(&reader, mr.reference.as_ref(), &mut sink)
            .await
            .expect("active");
        assert_eq!(sink.1[0].0, "message-0000000000000001");
        let parsed: Value = serde_json::from_str(&sink.1[0].1).expect("json");
        assert_eq!(parsed["messageId"], "m1");
        assert_eq!(parsed["blocks"][0]["text"], "hi");

        block_roots.release(br);
        message_roots.release(mr);
        store.idle().await;
        store.retire().await.expect("retire");
    }
}
