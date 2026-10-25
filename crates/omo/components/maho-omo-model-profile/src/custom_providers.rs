//! Port of omo-senpi `components/model-profile/custom-providers.ts` at omo `v5.1.47`: the providers
//! the user declared in `<agent_dir>/models.json` with models of their own.
//!
//! A builtin profile rung may resolve to one of these after its listed providers (#10485), but only
//! on a declared id: models.json entries are merged into a built-in provider's own catalog, so
//! matching by provider name alone would let that catalog serve ids the user never declared
//! (#9146). An entry that only overrides a built-in provider (`baseUrl`, credentials) declares no
//! models and is not listed.

use std::path::Path;

use maho_ext_api::JsonValue;

use crate::models_json::read_models_json;

/// Upstream `DeclaredModelProvider`: one models.json provider and the ids it declares, in
/// declaration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredModelProvider {
    pub provider: String,
    pub model_ids: Vec<String>,
}

fn declared_model_ids(entry: Option<&JsonValue>) -> Vec<String> {
    let Some(JsonValue::Object(entry)) = entry else {
        return Vec::new();
    };
    let Some(JsonValue::Array(models)) = entry.get("models") else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|model| {
            let JsonValue::Object(model) = model else {
                return None;
            };
            model
                .get("id")
                .and_then(JsonValue::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

/// Upstream `customModelProviders(agentDir)`: models.json providers that declare models of their
/// own, in declaration order. An absent agent dir, an absent file and a malformed `providers`
/// section all answer an empty list.
pub fn custom_model_providers(agent_dir: Option<&Path>) -> Vec<DeclaredModelProvider> {
    let Some(agent_dir) = agent_dir else {
        return Vec::new();
    };
    let Some(parsed) = read_models_json(agent_dir) else {
        return Vec::new();
    };
    let Some(JsonValue::Object(providers)) = parsed.get("providers") else {
        return Vec::new();
    };
    providers
        .iter()
        .filter_map(|(provider, entry)| {
            let model_ids = declared_model_ids(Some(entry));
            if model_ids.is_empty() {
                return None;
            }
            Some(DeclaredModelProvider { provider: provider.clone(), model_ids })
        })
        .collect()
}
