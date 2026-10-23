//! Port of senpi packages/ai/src/utils/reported-count.ts.

use serde_json::Value;

/// A provider-reported token count, or `None` when the provider sent none: absent, `null`, or not
/// a finite number all mean "not reported", never 0 (senpi#3879).
pub fn reported_count(value: Option<&Value>) -> Option<f64> {
    value?.as_f64().filter(|number| number.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_finite_numbers_are_reported() {
        assert_eq!(reported_count(Some(&json!(12))), Some(12.0));
        assert_eq!(reported_count(Some(&json!(0))), Some(0.0));
        assert_eq!(reported_count(Some(&json!(null))), None);
        assert_eq!(reported_count(Some(&json!("5"))), None);
        assert_eq!(reported_count(None), None);
    }
}
