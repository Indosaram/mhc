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

/// Generate a time-ordered UUIDv7. A supplied timestamp is preserved for follower ids.
pub fn uuidv7(timestamp_ms: Option<i64>) -> Result<String, UuidError> {
    let requested = timestamp_ms.unwrap_or_else(super::diagnostics::now_ms);
    if requested < 0 || u64::try_from(requested).map_or(true, |t| t > MAX_UUID_V7_TIMESTAMP) {
        return Err(UuidError::TimestampOutOfRange);
    }
    let mut state = GENERATOR.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let effective = match timestamp_ms {
        None => {
            let t = requested.max(state.last_ordinary_timestamp);
            state.last_ordinary_timestamp = t;
            t
        }
        Some(t) => t,
    };

    let mut bytes: [u8; 16] = *uuid::Uuid::new_v4().as_bytes();
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

    #[test]
    fn follower_ids_keep_timestamp_and_sort_after_leader() {
        let a = uuidv7(Some(0x0123_4567_89ab)).expect("uuid");
        let b = uuidv7(Some(0x0123_4567_89ab)).expect("uuid");
        assert!(a.starts_with("01234567-89ab-7"));
        assert!(b > a);
        assert_eq!(uuidv7(Some(-1)), Err(UuidError::TimestampOutOfRange));
        assert_eq!(uuidv7(Some(0x1_0000_0000_0000)), Err(UuidError::TimestampOutOfRange));
        let ordinary = uuidv7(None).expect("uuid");
        assert_eq!(ordinary.len(), 36);
        assert_eq!(&ordinary[14..15], "7");
    }
}
