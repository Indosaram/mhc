//! senpi `modes/rpc/compact-catalog-codec.ts`: the frozen-reference codec of the recovery
//! catalog (S09-Q1 A, RULING-57 Q1). Also houses the bounded-id / digest parsers from
//! `compact-recovery-ids.ts` so the catalog modules share one validated identifier grammar.

use crate::compact_content_store::{COMPACT_LEAF_CODE_UNITS, utf16_len, utf16le};
use crate::compact_read_lease::CompactError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

pub const BOUNDED_ID_MAX_BYTES: usize = 128;
pub const CATALOG_NODE_MAX_ENTRIES: usize = 128;
pub const CATALOG_FORMAT: &str = "senpi-recovery-catalog";
pub const CATALOG_VERSION: u64 = 1;
const CATALOG_ENCODING: &str = "utf16le";
/// A node is one store leaf: 32,768 UTF-16 code units, 64 KiB stored.
pub const CATALOG_NODE_MAX_UNITS: usize = COMPACT_LEAF_CODE_UNITS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityRefusal {
    Empty,
    Overlong,
    ForbiddenCharacter,
    Malformed,
    TooDeep,
    Reserved,
}

/// An opaque protocol identifier: ASCII from `[A-Za-z0-9][A-Za-z0-9:_-]*`, 1..=128 bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BoundedId(String);

impl BoundedId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for BoundedId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BoundedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A SHA-256 digest: exactly 64 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentDigest(String);

impl ContentDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ContentDigest {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_valid_bounded_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
}

pub fn parse_bounded_id(value: &str) -> Result<BoundedId, IdentityRefusal> {
    if value.is_empty() {
        return Err(IdentityRefusal::Empty);
    }
    if value.len() > BOUNDED_ID_MAX_BYTES {
        return Err(IdentityRefusal::Overlong);
    }
    if !is_valid_bounded_id(value) {
        return Err(IdentityRefusal::ForbiddenCharacter);
    }
    Ok(BoundedId(value.to_owned()))
}

pub fn parse_content_digest(value: &str) -> Result<ContentDigest, IdentityRefusal> {
    if value.is_empty() {
        return Err(IdentityRefusal::Empty);
    }
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(IdentityRefusal::Malformed);
    }
    Ok(ContentDigest(value.to_owned()))
}

pub fn try_protocol_id(value: &str) -> Result<BoundedId, CompactError> {
    parse_bounded_id(value).map_err(|reason| {
        CompactError::Io(format!(
            "not a bounded protocol id ({reason:?}): {} chars",
            value.len()
        ))
    })
}

