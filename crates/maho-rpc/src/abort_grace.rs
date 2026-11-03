//! The `abort { awaitIdle }` grace window (senpi `rpc/abort-grace.ts`), shared by the host that enforces it
//! and the client that sizes its request deadline from it. Kept free of host imports.
use serde_json::Value;

pub const ABORT_DEFAULT_GRACE_MS: u64 = 10_000;
pub const ABORT_MAX_GRACE_MS: u64 = 60_000;

/// A `graceMs` the host accepts: a whole number of milliseconds from 0 to [`ABORT_MAX_GRACE_MS`].
pub fn is_abort_grace_in_range(grace_ms: &Value) -> bool {
    match grace_ms {
        Value::Number(number) => match (number.as_u64(), number.as_f64()) {
            (Some(whole), _) => whole <= ABORT_MAX_GRACE_MS,
            (None, Some(float)) => {
                float.fract() == 0.0 && (0.0..=ABORT_MAX_GRACE_MS as f64).contains(&float)
            }
            (None, None) => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_whole_milliseconds_inside_the_window() {
        for value in [
            json!(0),
            json!(10_000),
            json!(60_000),
            json!(5.0),
            json!(-0.0),
        ] {
            assert!(is_abort_grace_in_range(&value), "{value}");
        }
    }

    #[test]
    fn rejects_fractions_negatives_overlong_and_non_numbers() {
        for value in [
            json!(1.5),
            json!(-1),
            json!(60_001),
            json!("10"),
            json!(null),
            json!(true),
        ] {
            assert!(!is_abort_grace_in_range(&value), "{value}");
        }
    }
}
