//! senpi `modes/rpc/compact-recovery-ids.ts`: the one identifier grammar of the recovery protocol
//! (S09-Q2 A, RULING-57 Q2). Every id, digest and ref is parsed here before any use as a key,
//! a path element, a log field or a lookup, and S15's admission ids reuse these parsers.
//!
//! A content ref is cut-scoped (RULING-58 (3)): it is the position of a node or component under
//! one catalog root, minted by this process for one token, and it resolves only under that token
//! through verified node reads.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const BOUNDED_ID_MAX_BYTES: usize = 128;
const COUNTER_DIGITS: usize = 16;
/// Child descriptors per catalog node (R4.B1), the bound of every child step.
pub const CATALOG_NODE_MAX_ENTRIES: usize = 128;
const STEP_DIGITS: usize = 3;
/// The deepest ref: 128^8 addressable children, far past any catalog, in 50 of the 128 bytes an id may have.
pub const CATALOG_REF_MAX_STEPS: usize = 8;
const REF_ROOT_LENGTH: usize = 2 + COUNTER_DIGITS; // "c-" + 16 digits
const REF_STEP_LENGTH: usize = 1 + STEP_DIGITS; // ":" + 3 digits

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentityRefusal {
    #[error("empty")]
    Empty,
    #[error("overlong")]
    Overlong,
    #[error("forbidden_character")]
    ForbiddenCharacter,
    #[error("malformed")]
    Malformed,
    #[error("too_deep")]
    TooDeep,
    #[error("reserved")]
    Reserved,
}

impl IdentityRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Overlong => "overlong",
            Self::ForbiddenCharacter => "forbidden_character",
            Self::Malformed => "malformed",
            Self::TooDeep => "too_deep",
            Self::Reserved => "reserved",
        }
    }
}

pub type IdentityOutcome<T> = Result<T, IdentityRefusal>;

/// An opaque protocol identifier: ASCII from the allow-list, 1..=128 bytes (S09-Q2 A, RULING-57 Q2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BoundedId(String);

impl BoundedId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
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

    pub fn into_inner(self) -> String {
        self.0
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

/// A catalog ref: a 16-digit catalog counter root and at most `CATALOG_REF_MAX_STEPS` three-digit child steps.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentRef(String);

impl ContentRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for ContentRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContentRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The reserved ref of a cut's loaded transcript base (S11c, RULING-70 (1)): the catalog root with the `base` step.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BaseRef(String);

impl BaseRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for BaseRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BaseRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the page route addresses: a catalog component, or the cut's base stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PageRef {
    Catalog(ContentRef),
    Base(BaseRef),
}

impl PageRef {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Catalog(r) => r.as_str(),
            Self::Base(r) => r.as_str(),
        }
    }
}

fn is_bounded_id_bytes(bytes: &[u8]) -> bool {
    let Some(&first) = bytes.first() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
}

fn has_reserved_base_step(value: &str) -> bool {
    value.split(':').skip(1).any(|segment| segment == "base")
}

fn is_content_ref_syntax(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("c-") else {
        return false;
    };
    let Some(root_digits) = rest.get(..COUNTER_DIGITS) else {
        return false;
    };
    if !root_digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let tail = &rest[COUNTER_DIGITS..];
    if tail.is_empty() {
        return true;
    }
    let mut rem = tail;
    while !rem.is_empty() {
        let Some(after_colon) = rem.strip_prefix(':') else {
            return false;
        };
        let Some(step) = after_colon.get(..STEP_DIGITS) else {
            return false;
        };
        if !step.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        rem = &after_colon[STEP_DIGITS..];
    }
    true
}

/// The allow-list check every identifier passes before any use as a key, a path element or a log field.
pub fn parse_bounded_id(value: &str) -> IdentityOutcome<BoundedId> {
    if value.is_empty() {
        return Err(IdentityRefusal::Empty);
    }
    if value.len() > BOUNDED_ID_MAX_BYTES {
        return Err(IdentityRefusal::Overlong);
    }
    if !is_bounded_id_bytes(value.as_bytes()) {
        return Err(IdentityRefusal::ForbiddenCharacter);
    }
    Ok(BoundedId(value.to_owned()))
}

pub fn parse_content_digest(value: &str) -> IdentityOutcome<ContentDigest> {
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

pub fn parse_content_ref(value: &str) -> IdentityOutcome<ContentRef> {
    let _ = parse_bounded_id(value)?;
    if has_reserved_base_step(value) {
        return Err(IdentityRefusal::Reserved);
    }
    if !is_content_ref_syntax(value) {
        return Err(IdentityRefusal::Malformed);
    }
    if (value.len() - REF_ROOT_LENGTH) / REF_STEP_LENGTH > CATALOG_REF_MAX_STEPS {
        return Err(IdentityRefusal::TooDeep);
    }
    Ok(ContentRef(value.to_owned()))
}

fn parse_base_ref(value: &str) -> IdentityOutcome<BaseRef> {
    let _ = parse_bounded_id(value)?;
    let Some(rest) = value.strip_prefix("c-") else {
        return Err(IdentityRefusal::Malformed);
    };
    let Some(digits) = rest.strip_suffix(":base") else {
        return Err(IdentityRefusal::Malformed);
    };
    if digits.len() != COUNTER_DIGITS || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(IdentityRefusal::Malformed);
    }
    Ok(BaseRef(value.to_owned()))
}

