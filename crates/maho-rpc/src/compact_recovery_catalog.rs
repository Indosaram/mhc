//! senpi `modes/rpc/compact-recovery-catalog.ts`: builds and reads immutable B-tree catalog
//! trees (R4.B1) with bounded node descriptors and verified pointer side-leaves.

use crate::compact_content_store::{COMPACT_LEAF_CODE_UNITS, CompactContentStore, LeafId};
use crate::compact_recovery_bytes::{
    CATALOG_FORMAT, CATALOG_VERSION, CancelSignal, CatalogDigester, CatalogDomain, CatalogEntry,
    FrozenBytes, ReadLeafFn, RecoveryBytesError, ValidCheck, catalog_digest, open_frozen_bytes,
    stored_bytes,
};
use crate::compact_recovery_ids::{
    BoundedId, CATALOG_NODE_MAX_ENTRIES, ContentDigest, ContentRef, child_ref, parse_bounded_id,
    parse_content_digest, protocol_id, ref_steps,
};
use futures::stream::{self, BoxStream, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const CATALOG_NODE_MAX_UNITS: usize = COMPACT_LEAF_CODE_UNITS;

/// One component as a family adapter yields it: its stored leaves in order and the owner that keeps them pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogComponent {
    pub kind: BoundedId,
    pub id: BoundedId,
    pub leaves: Vec<LeafId>,
    pub owner: LeafId,
}

/// A built catalog: the root node, the head of its private pointer leaves and the one owner of the whole tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRoot {
    pub ref_: ContentRef,
    pub node: LeafId,
    pub side: LeafId,
    pub side_digest: ContentDigest,
    pub owner: LeafId,
    pub digest: ContentDigest,
    pub length: usize,
    pub depth: usize,
    pub components: usize,
    pub nodes: usize,
}

pub enum CatalogRead {
    Node {
        ref_: ContentRef,
        entries: Vec<CatalogEntry>,
        digest: ContentDigest,
        length: usize,
    },
    Component {
        ref_: ContentRef,
        entry: CatalogEntry,
        bytes: Arc<dyn FrozenBytes>,
    },
}

/// The byte source of a read that must be a component; a node under a page ref is `Range`.
pub fn component_bytes(read: CatalogRead) -> Result<Arc<dyn FrozenBytes>, RecoveryBytesError> {
    match read {
        CatalogRead::Component { bytes, .. } => Ok(bytes),
        CatalogRead::Node { .. } => Err(RecoveryBytesError::Range),
    }
}

#[derive(Debug, Clone)]
pub struct PointerHead {
    pub side: LeafId,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone)]
enum Pointer {
    Node { node: LeafId, head: PointerHead },
    Component { leaves: Vec<LeafId> },
}

// --- Catalog node codec (from `compact-catalog-codec.ts`) ---

struct CatalogNodeWriter {
    parts: Vec<String>,
    units: usize,
}

impl CatalogNodeWriter {
    fn new() -> Self {
        let head_len = format!(
            "{{\"format\":\"{CATALOG_FORMAT}\",\"version\":{CATALOG_VERSION},\"entries\":["
        )
        .len();
        let tail_len = "]}".len();
        Self {
            parts: Vec::new(),
            units: head_len + tail_len,
        }
    }

    fn count(&self) -> usize {
        self.parts.len()
    }

    fn push(&mut self, entry: &CatalogEntry) -> bool {
        let part = format!(
            "{{\"kind\":\"{}\",\"id\":\"{}\",\"length\":{},\"digest\":\"{}\"}}",
            entry.kind.as_str(),
            entry.id.as_str(),
            entry.length,
            entry.digest.as_str()
        );
        let units = self.units + part.len() + usize::from(!self.parts.is_empty());
        if self.parts.len() == CATALOG_NODE_MAX_ENTRIES || units > CATALOG_NODE_MAX_UNITS {
            return false;
        }
        self.parts.push(part);
        self.units = units;
        true
    }

