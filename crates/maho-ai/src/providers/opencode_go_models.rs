//! Port of senpi packages/ai/src/providers/opencode-go.models.ts.
//!
//! `*_MODELS` is a plain function (not `models_generated::MODELS`'s static `LazyLock`) because
//! callers need an owned `Vec<Model>` for `CreateProviderOptions::models`; the source data is the
//! same embedded catalog todo 5 generated from senpi's `*.models.ts` shard.

use crate::providers::builtin_provider_models;
use crate::types::Model;

pub fn opencode_go_models() -> Vec<Model> {
    builtin_provider_models("opencode-go")
}
