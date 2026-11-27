//! senpi `modes/rpc/compact-catalog-calls.ts`: the `calls` family, streaming stored call
//! headers and splicing their slot descriptors with one value page resident at a time.

use crate::compact_call_model::FrozenCalls;
use crate::compact_catalog_registry::{
    CatalogFamily, MemberOrdinal, MemberPrefix, family_id, member_id,
};
use crate::compact_catalog_steps::{
    CatalogReader, CatalogStep, ContainerKind, StepSink, ValueStep, literal_steps, text_steps,
    walk_value_steps, walked_steps,
};
use crate::compact_catalog_writer::{CatalogComponentSink, empty_component, steps_component};
use crate::compact_content_store::LeafId;
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_values::ValueRef;
use async_trait::async_trait;
use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Map, Value};

const SLOTS: [&str; 4] = ["raw", "hook", "progress", "result"];

#[derive(Deserialize)]
struct ComponentPage {
    key: ValueRef,
    value: ValueRef,
    left: Option<ValueRef>,
    right: Option<ValueRef>,
}

fn collect_component_rows<'a>(
    reader: &'a CatalogReader<'a>,
    reference: Option<&'a ValueRef>,
    out: &'a mut Vec<(String, ValueRef)>,
) -> BoxFuture<'a, Result<(), CompactError>> {
    Box::pin(async move {
        let Some(reference) = reference else {
            return Ok(());
        };
        (reader.valid)()?;
        let page: ComponentPage =
            serde_json::from_str(&reader.store.read_leaf(reference.node).await?)?;
        (reader.valid)()?;
        collect_component_rows(reader, page.left.as_ref(), out).await?;
        let Value::String(key) = reader.values.read(&page.key).await? else {
            return Err(CompactError::read(CompactReadReason::Ownership));
        };
        (reader.valid)()?;
        out.push((key, page.value));
        collect_component_rows(reader, page.right.as_ref(), out).await?;
        Ok(())
    })
}

fn ownership() -> CompactError {
    CompactError::read(CompactReadReason::Ownership)
}

fn leaf_id(value: Option<&Value>) -> Result<LeafId, CompactError> {
    value.and_then(Value::as_i64).ok_or_else(ownership)
}

fn flat_slot(steps: &[ValueStep]) -> Result<Option<Map<String, Value>>, CompactError> {
    if let [ValueStep::Scalar(Value::Null)] = steps {
        return Ok(None);
    }
    let Some(ValueStep::Open(ContainerKind::Object)) = steps.first() else {
        return Err(ownership());
    };
    let Some(ValueStep::Close) = steps.last() else {
        return Err(ownership());
    };
    let mut fields = Map::new();
    let mut key: Option<String> = None;
    let mut text = String::new();
    for step in &steps[1..steps.len() - 1] {
        match step {
            ValueStep::Open(_) | ValueStep::Close => return Err(ownership()),
            ValueStep::String { text: chunk, more } => {
                text.push_str(chunk);
                if *more {
                    continue;
                }
                let value = std::mem::take(&mut text);
                if let Some(name) = key.take() {
                    fields.insert(name, Value::String(value));
                } else {
                    key = Some(value);
                }
            }
            ValueStep::Scalar(value) => {
                let Some(name) = key.take() else {
                    return Err(ownership());
                };
                fields.insert(name, value.clone());
            }
        }
    }
    Ok(Some(fields))
}

async fn raw_steps(
    reader: &CatalogReader<'_>,
    raw: Option<&Map<String, Value>>,
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    let Some(raw) = raw else {
        return literal_steps(&Value::Null, sink).await;
    };
    if raw.get("kind").and_then(Value::as_str) != Some("owned") {
        return literal_steps(&Value::Object(raw.clone()), sink).await;
    }
    let root = leaf_id(raw.get("root"))?;
    let tail_root = leaf_id(raw.get("tailRoot"))?;
    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
        .await?;
    sink.push(CatalogStep::Value(ValueStep::String {
        text: "kind".into(),
        more: false,
    }))
    .await?;
    literal_steps(&Value::String("owned".into()), sink).await?;
    sink.push(CatalogStep::Value(ValueStep::String {
        text: "text".into(),
        more: false,
    }))
    .await?;
    text_steps(
        &reader.store.text(),
        &[root, tail_root],
        reader.valid,
        "",
        sink,
    )
    .await?;
    sink.push(CatalogStep::Value(ValueStep::Close)).await
}

fn slot_ref(slot: Option<&Map<String, Value>>) -> Result<Option<ValueRef>, CompactError> {
    let Some(slot) = slot else {
        return Ok(None);
    };
    if let Some(value) = slot.get("value") {
        return Ok(Some(ValueRef::inline(value.clone())));
    }
    Ok(Some(ValueRef {
        node: leaf_id(slot.get("node"))?,
        owner: leaf_id(slot.get("owner"))?,
        value: None,
    }))
}

async fn emit_slot_steps(
    reader: &CatalogReader<'_>,
    name: &str,
    slot: Option<&Map<String, Value>>,
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    if name == "raw" {
        raw_steps(reader, slot, sink).await
    } else {
        let reference = slot_ref(slot)?;
        walked_steps(reader.values, reference.as_ref(), reader.valid, sink).await
    }
}

