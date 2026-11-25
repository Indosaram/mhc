//! Port of senpi packages/coding-agent/src/core/fast-model-pair.ts.

use maho_ai::model::Model;

const FAST_MODEL_SUFFIX: &str = "-fast";

pub trait FastPairRegistry {
    fn find(&self, provider: &str, id: &str) -> Option<Model>;
    fn get_upstream_model_id(&self, model: &Model) -> Option<String>;
    fn get_service_tier(&self, model: &Model) -> Option<String>;
}

fn request_model_id(registry: &dyn FastPairRegistry, model: &Model) -> String {
    registry
        .get_upstream_model_id(model)
        .unwrap_or_else(|| model.id.clone())
}

fn is_compatible_fast_variant(
    registry: &dyn FastPairRegistry,
    base_model: &Model,
    fast_model: &Model,
) -> bool {
    fast_model.provider == base_model.provider
        && fast_model.api == base_model.api
        && registry.get_service_tier(fast_model).as_deref() == Some("priority")
        && request_model_id(registry, fast_model) == request_model_id(registry, base_model)
}

pub fn find_base_model(registry: &dyn FastPairRegistry, fast_model: &Model) -> Option<Model> {
    let base_id = fast_model.id.strip_suffix(FAST_MODEL_SUFFIX)?;
    let base_model = registry.find(&fast_model.provider, base_id)?;
    is_compatible_fast_variant(registry, &base_model, fast_model).then_some(base_model)
}

pub fn find_fast_model(registry: &dyn FastPairRegistry, base_model: &Model) -> Option<Model> {
    if base_model.id.ends_with(FAST_MODEL_SUFFIX) {
        return None;
    }
    let fast_id = format!("{}{FAST_MODEL_SUFFIX}", base_model.id);
    let fast_model = registry.find(&base_model.provider, &fast_id)?;
    is_compatible_fast_variant(registry, base_model, &fast_model).then_some(fast_model)
}
