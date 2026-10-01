use regex::Regex;
use std::sync::OnceLock;

pub fn is_billing_error_message(message: Option<&str>) -> bool {
    static PATTERN: OnceLock<Result<Regex,regex::Error>> = OnceLock::new();
    message.is_some_and(|message| PATTERN.get_or_init(||Regex::new(r"(?i)credit[- ]balance|insufficient[_ -]quota|\bbilling\b|purchase credits|credits[-_ ]required|credits are required|usage_limit_reached|usage_not_included|usage limit has been reached")).as_ref().is_ok_and(|pattern|pattern.is_match(message)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_and_credits_are_billing_but_rate_limits_are_not() {
        for message in ["credit balance exhausted","insufficient_quota","BILLING limit","purchase credits","credits_required","credits are required","usage_limit_reached","usage_not_included","usage limit has been reached"] {assert!(is_billing_error_message(Some(message)));}
        for message in ["rate limit","HTTP 429","rebillingx"] {assert!(!is_billing_error_message(Some(message)));}
        assert!(!is_billing_error_message(None));
    }
}
