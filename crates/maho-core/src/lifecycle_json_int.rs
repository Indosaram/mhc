//! Integral JSON numbers, read the way a JavaScript writer means them.
//!
//! Durable lifecycle JSON is written by both the native port and the senpi/omo TypeScript side, whose only number
//! type is `f64` and whose `JSON.stringify` emits `1000` for an integral value but `1000.0` for one that has been
//! through a floating-point round trip. `serde_json` will not deserialize `1000.0` into a `u64`, so a timestamp that
//! is semantically an integer must be normalized here rather than at every typed field that receives it.
use serde_json::Value;

/// `Number.MAX_SAFE_INTEGER`. Above this a `f64` can no longer name every integer, so a value beyond it is not
/// trusted to be the integer its writer intended.
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// A non-negative integral number as `u64`; `None` for anything else.
///
/// A value with a fractional part is rejected rather than truncated: a truncated timestamp is a different instant,
/// and a truncated count is a different count.
pub fn safe_non_negative_integer(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    (number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&number)).then_some(number as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integral_floats_normalize_to_integers() {
        assert_eq!(safe_non_negative_integer(&json!(1000)), Some(1000));
        assert_eq!(safe_non_negative_integer(&json!(1000.0)), Some(1000));
        assert_eq!(safe_non_negative_integer(&json!(0.0)), Some(0));
        assert_eq!(safe_non_negative_integer(&json!(0)), Some(0));
        assert_eq!(
            safe_non_negative_integer(&json!(MAX_SAFE_INTEGER)),
            Some(MAX_SAFE_INTEGER as u64)
        );
    }

    #[test]
    fn non_integers_and_negatives_are_rejected() {
        assert_eq!(safe_non_negative_integer(&json!(1000.5)), None);
        assert_eq!(safe_non_negative_integer(&json!(-1)), None);
        assert_eq!(safe_non_negative_integer(&json!(-1.0)), None);
        assert_eq!(
            safe_non_negative_integer(&json!(MAX_SAFE_INTEGER * 2.0)),
            None
        );
        assert_eq!(safe_non_negative_integer(&json!("1000")), None);
        assert_eq!(safe_non_negative_integer(&json!(null)), None);
        assert_eq!(safe_non_negative_integer(&json!({})), None);
    }
}
