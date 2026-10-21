use super::{entry::WrapEntry, signatures::*};
use std::collections::BTreeSet;
pub fn next_link(prior: &str, digest: &str) -> Option<String> {
    let digest = decode_b64(digest, Some(32))?;
    let mut bytes = if prior.is_empty() {
        Vec::new()
    } else {
        decode_b64(prior, Some(32))?
    };
    bytes.extend(digest);
    Some(sha256_b64(&bytes))
}
pub fn wrap_set_digest(wraps: &[WrapEntry]) -> Option<String> {
    let seen: BTreeSet<_> = wraps.iter().map(|w| (w.epoch, &w.id)).collect();
    if seen.len() != wraps.len() {
        return None;
    }
    Some(sha256_b64(&canonical(&serde_json::json!(wraps)).ok()?))
}
