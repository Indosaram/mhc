//! Port of senpi packages/ai/src/providers/cursor.ts.
//!
//! The provider ships no static baseline: Cursor's catalog is discovered per account through
//! api/cursor-agent.ts (todo 12), which owns fetchCursorUsableModels, and the stored-catalog
//! regrouping lives in cursor/store-migration.ts (todo 12). Both are referenced through the
//! builtin api registry seam until those modules land.

use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::providers::builtin_api_streams;
use std::sync::Arc;

pub const CURSOR_BASE_URL: &str = "https://api2.cursor.sh";

pub fn cursor_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "cursor".into(),
        name: Some("Cursor".into()),
        base_url: Some(CURSOR_BASE_URL.to_owned()),
        headers: None,
        models: Vec::new(),
        fetch_models: None,
        restore_models: None,
        filter_models: None,
        api: ProviderApi::Single(builtin_api_streams("cursor-agent")),
    })
}
