//! Port of senpi packages/ai/src/api/openai-completions.lazy.ts.

use crate::types::ProviderStreams;
use std::sync::Arc;

pub fn open_ai_completions_api() -> Arc<dyn ProviderStreams> {
    super::openai_completions::open_ai_completions_api()
}
