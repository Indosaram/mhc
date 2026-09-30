//! Port of senpi packages/ai/src/providers/openai-completions.ts: a re-export shim of
//! `api/openai-completions.ts` (todo 10), which owns the wire implementation.
//!
//! senpi re-exports the api instance's `stream`/`streamSimple` under the provider-facing names
//! `stream[OI]Completions`/`streamSimple[OI]Completions`. The instance is resolved through the
//! builtin api registry on every call (the `api_registry` seam todo 5 installed) so this module
//! links without the wire module being ported yet; an unregistered api id yields the same
//! setup-error stream the lazy boundary produces.

use crate::providers::builtin_api_streams;
use crate::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions};

pub fn stream_openai_completions(
    model: &Model,
    context: &Context,
    options: Option<StreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("openai-completions").stream(model, context, options)
}

pub fn stream_simple_openai_completions(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("openai-completions").stream_simple(model, context, options)
}
