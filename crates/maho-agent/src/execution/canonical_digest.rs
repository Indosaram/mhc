//! Port of senpi `packages/agent/src/execution/canonical-digest.ts`.

use sha2::{Digest, Sha256};

use crate::durable::store::StoreValue;

/// The canonical JSON of a store value: object keys sorted by UTF-16 code unit, no whitespace.
/// Every execution digest (a batch manifest, a call's request, a frozen call, a tool schema)
/// hashes this form, so two sides that build the same value agree on its digest.
pub fn canonical_json(value: &StoreValue) -> String {
    match value {
        StoreValue::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        StoreValue::Object(map) => {
            let mut entries: Vec<(&String, &StoreValue)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(key, inner)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()),
                        canonical_json(inner)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned()),
    }
}

/// Lower-case hex SHA-256 of [`canonical_json`].
pub fn canonical_digest(value: &StoreValue) -> String {
    let encoded = canonical_json(value);
    let digest = Sha256::digest(encoded.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}
