//! senpi `modes/rpc/compact-catalog-writer.ts`: writes catalog components as bounded store
//! leaves with one-page JSON streaming.

use crate::compact_catalog_codec::BoundedId;
use crate::compact_catalog_steps::{CatalogStep, ContainerKind, StepSink, ValueStep};
use crate::compact_content_store::{
    COMPACT_LEAF_CODE_UNITS, CompactContentStore, LeafId, utf16_boundary, utf16_len,
};
use crate::compact_read_lease::CompactError;
use async_trait::async_trait;

/// One component as a family adapter yields it: its stored leaves in order and the owner that
/// keeps them pinned (mirrors `compact-recovery-catalog.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogComponent {
    pub kind: BoundedId,
    pub id: BoundedId,
    pub leaves: Vec<LeafId>,
    pub owner: LeafId,
}

#[async_trait]
pub trait CatalogComponentSink: Send {
    async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError>;
}

/// Writes one catalog component as bounded store leaves: text arrives in fragments of any size and
/// leaves at most one open leaf (`<= COMPACT_LEAF_CODE_UNITS`) in memory.
pub struct CatalogComponentWriter {
    store: CompactContentStore,
    leaves: Vec<LeafId>,
    open: String,
}

impl CatalogComponentWriter {
    pub fn new(store: CompactContentStore) -> Self {
        Self {
            store,
            leaves: Vec::new(),
            open: String::new(),
        }
    }

    pub fn push(&mut self, text: &str) -> Result<(), CompactError> {
        let mut rest = text;
        while utf16_len(&self.open) + utf16_len(rest) > COMPACT_LEAF_CODE_UNITS as i64 {
            let take = COMPACT_LEAF_CODE_UNITS
                .saturating_sub(usize::try_from(utf16_len(&self.open)).unwrap_or(0));
            let split = utf16_boundary(rest, take);
            let split = if split == 0 && self.open.is_empty() {
                rest.chars().next().map_or(0, char::len_utf8)
            } else {
                split
            };
            let mut leaf = std::mem::take(&mut self.open);
            leaf.push_str(&rest[..split]);
            self.seal(&leaf)?;
            rest = &rest[split..];
        }
        self.open.push_str(rest);
        Ok(())
    }

    pub fn finish(
        mut self,
        kind: BoundedId,
        id: BoundedId,
    ) -> Result<CatalogComponent, CompactError> {
        if !self.open.is_empty() {
            let tail = std::mem::take(&mut self.open);
            self.seal(&tail)?;
        }
        let owner = self.store.text().own(&self.leaves)?;
        for &leaf in &self.leaves {
            self.store.adjust(leaf, -1, 0)?;
        }
        Ok(CatalogComponent {
            kind,
            id,
            leaves: std::mem::take(&mut self.leaves),
            owner,
        })
    }

    pub fn abort(&mut self) {
        for leaf in self.leaves.drain(..) {
            let _ = self.store.adjust(leaf, -1, 0);
        }
        self.open.clear();
    }

    fn seal(&mut self, text: &str) -> Result<(), CompactError> {
        let id = self.store.create_leaf(text, true)?;
        self.leaves.push(id);
        Ok(())
    }
}

struct Frame {
    container: ContainerKind,
    emitted: usize,
}

/// Streams `CatalogStep`s into canonical JSON fragments pushed into a `CatalogComponentWriter`.
pub struct CatalogJsonEncoder {
    writer: CatalogComponentWriter,
    stack: Vec<Frame>,
    in_string: bool,
    in_raw: bool,
}

impl CatalogJsonEncoder {
    pub fn new(store: CompactContentStore) -> Self {
        Self {
            writer: CatalogComponentWriter::new(store),
            stack: Vec::new(),
            in_string: false,
            in_raw: false,
        }
    }

    fn separator(&mut self) -> &'static str {
        let Some(frame) = self.stack.last_mut() else {
            return "";
        };
        let count = frame.emitted;
        frame.emitted += 1;
        match frame.container {
            ContainerKind::Array => {
                if count > 0 {
                    ","
                } else {
                    ""
                }
            }
            ContainerKind::Object => {
                if count == 0 {
                    ""
                } else if count.is_multiple_of(2) {
                    ","
                } else {
                    ":"
                }
            }
        }
    }

    pub fn finish(self, kind: BoundedId, id: BoundedId) -> Result<CatalogComponent, CompactError> {
        self.writer.finish(kind, id)
    }

    pub fn abort(&mut self) {
        self.writer.abort();
    }
}