/// A literal or generated protocol identifier; a refusal is a programming error, never input.
pub fn protocol_id(value: &str) -> BoundedId {
    try_protocol_id(value).unwrap_or_else(|err| panic!("{err}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogDomain {
    Leaf,
    Node,
    Root,
    Pointer,
    Fragment,
}

impl CatalogDomain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Leaf => "leaf",
            Self::Node => "node",
            Self::Root => "root",
            Self::Pointer => "pointer",
            Self::Fragment => "fragment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub kind: BoundedId,
    pub id: BoundedId,
    pub length: usize,
    pub digest: ContentDigest,
}

pub fn stored_bytes(text: &str) -> Vec<u8> {
    utf16le(text)
}

/// Encodes raw UTF-16 code units as UTF-16LE (including lone surrogates when testing).
pub fn stored_bytes_u16(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

/// A digest over `length` bytes fed in order; `finish` after exactly that many.
pub struct CatalogDigester {
    length: usize,
    fed: usize,
    hasher: Sha256,
}

impl CatalogDigester {
    pub fn new(domain: CatalogDomain, length: usize) -> Self {
        let tag = format!(
            "{CATALOG_FORMAT}/{CATALOG_VERSION}/{CATALOG_ENCODING}/{}",
            domain.as_str()
        );
        let tag_bytes = tag.as_bytes();
        let mut hasher = Sha256::new();
        hasher.update([u8::try_from(tag_bytes.len()).unwrap_or(u8::MAX)]);
        hasher.update(tag_bytes);
        hasher.update((u64::try_from(length).unwrap_or(u64::MAX)).to_be_bytes());
        Self {
            length,
            fed: 0,
            hasher,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.fed += bytes.len();
        self.hasher.update(bytes);
    }

    pub fn finish(self) -> Result<ContentDigest, CompactError> {
        if self.fed != self.length {
            return Err(CompactError::Io(format!(
                "catalog digest fed {} of {} bytes",
                self.fed, self.length
            )));
        }
        let hex_digest = hex::encode(self.hasher.finalize());
        parse_content_digest(&hex_digest)
            .map_err(|_| CompactError::Io("sha256 produced a malformed digest".into()))
    }
}

pub fn catalog_digest(domain: CatalogDomain, bytes: &[u8]) -> Result<ContentDigest, CompactError> {
    let mut digester = CatalogDigester::new(domain, bytes.len());
    digester.update(bytes);
    digester.finish()
}

fn head() -> String {
    format!("{{\"format\":\"{CATALOG_FORMAT}\",\"version\":{CATALOG_VERSION},\"entries\":[")
}

const TAIL: &str = "]}";

fn encode_entry(entry: &CatalogEntry) -> String {
    format!(
        "{{\"kind\":\"{}\",\"id\":\"{}\",\"length\":{},\"digest\":\"{}\"}}",
        entry.kind, entry.id, entry.length, entry.digest
    )
}

/// Packs entries in order; `push` refuses, without retaining, the entry that would exceed either
/// bound.
pub struct CatalogNodeWriter {
    parts: Vec<String>,
    units: usize,
}

impl Default for CatalogNodeWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl CatalogNodeWriter {
    pub fn new() -> Self {
        Self {
            parts: Vec::new(),
            units: head().len() + TAIL.len(),
        }
    }

    pub fn count(&self) -> usize {
        self.parts.len()
    }

    pub fn push(&mut self, entry: &CatalogEntry) -> bool {
        let part = encode_entry(entry);
        let part_units = usize::try_from(utf16_len(&part)).unwrap_or(usize::MAX);
        let units = self.units + part_units + usize::from(!self.parts.is_empty());
        if self.parts.len() == CATALOG_NODE_MAX_ENTRIES || units > CATALOG_NODE_MAX_UNITS {
            return false;
        }
        self.parts.push(part);
        self.units = units;
        true
    }

    pub fn encode(&self) -> String {
        format!("{}{}{}", head(), self.parts.join(","), TAIL)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogEntry {
    kind: String,
    id: String,
    length: u64,
    digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogNode {
    format: String,
    version: u64,
    entries: Vec<RawCatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogDecode {
    Ok(Vec<CatalogEntry>),
    SchemaError,
}

/// Decodes verified node bytes only: the caller checks the digest first (RULING-57 Q1 (3)).
pub fn decode_catalog_node(text: &str) -> CatalogDecode {
    let Ok(parsed) = serde_json::from_str::<RawCatalogNode>(text) else {
        return CatalogDecode::SchemaError;
    };
    if parsed.format != CATALOG_FORMAT
        || parsed.version != CATALOG_VERSION
        || parsed.entries.len() > CATALOG_NODE_MAX_ENTRIES
    {
        return CatalogDecode::SchemaError;
    }
    let mut entries = Vec::with_capacity(parsed.entries.len());
    for raw in parsed.entries {
        let (Ok(kind), Ok(id), Ok(digest), Ok(length)) = (
            parse_bounded_id(&raw.kind),
            parse_bounded_id(&raw.id),
            parse_content_digest(&raw.digest),
            usize::try_from(raw.length),
        ) else {
            return CatalogDecode::SchemaError;
        };
        entries.push(CatalogEntry {
            kind,
            id,
            length,
            digest,
        });
    }
    CatalogDecode::Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    // senpi 4272-recovery-catalog-codec: pinned golden encoding, digest, and domain separation.
    #[test]
    fn golden_node_encoding_digest_and_decode_match_spec() {
        let golden = vec![
            CatalogEntry {
                kind: protocol_id("lifecycle"),
                id: protocol_id("lifecycle"),
                length: 42,
                digest: parse_content_digest(&"00".repeat(32)).expect("digest"),
            },
            CatalogEntry {
                kind: protocol_id("node"),
                id: protocol_id("node-0000000000000001"),
                length: 70_000,
                digest: parse_content_digest(&"ff".repeat(32)).expect("digest"),
            },
        ];
        let mut writer = CatalogNodeWriter::new();
        for entry in &golden {
            assert!(writer.push(entry));
        }
        let text = writer.encode();
        let expected_text = format!(
            "{{\"format\":\"senpi-recovery-catalog\",\"version\":1,\"entries\":[{{\"kind\":\"lifecycle\",\"id\":\"lifecycle\",\"length\":42,\"digest\":\"{}\"}},{{\"kind\":\"node\",\"id\":\"node-0000000000000001\",\"length\":70000,\"digest\":\"{}\"}}]}}",
            "00".repeat(32),
            "ff".repeat(32)
        );
        assert_eq!(text, expected_text);
        let bytes = stored_bytes(&text);
        assert_eq!(bytes.len(), 642);
        assert_eq!(
            catalog_digest(CatalogDomain::Node, &bytes)
                .expect("digest")
                .as_str(),
            "fa1de8cdae098d7eb81dfdda48b25efc90103c0411f11f69f8a2946fb050e794"
        );
        assert_eq!(
            catalog_digest(CatalogDomain::Leaf, &[])
                .expect("empty")
                .as_str(),
            "3d5f96ca2a52d51a2ddb9a6cf945cf997897987a6fb89d78921d25353bd057ed"
        );
        assert_eq!(decode_catalog_node(&text), CatalogDecode::Ok(golden));
        assert_eq!(
            CatalogNodeWriter::new().encode(),
            "{\"format\":\"senpi-recovery-catalog\",\"version\":1,\"entries\":[]}"
        );
    }

    #[test]
    fn domain_separation_and_fragmented_utf16_digester() {
        let whole = stored_bytes("a\u{1F600}b");
        assert_eq!(whole.len(), 8);
        let mut digester = CatalogDigester::new(CatalogDomain::Leaf, whole.len());
        digester.update(&stored_bytes_u16(&[b'a' as u16, 0xd83d]));
        digester.update(&stored_bytes_u16(&[0xde00, b'b' as u16]));
        assert_eq!(
            digester.finish().expect("finish"),
            catalog_digest(CatalogDomain::Leaf, &whole).expect("whole")
        );

        let d_leaf = catalog_digest(CatalogDomain::Leaf, &whole).expect("leaf");
        let d_node = catalog_digest(CatalogDomain::Node, &whole).expect("node");
        let d_root = catalog_digest(CatalogDomain::Root, &whole).expect("root");
        let d_ptr = catalog_digest(CatalogDomain::Pointer, &whole).expect("ptr");
        assert_ne!(d_leaf, d_node);
        assert_ne!(d_node, d_root);
        assert_ne!(d_root, d_ptr);
        assert_ne!(d_leaf, d_ptr);
    }
}
