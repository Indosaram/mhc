//! senpi `modes/rpc/compact-catalog-steps.ts`: incremental value/text/literal step producers
//! for recovery catalog serialization with one-page bounded residency.

use crate::compact_content_store::{CompactContentStore, LeafId};
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_seeded_draft::SeededFields;
use crate::compact_text_roots::CompactTextRoots;
use crate::compact_values::{CompactValues, ValuePage, ValueRef};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Array,
    Object,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValueStep {
    Scalar(Value),
    String { text: String, more: bool },
    Open(ContainerKind),
    Close,
}

/// A walk step, or a chunk of stored JSON text spliced in verbatim as one value (`more` until
/// its last chunk).
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogStep {
    Value(ValueStep),
    Raw { text: String, more: bool },
}

/// Receives catalog steps one at a time so serializers never materialize a whole value or text.
#[async_trait]
pub trait StepSink: Send {
    async fn push(&mut self, step: CatalogStep) -> Result<(), CompactError>;
}

/// What every family adapter reads with: the cut's store, its value reader and the cut's validity
/// check.
pub struct CatalogReader<'a> {
    pub store: &'a CompactContentStore,
    pub values: &'a CompactValues,
    pub valid: &'a (dyn Fn() -> Result<(), CompactError> + Sync),
}

struct WalkFrame {
    kind: ContainerKind,
    items: Vec<ValueRef>,
    index: usize,
    next: Option<ValueRef>,
}

/// Incremental value walk: one page resident at a time and one frame per open container, whatever
/// the value's size or depth. `HeldPage` is dropped before pushing steps so nested walks never
/// stack page residency.
pub async fn walk_value_steps(
    values: &CompactValues,
    reference: &ValueRef,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    let mut stack: Vec<WalkFrame> = Vec::new();
    let mut pending: Option<ValueRef> = Some(reference.clone());
    let mut chunked = false;
    loop {
        if let Some(current) = pending.take() {
            if current.is_inline() {
                let value = current.value.unwrap_or(Value::Null);
                match value {
                    Value::String(text) => {
                        sink.push(CatalogStep::Value(ValueStep::String { text, more: false }))
                            .await?;
                    }
                    other => {
                        sink.push(CatalogStep::Value(ValueStep::Scalar(other)))
                            .await?;
                    }
                }
                continue;
            }
            let page = {
                let held = values.page(&current).await?;
                valid()?;
                held.value.clone()
            };
            if chunked && !matches!(page, ValuePage::String { .. }) {
                return Err(CompactError::read(CompactReadReason::Ownership));
            }
            match page {
                ValuePage::Scalar { value } => {
                    sink.push(CatalogStep::Value(ValueStep::Scalar(value)))
                        .await?;
                }
                ValuePage::String { text, next } => {
                    chunked = next.is_some();
                    pending = next;
                    sink.push(CatalogStep::Value(ValueStep::String {
                        text,
                        more: chunked,
                    }))
                    .await?;
                }
                ValuePage::Array { items, next } => {
                    stack.push(WalkFrame {
                        kind: ContainerKind::Array,
                        items,
                        index: 0,
                        next,
                    });
                    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Array)))
                        .await?;
                }
                ValuePage::Object { items, next } => {
                    let flat: Vec<ValueRef> = items.into_iter().flat_map(|(k, v)| [k, v]).collect();
                    stack.push(WalkFrame {
                        kind: ContainerKind::Object,
                        items: flat,
                        index: 0,
                        next,
                    });
                    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
                        .await?;
                }
            }
            continue;
        }
        let Some(frame) = stack.last_mut() else {
            return Ok(());
        };
        if frame.index < frame.items.len() {
            pending = Some(frame.items[frame.index].clone());
            frame.index += 1;
        } else if let Some(next_ref) = frame.next.take() {
            let page = {
                let held = values.page(&next_ref).await?;
                valid()?;
                held.value.clone()
            };
            match (frame.kind, page) {
                (ContainerKind::Array, ValuePage::Array { items, next }) => {
                    frame.items = items;
                    frame.index = 0;
                    frame.next = next;
                }
                (ContainerKind::Object, ValuePage::Object { items, next }) => {
                    frame.items = items.into_iter().flat_map(|(k, v)| [k, v]).collect();
                    frame.index = 0;
                    frame.next = next;
                }
                _ => return Err(CompactError::read(CompactReadReason::Ownership)),
            }
        } else {
            stack.pop();
            sink.push(CatalogStep::Value(ValueStep::Close)).await?;
        }
    }
}

enum LiteralChild<'a> {
    Key(&'a str),
    Val(&'a Value),
}

struct LiteralFrame<'a> {
    children: Vec<LiteralChild<'a>>,
    index: usize,
}

