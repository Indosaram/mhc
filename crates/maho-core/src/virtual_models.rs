//! Port of senpi packages/coding-agent/src/core/virtual-models.ts (catalog and branch helpers).
//!
//! Virtual models are catalog entries that route each request to a physical model. The selection may
//! name a virtual model; everything below the routing step only sees physical models. A virtual model
//! never reaches a provider. Branch entries and messages are the JSON session shapes.
//!
//! Not ported here: `withVirtualModels`'s `Provider` wrapper (unrouted stream, auth) and the router
//! request types, which belong to the model runtime; `physical_catalog` carries its catalog rule.

use std::collections::{BTreeMap, HashSet};

use maho_ai::model::Model;
use maho_ai::types::{InputModality, ModelCost, ModelThinkingLevel};
use serde_json::Value;

/// API id of virtual catalog entries. Requests for it fail unless routed first.
pub const VIRTUAL_MODEL_API: &str = "pi-virtual";

/// Custom entry type that stores router state on the session branch.
pub const VIRTUAL_MODEL_STATE_ENTRY: &str = "pi.virtual-model-state";

/// The catalog fields of a virtual model definition (upstream `Omit<VirtualModelDefinition, "route">`).
#[derive(Debug, Clone, Default)]
pub struct VirtualModelDefinition {
    pub provider: String,
    pub id: String,
    pub name: String,
    /// Thinking levels offered for selection. Defaults to `[Off]`.
    pub thinking_levels: Option<Vec<ModelThinkingLevel>>,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    /// Defaults to text and images.
    pub input: Option<Vec<InputModality>>,
}

