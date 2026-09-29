//! Port of senpi packages/ai/src/providers/openai-completions.ts: a re-export shim of
//! `api/openai-completions.ts` (todo 10). The wire implementation lives in `api::openai_completions`;
//! this module re-exports its stream functions under the provider-facing names senpi uses.

pub use crate::api::openai_completions::{stream as stream_openai_completions, stream_simple as stream_simple_openai_completions};
