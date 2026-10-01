//! Port of senpi packages/ai/src/utils/uuid.ts.

use std::sync::Mutex;

const MAX_UUID_V7_TIMESTAMP: u64 = 0xffff_ffff_ffff;
const MAX_SEQUENCE: u64 = (1 << 41) - 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UuidError {
    #[error("UUIDv7 timestamp must be an integer between 0 and {MAX_UUID_V7_TIMESTAMP}")]
    TimestampOutOfRange,
    #[error("UUIDv7 generator sequence exhausted")]
    SequenceExhausted,
}

struct Generator {
    last_ordinary_timestamp: i64,
    sequence: Option<u64>,
}

static GENERATOR: Mutex<Generator> = Mutex::new(Generator { last_ordinary_timestamp: -1, sequence: None });

/// Test-only override of the "current time" `uuidv7(None)` reads, mirroring `vi.useFakeTimers()`
/// plus `vi.setSystemTime()`; nextest runs each `#[test]` in its own process, so this static resets per test.
#[cfg(test)]
static MOCK_NOW_MS: Mutex<Option<i64>> = Mutex::new(None);

#[cfg(test)]
fn current_time_ms() -> i64 {
    MOCK_NOW_MS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).unwrap_or_else(super::diagnostics::now_ms)
}

#[cfg(not(test))]
fn current_time_ms() -> i64 {
    super::diagnostics::now_ms()
}

/// Test-only override of the random bytes `uuidv7` mixes into the sequence seed and node bits, mirroring `vi.stubGlobal("crypto", { getRandomValues })`.
#[cfg(test)]
type MockRandomBytesFn = Box<dyn Fn() -> [u8; 16] + Send>;
#[cfg(test)]
static MOCK_RANDOM_BYTES: Mutex<Option<MockRandomBytesFn>> = Mutex::new(None);

#[cfg(test)]
fn random_bytes() -> [u8; 16] {
    let guard = MOCK_RANDOM_BYTES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(f) => f(),
        None => *uuid::Uuid::new_v4().as_bytes(),
    }
}

#[cfg(not(test))]
fn random_bytes() -> [u8; 16] {
    *uuid::Uuid::new_v4().as_bytes()
}

