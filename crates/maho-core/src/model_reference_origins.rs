//! Port of senpi packages/coding-agent/src/core/model-reference-origins.ts.
//!
//! Registers only schema-listed value slots of a models.json provider as trusted reference origins, on the
//! retained snapshot (upstream remembers the origin in a WeakMap; here it is stored on the value itself).

use crate::config_reference_resolver::{
    ConfigReferenceContext, ConfigValue, remember_config_reference_origin,
};
use crate::model_config_schema::ModelsJsonProvider;

pub fn remember_model_reference_origins(
    provider: &mut ModelsJsonProvider,
    origin: &str,
    provider_id: &str,
) {
    let remember = |value: &mut ConfigValue, field: String| {
        remember_config_reference_origin(value, ConfigReferenceContext::trusted(origin, field));
    };
    let field = format!("providers.{provider_id}");
    if let Some(api_key) = provider.api_key.as_mut() {
        remember(api_key, format!("{field}.apiKey"));
    }
    for (name, value) in provider.headers.iter_mut().flatten() {
        remember(value, format!("{field}.headers.{name}"));
    }
    for (index, model) in provider.models.iter_mut().flatten().enumerate() {
        for (name, value) in model.headers.iter_mut().flatten() {
            remember(value, format!("{field}.models.{index}.headers.{name}"));
        }
    }
    for (id, model) in provider.model_overrides.iter_mut().flatten() {
        for (name, value) in model.headers.iter_mut().flatten() {
            remember(value, format!("{field}.modelOverrides.{id}.headers.{name}"));
        }
    }
    if let Some(policy) = provider.credentials.as_mut() {
        for (name, slot) in policy.slots.iter_mut().flatten() {
            if let Some(value) = slot.value.as_mut() {
                remember(value, format!("{field}.credentials.slots.{name}.value"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_reference_resolver::config_reference_context;

    /// senpi test/suite/regressions/3858-model-references.test.ts: loaded references carry their models.json
    /// origin and field.
    #[test]
    fn every_declared_slot_is_remembered_as_trusted() {
        let mut provider: ModelsJsonProvider = serde_json::from_value(serde_json::json!({
            "apiKey": {"$env": "K"},
            "headers": {"x": {"$env": "H"}},
            "models": [{"id": "m", "headers": {"y": {"$env": "M"}}}],
            "credentials": {"slots": {"work": {"value": {"$env": "W"}}}}
        }))
        .expect("provider");
        remember_model_reference_origins(&mut provider, "/cfg/models.json", "p");
        let api_key = provider.api_key.as_ref().expect("key");
        let source = config_reference_context(api_key, None).expect("origin");
        assert_eq!(source.authority, "trusted");
        assert_eq!(source.field, "providers.p.apiKey");
        assert_eq!(source.origin, "/cfg/models.json");
        let model = &provider.models.as_ref().expect("models")[0];
        assert_eq!(
            config_reference_context(&model.headers.as_ref().expect("headers")["y"], None)
                .expect("origin")
                .field,
            "providers.p.models.0.headers.y"
        );
        let slot = &provider
            .credentials
            .as_ref()
            .expect("policy")
            .slots
            .as_ref()
            .expect("slots")["work"];
        assert!(config_reference_context(slot.value.as_ref().expect("value"), None).is_some());
    }
}
