//! Port of senpi `packages/agent/src/conformance/suite-hash.ts`.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SuiteHashEntry {
    pub id: String,
    pub impl_hash: String,
}

/// Lower-case hex SHA-256 over `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The suite hash: SHA-256 over one `id<TAB>impl_hash` line per case, sorted by ID.
/// It changes when a case is added, removed or renamed, or when any case implementation changes,
/// and never with registration order.
pub fn compute_suite_hash(entries: &[SuiteHashEntry]) -> Result<String, String> {
    let mut ids = BTreeSet::new();
    for entry in entries {
        if !ids.insert(entry.id.as_str()) {
            return Err(format!(
                "duplicate conformance case ID in the suite hash: {}",
                entry.id
            ));
        }
    }
    let mut lines: Vec<String> = entries
        .iter()
        .map(|entry| format!("{}\t{}\n", entry.id, entry.impl_hash))
        .collect();
    lines.sort();
    Ok(sha256_hex(lines.join("").as_bytes()))
}