/// Generate a time-ordered UUIDv7. A supplied timestamp is preserved for follower ids.
///
/// The timestamp is `f64` (mirroring the TS `number` parameter) so that non-integer and
/// non-finite inputs are representable and rejected at runtime with the same
/// `UuidError::TimestampOutOfRange` TS raises via `Number.isInteger` / range checks, rather than
/// being rejected at compile time by a narrower Rust integer type.
pub fn uuidv7(timestamp_ms: Option<f64>) -> Result<String, UuidError> {
    let requested_f64 = timestamp_ms.unwrap_or_else(|| current_time_ms() as f64);
    if !requested_f64.is_finite() || requested_f64.trunc() != requested_f64 || requested_f64 < 0.0 || requested_f64 > MAX_UUID_V7_TIMESTAMP as f64 {
        return Err(UuidError::TimestampOutOfRange);
    }
    let requested = requested_f64 as i64;
    let mut state = GENERATOR.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let effective = match timestamp_ms {
        None => {
            let t = requested.max(state.last_ordinary_timestamp);
            state.last_ordinary_timestamp = t;
            t
        }
        Some(_) => requested,
    };

    let mut bytes: [u8; 16] = random_bytes();
    let sequence = match state.sequence {
        None => (u64::from(bytes[1]) << 32)
            | (u64::from(bytes[2]) << 24)
            | (u64::from(bytes[3]) << 16)
            | (u64::from(bytes[4]) << 8)
            | u64::from(bytes[5]),
        Some(MAX_SEQUENCE) => return Err(UuidError::SequenceExhausted),
        Some(previous) => previous + 1,
    };
    state.sequence = Some(sequence);
    drop(state);

    let timestamp = effective.unsigned_abs();
    for (index, byte) in bytes.iter_mut().enumerate().take(6) {
        *byte = ((timestamp >> ((5 - index) * 8)) & 0xff) as u8;
    }
    bytes[6] = 0x70 | ((sequence >> 37) & 0x0f) as u8;
    bytes[7] = ((sequence >> 29) & 0xff) as u8;
    bytes[8] = 0x80 | ((sequence >> 23) & 0x3f) as u8;
    bytes[9] = ((sequence >> 15) & 0xff) as u8;
    bytes[10] = ((sequence >> 7) & 0xff) as u8;
    bytes[11] = (((sequence & 0x7f) << 1) as u8) | (bytes[11] & 0x01);
    Ok(uuid::Uuid::from_bytes(bytes).hyphenated().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UUIDv7 regex: version nibble `7`, variant `8|9|a|b`.
    fn assert_matches_uuid_v7(id: &str) {
        let bytes = id.as_bytes();
        assert_eq!(id.len(), 36, "expected 36-char UUID, got {id}");
        assert_eq!(bytes[8], b'-');
        assert_eq!(bytes[13], b'-');
        assert_eq!(bytes[14], b'7');
        assert_eq!(bytes[18], b'-');
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'), "bad variant nibble in {id}");
        assert_eq!(bytes[23], b'-');
        for (i, &b) in bytes.iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                continue;
            }
            assert!(b.is_ascii_hexdigit(), "non-hex byte at {i} in {id}");
        }
    }

    fn parse_timestamp(uuid: &str) -> u64 {
        let hex: String = uuid.chars().filter(|c| *c != '-').collect();
        u64::from_str_radix(&hex[..12], 16).expect("hex timestamp")
    }

    /// Sets the mock clock `uuidv7(None)` reads for the current test process, mirroring
    /// `vi.setSystemTime`.
    fn set_mock_now_ms(value: i64) {
        *MOCK_NOW_MS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(value);
    }

    /// Port of "generates ordered UUIDv7s while preserving follower timestamps" (uuid.test.ts).
    #[test]
    fn generates_ordered_uuid_v7s_while_preserving_follower_timestamps() {
        const TIMESTAMP: i64 = 0x0123_4567_89ab;
        set_mock_now_ms(TIMESTAMP);

        let first = uuidv7(None).expect("uuid");
        let second = uuidv7(None).expect("uuid");
        set_mock_now_ms(TIMESTAMP - 1);
        let after_rollback = uuidv7(None).expect("uuid");
        set_mock_now_ms(TIMESTAMP + 1);
        let after_advance = uuidv7(None).expect("uuid");
        let ordinary_ids = [first, second, after_rollback, after_advance];
        let follower_timestamp = TIMESTAMP - 1_000;
        let followers =
            [uuidv7(Some(follower_timestamp as f64)).expect("uuid"), uuidv7(Some(follower_timestamp as f64)).expect("uuid")];

        for id in ordinary_ids.iter().chain(followers.iter()) {
            assert_matches_uuid_v7(id);
        }
        let mut sorted = ordinary_ids.clone();
        sorted.sort();
        assert_eq!(ordinary_ids, sorted, "ordinary ids must already be sorted");
        let unique: std::collections::HashSet<_> = ordinary_ids.iter().collect();
        assert_eq!(unique.len(), ordinary_ids.len(), "ordinary ids must be unique");
        let timestamps: Vec<u64> = ordinary_ids.iter().map(|id| parse_timestamp(id)).collect();
        assert_eq!(timestamps, vec![TIMESTAMP as u64, TIMESTAMP as u64, TIMESTAMP as u64, (TIMESTAMP + 1) as u64]);
        let follower_timestamps: Vec<u64> = followers.iter().map(|id| parse_timestamp(id)).collect();
        assert_eq!(follower_timestamps, vec![follower_timestamp as u64, follower_timestamp as u64]);
        let follower_set: std::collections::HashSet<_> = followers.iter().collect();
        assert_eq!(follower_set.len(), followers.len(), "follower ids must be unique");
    }

    /// Port of "uses fresh randomness for every UUID tail" (uuid.test.ts).
    #[test]
    fn uses_fresh_randomness_for_every_uuid_tail() {
        const TIMESTAMP: i64 = 0x0123_4567_89ab;
        let counter = std::sync::atomic::AtomicU8::new(0);
        *MOCK_RANDOM_BYTES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Box::new(move || {
            let byte = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            [byte; 16]
        }));

        let first = uuidv7(Some(TIMESTAMP as f64)).expect("uuid");
        let second = uuidv7(Some(TIMESTAMP as f64)).expect("uuid");
        assert_eq!(&first[first.len() - 8..], "01010101");
        assert_eq!(&second[second.len() - 8..], "02020202");
    }

    /// Port of "accepts timestamp boundary %s" (uuid.test.ts it.each([0, 2 ** 48 - 1])).
    #[test]
    fn accepts_timestamp_boundary() {
        let cases: &[(&str, f64)] = &[("accepts timestamp boundary 0", 0.0), ("accepts timestamp boundary 281474976710655", 0xffff_ffff_ffffu64 as f64)];
        for (title, timestamp) in cases {
            let id = uuidv7(Some(*timestamp)).unwrap_or_else(|e| panic!("case {title}: {e}"));
            assert_eq!(parse_timestamp(&id), *timestamp as u64, "case: {title}");
        }
    }

    /// Port of "rejects invalid timestamp %s" (uuid.test.ts it.each([-1, 2**48, 1.5, NaN, +Infinity])),
    /// title carried verbatim. `timestamp_ms` is `f64` (mirroring the TS `number` parameter), so
    /// all five TS values -- including the non-integer and non-finite ones -- are representable
    /// and rejected at runtime exactly as `Number.isInteger`/range checks do in TS.
    #[test]
    fn rejects_invalid_timestamp() {
        let cases: &[(&str, f64)] = &[
            ("rejects invalid timestamp -1", -1.0),
            ("rejects invalid timestamp 281474976710656", 0x1_0000_0000_0000u64 as f64),
            ("rejects invalid timestamp 1.5", 1.5),
            ("rejects invalid timestamp NaN", f64::NAN),
            ("rejects invalid timestamp Infinity", f64::INFINITY),
        ];
        for (title, timestamp) in cases {
            assert_eq!(uuidv7(Some(*timestamp)), Err(UuidError::TimestampOutOfRange), "case: {title}");
        }
    }
}
