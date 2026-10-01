//! Port of senpi packages/ai/src/auth/headers.ts.
//!
//! Node-private copy: `src/auth/headers.rs` is owned by node 13-auth, which has not merged into
//! this lane's base. `api/openai-client-auth.ts` (ported here) needs `hasHeader` and
//! `hasCredentialHeaders`, so the file is ported verbatim under the owning module's directory to
//! keep this lane self-contained. When 13-auth lands, delete this module and call
//! `crate::auth::headers` instead.

use crate::types::ProviderHeaders;

const CREDENTIAL_HEADER_SUFFIXES: [&str; 6] =
    ["api-key", "api-token", "auth-token", "access-token", "authorization", "client-secret"];

pub fn is_credential_header_name(name: &str) -> bool {
    let normalized = name.to_lowercase();
    CREDENTIAL_HEADER_SUFFIXES
        .iter()
        .any(|suffix| normalized == *suffix || normalized.ends_with(&format!("-{suffix}")))
}

pub fn has_header(headers: Option<&ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else { return false };
    match effective_headers(headers).get(&name.to_lowercase()) {
        Some(Some(value)) => !value.trim().is_empty(),
        _ => false,
    }
}

pub fn has_credential_headers(headers: Option<&ProviderHeaders>) -> bool {
    let Some(headers) = headers else { return false };
    for (name, value) in effective_headers(headers) {
        if !is_credential_header_name(&name) {
            continue;
        }
        let Some(value) = value else { continue };
        let normalized = value.trim();
        if normalized.is_empty() {
            continue;
        }
        if name.ends_with("authorization") && is_bare_basic_or_bearer(normalized) {
            continue;
        }
        return true;
    }
    false
}

/// `^(?:basic|bearer)\s*$` (case-insensitive).
fn is_bare_basic_or_bearer(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower
        .strip_prefix("basic")
        .or_else(|| lower.strip_prefix("bearer"))
        .is_some_and(|rest| rest.chars().all(char::is_whitespace))
}

fn effective_headers(headers: &ProviderHeaders) -> std::collections::BTreeMap<String, Option<String>> {
    let mut effective = std::collections::BTreeMap::new();
    for (name, value) in headers {
        effective.insert(name.to_lowercase(), value.clone());
    }
    effective
}
