//! Port of senpi imagegen/credentials.ts.
use crate::auth::Credentials;
use std::collections::BTreeMap;

pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

pub fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Compares two base URLs ignoring trailing slashes.
pub fn same_base_url(left: &str, right: &str) -> bool {
    left.trim_end_matches('/') == right.trim_end_matches('/')
}

/// Seeded placeholder keys (`SK-SENTINEL-DO-NOT-LOG-*`) are not credentials.
pub fn is_placeholder_key(api_key: &str) -> bool {
    api_key
        .to_ascii_uppercase()
        .starts_with("SK-SENTINEL-DO-NOT-LOG")
}

pub fn credential_parts(value: Option<Credentials>) -> Option<(String, BTreeMap<String, String>)> {
    let value = value?;
    let key = non_empty(value.api_key.as_deref())?;
    if is_placeholder_key(key) {
        return None;
    }
    let headers = value
        .headers
        .into_iter()
        .filter_map(|(name, value)| {
            value
                .filter(|value| !value.trim().is_empty())
                .map(|value| (name, value))
        })
        .collect();
    Some((key.into(), headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base_urls_compare_without_trailing_slashes() {
        assert!(same_base_url("https://api.openai.com/v1/", OPENAI_BASE_URL));
        assert!(same_base_url("https://a/v1//", "https://a/v1"));
        assert!(!same_base_url("https://a/v1", "https://b/v1"));
    }
}