/// Whether a model's (or message's) api names a virtual model.
pub fn is_virtual_model(api: &str) -> bool {
    api == VIRTUAL_MODEL_API
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Latest successful response. Its model is physical: failed or aborted requests are skipped.
pub fn find_latest_response(messages: &[Value]) -> Option<&Value> {
    messages.iter().rev().find(|message| {
        str_field(message, "role") == Some("assistant")
            && !matches!(str_field(message, "stopReason"), Some("error" | "aborted"))
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSelection {
    pub provider: String,
    pub model_id: String,
}

fn model_change(entry: &Value) -> Option<BranchSelection> {
    (str_field(entry, "type") == Some("model_change")).then(|| BranchSelection {
        provider: str_field(entry, "provider").unwrap_or_default().to_owned(),
        model_id: str_field(entry, "modelId").unwrap_or_default().to_owned(),
    })
}

/// The model selection a session branch records. A virtual `model_change` holds until the next
/// `model_change`; otherwise the latest physical response wins. A virtual model that is no longer
/// registered does not hold. `get_model_api` returns the catalog api of a model, when registered.
pub fn get_branch_selection(
    branch: &[Value],
    get_model_api: impl FnOnce(&str, &str) -> Option<String>,
) -> Option<BranchSelection> {
    for (index, entry) in branch.iter().enumerate().rev() {
        if let Some(change) = model_change(entry) {
            return Some(change);
        }
        let Some(message) = entry
            .get("message")
            .filter(|_| str_field(entry, "type") == Some("message"))
        else {
            continue;
        };
        if str_field(message, "role") != Some("assistant")
            || is_virtual_model(str_field(message, "api").unwrap_or(""))
        {
            continue;
        }
        let response = BranchSelection {
            provider: str_field(message, "provider")
                .unwrap_or_default()
                .to_owned(),
            model_id: str_field(message, "model").unwrap_or_default().to_owned(),
        };
        let change = branch[..index].iter().rev().find_map(model_change);
        return Some(match change {
            Some(change)
                if get_model_api(&change.provider, &change.model_id)
                    .is_some_and(|api| is_virtual_model(&api)) =>
            {
                change
            }
            _ => response,
        });
    }
    None
}

/// Latest router state a session branch stores for a virtual model.
pub fn get_virtual_model_state<'a>(
    branch: &'a [Value],
    provider: &str,
    model_id: &str,
) -> Option<&'a Value> {
    branch.iter().rev().find_map(|entry| {
        if str_field(entry, "type") != Some("custom")
            || str_field(entry, "customType") != Some(VIRTUAL_MODEL_STATE_ENTRY)
        {
            return None;
        }
        let data = entry.get("data")?;
        (str_field(data, "provider") == Some(provider)
            && str_field(data, "modelId") == Some(model_id))
        .then(|| data.get("state"))
        .flatten()
    })
}

/// Build the catalog entry of a virtual model.
pub fn create_virtual_model(definition: &VirtualModelDefinition) -> Model {
    let levels = definition
        .thinking_levels
        .clone()
        .unwrap_or_else(|| vec![ModelThinkingLevel::Off]);
    let thinking_level_map: BTreeMap<ModelThinkingLevel, Option<String>> = ModelThinkingLevel::ALL
        .iter()
        .map(|level| {
            (
                *level,
                levels.contains(level).then(|| level.as_str().to_owned()),
            )
        })
        .collect();
    Model {
        id: definition.id.clone(),
        name: definition.name.clone(),
        api: VIRTUAL_MODEL_API.to_owned(),
        provider: definition.provider.clone(),
        base_url: String::new(),
        reasoning: levels.iter().any(|level| *level != ModelThinkingLevel::Off),
        default_thinking_level: None,
        thinking_level_map: Some(thinking_level_map),
        prompt_cache: None,
        supports_assistant_prefill: None,
        input: definition
            .input
            .clone()
            .unwrap_or_else(|| vec![InputModality::Text, InputModality::Image]),
        cost: ModelCost::default(),
        context_window: definition.context_window.unwrap_or(0),
        max_tokens: definition.max_tokens.unwrap_or(0),
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: None,
        deprecated: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
        input_limits: None,
    }
}

/// A provider's catalog with its virtual models added: a virtual model hides a physical chat model with
/// the same id (a catalog refresh can add one after registration), and stale virtual entries are dropped.
pub fn physical_catalog(provider_models: Vec<Model>, virtual_models: &[Model]) -> Vec<Model> {
    let ids: HashSet<&str> = virtual_models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    provider_models
        .into_iter()
        .filter(|model| !is_virtual_model(&model.api) && !ids.contains(model.id.as_str()))
        .chain(virtual_models.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_a_virtual_catalog_entry_with_only_offered_levels() {
        let model = create_virtual_model(&VirtualModelDefinition {
            provider: "router".into(),
            id: "auto".into(),
            name: "Auto".into(),
            thinking_levels: Some(vec![ModelThinkingLevel::Off, ModelThinkingLevel::High]),
            ..VirtualModelDefinition::default()
        });
        assert!(is_virtual_model(&model.api));
        assert!(model.reasoning);
        let map = model.thinking_level_map.unwrap_or_default();
        assert_eq!(
            map.get(&ModelThinkingLevel::High),
            Some(&Some("high".to_owned()))
        );
        assert_eq!(map.get(&ModelThinkingLevel::Low), Some(&None));
        assert_eq!(model.input, vec![InputModality::Text, InputModality::Image]);
        assert!(!create_virtual_model(&VirtualModelDefinition::default()).reasoning);
    }

    #[test]
    fn a_registered_virtual_change_holds_over_physical_responses() {
        let branch = vec![
            json!({"type": "model_change", "provider": "router", "modelId": "auto"}),
            json!({"type": "message", "message": {"role": "assistant", "api": "openai-responses", "provider": "openai", "model": "gpt"}}),
        ];
        let virtual_api = |_: &str, _: &str| Some(VIRTUAL_MODEL_API.to_owned());
        let selection = get_branch_selection(&branch, virtual_api);
        assert_eq!(
            selection,
            Some(BranchSelection {
                provider: "router".into(),
                model_id: "auto".into()
            })
        );
        let selection = get_branch_selection(&branch, |_, _| None);
        assert_eq!(
            selection,
            Some(BranchSelection {
                provider: "openai".into(),
                model_id: "gpt".into()
            })
        );
        assert_eq!(get_branch_selection(&[], |_, _| None), None);
    }

    #[test]
    fn reads_router_state_and_the_latest_successful_response() {
        let branch = vec![
            json!({"type": "custom", "customType": VIRTUAL_MODEL_STATE_ENTRY, "data": {"provider": "r", "modelId": "a", "state": 1}}),
            json!({"type": "custom", "customType": VIRTUAL_MODEL_STATE_ENTRY, "data": {"provider": "r", "modelId": "b", "state": 2}}),
        ];
        assert_eq!(get_virtual_model_state(&branch, "r", "a"), Some(&json!(1)));
        assert_eq!(get_virtual_model_state(&branch, "r", "c"), None);
        let messages = vec![
            json!({"role": "assistant", "stopReason": "stop", "id": 1}),
            json!({"role": "assistant", "stopReason": "error", "id": 2}),
        ];
        assert_eq!(
            find_latest_response(&messages).and_then(|m| m.get("id")),
            Some(&json!(1))
        );
    }

    #[test]
    fn a_virtual_model_hides_a_physical_model_with_its_id() {
        let mut physical = create_virtual_model(&VirtualModelDefinition {
            id: "auto".into(),
            ..Default::default()
        });
        physical.api = "openai-responses".into();
        let mut other = physical.clone();
        other.id = "gpt".into();
        let stale = create_virtual_model(&VirtualModelDefinition {
            id: "old".into(),
            ..Default::default()
        });
        let virtual_model = create_virtual_model(&VirtualModelDefinition {
            id: "auto".into(),
            ..Default::default()
        });
        let catalog = physical_catalog(
            vec![physical, other, stale],
            std::slice::from_ref(&virtual_model),
        );
        let ids: Vec<(&str, &str)> = catalog
            .iter()
            .map(|m| (m.id.as_str(), m.api.as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![("gpt", "openai-responses"), ("auto", VIRTUAL_MODEL_API)]
        );
    }
}
