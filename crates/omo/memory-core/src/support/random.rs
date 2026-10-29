//! Process-local identifier and nonce generation.
//!
//! `node:crypto`'s `randomUUID()` is only used for lock nonces and temporary
//! file names: values must be unique within and across processes, never
//! guessable-proof. This module hashes process identity plus a monotonic
//! counter, which keeps the property the callers rely on without a dependency.

use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::sha256::sha256_hex;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A 32-character lowercase hex identifier, unique per call per process.
pub fn random_id() -> String {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let seed = format!("{}-{nanos}-{sequence}", process::id());
    let digest = sha256_hex(seed.as_bytes());
    digest.chars().take(32).collect()
}

/// UUID-v4-shaped identifier (`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`).
pub fn random_uuid() -> String {
    let hex = random_id();
    let mut chars: Vec<char> = hex.chars().collect();
    if chars.len() < 32 {
        while chars.len() < 32 {
            chars.push('0');
        }
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
