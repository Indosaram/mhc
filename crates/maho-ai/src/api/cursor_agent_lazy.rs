//! Port of senpi packages/ai/src/api/cursor-agent.lazy.ts.
// ported by todo 12
//!
//! The TS wrapper exists only to keep the Node-only HTTP/2 transport out of
//! browser/eager import graphs through a variable-specifier dynamic import;
//! Rust has no bundler to guard against, so this module is the direct
//! ProviderStreams entry point the lazy wrapper would have produced.

use crate::types::{Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::cursor_agent::{self, CursorDiscoveredModel, FetchCursorUsableModelsOptions};

pub use super::cursor_agent::{stream, stream_simple};

pub fn fetch_cursor_usable_models(
    options: FetchCursorUsableModelsOptions,
) -> impl std::future::Future<Output = Option<Vec<CursorDiscoveredModel>>> {
    cursor_agent::fetch_cursor_usable_models(options)
}

pub struct CursorAgentApi;

impl ProviderStreams for CursorAgentApi {
    fn stream(&self, model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
        cursor_agent::stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        cursor_agent::stream_simple(model, context, options)
    }
}

pub fn cursor_agent_api() -> CursorAgentApi {
    CursorAgentApi
}
