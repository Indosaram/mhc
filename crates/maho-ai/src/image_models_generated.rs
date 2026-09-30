//! Port of senpi packages/ai/src/image-models.generated.ts.
//!
//! Generated data: `crates/maho-ai/data/image-models.json` is written by `bun tools/golden/gen-models.mjs`
//! from the pinned senpi `IMAGE_MODELS` export.

use crate::types::ImagesModel;
use indexmap::IndexMap;
use std::sync::LazyLock;

pub const IMAGE_MODELS_JSON: &str = include_str!("../data/image-models.json");

pub static IMAGE_MODELS: LazyLock<IndexMap<String, IndexMap<String, ImagesModel>>> = LazyLock::new(|| {
    serde_json::from_str(IMAGE_MODELS_JSON).unwrap_or_else(|error| panic!("embedded image-models.json is invalid: {error}"))
});
