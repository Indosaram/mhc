//! Port of senpi packages/ai/src/image-models.generated.ts.
//!
//! Generated data: `crates/maho-ai/data/image-models.json` came from the senpi `IMAGE_MODELS` export before senpi#9948
//! folded it into models.generated.ts; `tools/golden/gen-models.mjs` no longer writes it (docs/divergences.md CAT-1).

use crate::types::ImagesModel;
use indexmap::IndexMap;
use std::sync::LazyLock;

pub const IMAGE_MODELS_JSON: &str = include_str!("../data/image-models.json");

pub static IMAGE_MODELS: LazyLock<IndexMap<String, IndexMap<String, ImagesModel>>> =
    LazyLock::new(|| {
        serde_json::from_str(IMAGE_MODELS_JSON)
            .unwrap_or_else(|error| panic!("embedded image-models.json is invalid: {error}"))
    });