/// Either grammar, checked before any use; the two never overlap (`base` is not three digits).
pub fn parse_page_ref(value: &str) -> IdentityOutcome<PageRef> {
    if let Ok(base) = parse_base_ref(value) {
        return Ok(PageRef::Base(base));
    }
    parse_content_ref(value).map(PageRef::Catalog)
}

/// A literal or generated protocol identifier; a refusal is a programming error, never input.
pub fn protocol_id(value: &str) -> BoundedId {
    parse_bounded_id(value).unwrap_or_else(|reason| {
        panic!(
            "not a bounded protocol id ({}): {} chars",
            reason.as_str(),
            value.len()
        )
    })
}

fn fixed(value: u64, digits: usize, bound: u64, what: &str) -> String {
    assert!(value < bound, "{what} out of range: {value}");
    format!("{value:0digits$}")
}

fn minted(value: &str) -> ContentRef {
    parse_content_ref(value).unwrap_or_else(|reason| panic!("catalog ref {}", reason.as_str()))
}

pub fn catalog_root_ref(counter: u64) -> ContentRef {
    let bound = 10_u64.pow(COUNTER_DIGITS as u32);
    minted(&format!(
        "c-{}",
        fixed(counter, COUNTER_DIGITS, bound, "catalog counter")
    ))
}

pub fn child_ref(parent: &ContentRef, index: usize) -> ContentRef {
    minted(&format!(
        "{}:{}",
        parent.as_str(),
        fixed(
            index as u64,
            STEP_DIGITS,
            CATALOG_NODE_MAX_ENTRIES as u64,
            "catalog child index",
        )
    ))
}

/// The one base ref under a catalog root; a ref with steps is a programming error.
pub fn base_ref(root: &ContentRef) -> BaseRef {
    parse_base_ref(&format!("{}:base", root.as_str()))
        .unwrap_or_else(|reason| panic!("base ref {}", reason.as_str()))
}

/// The child indexes from the catalog root to the ref.
pub fn ref_steps(ref_: &ContentRef) -> Vec<usize> {
    ref_.as_str()
        .split(':')
        .skip(1)
        .filter_map(|step| step.parse::<usize>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_id_enforces_ascii_allow_list_and_length() {
        assert_eq!(parse_bounded_id(""), Err(IdentityRefusal::Empty));
        assert_eq!(
            parse_bounded_id(&"a".repeat(BOUNDED_ID_MAX_BYTES + 1)),
            Err(IdentityRefusal::Overlong)
        );
        for bad in [
            "-leading", "_leading", ":leading", "a/b", "a..b", "a b", "a.b", "é",
        ] {
            assert_eq!(
                parse_bounded_id(bad),
                Err(IdentityRefusal::ForbiddenCharacter),
                "{bad}"
            );
        }
        let ok = parse_bounded_id("adm-1_foo:bar").expect("valid");
        assert_eq!(ok.as_str(), "adm-1_foo:bar");
    }

    #[test]
    fn content_digest_requires_64_lowercase_hex() {
        assert_eq!(parse_content_digest(""), Err(IdentityRefusal::Empty));
        assert_eq!(parse_content_digest("abc"), Err(IdentityRefusal::Malformed));
        assert_eq!(
            parse_content_digest(&"A".repeat(64)),
            Err(IdentityRefusal::Malformed)
        );
        let d = parse_content_digest(&"0".repeat(64)).expect("valid");
        assert_eq!(d.as_str().len(), 64);
    }

    #[test]
    fn content_ref_and_page_ref_distinguish_base_and_depth_bounds() {
        let root = catalog_root_ref(42);
        assert_eq!(root.as_str(), "c-0000000000000042");
        let child = child_ref(&root, 7);
        assert_eq!(child.as_str(), "c-0000000000000042:007");
        assert_eq!(ref_steps(&child), vec![7]);

        let base = base_ref(&root);
        assert_eq!(base.as_str(), "c-0000000000000042:base");
        assert_eq!(
            parse_content_ref(base.as_str()),
            Err(IdentityRefusal::Reserved)
        );
        assert_eq!(
            parse_content_ref("c-0000000000000042:base:000"),
            Err(IdentityRefusal::Reserved)
        );
        assert_eq!(parse_page_ref(base.as_str()), Ok(PageRef::Base(base)));
        assert_eq!(
            parse_page_ref(child.as_str()),
            Ok(PageRef::Catalog(child.clone()))
        );

        let mut deep = root;
        for _ in 0..CATALOG_REF_MAX_STEPS {
            deep = child_ref(&deep, 1);
        }
        assert!(parse_content_ref(deep.as_str()).is_ok());
        let too_deep = format!("{}:001", deep.as_str());
        assert_eq!(parse_content_ref(&too_deep), Err(IdentityRefusal::TooDeep));
    }
}
