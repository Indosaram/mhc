//! Port of senpi packages/ai/src/cursor/selection-descriptor.ts.
// ported by todo 13

use crate::cursor::model_capabilities::{get_cursor_variant_alias, CursorLevelSpec, CURSOR_MODEL_CAPABILITIES};
use crate::model::{CursorAgentCompat, Model};
use crate::types::{ModelThinkingLevel, ThinkingSelection, ThinkingSelectionSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorResolvedParameter {
    pub id: &'static str,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorResolvedSelection {
    pub model_id: String,
    pub parameters: Vec<CursorResolvedParameter>,
}

fn identity_compat(model: &Model) -> Option<CursorAgentCompat> {
    let compat = model.compat.as_ref()?.cursor_agent();
    if compat.cursor_reasoning.is_none() { None } else { Some(compat) }
}

/// Catalog-guaranteed suffix alias for a base + level, or `None`. Cursor Run
/// rejects bare capability ids with Connect `not_found` (issue #1008; live
/// probes 2026-08-20), so every resolvable level prefers the suffix variant id
/// the `GetUsableModels` catalog actually serves. Candidates try the level's
/// wire value first, then the level token itself (`extra-high` vs `xhigh`),
/// with thinking-infixed forms for thinking Claude identities.
fn suffix_alias_id(compat: &CursorAgentCompat, level: ModelThinkingLevel, spec: &CursorLevelSpec) -> Option<String> {
    let reasoning = compat.cursor_reasoning.as_ref().expect("checked by caller");
    let level_token = level.as_str();
    let suffixes: Vec<&str> =
        if level == ModelThinkingLevel::Off { vec!["none"] } else if spec.value == level_token { vec![spec.value.as_str()] } else { vec![spec.value.as_str(), level_token] };
    for suffix in suffixes {
        let candidates: Vec<String> = if reasoning.thinking_mode == Some(true) {
            vec![
                format!("{}-thinking-{}", reasoning.capability_id, suffix),
                format!("{}-{}-thinking", reasoning.capability_id, suffix),
            ]
        } else {
            vec![format!("{}-{}", reasoning.capability_id, suffix)]
        };
        for candidate in candidates {
            if let Some(alias) = get_cursor_variant_alias(&candidate) {
                return Some(alias.legacy_variant_id);
            }
        }
    }
    None
}

fn build_parameters(capability_id: &str, value: &str, thinking_mode: Option<bool>) -> Vec<CursorResolvedParameter> {
    let Some(capability) = CURSOR_MODEL_CAPABILITIES.get(capability_id) else { return Vec::new() };
    let mut out = Vec::new();
    for id in &capability.parameter_order {
        match *id {
            "thinking" => out.push(CursorResolvedParameter {
                id: "thinking",
                value: if thinking_mode == Some(true) { "true".to_owned() } else { "false".to_owned() },
            }),
            "context" => {
                let context = capability.request_context.as_ref().or(capability.default_context.as_ref());
                if let Some(context) = context {
                    out.push(CursorResolvedParameter { id: "context", value: context.clone() });
                }
            }
            "effort" => out.push(CursorResolvedParameter { id: "effort", value: value.to_owned() }),
            "reasoning" => out.push(CursorResolvedParameter { id: "reasoning", value: value.to_owned() }),
            "fast" => out.push(CursorResolvedParameter { id: "fast", value: "false".to_owned() }),
            _ => {}
        }
    }
    out
}

/// Resolve a Cursor model + thinking selection to its wire descriptor: the exact
/// model id plus ordered parameters. Both Cursor transports consume this; the
/// native lane renders parameters into protobuf, the CLI lane renders a model
/// string. Absent/unsupported selections return the representative or upstream
/// id with zero parameters. Identities derived from unlisted suffix variants
/// resolve each level through `cursorReasoning.variantIds` before any capability
/// lookup (senpi#2038).
pub fn resolve_cursor_selection_descriptor(
    model: &Model,
    selection: Option<&ThinkingSelection>,
) -> CursorResolvedSelection {
    let fallback = || CursorResolvedSelection {
        model_id: model.upstream_model_id.clone().unwrap_or_else(|| model.id.clone()),
        parameters: Vec::new(),
    };
    let Some(compat) = identity_compat(model) else { return fallback() };
    let reasoning = compat.cursor_reasoning.as_ref().expect("checked by identity_compat");

    let Some(selection) = selection else {
        return CursorResolvedSelection { model_id: reasoning.representative_variant_id.clone(), parameters: Vec::new() };
    };

    if selection.source == ThinkingSelectionSource::LegacyVariant {
        let legacy_id = selection.legacy_variant_id.clone();
        let allowlisted = legacy_id.as_deref().is_some_and(|id| {
            get_cursor_variant_alias(id).is_some()
                || reasoning.variant_ids.as_ref().is_some_and(|map| map.values().any(|v| v == id))
        });
        if !allowlisted {
            return CursorResolvedSelection { model_id: reasoning.representative_variant_id.clone(), parameters: Vec::new() };
        }
        return CursorResolvedSelection { model_id: legacy_id.expect("allowlisted implies present"), parameters: Vec::new() };
    }

    if let Some(variant_ids) = reasoning.variant_ids.as_ref() {
        if let Some(derived_variant_id) = variant_ids.get(&selection.level) {
            return CursorResolvedSelection { model_id: derived_variant_id.clone(), parameters: Vec::new() };
        }
        return CursorResolvedSelection { model_id: reasoning.representative_variant_id.clone(), parameters: Vec::new() };
    }

    let capability = CURSOR_MODEL_CAPABILITIES.get(reasoning.capability_id.as_str());
    let spec = capability.and_then(|c| c.levels.get(&selection.level));
    let (Some(_capability), Some(spec)) = (capability, spec) else {
        return CursorResolvedSelection { model_id: reasoning.representative_variant_id.clone(), parameters: Vec::new() };
    };

    if let Some(suffix_id) = suffix_alias_id(&compat, selection.level, spec) {
        return CursorResolvedSelection { model_id: suffix_id, parameters: Vec::new() };
    }

    if spec.encoding == "variant-id" {
        return CursorResolvedSelection { model_id: reasoning.representative_variant_id.clone(), parameters: Vec::new() };
    }

    CursorResolvedSelection {
        model_id: reasoning.capability_id.clone(),
        parameters: build_parameters(&reasoning.capability_id, &spec.value, reasoning.thinking_mode),
    }
}

/// Render the resolved descriptor as one CLI `--model` argv element (bracket or suffix form).
pub fn render_cursor_cli_model_string(model: &Model, selection: Option<&ThinkingSelection>) -> String {
    let resolved = resolve_cursor_selection_descriptor(model, selection);
    if resolved.parameters.is_empty() {
        return resolved.model_id;
    }
    let args =
        resolved.parameters.iter().map(|p| format!("{}={}", p.id, p.value)).collect::<Vec<_>>().join(",");
    format!("{}[{}]", resolved.model_id, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CursorReasoning, ModelCompat};
    use crate::types::{InputModality, ModelCost};
    use std::collections::BTreeMap;

    fn base_model() -> Model {
        Model {
            id: "cursor-grok-4.6".to_owned(),
            name: "Cursor Grok 4.6".to_owned(),
            api: "cursor-agent".to_owned(),
            provider: "cursor".to_owned(),
            base_url: "https://api2.cursor.sh".to_owned(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![InputModality::Text],
            cost: ModelCost::default(),
            context_window: 500_000,
            max_tokens: 64_000,
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
            compat: None,
        }
    }

    fn with_reasoning(mut model: Model, reasoning: CursorReasoning) -> Model {
        let mut map = serde_json::Map::new();
        map.insert("cursorReasoning".to_owned(), serde_json::to_value(reasoning).expect("serialize"));
        model.compat = Some(ModelCompat(map));
        model
    }

    #[test]
    fn returns_fallback_when_model_has_no_cursor_reasoning_compat() {
        let model = base_model();
        let resolved = resolve_cursor_selection_descriptor(&model, None);
        assert_eq!(resolved, CursorResolvedSelection { model_id: "cursor-grok-4.6".to_owned(), parameters: Vec::new() });
    }

    #[test]
    fn returns_representative_when_selection_is_none() {
        let model = with_reasoning(
            base_model(),
            CursorReasoning {
                capability_id: "cursor-grok-4.6".to_owned(),
                thinking_mode: None,
                representative_variant_id: "cursor-grok-4.6-medium".to_owned(),
                variant_ids: None,
            },
        );
        let resolved = resolve_cursor_selection_descriptor(&model, None);
        assert_eq!(resolved.model_id, "cursor-grok-4.6-medium");
        assert!(resolved.parameters.is_empty());
    }

    #[test]
    fn resolves_a_capability_level_to_its_suffix_variant() {
        let model = with_reasoning(
            base_model(),
            CursorReasoning {
                capability_id: "cursor-grok-4.6".to_owned(),
                thinking_mode: None,
                representative_variant_id: "cursor-grok-4.6-medium".to_owned(),
                variant_ids: None,
            },
        );
        let selection = ThinkingSelection { level: ModelThinkingLevel::High, source: ThinkingSelectionSource::Explicit, legacy_variant_id: None };
        let resolved = resolve_cursor_selection_descriptor(&model, Some(&selection));
        assert_eq!(resolved.model_id, "cursor-grok-4.6-high");
        assert!(resolved.parameters.is_empty());
    }

    #[test]
    fn accepts_a_derived_legacy_variant_selection_and_rejects_unknown_ones() {
        let variant_ids = BTreeMap::from([(ModelThinkingLevel::Xhigh, "grok-4.7-xhigh".to_owned())]);
        let model = with_reasoning(
            base_model(),
            CursorReasoning {
                capability_id: "grok-4.7".to_owned(),
                thinking_mode: None,
                representative_variant_id: "grok-4.7-medium".to_owned(),
                variant_ids: Some(variant_ids),
            },
        );
        let selection = ThinkingSelection {
            level: ModelThinkingLevel::Xhigh,
            source: ThinkingSelectionSource::LegacyVariant,
            legacy_variant_id: Some("grok-4.7-xhigh".to_owned()),
        };
        assert_eq!(
            resolve_cursor_selection_descriptor(&model, Some(&selection)),
            CursorResolvedSelection { model_id: "grok-4.7-xhigh".to_owned(), parameters: Vec::new() }
        );
        let unknown = ThinkingSelection {
            level: ModelThinkingLevel::Low,
            source: ThinkingSelectionSource::LegacyVariant,
            legacy_variant_id: Some("grok-4.7-turbo".to_owned()),
        };
        assert_eq!(
            resolve_cursor_selection_descriptor(&model, Some(&unknown)),
            CursorResolvedSelection { model_id: "grok-4.7-medium".to_owned(), parameters: Vec::new() }
        );
    }

    #[test]
    fn renders_the_cli_model_string_with_bracketed_parameters() {
        let model = with_reasoning(
            base_model(),
            CursorReasoning {
                capability_id: "gpt-5.1".to_owned(),
                thinking_mode: None,
                representative_variant_id: "gpt-5.1".to_owned(),
                variant_ids: None,
            },
        );
        let selection = ThinkingSelection { level: ModelThinkingLevel::Low, source: ThinkingSelectionSource::Explicit, legacy_variant_id: None };
        let rendered = render_cursor_cli_model_string(&model, Some(&selection));
        assert_eq!(rendered, "gpt-5.1[reasoning=low]");
    }

    #[test]
    fn renders_the_cli_model_string_without_brackets_when_there_are_no_parameters() {
        let model = with_reasoning(
            base_model(),
            CursorReasoning {
                capability_id: "cursor-grok-4.6".to_owned(),
                thinking_mode: None,
                representative_variant_id: "cursor-grok-4.6-medium".to_owned(),
                variant_ids: None,
            },
        );
        assert_eq!(render_cursor_cli_model_string(&model, None), "cursor-grok-4.6-medium");
    }
}
