//! senpi `modes/rpc/compact-catalog-pointers.ts`: a catalog node's private pointers stored as
//! a bounded tree of verified side leaves.

use crate::compact_catalog_codec::{
    CatalogDomain, ContentDigest, catalog_digest, parse_content_digest, stored_bytes,
};
use crate::compact_content_store::{COMPACT_LEAF_CODE_UNITS, LeafId};
use crate::compact_read_lease::CompactError;
use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;
use std::future::Future;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerHead {
    pub side: LeafId,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pointer {
    Node { node: LeafId, head: PointerHead },
    Component { leaves: Vec<LeafId> },
}

struct OpenLeaf {
    tokens: Vec<String>,
    units: usize,
}

fn open_leaf(height: usize) -> OpenLeaf {
    let shell = format!("{{\"h\":{height},\"v\":[]}}");
    OpenLeaf {
        tokens: Vec::new(),
        units: shell.len(),
    }
}

fn head_token(head: &PointerHead) -> String {
    format!("{{\"s\":{},\"d\":\"{}\"}}", head.side, head.digest)
}

/// Writes a catalog node's private pointer tree as bounded side leaves.
pub struct CatalogPointerWriter<F> {
    open: Vec<OpenLeaf>,
    create: F,
}

impl<F> CatalogPointerWriter<F>
where
    F: FnMut(&str) -> Result<LeafId, CompactError>,
{
    pub fn new(create: F) -> Self {
        Self {
            open: Vec::new(),
            create,
        }
    }

    pub fn node(&mut self, node: LeafId, head: &PointerHead) -> Result<(), CompactError> {
        let token = format!(
            "{{\"n\":{node},\"s\":{},\"d\":\"{}\"}}",
            head.side, head.digest
        );
        self.push(0, &token)
    }

    pub fn component(&mut self, leaves: &[LeafId]) -> Result<(), CompactError> {
        self.push(0, &format!("{{\"c\":{}}}", leaves.len()))?;
        for leaf in leaves {
            self.push(0, &leaf.to_string())?;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<PointerHead, CompactError> {
        self.ensure_level(0);
        let mut height = 0;
        loop {
            if height == self.open.len() - 1 {
                return self.seal(height);
            }
            let head = self.seal(height)?;
            self.push(height + 1, &head_token(&head))?;
            height += 1;
        }
    }

    fn ensure_level(&mut self, height: usize) {
        while self.open.len() <= height {
            self.open.push(open_leaf(self.open.len()));
        }
    }

    fn push(&mut self, height: usize, token: &str) -> Result<(), CompactError> {
        self.ensure_level(height);
        let units = {
            let open = &self.open[height];
            open.units + token.len() + usize::from(!open.tokens.is_empty())
        };
        if units > COMPACT_LEAF_CODE_UNITS {
            let sealed = self.seal(height)?;
            self.push(height + 1, &head_token(&sealed))?;
            return self.push(height, token);
        }
        let open = &mut self.open[height];
        open.tokens.push(token.to_owned());
        open.units = units;
        Ok(())
    }

    fn seal(&mut self, height: usize) -> Result<PointerHead, CompactError> {
        let tokens = std::mem::take(&mut self.open[height].tokens);
        self.open[height] = open_leaf(height);
        let text = format!("{{\"h\":{height},\"v\":[{}]}}", tokens.join(","));
        let side = (self.create)(&text)?;
        let digest = catalog_digest(CatalogDomain::Pointer, &stored_bytes(&text))?;
        Ok(PointerHead { side, digest })
    }
}

fn corrupt() -> CompactError {
    CompactError::Io("compact read refused (integrity)".into())
}

fn parse_digest(raw: &str) -> Result<ContentDigest, CompactError> {
    parse_content_digest(raw).map_err(|_| corrupt())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SideLeafRaw {
    h: u64,
    v: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexEntryRaw {
    s: i64,
    d: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentCountRaw {
    c: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeChildRaw {
    n: i64,
    s: i64,
    d: String,
}

#[derive(Debug)]
enum DataToken {
    Leaf(LeafId),
    ComponentCount(usize),
    Node {
        n: LeafId,
        s: LeafId,
        d: ContentDigest,
    },
}

fn decode_data_tokens(values: Vec<Value>) -> Result<Vec<DataToken>, CompactError> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        if let Some(id) = value.as_i64() {
            if id <= 0 {
                return Err(corrupt());
            }
            out.push(DataToken::Leaf(id));
            continue;
        }
        if let Ok(comp) = serde_json::from_value::<ComponentCountRaw>(value.clone()) {
            out.push(DataToken::ComponentCount(comp.c));
            continue;
        }
        if let Ok(node) = serde_json::from_value::<NodeChildRaw>(value) {
            if node.n <= 0 || node.s <= 0 {
                return Err(corrupt());
            }
            out.push(DataToken::Node {
                n: node.n,
                s: node.s,
                d: parse_digest(&node.d)?,
            });
            continue;
        }
        return Err(corrupt());
    }
    Ok(out)
}

fn decode_index_entries(values: Vec<Value>) -> Result<Vec<PointerHead>, CompactError> {
    if values.is_empty() {
        return Err(corrupt());
    }
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let raw = serde_json::from_value::<IndexEntryRaw>(value).map_err(|_| corrupt())?;
        if raw.s <= 0 {
            return Err(corrupt());
        }
        out.push(PointerHead {
            side: raw.s,
            digest: parse_digest(&raw.d)?,
        });
    }
    Ok(out)
}

struct ScanDriver {
    step: usize,
    child: usize,
    skip: usize,
    wanted: usize,
    leaves: Option<Vec<LeafId>>,
}

impl ScanDriver {
    fn new(step: usize) -> Self {
        Self {
            step,
            child: 0,
            skip: 0,
            wanted: 0,
            leaves: None,
        }
    }

    fn feed(&mut self, token: DataToken) -> Result<Option<Pointer>, CompactError> {
        if self.leaves.is_some() || self.skip > 0 {
            let DataToken::Leaf(leaf) = token else {
                return Err(corrupt());
            };
            if let Some(leaves) = self.leaves.as_mut() {
                leaves.push(leaf);
                if leaves.len() == self.wanted {
                    return Ok(Some(Pointer::Component {
                        leaves: std::mem::take(leaves),
                    }));
                }
            } else {
                self.skip -= 1;
            }
            return Ok(None);
        }
        let here = self.child;
        self.child += 1;
        match token {
            DataToken::Leaf(_) => Err(corrupt()),
            DataToken::Node { n, s, d } => {
                if here == self.step {
                    Ok(Some(Pointer::Node {
                        node: n,
                        head: PointerHead { side: s, digest: d },
                    }))
                } else {
                    Ok(None)
                }
            }
            DataToken::ComponentCount(c) => {
                if here != self.step {
                    self.skip = c;
                    Ok(None)
                } else if c == 0 {
                    Ok(Some(Pointer::Component { leaves: Vec::new() }))
                } else {
                    self.leaves = Some(Vec::with_capacity(c));
                    self.wanted = c;
                    Ok(None)
                }
            }
        }
    }

    fn unfinished(&self) -> bool {
        self.leaves.is_some() || self.skip > 0
    }
}

fn scan_leaf<'a, F, Fut>(
    read: &'a F,
    head: PointerHead,
    expected_height: Option<u64>,
    driver: &'a mut ScanDriver,
) -> BoxFuture<'a, Result<Option<Pointer>, CompactError>>
where
    F: Fn(LeafId) -> Fut + Sync,
    Fut: Future<Output = Result<String, CompactError>> + Send + 'a,
{
    Box::pin(async move {
        let text = read(head.side).await?;
        if catalog_digest(CatalogDomain::Pointer, &stored_bytes(&text))? != head.digest {
            return Err(corrupt());
        }
        let leaf: SideLeafRaw = serde_json::from_str(&text).map_err(|_| corrupt())?;
        if expected_height.is_some_and(|h| leaf.h != h) {
            return Err(corrupt());
        }
        if leaf.h == 0 {
            for token in decode_data_tokens(leaf.v)? {
                if let Some(found) = driver.feed(token)? {
                    return Ok(Some(found));
                }
            }
            return Ok(None);
        }
        for below in decode_index_entries(leaf.v)? {
            if let Some(found) = scan_leaf(read, below, Some(leaf.h - 1), driver).await? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    })
}

/// The pointer of child `step`, or `None` past the last child; a malformed list is `integrity`.
pub async fn catalog_pointer<F, Fut>(
    read: F,
    head: &PointerHead,
    step: usize,
) -> Result<Option<Pointer>, CompactError>
where
    F: Fn(LeafId) -> Fut + Sync,
    Fut: Future<Output = Result<String, CompactError>> + Send,
{
    let mut driver = ScanDriver::new(step);
    let found = scan_leaf(&read, head.clone(), None, &mut driver).await?;
    if found.is_some() {
        return Ok(found);
    }
    if driver.unfinished() {
        return Err(corrupt());
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[tokio::test]
    async fn spills_long_pointer_lists_across_height_levels_and_verifies_digests() {
        let leaves = Mutex::new(HashMap::<LeafId, String>::new());
        let mut next_id: LeafId = 1;
        let mut writer = CatalogPointerWriter::new(|text| {
            assert!(text.len() <= COMPACT_LEAF_CODE_UNITS);
            let id = next_id;
            next_id += 1;
            leaves
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id, text.to_owned());
            Ok(id)
        });

        // Write enough component leaves to force multi-leaf / multi-height side tree.
        for i in 0..3000i64 {
            writer
                .component(&[i * 2 + 1, i * 2 + 2])
                .expect("component");
        }
        let child_head = PointerHead {
            side: 9999,
            digest: parse_content_digest(&"ab".repeat(32)).expect("digest"),
        };
        writer.node(777, &child_head).expect("node");
        let head = writer.finish().expect("finish");
        assert!(
            leaves
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
                > 1
        );

        let read = |id: LeafId| {
            let text = leaves
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&id)
                .cloned();
            async move { text.ok_or_else(|| CompactError::Io("missing".into())) }
        };

        assert_eq!(
            catalog_pointer(read, &head, 0).await.expect("p0"),
            Some(Pointer::Component { leaves: vec![1, 2] })
        );
        assert_eq!(
            catalog_pointer(read, &head, 2999).await.expect("p2999"),
            Some(Pointer::Component {
                leaves: vec![5999, 6000]
            })
        );
        assert_eq!(
            catalog_pointer(read, &head, 3000).await.expect("node"),
            Some(Pointer::Node {
                node: 777,
                head: child_head
            })
        );
        assert_eq!(catalog_pointer(read, &head, 3001).await.expect("end"), None);

        // Corrupt the root side leaf -> integrity failure
        leaves
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(head.side, "{\"h\":0,\"v\":[]}".into());
        assert!(catalog_pointer(read, &head, 0).await.is_err());
    }
}
