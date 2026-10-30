use super::compile::MemoryExpiry;
use crate::memfs::expiry::is_memory_expired;

/// The part of the clock a compiled block depends on: which of its revision's `expires` instants
/// have passed. Two instants with the same slot compile to the same bytes, so a block is cached
/// per (revision, slot).
pub fn expiry_slot(expiries: &[MemoryExpiry], as_of_ms: i64) -> String {
    let mut passed: Vec<String> = expiries
        .iter()
        .filter(|expiry| is_memory_expired(Some(expiry.expires_at_ms), as_of_ms))
        .map(|expiry| format!("{}@{}", expiry.path, expiry.expires_at_ms))
        .collect();
    passed.sort();
    passed.join("|")
}
