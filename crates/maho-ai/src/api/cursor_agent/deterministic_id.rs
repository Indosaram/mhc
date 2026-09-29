//! Port of senpi packages/ai/src/api/cursor-agent/deterministic-id.ts.
// ported by todo 12

use sha2::{Digest, Sha256};

/// A UUID-shaped string: five hex groups in the canonical 8-4-4-4-12 layout.
///
/// NOT a spec-compliant RFC 4122 UUID -- the version/variant nibbles are left
/// as raw hash output -- but the shape passes everywhere a UUID string is
/// expected.
pub type DeterministicUuid = String;

/// Format the leading 128 bits of `seed`'s SHA-256 digest as a v4-shape UUID
/// (8-4-4-4-12 hex groups).
///
/// Deterministic: identical seeds always map to the same id, so callers get
/// stable ids across requests / conversation turns (reusing message-blob ids,
/// keying prompt caches) without persisting a seed->id mapping.
pub fn deterministic_uuid(seed: &str) -> DeterministicUuid {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let hex = hex::encode(digest);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_same_seed_when_hashed_twice_then_ids_match() {
        let a = deterministic_uuid("cursor-conversation-1");
        let b = deterministic_uuid("cursor-conversation-1");
        assert_eq!(a, b);
    }

    #[test]
    fn given_different_seeds_when_hashed_then_ids_differ() {
        let a = deterministic_uuid("seed-a");
        let b = deterministic_uuid("seed-b");
        assert_ne!(a, b);
    }

    #[test]
    fn given_seed_when_hashed_then_shape_is_canonical_uuid_layout() {
        let id = deterministic_uuid("shape-check");
        let groups: Vec<&str> = id.split('-').collect();
        assert_eq!(groups.len(), 5);
        assert_eq!(groups[0].len(), 8);
        assert_eq!(groups[1].len(), 4);
        assert_eq!(groups[2].len(), 4);
        assert_eq!(groups[3].len(), 4);
        assert_eq!(groups[4].len(), 12);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    }
}
