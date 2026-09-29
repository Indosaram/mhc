//! Port of senpi packages/ai/src/utils/headers.ts.

use crate::types::ProviderHeaders;
use std::collections::BTreeMap;

pub fn headers_to_record(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in headers {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        result
            .entry(key.as_str().to_owned())
            .and_modify(|existing| {
                existing.push_str(", ");
                existing.push_str(&value);
            })
            .or_insert(value);
    }
    result
}

pub fn provider_headers_to_record(headers: Option<&ProviderHeaders>) -> Option<BTreeMap<String, String>> {
    let result: BTreeMap<String, String> =
        headers?.iter().filter_map(|(k, v)| v.as_ref().map(|v| (k.clone(), v.clone()))).collect();
    if result.is_empty() { None } else { Some(result) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_null_headers_and_empty_maps() {
        let mut headers = ProviderHeaders::new();
        headers.insert("a".into(), Some("1".into()));
        headers.insert("b".into(), None);
        assert_eq!(provider_headers_to_record(Some(&headers)).map(|m| m.len()), Some(1));
        let mut only_null = ProviderHeaders::new();
        only_null.insert("b".into(), None);
        assert_eq!(provider_headers_to_record(Some(&only_null)), None);
        assert_eq!(provider_headers_to_record(None), None);

        let mut map = reqwest::header::HeaderMap::new();
        map.append("x-a", reqwest::header::HeaderValue::from_static("1"));
        map.append("x-a", reqwest::header::HeaderValue::from_static("2"));
        assert_eq!(headers_to_record(&map).get("x-a").map(String::as_str), Some("1, 2"));
    }
}
