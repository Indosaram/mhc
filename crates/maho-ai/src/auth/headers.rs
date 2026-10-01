//! Port of senpi packages/ai/src/auth/headers.ts.

use crate::types::ProviderHeaders;
use fancy_regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

const CREDENTIAL_HEADER_SUFFIXES: &[&str] =
    &["api-key", "api-token", "auth-token", "access-token", "authorization", "client-secret"];

static BASIC_OR_BEARER_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:basic|bearer)\s*$").expect("valid regex"));

pub fn is_credential_header_name(name: &str) -> bool {
    let normalized = name.to_lowercase();
    CREDENTIAL_HEADER_SUFFIXES
        .iter()
        .any(|suffix| normalized == *suffix || normalized.ends_with(&format!("-{suffix}")))
}

/// `hasHeader`: true when `headers` carries a non-blank value for `name` (case-insensitive).
pub fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else { return false };
    let lowered = name.to_lowercase();
    match effective_headers(headers).get(&lowered) {
        Some(Some(value)) => !value.trim().is_empty(),
        _ => false,
    }
}

pub fn has_credential_headers(headers: Option<&ProviderHeaders>) -> bool {
    let Some(headers) = headers else { return false };
    for (name, value) in effective_headers(headers) {
        let Some(value) = value else { continue };
        if !is_credential_header_name(&name) {
            continue;
        }
        let normalized = value.trim();
        if normalized.is_empty() {
            continue;
        }
        if name.ends_with("authorization")
            && BASIC_OR_BEARER_ONLY.is_match(&normalized.to_lowercase()).unwrap_or(false)
        {
            continue;
        }
        return true;
    }
    false
}

fn effective_headers(headers: &ProviderHeaders) -> HashMap<String, Option<String>> {
    let mut effective = HashMap::new();
    for (name, value) in headers {
        effective.insert(name.to_lowercase(), value.clone());
    }
    effective
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_of(name: &str, value: &str) -> ProviderHeaders {
        let mut headers = ProviderHeaders::new();
        headers.insert(name.to_string(), Some(value.to_string()));
        headers
    }

    #[test]
    fn recognizes_credential_bearing_header_names() {
        for name in [
            "Authorization",
            "Proxy-Authorization",
            "x-api-key",
            "x-goog-api-key",
            "anthropic-api-key",
            "cf-aig-authorization",
            "x-auth-token",
            "x-access-token",
            "cf-access-client-secret",
        ] {
            assert!(is_credential_header_name(name), "{name} should be credential-bearing");
            assert!(has_credential_headers(Some(&headers_of(name, "credential"))), "{name} should be detected");
        }
    }

    #[test]
    fn does_not_treat_non_credential_headers_as_credential_bearing() {
        for name in ["User-Agent", "x-request-id", "x-trace-token", "content-type"] {
            assert!(!is_credential_header_name(name), "{name} should not be credential-bearing");
            assert!(!has_credential_headers(Some(&headers_of(name, "metadata"))), "{name} should not be detected");
        }
    }

    #[test]
    fn uses_last_case_insensitive_value_and_rejects_empty_authorization_schemes() {
        let mut headers = ProviderHeaders::new();
        headers.insert("Authorization".into(), Some("Bearer configured".into()));
        headers.insert("authorization".into(), Some(String::new()));
        assert!(!has_credential_headers(Some(&headers)));

        let mut bearer_only = ProviderHeaders::new();
        bearer_only.insert("Authorization".into(), Some("Bearer ".into()));
        assert!(!has_credential_headers(Some(&bearer_only)));

        let mut blank_key = ProviderHeaders::new();
        blank_key.insert("x-api-key".into(), Some("   ".into()));
        assert!(!has_credential_headers(Some(&blank_key)));
    }
}
