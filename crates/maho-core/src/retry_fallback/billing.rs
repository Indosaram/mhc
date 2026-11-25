use regex::Regex;
use std::sync::OnceLock;

pub fn is_billing_error_message(message: Option<&str>) -> bool {
    static BILLING_PATTERN: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
    static STRONG_BILLING_PATTERN: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
    static TRANSIENT_SERVER_SIGNAL: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();

    let Some(message) = message else {
        return false;
    };
    let strong = STRONG_BILLING_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)credit[- ]balance|insufficient[_ -]quota|purchase credits|credits[-_ ]required|credits are required|usage_limit_reached|usage_not_included|usage limit has been reached")
    });
    if strong.as_ref().is_ok_and(|p| p.is_match(message)) {
        return true;
    }
    let billing = BILLING_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)credit[- ]balance|insufficient[_ -]quota|\bbilling\b|purchase credits|credits[-_ ]required|credits are required|usage_limit_reached|usage_not_included|usage limit has been reached")
    });
    if !billing.as_ref().is_ok_and(|p| p.is_match(message)) {
        return false;
    }
    // senpi#4632: bare "Billing" prose on a transient 5xx is a gateway dependency failure,
    // not a spent account — it must not pin the fallback or hold the whole provider.
    let transient = TRANSIENT_SERVER_SIGNAL.get_or_init(|| {
        Regex::new(
            r#"(?i)(?:^|[^0-9])(?:500|502|503|504|520|522|524)(?:[^0-9]|$)|"type"\s*:\s*"server_error""#,
        )
    });
    !transient.as_ref().is_ok_and(|p| p.is_match(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_and_credits_are_billing_but_rate_limits_are_not() {
        for message in [
            "credit balance exhausted",
            "insufficient_quota",
            "BILLING limit",
            "purchase credits",
            "credits_required",
            "credits are required",
            "usage_limit_reached",
            "usage_not_included",
            "usage limit has been reached",
        ] {
            assert!(is_billing_error_message(Some(message)));
        }
        for message in ["rate limit", "HTTP 429", "rebillingx"] {
            assert!(!is_billing_error_message(Some(message)));
        }
        assert!(!is_billing_error_message(None));
    }
}
