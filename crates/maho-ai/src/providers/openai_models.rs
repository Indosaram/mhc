//! Port of senpi packages/ai/src/providers/openai.models.ts.

use crate::providers::builtin_provider_models;
use crate::types::Model;

pub fn openai_models() -> Vec<Model> {
    builtin_provider_models("openai")
}
