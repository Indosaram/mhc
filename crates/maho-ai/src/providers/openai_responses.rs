//! Port of senpi packages/ai/src/providers/openai-responses.ts: a re-export shim of
//! `api/openai-responses.ts` (todo 10).

pub use crate::api::openai_responses::{stream as stream_openai_responses, stream_simple as stream_simple_openai_responses};
