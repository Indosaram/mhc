//! Port of senpi `packages/coding-agent/src/core/compaction/estimate-cache-key.ts`.
//!
//! The reuse key for the per-message estimate caches, derived from the message's JSON text. Under Bun
//! the key is the text's length plus a 128-bit digest so a cache entry does not hold a second copy of
//! the message's text (review of senpi#2884); this port uses SHA-256 truncated to 128 bits for the
//! same purpose.
//!
//! The transient marker is a non-enumerable symbol in TS, so it never reaches a request or a key. Rust
//! messages are `serde_json::Value`s, which carry no hidden fields, so transient messages are tracked
//! by the caller through `TransientMessage`, a wrapper that never serializes its flag.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The message's JSON text, or `None` when it cannot be serialized (such a message is never cached).
pub fn serialize_for_estimate<T: Serialize + ?Sized>(message: &T) -> Option<String> {
    serde_json::to_string(message).ok()
}

/// The cache key for a serialized message: its byte length and a 128-bit digest of the text.
pub fn estimate_cache_key(serialized: &str) -> String {
    let digest = Sha256::digest(serialized.as_bytes());
    format!("{}:{}", serialized.len(), hex::encode(&digest[..16]))
}

/// A message that exists for one request only (the runner's per-turn clone, admission's sizing probes).
/// Estimators compute such messages directly instead of paying for a key and a cache entry.
#[derive(Debug, Clone, PartialEq)]
pub struct TransientMessage(pub Value);

impl Serialize for TransientMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

pub fn mark_transient_message(message: Value) -> TransientMessage {
    TransientMessage(message)
}

/// Whether an estimator input is transient: always true for a `TransientMessage`.
pub fn is_transient_message(_message: &TransientMessage) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_differ_whenever_the_text_differs() {
        let a = serialize_for_estimate(&json!({"role":"user","content":"a"})).unwrap_or_default();
        let b = serialize_for_estimate(&json!({"role":"user","content":"b"})).unwrap_or_default();
        assert_ne!(estimate_cache_key(&a), estimate_cache_key(&b));
        assert_eq!(estimate_cache_key(&a), estimate_cache_key(&a.clone()));
        assert!(estimate_cache_key(&a).starts_with(&format!("{}:", a.len())));
        assert!(estimate_cache_key(&a).len() < a.len() + 40);
    }

    #[test]
    fn the_transient_marker_never_reaches_the_serialized_text() {
        let message = json!({"role":"user","content":"x"});
        let transient = mark_transient_message(message.clone());
        assert!(is_transient_message(&transient));
        assert_eq!(
            serialize_for_estimate(&transient),
            serialize_for_estimate(&message)
        );
    }
}
