//! Lenient `Option<usize>` deserialization for tool parameters the JSON schema declares as
//! `"type":"number"` (as upstream senpi does): models may send `100.0`, which must mean `100`.

use serde::de::{Deserializer, Error, Unexpected, Visitor};
use std::fmt;

/// Accepts JSON integers and integral, finite, non-negative floats (`100.0` -> `100`);
/// rejects fractional, negative, NaN/infinite, and out-of-range values. `null` -> `None`.
/// Use with `#[serde(default, deserialize_with = "crate::json_usize::option_usize")]`.
pub fn option_usize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<usize>, D::Error> {
    deserializer.deserialize_option(OptionUsizeVisitor)
}

/// `Option<u32>` counterpart of [`option_usize`] with the same rules; values above `u32::MAX` are
/// rejected. Use with `#[serde(default, deserialize_with = "crate::json_usize::option_u32")]`.
pub fn option_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    option_usize(deserializer)?
        .map(|n| {
            u32::try_from(n).map_err(|_| {
                D::Error::invalid_value(Unexpected::Unsigned(n as u64), &OptionUsizeVisitor)
            })
        })
        .transpose()
}

struct OptionUsizeVisitor;

impl<'de> Visitor<'de> for OptionUsizeVisitor {
    type Value = Option<usize>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a non-negative integer")
    }
    fn visit_none<E: Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
    fn visit_u64<E: Error>(self, v: u64) -> Result<Self::Value, E> {
        usize::try_from(v)
            .map(Some)
            .map_err(|_| E::invalid_value(Unexpected::Unsigned(v), &self))
    }
    fn visit_i64<E: Error>(self, v: i64) -> Result<Self::Value, E> {
        usize::try_from(v)
            .map(Some)
            .map_err(|_| E::invalid_value(Unexpected::Signed(v), &self))
    }
    fn visit_f64<E: Error>(self, v: f64) -> Result<Self::Value, E> {
        // 2^64 as f64; any finite integral value below it fits in u64.
        if v.is_finite() && v >= 0.0 && v.fract() == 0.0 && v < 18_446_744_073_709_551_616.0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = v as u64;
            return self.visit_u64(n);
        }
        Err(E::invalid_value(Unexpected::Float(v), &self))
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Input {
        #[serde(default, deserialize_with = "super::option_usize")]
        limit: Option<usize>,
    }

    fn parse(value: serde_json::Value) -> Result<Option<usize>, serde_json::Error> {
        serde_json::from_value::<Input>(value).map(|input| input.limit)
    }

    #[test]
    fn accepts_integers_integral_floats_and_absence() {
        assert_eq!(parse(serde_json::json!({"limit": 7})).ok(), Some(Some(7)));
        assert_eq!(
            parse(serde_json::json!({"limit": 100.0})).ok(),
            Some(Some(100))
        );
        assert_eq!(parse(serde_json::json!({"limit": 0.0})).ok(), Some(Some(0)));
        assert_eq!(parse(serde_json::json!({"limit": null})).ok(), Some(None));
        assert_eq!(parse(serde_json::json!({})).ok(), Some(None));
    }

    #[test]
    fn rejects_fractional_negative_and_non_numbers() {
        for bad in [
            serde_json::json!({"limit": 1.5}),
            serde_json::json!({"limit": -1}),
            serde_json::json!({"limit": -1.0}),
            serde_json::json!({"limit": 1e30}),
            serde_json::json!({"limit": "7"}),
        ] {
            let err = parse(bad.clone()).expect_err("must reject");
            assert!(
                err.to_string().contains("a non-negative integer"),
                "{bad}: {err}"
            );
        }
    }

    fn parse_grep(
        value: serde_json::Value,
    ) -> Result<crate::grep::GrepToolInput, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn grep_integer_args_accept_integers_and_integral_floats() {
        let input = parse_grep(serde_json::json!({
            "pattern": "x", "context": 3.0, "before": 7, "after": 0.0, "timeoutMs": 5000.0
        }))
        .expect("integral values parse");
        assert_eq!(input.context, Some(3));
        assert_eq!(input.before, Some(7));
        assert_eq!(input.after, Some(0));
        assert_eq!(input.timeout_ms, Some(5000));
        let absent = parse_grep(serde_json::json!({"pattern": "x"})).expect("absent parses");
        assert_eq!(absent.context, None);
        assert_eq!(absent.timeout_ms, None);
    }

    #[test]
    fn grep_integer_args_reject_fractional_negative_and_out_of_range() {
        for field in ["context", "before", "after", "timeoutMs"] {
            for bad in [
                serde_json::json!(1.5),
                serde_json::json!(-1),
                serde_json::json!(4_294_967_296_u64),
                serde_json::json!("7"),
            ] {
                let err = parse_grep(serde_json::json!({"pattern": "x", field: bad.clone()}))
                    .err()
                    .unwrap_or_else(|| panic!("{field}={bad} must reject"));
                assert!(
                    err.to_string().contains("a non-negative integer"),
                    "{field}={bad}: {err}"
                );
            }
        }
    }
}
