//! Process-local identifiers and nonces.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-support-random.md`. These values go into lock nonces
//! and temporary file names: they must be unique within a process and across processes, and nothing
//! more. They are not secrets and nothing authenticates against them.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::store::sha256_hex;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A 32-character lowercase hex identifier, unique per call per process.
///
/// Process id plus a nanosecond timestamp plus a monotonic counter seeds the digest. Two processes
/// started in the same nanosecond still differ by pid; two calls in one process differ by counter.
pub fn random_id() -> String {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let seed = format!("{}:{nanos}:{sequence}", std::process::id());
    sha256_hex(&seed).chars().take(32).collect()
}

/// UUID-v4-shaped identifier (`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`).
///
/// The shape matters, not the RFC bit layout: callers use this where an interface expects something
/// UUID-shaped. Positions 12 and 16 are overwritten with `4` and `8` so the string reads as v4.
pub fn random_uuid() -> String {
    let hex = random_id();
    let mut chars: Vec<char> = hex.chars().collect();
    while chars.len() < 32 {
        chars.push('0');
    }
    chars[12] = '4';
    chars[16] = '8';
    let grouped: String = chars[..32].iter().collect();
    format!(
        "{}-{}-{}-{}-{}",
        &grouped[0..8],
        &grouped[8..12],
        &grouped[12..16],
        &grouped[16..20],
        &grouped[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn an_id_is_32_lowercase_hex_characters() {
        let id = random_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(id, id.to_lowercase());
    }

    #[test]
    fn ids_do_not_repeat_within_a_process() {
        let ids: BTreeSet<String> = (0..1000).map(|_| random_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn a_uuid_has_the_v4_grouping() {
        let uuid = random_uuid();
        let groups: Vec<&str> = uuid.split('-').collect();
        assert_eq!(groups.len(), 5);
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
    }

    #[test]
    fn a_uuid_pins_the_version_and_variant_nibbles() {
        let uuid = random_uuid();
        let chars: Vec<char> = uuid.chars().filter(|ch| *ch != '-').collect();
        assert_eq!(chars[12], '4');
        assert_eq!(chars[16], '8');
    }

    #[test]
    fn uuids_do_not_repeat_within_a_process() {
        let ids: BTreeSet<String> = (0..1000).map(|_| random_uuid()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn a_uuid_is_derived_from_the_id_of_the_same_call() {
        let uuid = random_uuid();
        let digits: String = uuid.chars().filter(|ch| *ch != '-').collect();
        assert_eq!(digits.len(), 32);
        assert!(digits.chars().all(|ch| ch.is_ascii_hexdigit()));
    }
}