    fn encode(&self) -> String {
        format!(
            "{{\"format\":\"{CATALOG_FORMAT}\",\"version\":{CATALOG_VERSION},\"entries\":[{}]}}",
            self.parts.join(",")
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogEntry {
    kind: String,
    id: String,
    length: usize,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogNode {
    format: String,
    version: u32,
    entries: Vec<RawCatalogEntry>,
}

fn decode_catalog_node(text: &str) -> Result<Vec<CatalogEntry>, RecoveryBytesError> {
    let raw: RawCatalogNode =
        serde_json::from_str(text).map_err(|_| RecoveryBytesError::Integrity)?;
    if raw.format != CATALOG_FORMAT
        || raw.version != CATALOG_VERSION
        || raw.entries.len() > CATALOG_NODE_MAX_ENTRIES
    {
        return Err(RecoveryBytesError::Integrity);
    }
    let mut entries = Vec::with_capacity(raw.entries.len());
    for item in raw.entries {
        let kind = parse_bounded_id(&item.kind).map_err(|_| RecoveryBytesError::Integrity)?;
        let id = parse_bounded_id(&item.id).map_err(|_| RecoveryBytesError::Integrity)?;
        let digest =
            parse_content_digest(&item.digest).map_err(|_| RecoveryBytesError::Integrity)?;
        entries.push(CatalogEntry {
            kind,
            id,
            length: item.length,
            digest,
        });
    }
    Ok(entries)
}

// --- Catalog pointers B-tree (from `compact-catalog-pointers.ts`) ---

struct OpenPointerLeaf {
    tokens: Vec<String>,
    units: usize,
}

impl OpenPointerLeaf {
    fn new(height: usize) -> Self {
        let units = format!("{{\"h\":{height},\"v\":[]}}").len();
        Self {
            tokens: Vec::new(),
            units,
        }
    }
}

struct CatalogPointerWriter<'a> {
    open: Vec<OpenPointerLeaf>,
    create: &'a mut dyn FnMut(&str) -> Result<LeafId, RecoveryBytesError>,
}

impl<'a> CatalogPointerWriter<'a> {
    fn new(create: &'a mut dyn FnMut(&str) -> Result<LeafId, RecoveryBytesError>) -> Self {
        Self {
            open: Vec::new(),
            create,
        }
    }

    fn node(&mut self, node: LeafId, head: &PointerHead) -> Result<(), RecoveryBytesError> {
        self.push(
            0,
            format!(
                "{{\"n\":{node},\"s\":{},\"d\":\"{}\"}}",
                head.side,
                head.digest.as_str()
            ),
        )
    }

    fn component(&mut self, leaves: &[LeafId]) -> Result<(), RecoveryBytesError> {
        self.push(0, format!("{{\"c\":{}}}", leaves.len()))?;
        for leaf in leaves {
            self.push(0, leaf.to_string())?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<PointerHead, RecoveryBytesError> {
        self.ensure_level(0);
        let mut height = 0;
        loop {
            if height == self.open.len() - 1 {
                return self.seal(height);
            }
            let head = self.seal(height)?;
            let token = format!("{{\"s\":{},\"d\":\"{}\"}}", head.side, head.digest.as_str());
            self.push(height + 1, token)?;
            height += 1;
        }
    }

    fn ensure_level(&mut self, height: usize) {
        while self.open.len() <= height {
            self.open.push(OpenPointerLeaf::new(self.open.len()));
        }
    }

    fn push(&mut self, height: usize, token: String) -> Result<(), RecoveryBytesError> {
        self.ensure_level(height);
        let units = self.open[height].units
            + token.len()
            + usize::from(!self.open[height].tokens.is_empty());
        if units > COMPACT_LEAF_CODE_UNITS {
            let head = self.seal(height)?;
            let head_tok = format!("{{\"s\":{},\"d\":\"{}\"}}", head.side, head.digest.as_str());
            self.push(height + 1, head_tok)?;
            return self.push(height, token);
        }
        self.open[height].tokens.push(token);
        self.open[height].units = units;
        Ok(())
    }

    fn seal(&mut self, height: usize) -> Result<PointerHead, RecoveryBytesError> {
        let text = format!(
            "{{\"h\":{height},\"v\":[{}]}}",
            self.open[height].tokens.join(",")
        );
        self.open[height] = OpenPointerLeaf::new(height);
        let side = (self.create)(&text)?;
        let digest = catalog_digest(CatalogDomain::Pointer, &stored_bytes(&text));
        Ok(PointerHead { side, digest })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SideLeafJson {
    h: usize,
    v: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadEntryJson {
    s: LeafId,
    d: String,
}

async fn read_side_leaf(
    read: &ReadLeafFn,
    head: &PointerHead,
    expected_height: Option<usize>,
) -> Result<SideLeafJson, RecoveryBytesError> {
    let text = read(head.side).await?;
    if catalog_digest(CatalogDomain::Pointer, &stored_bytes(&text)) != head.digest {
        return Err(RecoveryBytesError::Integrity);
    }
    let parsed: SideLeafJson =
        serde_json::from_str(&text).map_err(|_| RecoveryBytesError::Integrity)?;
    if let Some(h) = expected_height
        && parsed.h != h
    {
        return Err(RecoveryBytesError::Integrity);
    }
    Ok(parsed)
}

async fn catalog_pointer(
    read: &ReadLeafFn,
    head: &PointerHead,
    target_step: usize,
) -> Result<Option<Pointer>, RecoveryBytesError> {
    let root = read_side_leaf(read, head, None).await?;
    let mut stack: Vec<(usize, Vec<Value>, usize)> = vec![(root.h, root.v, 0)];
    let mut step = 0usize;
    let mut active_component: Option<(usize, Vec<LeafId>)> = None;

    while let Some((height, items, idx)) = stack.last_mut() {
        if *idx >= items.len() {
            stack.pop();
            continue;
        }
        let item = items[*idx].clone();
        *idx += 1;

        if *height > 0 {
            let child_head: HeadEntryJson =
                serde_json::from_value(item).map_err(|_| RecoveryBytesError::Integrity)?;
            let digest =
                parse_content_digest(&child_head.d).map_err(|_| RecoveryBytesError::Integrity)?;
            let child = read_side_leaf(
                read,
                &PointerHead {
                    side: child_head.s,
                    digest,
                },
                Some(*height - 1),
            )
            .await?;
            stack.push((child.h, child.v, 0));
            continue;
        }

        if let Some((expected_count, ref mut leaves)) = active_component {
            let leaf_id = item
                .as_i64()
                .filter(|&id| id > 0)
                .ok_or(RecoveryBytesError::Integrity)?;
            leaves.push(leaf_id);
            if leaves.len() == expected_count {
                let (_, completed) = active_component.take().unwrap_or((0, Vec::new()));
                if step == target_step {
                    return Ok(Some(Pointer::Component { leaves: completed }));
                }
                step += 1;
            }
            continue;
        }

        let obj = item.as_object().ok_or(RecoveryBytesError::Integrity)?;
        if let Some(c_val) = obj.get("c")
            && obj.len() == 1
        {
            let count = c_val
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or(RecoveryBytesError::Integrity)?;
            if count == 0 {
                if step == target_step {
                    return Ok(Some(Pointer::Component { leaves: Vec::new() }));
                }
                step += 1;
            } else {
                active_component = Some((count, Vec::with_capacity(count)));
            }
        } else if obj.contains_key("n")
            && obj.contains_key("s")
            && obj.contains_key("d")
            && obj.len() == 3
        {
            let node = obj["n"]
                .as_i64()
                .filter(|&n| n > 0)
                .ok_or(RecoveryBytesError::Integrity)?;
            let side = obj["s"]
                .as_i64()
                .filter(|&s| s > 0)
                .ok_or(RecoveryBytesError::Integrity)?;
            let digest_str = obj["d"].as_str().ok_or(RecoveryBytesError::Integrity)?;
            let digest =
                parse_content_digest(digest_str).map_err(|_| RecoveryBytesError::Integrity)?;
            if step == target_step {
                return Ok(Some(Pointer::Node {
                    node,
                    head: PointerHead { side, digest },
                }));
            }
            step += 1;
        } else {
            return Err(RecoveryBytesError::Integrity);
        }
    }
    Ok(None)
}

struct Level {
    writer: CatalogNodeWriter,
    /// Records (0 = marker for pending side leaf, or encoded token stream) by building sides at seal time.
    pointer_ops: Vec<PointerOp>,
    owners: Vec<LeafId>,
}

struct CatalogBuildGuard<'a> {
    store: &'a CompactContentStore,
    buffered: &'a AtomicUsize,
    held: BTreeSet<LeafId>,
    levels: Vec<Level>,
}

impl Drop for CatalogBuildGuard<'_> {
    fn drop(&mut self) {
        let count: usize = self.levels.iter().map(|level| level.writer.count()).sum();
        self.buffered.fetch_sub(count, Ordering::SeqCst);
        for &id in &self.held {
            if let Err(error) = self.store.adjust(id, -1, 0) {
                eprintln!("catalog build cleanup failed: {error}");
            }
        }
    }
}

enum PointerOp {
    Component(Vec<LeafId>),
    Node(LeafId, PointerHead),
}

struct Sealed {
    entry: CatalogEntry,
    node: LeafId,
    head: PointerHead,
    owner: LeafId,
}

enum Child {
    Component(Vec<LeafId>),
    Node(LeafId, PointerHead),
}

/// Builds and reads immutable catalog trees (R4.B1).
pub struct CompactRecoveryCatalog {
    buffered: AtomicUsize,
    peak_buffered: AtomicUsize,
}

impl Default for CompactRecoveryCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl CompactRecoveryCatalog {
    pub fn new() -> Self {
        Self {
            buffered: AtomicUsize::new(0),
            peak_buffered: AtomicUsize::new(0),
        }
    }

    pub fn peak_buffered(&self) -> usize {
        self.peak_buffered.load(Ordering::SeqCst)
    }

    pub async fn build_iter(
        &self,
        store: &CompactContentStore,
        ref_: ContentRef,
        components: impl IntoIterator<Item = CatalogComponent>,
        valid: ValidCheck<'_>,
    ) -> Result<CatalogRoot, RecoveryBytesError> {
        let items: Vec<CatalogComponent> = components.into_iter().collect();
        let mut s = stream::iter(items).boxed();
        self.build(store, ref_, &mut s, valid).await
    }

    pub async fn build(
        &self,
        store: &CompactContentStore,
        ref_: ContentRef,
        components: &mut BoxStream<'_, CatalogComponent>,
        valid: ValidCheck<'_>,
    ) -> Result<CatalogRoot, RecoveryBytesError> {
        let mut guard = CatalogBuildGuard {
            store,
            buffered: &self.buffered,
            held: BTreeSet::new(),
            levels: Vec::new(),
        };
        let mut count = 0usize;
        let mut nodes = 0usize;

        async {
            while let Some(component) = components.next().await {
                let entry = self.describe(store, &component, valid).await?;
                Self::place(
                    store,
                    &self.buffered,
                    &self.peak_buffered,
                    &mut guard.held,
                    &mut guard.levels,
                    &mut nodes,
                    0,
                    entry,
                    Child::Component(component.leaves),
                    component.owner,
                )?;
                count += 1;
            }
            let mut level = 0usize;
            loop {
                if guard.levels.len() <= level {
                    guard.levels.push(Self::new_level());
                }
                let top = level == guard.levels.len() - 1;
                let sealed = Self::seal(
                    store,
                    &self.buffered,
                    &mut guard.held,
                    &mut guard.levels,
                    &mut nodes,
                    level,
                )?;
                if !top {
                    Self::place(
                        store,
                        &self.buffered,
                        &self.peak_buffered,
                        &mut guard.held,
                        &mut guard.levels,
                        &mut nodes,
                        level + 1,
                        sealed.entry,
                        Child::Node(sealed.node, sealed.head),
                        sealed.owner,
                    )?;
                    level += 1;
                    continue;
                }
                let root = self
                    .finish_root(store, ref_, sealed, level + 1, count, nodes)
                    .await?;
                guard.held.remove(&root.owner);
                return Ok(root);
            }
        }
        .await
    }

    fn new_level() -> Level {
        Level {
            writer: CatalogNodeWriter::new(),
            pointer_ops: Vec::new(),
            owners: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn place(
        store: &CompactContentStore,
        buffered: &AtomicUsize,
        peak_buffered: &AtomicUsize,
        held: &mut BTreeSet<LeafId>,
        levels: &mut Vec<Level>,
        nodes: &mut usize,
        level: usize,
        entry: CatalogEntry,
        child: Child,
        owner: LeafId,
    ) -> Result<(), RecoveryBytesError> {
        while levels.len() <= level {
            levels.push(Self::new_level());
        }
        if !levels[level].writer.push(&entry) {
            let sealed = Self::seal(store, buffered, held, levels, nodes, level)?;
            Self::place(
                store,
                buffered,
                peak_buffered,
                held,
                levels,
                nodes,
                level + 1,
                sealed.entry,
                Child::Node(sealed.node, sealed.head),
                sealed.owner,
            )?;
            if !levels[level].writer.push(&entry) {
                return Err(RecoveryBytesError::SizeLimit);
            }
        }
        let cur = buffered.fetch_add(1, Ordering::SeqCst) + 1;
        peak_buffered.fetch_max(cur, Ordering::SeqCst);
        if level == 0 && owner != 0 {
            store.adjust(owner, 1, 0)?;
            held.insert(owner);
        }
        levels[level].owners.push(owner);
        match child {
            Child::Component(leaves) => {
                levels[level].pointer_ops.push(PointerOp::Component(leaves))
            }
            Child::Node(node, head) => levels[level].pointer_ops.push(PointerOp::Node(node, head)),
        }
        Ok(())
    }

    fn seal(
        store: &CompactContentStore,
        buffered: &AtomicUsize,
        held: &mut BTreeSet<LeafId>,
        levels: &mut [Level],
        nodes: &mut usize,
        level: usize,
    ) -> Result<Sealed, RecoveryBytesError> {
        let sealed_level = std::mem::replace(&mut levels[level], Self::new_level());
        buffered.fetch_sub(sealed_level.writer.count(), Ordering::SeqCst);
        let text = sealed_level.writer.encode();
        let node = store.create_leaf(&text, true)?;
        held.insert(node);

        let mut sides = Vec::new();
        let mut create_side = |side_text: &str| -> Result<LeafId, RecoveryBytesError> {
            let side = store.create_leaf(side_text, true)?;
            held.insert(side);
            sides.push(side);
            Ok(side)
        };
        let mut ptr_writer = CatalogPointerWriter::new(&mut create_side);
        for op in &sealed_level.pointer_ops {
            match op {
                PointerOp::Component(leaves) => ptr_writer.component(leaves)?,
                PointerOp::Node(child_node, head) => ptr_writer.node(*child_node, head)?,
            }
        }
        let head = ptr_writer.finish()?;
        drop(ptr_writer);

        let mut all_refs = Vec::with_capacity(1 + sides.len() + sealed_level.owners.len());
        all_refs.push(node);
        all_refs.extend_from_slice(&sides);
        all_refs.extend_from_slice(&sealed_level.owners);
        let owner = store.text().own(&all_refs)?;
        held.insert(owner);
        for taken in all_refs {
            if taken != 0 {
                store.adjust(taken, -1, 0)?;
                held.remove(&taken);
            }
        }
        let bytes = stored_bytes(&text);
        *nodes += 1;
        let id = protocol_id(&format!("node-{:016}", *nodes));
        Ok(Sealed {
            entry: CatalogEntry {
                kind: protocol_id("node"),
                id,
                length: bytes.len(),
                digest: catalog_digest(CatalogDomain::Node, &bytes),
            },
            node,
            head,
            owner,
        })
    }

    pub async fn read(
        &self,
        store: &CompactContentStore,
        root: &CatalogRoot,
        ref_: &ContentRef,
        valid: ValidCheck<'_>,
        signal: Option<&CancelSignal>,
    ) -> Result<CatalogRead, RecoveryBytesError> {
        let root_prefix = format!("{}:", root.ref_.as_str());
        if ref_ != &root.ref_ && !ref_.as_str().starts_with(&root_prefix) {
            return Err(RecoveryBytesError::Range);
        }
        let steps = ref_steps(ref_);
        let mut node = root.node;
        let mut head = PointerHead {
            side: root.side,
            digest: root.side_digest.clone(),
        };
        let mut expected_digest = root.digest.clone();
        let mut expected_length = root.length;
        let mut here = root.ref_.clone();

        let stored_read: ReadLeafFn = {
            let store = store.clone();
            Arc::new(move |id| {
                let store = store.clone();
                Box::pin(async move {
                    match store.read_leaf(id).await {
                        Ok(text) => Ok(text),
                        Err(e)
                            if e.read_reason()
                                == Some(
                                    crate::compact_read_lease::CompactReadReason::Ownership,
                                ) =>
                        {
                            Err(RecoveryBytesError::Integrity)
                        }
                        Err(e) => Err(e.into()),
                    }
                })
            })
        };

        for depth in 0.. {
            let text = if depth == 0 {
                store.read_leaf(node).await?
            } else {
                stored_read(node).await?
            };
            let domain = if depth == 0 {
                CatalogDomain::Root
            } else {
                CatalogDomain::Node
            };
            if catalog_digest(domain, &stored_bytes(&text)) != expected_digest {
                return Err(RecoveryBytesError::Integrity);
            }
            let entries = decode_catalog_node(&text)?;
            let Some(&step) = steps.get(depth) else {
                return Ok(CatalogRead::Node {
                    ref_: here,
                    entries,
                    digest: expected_digest,
                    length: expected_length,
                });
            };
            let Some(entry) = entries.get(step).cloned() else {
                return Err(RecoveryBytesError::Range);
            };
            let pointer = catalog_pointer(&stored_read, &head, step)
                .await?
                .ok_or(RecoveryBytesError::Integrity)?;
            here = child_ref(&here, step);
            match pointer {
                Pointer::Component { leaves } => {
                    if depth + 1 != steps.len() {
                        return Err(RecoveryBytesError::Range);
                    }
                    let bytes = open_frozen_bytes(
                        entry.length,
                        &entry.digest,
                        leaves,
                        stored_read.clone(),
                        valid,
                        signal,
                    )
                    .await?;
                    return Ok(CatalogRead::Component {
                        ref_: here,
                        entry,
                        bytes,
                    });
                }
                Pointer::Node {
                    node: next_node,
                    head: next_head,
                } => {
                    node = next_node;
                    head = next_head;
                    expected_digest = entry.digest;
                    expected_length = entry.length;
                }
            }
        }
        unreachable!()
    }

    pub fn release(&self, store: &CompactContentStore, root: &CatalogRoot) {
        let _ = store.adjust(root.owner, -1, 0);
    }

    async fn describe(
        &self,
        store: &CompactContentStore,
        component: &CatalogComponent,
        valid: ValidCheck<'_>,
    ) -> Result<CatalogEntry, RecoveryBytesError> {
        let mut length = 0usize;
        for &leaf in &component.leaves {
            let units = store.leaf_units(leaf)?;
            length += usize::try_from(units * 2).unwrap_or(0);
        }
        let mut digester = CatalogDigester::new(CatalogDomain::Leaf, length);
        for &leaf in &component.leaves {
            let text = store.read_leaf(leaf).await?;
            digester.update(&stored_bytes(&text));
            valid()?;
        }
        Ok(CatalogEntry {
            kind: component.kind.clone(),
            id: component.id.clone(),
            length,
            digest: digester.finish()?,
        })
    }

    async fn finish_root(
        &self,
        store: &CompactContentStore,
        ref_: ContentRef,
        sealed: Sealed,
        depth: usize,
        components: usize,
        nodes: usize,
    ) -> Result<CatalogRoot, RecoveryBytesError> {
        let text = store.read_leaf(sealed.node).await?;
        let bytes = stored_bytes(&text);
        let digest = catalog_digest(CatalogDomain::Root, &bytes);
        Ok(CatalogRoot {
            ref_,
            node: sealed.node,
            side: sealed.head.side,
            side_digest: sealed.head.digest,
            owner: sealed.owner,
            digest,
            length: bytes.len(),
            depth,
            components,
            nodes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_recovery_bytes::produce_page;
    use crate::compact_recovery_ids::catalog_root_ref;

    fn ok_valid() -> Result<(), RecoveryBytesError> {
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_build_releases_held_components_and_buffer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let catalog = CompactRecoveryCatalog::new();
        let leaf = store.create_leaf("catalog item", false).expect("leaf");
        let component = CatalogComponent {
            kind: protocol_id("entry"),
            id: protocol_id("e1"),
            leaves: vec![leaf],
            owner: leaf,
        };
        let mut components = stream::iter([component]).chain(stream::pending()).boxed();
        let mut build =
            Box::pin(catalog.build(&store, catalog_root_ref(1), &mut components, &ok_valid));
        std::future::poll_fn(|cx| {
            assert!(build.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(catalog.buffered.load(Ordering::SeqCst), 1);
        drop(build);
        store.adjust(leaf, -1, 0).expect("producer release");
        assert_eq!(catalog.buffered.load(Ordering::SeqCst), 0);
        assert_eq!(store.status().resident_payload_bytes, 0);
        store.idle().await;
        store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn builds_multi_level_catalog_reads_verified_components_and_releases_to_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let catalog = CompactRecoveryCatalog::new();

        // >128 components forces 2 levels!
        let mut comps = Vec::new();
        for i in 0..130 {
            let leaf = store.create_leaf(&format!("val-{i}"), false).expect("leaf");
            comps.push(CatalogComponent {
                kind: protocol_id("entry"),
                id: protocol_id(&format!("e-{i}")),
                leaves: vec![leaf],
                owner: leaf,
            });
        }
        let root_ref = catalog_root_ref(1);
        let root = catalog
            .build_iter(&store, root_ref.clone(), comps.clone(), &ok_valid)
            .await
            .expect("build");
        // Drop producer's refs; catalog root owner keeps everything alive.
        for c in &comps {
            store.adjust(c.owner, -1, 0).expect("drop producer");
        }
        assert_eq!(root.components, 130);
        assert_eq!(root.depth, 2);

        let top = catalog
            .read(&store, &root, &root_ref, &ok_valid, None)
            .await
            .expect("read root");
        assert!(matches!(top, CatalogRead::Node { entries, .. } if entries.len() == 2));

        let c129_ref = child_ref(&child_ref(&root_ref, 1), 1);
        let read_comp = catalog
            .read(&store, &root, &c129_ref, &ok_valid, None)
            .await
            .expect("read component 129");
        let bytes = component_bytes(read_comp).expect("component bytes");
        let page = produce_page(bytes.as_ref(), 0, Some(&ok_valid))
            .await
            .expect("page");
        assert_eq!(page.bytes, stored_bytes("val-129"));

        let missing_ref = child_ref(&child_ref(&root_ref, 1), 50);
        assert!(matches!(
            catalog
                .read(&store, &root, &missing_ref, &ok_valid, None)
                .await,
            Err(RecoveryBytesError::Range)
        ));

        catalog.release(&store, &root);
        store.idle().await;
        let status = store.status();
        assert_eq!(status.resident_payload_bytes, 0);
        assert_eq!(status.structured_bytes.resident, 0);
        store.retire().await.expect("retire");
    }
}
