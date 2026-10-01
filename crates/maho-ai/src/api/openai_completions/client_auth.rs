//! Port of senpi packages/ai/src/api/openai-client-auth.ts.
//!
//! Node-private copy: `src/api/openai_client_auth.rs` is owned by node 12-misc. Kept here so this
//! lane compiles standalone; delete it once 12-misc merges.

use super::auth_headers::{has_credential_headers, has_header};
use crate::types::ProviderHeaders;

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
    let mut merged: ProviderHeaders = ProviderHeaders::new();
    merged.insert("Authorization".into(), None);
    if let Some(headers) = headers {
        for (name, value) in headers {
            merged.insert(name.clone(), value.clone());
        }
    }
    Ok(OpenAiClientAuth { api_key: "unused".into(), headers: Some(merged) })
}
