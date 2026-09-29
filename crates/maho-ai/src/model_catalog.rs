//! Port of senpi packages/ai/src/model-catalog.ts.

use crate::types::Model;
use indexmap::IndexMap;

/// API -> model id -> model groups, as the per-provider `*.models.ts` shards are keyed.
pub type ModelGroups = IndexMap<String, IndexMap<String, Model>>;

/// `Object.assign({}, ...Object.values(groups))`: later groups overwrite earlier ids in place.
pub fn flatten_model_catalog(_provider: &str, groups: &ModelGroups) -> IndexMap<String, Model> {
    let mut flat = IndexMap::new();
    for group in groups.values() {
        for (id, model) in group {
            flat.insert(id.clone(), model.clone());
        }
    }
    flat
}