#[async_trait]
impl StepSink for CatalogJsonEncoder {
    async fn push(&mut self, step: CatalogStep) -> Result<(), CompactError> {
        match step {
            CatalogStep::Value(ValueStep::Scalar(value)) => {
                let sep = self.separator();
                let serialized = serde_json::to_string(&value)?;
                self.writer.push(sep)?;
                self.writer.push(&serialized)?;
            }
            CatalogStep::Value(ValueStep::String { text, more }) => {
                if !self.in_string {
                    let sep = self.separator();
                    self.writer.push(sep)?;
                    self.writer.push("\"")?;
                }
                let escaped = serde_json::to_string(&text)?;
                let inner = &escaped[1..escaped.len() - 1];
                self.writer.push(inner)?;
                if !more {
                    self.writer.push("\"")?;
                }
                self.in_string = more;
            }
            CatalogStep::Raw { text, more } => {
                if !self.in_raw {
                    let sep = self.separator();
                    self.writer.push(sep)?;
                }
                self.writer.push(&text)?;
                self.in_raw = more;
            }
            CatalogStep::Value(ValueStep::Open(container)) => {
                let sep = self.separator();
                self.writer.push(sep)?;
                self.writer.push(match container {
                    ContainerKind::Array => "[",
                    ContainerKind::Object => "{",
                })?;
                self.stack.push(Frame {
                    container,
                    emitted: 0,
                });
            }
            CatalogStep::Value(ValueStep::Close) => {
                let closed = self.stack.pop().map(|f| f.container);
                self.writer.push(match closed {
                    Some(ContainerKind::Array) => "]",
                    _ => "}",
                })?;
            }
        }
        Ok(())
    }
}

/// One component as the catalog consumes it: `fill` writes its text, `sink` receives it once, and
/// its owner is always released immediately afterward.
pub async fn written_component<F, Fut>(
    store: &CompactContentStore,
    kind: BoundedId,
    id: BoundedId,
    sink: &mut dyn CatalogComponentSink,
    fill: F,
) -> Result<(), CompactError>
where
    F: FnOnce(CatalogComponentWriter) -> Fut,
    Fut: std::future::Future<Output = Result<CatalogComponentWriter, CompactError>>,
{
    let writer = fill(CatalogComponentWriter::new(store.clone())).await?;
    let component = writer.finish(kind, id)?;
    let owner = component.owner;
    let res = sink.accept(component).await;
    let release_res = store.adjust(owner, -1, 0);
    res?;
    release_res
}

/// A member whose value streams from steps: canonical JSON in bounded leaves with one value page
/// resident.
pub async fn steps_component<F, Fut>(
    store: &CompactContentStore,
    kind: BoundedId,
    id: BoundedId,
    sink: &mut dyn CatalogComponentSink,
    drive: F,
) -> Result<(), CompactError>
where
    F: FnOnce(CatalogJsonEncoder) -> Fut,
    Fut: std::future::Future<Output = Result<CatalogJsonEncoder, CompactError>>,
{
    let encoder = drive(CatalogJsonEncoder::new(store.clone())).await?;
    let component = encoder.finish(kind, id)?;
    let owner = component.owner;
    let res = sink.accept(component).await;
    let release_res = store.adjust(owner, -1, 0);
    res?;
    release_res
}

/// A family with no members: exactly one component `<family>/<family>` with no leaves.
pub fn empty_component(family: BoundedId) -> CatalogComponent {
    CatalogComponent {
        kind: family.clone(),
        id: family,
        leaves: Vec::new(),
        owner: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_catalog_codec::protocol_id;
    use crate::compact_catalog_steps::{literal_steps, walk_value_steps};
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_values::CompactValues;
    use serde_json::json;

    struct Collect(Vec<(CatalogComponent, String)>);

    #[async_trait]
    impl CatalogComponentSink for Collect {
        async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError> {
            self.0.push((component, String::new()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn encodes_chunked_strings_and_releases_component_owner_after_accept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let values = CompactValues::new(store.clone());
        let input = json!({"k": "a".repeat(5000), "n": [1, 2, true, null]});
        let reference = values.own(&input).expect("own");
        let ok = || -> Result<(), CompactError> { Ok(()) };

        struct Verifier {
            store: CompactContentStore,
            expected: serde_json::Value,
        }
        #[async_trait]
        impl CatalogComponentSink for Verifier {
            async fn accept(&mut self, component: CatalogComponent) -> Result<(), CompactError> {
                let mut text = String::new();
                for leaf in &component.leaves {
                    text.push_str(&self.store.read_leaf(*leaf).await?);
                }
                let parsed: serde_json::Value = serde_json::from_str(&text)?;
                assert_eq!(parsed, self.expected);
                Ok(())
            }
        }

        let mut verifier = Verifier {
            store: store.clone(),
            expected: input,
        };
        let reference_for_steps = reference.clone();
        steps_component(
            &store,
            protocol_id("active"),
            protocol_id("message-0"),
            &mut verifier,
            |mut encoder| async move {
                walk_value_steps(&values, &reference_for_steps, &ok, &mut encoder).await?;
                Ok(encoder)
            },
        )
        .await
        .expect("steps");

        let mut collector = Collect(Vec::new());
        steps_component(
            &store,
            protocol_id("run"),
            protocol_id("run"),
            &mut collector,
            |mut encoder| async move {
                literal_steps(&json!({"runState": "idle"}), &mut encoder).await?;
                Ok(encoder)
            },
        )
        .await
        .expect("literal");

        store.adjust(reference.owner, -1, 0).expect("release");
        store.idle().await;
        assert_eq!(store.status().structured_bytes.resident, 0);
        store.retire().await.expect("retire");
    }
}
