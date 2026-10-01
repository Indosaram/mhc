//! Port of senpi packages/ai/src/api/openai-client-auth.ts.
//!
//! `hasHeader`/`hasCredentialHeaders` live in senpi's auth/headers.ts (todo 13). This module needs
//! them to stand alone until that lane lands, so the two predicates are ported here verbatim and
//! should be replaced by `crate::auth::headers::*` once todo 13 fills that module.

use crate::types::ProviderHeaders;

const CREDENTIAL_HEADER_SUFFIXES: &[&str] =
    &["api-key", "api-token", "auth-token", "access-token", "authorization", "client-secret"];

fn is_credential_header_name(name: &str) -> bool {
    let normalized = name.to_lowercase();
    CREDENTIAL_HEADER_SUFFIXES
        .iter()
        .any(|suffix| normalized == *suffix || normalized.ends_with(&format!("-{suffix}")))
}

fn effective_headers(headers: &ProviderHeaders) -> std::collections::BTreeMap<String, Option<String>> {
    headers.iter().map(|(name, value)| (name.to_lowercase(), value.clone())).collect()
}

pub fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else { return false };
    effective_headers(headers)
        .get(&name.to_lowercase())
        .and_then(|value| value.as_ref())
        .is_some_and(|value| !value.trim().is_empty())
}

pub fn has_credential_headers(headers: Option<&ProviderHeaders>) -> bool {
    let Some(headers) = headers else { return false };
    for (name, value) in effective_headers(headers) {
        if !is_credential_header_name(&name) {
            continue;
        }
        let Some(normalized) = value.as_ref().map(|value| value.trim().to_owned()) else { continue };
        if normalized.is_empty() {
            continue;
        }
        if name.ends_with("authorization") {
            let lowered = normalized.to_lowercase();
            if lowered == "basic" || lowered == "bearer" {
                continue;
            }
        }
        return true;
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiClientAuth {
    pub api_key: String,
    pub headers: Option<ProviderHeaders>,
}

pub fn resolve_openai_client_auth(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&ProviderHeaders>,
) -> Result<OpenAiClientAuth, String> {
    if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
        return Ok(OpenAiClientAuth { api_key: api_key.to_owned(), headers: headers.cloned() });
    }
    if !has_credential_headers(headers) {
        return Err(format!("No API key for provider: {provider}"));
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok(OpenAiClientAuth { api_key: "unused".into(), headers: headers.cloned() });
    }
    let mut with_authorization: ProviderHeaders = ProviderHeaders::new();
    with_authorization.insert("Authorization".into(), None);
    for (name, value) in headers.unwrap_or(&ProviderHeaders::new()) {
        with_authorization.insert(name.clone(), value.clone());
    }
    Ok(OpenAiClientAuth { api_key: "unused".into(), headers: Some(with_authorization) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(entries: &[(&str, Option<&str>)]) -> ProviderHeaders {
        entries.iter().map(|(k, v)| ((*k).to_owned(), v.map(str::to_owned))).collect()
    }

    #[test]
    fn an_explicit_api_key_wins() {
        let auth = resolve_openai_client_auth("openai", Some("sk-x"), None).expect("auth");
        assert_eq!(auth.api_key, "sk-x");
        assert_eq!(auth.headers, None);
    }

    #[test]
    fn missing_credentials_report_the_ts_error() {
        assert_eq!(
            resolve_openai_client_auth("openai", None, None),
            Err("No API key for provider: openai".into())
        );
        assert_eq!(
            resolve_openai_client_auth("openai", Some(""), Some(&headers(&[("x-other", Some("1"))]))),
            Err("No API key for provider: openai".into())
        );
    }

    #[test]
    fn authorization_headers_pass_through_without_an_extra_override() {
        let provided = headers(&[("Authorization", Some("Bearer t"))]);
        let auth = resolve_openai_client_auth("openai", None, Some(&provided)).expect("auth");
        assert_eq!(auth.api_key, "unused");
        assert_eq!(auth.headers, Some(provided));
    }

    #[test]
    fn other_credential_headers_get_an_explicit_authorization_deletion() {
        let auth = resolve_openai_client_auth("openai", None, Some(&headers(&[("x-api-key", Some("k"))])))
            .expect("auth");
        let resolved = auth.headers.expect("headers");
        assert_eq!(resolved.get("Authorization"), Some(&None));
        assert_eq!(resolved.get("x-api-key"), Some(&Some("k".to_owned())));
    }

    #[test]
    fn bare_basic_or_bearer_values_do_not_count_as_credentials() {
        assert!(!has_credential_headers(Some(&headers(&[("Authorization", Some("Bearer"))]))));
        assert!(has_credential_headers(Some(&headers(&[("Authorization", Some("Bearer t"))]))));
    }
}
