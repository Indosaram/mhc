//! Port of senpi packages/ai/src/providers/opencode-go.ts.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::{builtin_api_streams, opencode_headers::with_open_code_session_header};
use indexmap::IndexMap;
use std::sync::Arc;

pub fn opencode_go_provider() -> Arc<dyn Provider> {
    let mut api = IndexMap::new();
    for id in ["anthropic-messages", "openai-completions", "openai-responses"] {
        api.insert(id.to_owned(), with_open_code_session_header(builtin_api_streams(id)));
    }
    create_provider(CreateProviderOptions {
        id: "opencode-go".into(),
        name: Some("OpenCode Go".into()),
        base_url: None,
        headers: None,
        models: super::opencode_go_models::opencode_go_models(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::ByApi(api),
    })
}