struct HeaderStepFilter<'a> {
    reader: &'a CatalogReader<'a>,
    sink: &'a mut dyn StepSink,
    depth: usize,
    key: Option<String>,
    slot: Option<(String, Vec<ValueStep>)>,
}

#[async_trait]
impl StepSink for HeaderStepFilter<'_> {
    async fn push(&mut self, step: CatalogStep) -> Result<(), CompactError> {
        let CatalogStep::Value(step) = step else {
            return Err(ownership());
        };
        let outer = self.depth;
        match &step {
            ValueStep::Open(_) => self.depth += 1,
            ValueStep::Close => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        if outer == 0 || self.depth == 0 {
            return self.sink.push(CatalogStep::Value(step)).await;
        }
        if let Some(key) = self.key.as_mut() {
            let ValueStep::String { text, more } = &step else {
                return Err(ownership());
            };
            key.push_str(text);
            let more = *more;
            let full_key = if more { None } else { self.key.take() };
            self.sink.push(CatalogStep::Value(step)).await?;
            if let Some(full_key) = full_key {
                self.slot = SLOTS
                    .contains(&full_key.as_str())
                    .then(|| (full_key, Vec::new()));
            }
            return Ok(());
        }
        let finished_string = matches!(step, ValueStep::String { more: false, .. });
        let is_chunked_string = matches!(step, ValueStep::String { more: true, .. });
        if let Some((_, steps)) = self.slot.as_mut() {
            steps.push(step);
        } else {
            self.sink.push(CatalogStep::Value(step)).await?;
        }
        if self.depth != 1 || is_chunked_string {
            return Ok(());
        }
        let _ = finished_string;
        if let Some((name, steps)) = self.slot.take() {
            let flat = flat_slot(&steps)?;
            emit_slot_steps(self.reader, &name, flat.as_ref(), self.sink).await?;
        }
        self.key = Some(String::new());
        Ok(())
    }
}

async fn header_steps(
    reader: &CatalogReader<'_>,
    reference: &ValueRef,
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    let mut filter = HeaderStepFilter {
        reader,
        sink,
        depth: 0,
        key: Some(String::new()),
        slot: None,
    };
    walk_value_steps(reader.values, reference, reader.valid, &mut filter).await
}

/// The calls family: one `call-<creation ordinal>` member per outstanding call of the cut, or the
/// explicit empty.
pub async fn calls_components(
    reader: &CatalogReader<'_>,
    calls: &FrozenCalls,
    sink: &mut dyn CatalogComponentSink,
) -> Result<(), CompactError> {
    let calls_id = family_id(CatalogFamily::Calls);
    let mut rows = Vec::new();
    collect_component_rows(reader, calls.reference.as_ref(), &mut rows).await?;
    if rows.is_empty() {
        return sink.accept(empty_component(calls_id)).await;
    }
    for (order, reference) in rows {
        let ordinal = order.replace('.', "-");
        let member = member_id(MemberPrefix::Call, MemberOrdinal::Text(&ordinal));
        steps_component(
            reader.store,
            calls_id.clone(),
            member,
            sink,
            |mut encoder| async move {
                header_steps(reader, &reference, &mut encoder).await?;
                Ok(encoder)
            },
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_call_model::CallScope;
    use crate::compact_catalog_writer::CatalogComponent;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_session_calls::{CallStep, CompactSessionCalls, ExecutionStart};
    use crate::compact_values::CompactValues;
    use serde_json::json;

    #[tokio::test]
    async fn streams_call_headers_and_splices_slots_with_one_page_resident() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            crate::compact_content_store::CompactContentStore::new(CompactContentStoreOptions {
                dir: dir.path().join("epoch").display().to_string(),
                ..Default::default()
            });
        let values = CompactValues::new(store.clone());
        let mut calls = CompactSessionCalls::new(store.clone());
        let scope = CallScope {
            session_epoch: "E".into(),
            turn_id: Some("T".into()),
        };
        calls
            .execution_start(
                &CallStep {
                    sequence: 1,
                    scope: scope.clone(),
                },
                ExecutionStart {
                    tool_call_id: "tc1".into(),
                    tool_name: "read".into(),
                    args: json!({"path": "a"}),
                    parent_tool_call_id: None,
                    key: None,
                },
            )
            .expect("start");
        calls
            .execution_update(
                &CallStep { sequence: 2, scope },
                "tc1",
                &json!({"path": "a"}),
                &json!({"big": "x".repeat(5000)}),
            )
            .expect("update");
        let frozen = calls.freeze().expect("freeze");
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
        let mut sink = Capture(store.clone(), Vec::new());
        calls_components(&reader, &frozen, &mut sink)
            .await
            .expect("calls");
        assert_eq!(sink.1.len(), 1);
        assert_eq!(sink.1[0].0, "call-0000000000000001-0000");
        let parsed: Value = serde_json::from_str(&sink.1[0].1).expect("json");
        assert_eq!(parsed["progress"]["big"], "x".repeat(5000));
        assert_eq!(values.peak_resident_pages(), 1);
        assert_eq!(values.resident_pages(), 0);
        calls.release(&frozen).expect("release");
        store.retire().await.expect("retire");
    }
}
