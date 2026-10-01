//! Port of senpi packages/ai/src/providers/qwen-token-plan-individual.models.ts.
//!
//! `*_MODELS` is a plain function (not `models_generated::MODELS`'s static `LazyLock`) because
//! callers need an owned `Vec<Model>` for `CreateProviderOptions::models`; the source data is the
//! same embedded catalog todo 5 generated from senpi's `*.models.ts` shard.

use crate::providers::builtin_provider_models;
use crate::types::Model;

pub fn qwen_token_plan_individual_models() -> Vec<Model> {
    builtin_provider_models("qwen-token-plan-individual")
}
