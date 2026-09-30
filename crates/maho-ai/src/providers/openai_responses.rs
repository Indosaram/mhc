//! Port of senpi packages/ai/src/providers/openai-responses.ts: a re-export shim of
//! `api/openai-responses.ts` (todo 10). Resolved through the builtin api registry seam.

use crate::providers::builtin_api_streams;
use crate::types::{AssistantMessageEventStream, Context, Model, SimpleStreamOptions, StreamOptions};

pub fn stream_openai_responses(
    model: &Model,
    context: &Context,
    options: Option<StreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("openai-responses").stream(model, context, options)
}

pub fn stream_simple_openai_responses(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    builtin_api_streams("openai-responses").stream_simple(model, context, options)
}