/// The walk steps of a JSON-normal value already in memory, produced iteratively with one frame
/// per open container, so a deep value never recurses.
pub async fn literal_steps(value: &Value, sink: &mut dyn StepSink) -> Result<(), CompactError> {
    let mut stack: Vec<LiteralFrame<'_>> = Vec::new();
    let mut pending: Option<LiteralChild<'_>> = Some(LiteralChild::Val(value));
    loop {
        if let Some(item) = pending.take() {
            match item {
                LiteralChild::Key(key) => {
                    sink.push(CatalogStep::Value(ValueStep::String {
                        text: key.to_owned(),
                        more: false,
                    }))
                    .await?;
                }
                LiteralChild::Val(Value::String(text)) => {
                    sink.push(CatalogStep::Value(ValueStep::String {
                        text: text.clone(),
                        more: false,
                    }))
                    .await?;
                }
                LiteralChild::Val(Value::Array(items)) => {
                    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Array)))
                        .await?;
                    stack.push(LiteralFrame {
                        children: items.iter().map(LiteralChild::Val).collect(),
                        index: 0,
                    });
                }
                LiteralChild::Val(Value::Object(map)) => {
                    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
                        .await?;
                    let mut children = Vec::with_capacity(map.len() * 2);
                    for (k, v) in map {
                        children.push(LiteralChild::Key(k.as_str()));
                        children.push(LiteralChild::Val(v));
                    }
                    stack.push(LiteralFrame { children, index: 0 });
                }
                LiteralChild::Val(scalar) => {
                    sink.push(CatalogStep::Value(ValueStep::Scalar(scalar.clone())))
                        .await?;
                }
            }
            continue;
        }
        let Some(frame) = stack.last_mut() else {
            return Ok(());
        };
        if frame.index < frame.children.len() {
            let next = match &frame.children[frame.index] {
                LiteralChild::Key(k) => LiteralChild::Key(k),
                LiteralChild::Val(v) => LiteralChild::Val(v),
            };
            frame.index += 1;
            pending = Some(next);
        } else {
            stack.pop();
            sink.push(CatalogStep::Value(ValueStep::Close)).await?;
        }
    }
}

/// Stored JSON text (one value split across leaves) as the value itself, one leaf resident at a
/// time.
pub async fn stored_steps(
    store: &CompactContentStore,
    leaves: &[LeafId],
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    for (index, &leaf) in leaves.iter().enumerate() {
        let text = store.read_leaf(leaf).await?;
        sink.push(CatalogStep::Raw {
            text,
            more: index + 1 < leaves.len(),
        })
        .await?;
    }
    Ok(())
}

/// Stored text ranges as one JSON string, chunk by chunk, then a literal suffix; an empty range is
/// `""`.
pub async fn text_steps(
    texts: &CompactTextRoots,
    roots: &[LeafId],
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    suffix: &str,
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    for &root in roots {
        for chunk in texts.chunks(root, valid).await? {
            sink.push(CatalogStep::Value(ValueStep::String {
                text: chunk,
                more: true,
            }))
            .await?;
        }
    }
    sink.push(CatalogStep::Value(ValueStep::String {
        text: suffix.to_owned(),
        more: false,
    }))
    .await
}

/// Seeded fields (a record of stored values) as one object, each value walked with one page
/// resident.
pub async fn field_steps(
    values: &CompactValues,
    fields: &SeededFields,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    sink.push(CatalogStep::Value(ValueStep::Open(ContainerKind::Object)))
        .await?;
    for (name, reference) in fields {
        sink.push(CatalogStep::Value(ValueStep::String {
            text: name.clone(),
            more: false,
        }))
        .await?;
        walk_value_steps(values, reference, valid, sink).await?;
    }
    sink.push(CatalogStep::Value(ValueStep::Close)).await
}

pub async fn walked_steps(
    values: &CompactValues,
    reference: Option<&ValueRef>,
    valid: &(dyn Fn() -> Result<(), CompactError> + Sync),
    sink: &mut dyn StepSink,
) -> Result<(), CompactError> {
    match reference {
        Some(reference) => walk_value_steps(values, reference, valid, sink).await,
        None => literal_steps(&Value::Null, sink).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    struct VecSink(Vec<CatalogStep>);

    #[async_trait]
    impl StepSink for VecSink {
        async fn push(&mut self, step: CatalogStep) -> Result<(), CompactError> {
            self.0.push(step);
            Ok(())
        }
    }

    #[tokio::test]
    async fn walks_paged_values_with_at_most_one_resident_page() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let values = CompactValues::new(store.clone());
        let big = "x".repeat(5000);
        let reference = values
            .own(&json!({"k": big, "arr": [1, true, null]}))
            .expect("own");
        let ok = || -> Result<(), CompactError> { Ok(()) };
        let mut sink = VecSink(Vec::new());
        walk_value_steps(&values, &reference, &ok, &mut sink)
            .await
            .expect("walk");
        assert_eq!(values.peak_resident_pages(), 1);
        assert_eq!(values.resident_pages(), 0);
        assert!(
            sink.0
                .iter()
                .any(|s| matches!(s, CatalogStep::Value(ValueStep::String { more: true, .. })))
        );
        store.adjust(reference.owner, -1, 0).expect("release");
        store.retire().await.expect("retire");
    }
}
