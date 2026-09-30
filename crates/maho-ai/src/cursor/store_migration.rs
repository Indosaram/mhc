//! Port of senpi packages/ai/src/cursor/store-migration.ts.
// ported by todo 13

use std::collections::{BTreeMap, BTreeSet};

use crate::cursor::catalog_grouping::{
    derive_cursor_variant_aliases, normalize_cursor_catalog, CursorCatalogEntry, CursorCatalogRawEntry,
};
use crate::cursor::context_limit_store::resolve_cursor_context_window;
use crate::cursor::model_capabilities::{get_cursor_variant_alias, parse_cursor_variant_id, CursorVariantAlias};
use crate::model::{CursorAgentCompat, CursorReasoning, Model, ModelCompat};
use crate::types::{InputModality, ModelCost, ModelThinkingLevel, ThinkingLevelMap};

/// `model.compat?.cursorReasoning`.
fn cursor_reasoning(model: &Model) -> Option<CursorReasoning> {
    model.compat.as_ref().and_then(|compat| compat.cursor_agent().cursor_reasoning)
}

/// `model.compat?.cursorMaxMode === true`.
fn cursor_max_mode(model: &Model) -> bool {
    model.compat.as_ref().is_some_and(|compat| compat.cursor_agent().cursor_max_mode == Some(true))
}

fn to_modality(value: &str) -> Option<InputModality> {
    match value {
        "text" => Some(InputModality::Text),
        "image" => Some(InputModality::Image),
        "video" => Some(InputModality::Video),
        _ => None,
    }
}

/// Serialize a typed Cursor compat object into the JSON-object `ModelCompat` the `Model` carries.
fn compat_value(compat: &CursorAgentCompat) -> ModelCompat {
    match serde_json::to_value(compat) {
        Ok(serde_json::Value::Object(map)) => ModelCompat(map),
        _ => ModelCompat::default(),
    }
}

/// Fill levels a stored static group lacks from flat alias rows regrouped beside it;
/// the group's own levels, representative, and compat metadata are never replaced.
fn absorb_static_levels(current: &Model, flat_group: Option<&Model>) -> Model {
    let additions: Vec<(ModelThinkingLevel, String)> = flat_group
        .and_then(|model| model.thinking_level_map.as_ref())
        .map(|map| {
            map.iter()
                .filter(|(level, value)| {
                    value.is_some()
                        && current
                            .thinking_level_map
                            .as_ref()
                            .and_then(|current_map| current_map.get(level))
                            .cloned()
                            .flatten()
                            .is_none()
                })
                .map(|(level, value)| (*level, value.clone().expect("filtered to present values")))
                .collect()
        })
        .unwrap_or_default();
    if additions.is_empty() {
        return current.clone();
    }
    let mut thinking_level_map = current.thinking_level_map.clone().unwrap_or_default();
    for (level, value) in additions {
        thinking_level_map.insert(level, Some(value));
    }
    Model { thinking_level_map: Some(thinking_level_map), ..current.clone() }
}

fn entry_to_model(entry: &CursorCatalogEntry, max_tokens_by_id: &BTreeMap<String, u64>) -> Model {
    let representative = entry
        .representative_variant_id
        .clone()
        .or_else(|| entry.legacy_aliases.first().cloned())
        .unwrap_or_else(|| entry.id.clone());
    let max_tokens = max_tokens_by_id
        .get(&representative)
        .or_else(|| max_tokens_by_id.get(&entry.id))
        .copied()
        .unwrap_or(64_000);
    let cursor_reasoning = match (&entry.capability_id, &entry.representative_variant_id) {
        (Some(capability_id), Some(representative_variant_id)) => Some(CursorReasoning {
            capability_id: capability_id.clone(),
            thinking_mode: entry.thinking_mode,
            representative_variant_id: representative_variant_id.clone(),
            variant_ids: entry.variant_ids.clone(),
        }),
        _ => None,
    };
    let compat = CursorAgentCompat {
        cursor_max_mode: if entry.cursor_max_mode { Some(true) } else { None },
        cursor_reasoning,
    };
    Model {
        id: entry.id.clone(),
        name: entry.name.clone(),
        api: "cursor-agent".to_owned(),
        provider: "cursor".to_owned(),
        base_url: "https://api2.cursor.sh".to_owned(),
        reasoning: entry.reasoning,
        thinking_level_map: entry.thinking_level_map.clone(),
        input: entry.input.iter().filter_map(|modality| to_modality(modality)).collect(),
        cost: ModelCost::default(),
        context_window: resolve_cursor_context_window(&entry.id, entry.window) as u64,
        max_tokens,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        upstream_model_id: match &entry.representative_variant_id {
            Some(id) if id != &entry.id => Some(id.clone()),
            _ => None,
        },
        service_tier: None,
        recover_text_tool_calls: None,
        compat: Some(compat_value(&compat)),
    }
}

/// Idempotent stored-catalog transform: pre-grouping 204-variant cursor entries
/// are regrouped into selectable identities, including families the static
/// alias table does not list yet, which are derived over the stored batch
/// (senpi#2038). An already-grouped identity (static or derived) always wins
/// over flat rows aliasing it, regardless of input order, and absorbs their
/// levels without losing its own metadata; conflicting derived variants stay
/// flat, and duplicate identities coalesce at their first position in stable
/// input order.
pub fn regroup_stored_cursor_models(models: &[Model]) -> Vec<Model> {
    // Insertion-ordered maps: `Map` in the TS source, `Vec` of pairs here.
    let mut existing_groups: Vec<(String, Model)> = Vec::new();
    let mut existing_derived: Vec<(String, Model)> = Vec::new();
    let mut represented_target_by_id: BTreeMap<String, String> = BTreeMap::new();
    for model in models {
        let Some(reasoning) = cursor_reasoning(model) else { continue };
        if existing_groups.iter().any(|(id, _)| id == &model.id) {
            continue;
        }
        existing_groups.push((model.id.clone(), model.clone()));
        let Some(variant_ids) = reasoning.variant_ids else { continue };
        existing_derived.push((model.id.clone(), model.clone()));
        for id in variant_ids.values() {
            represented_target_by_id.insert(id.clone(), model.id.clone());
        }
    }
    let mut derive_input: Vec<String> = models
        .iter()
        .filter(|model| cursor_reasoning(model).is_none())
        .map(|model| model.id.clone())
        .collect();
    for (_, model) in &existing_derived {
        if let Some(reasoning) = cursor_reasoning(model)
            && let Some(variant_ids) = reasoning.variant_ids
        {
            derive_input.extend(variant_ids.values().cloned());
        }
    }
    let derived = derive_cursor_variant_aliases(&derive_input);
    let is_legacy = |model: &Model| -> bool {
        cursor_reasoning(model).is_none()
            && (get_cursor_variant_alias(&model.id).is_some()
                || derived.contains_key(&model.id)
                || represented_target_by_id.contains_key(&model.id))
    };
    let legacy: Vec<&Model> = models.iter().filter(|model| is_legacy(model)).collect();
    let max_tokens_by_id: BTreeMap<String, u64> =
        models.iter().map(|model| (model.id.clone(), model.max_tokens)).collect();
    let regrouped: Vec<(String, Model)> = normalize_cursor_catalog(
        &legacy
            .iter()
            .map(|model| CursorCatalogRawEntry {
                id: model.id.clone(),
                name: model.name.clone(),
                input: model
                    .input
                    .iter()
                    .filter(|modality| **modality != InputModality::Video)
                    .map(|modality| match modality {
                        InputModality::Text => "text".to_owned(),
                        InputModality::Image => "image".to_owned(),
                        InputModality::Video => "video".to_owned(),
                    })
                    .collect(),
                cursor_max_mode: cursor_max_mode(model),
            })
            .collect::<Vec<_>>(),
    )
    .into_iter()
    .map(|entry| {
        let id = entry.id.clone();
        (id, entry_to_model(&entry, &max_tokens_by_id))
    })
    .collect();
    let mut merged: Vec<(String, Model)> = Vec::new();
    for (target_id, current) in &existing_groups {
        let kept = cursor_reasoning(current).expect("existing groups carry cursorReasoning");
        if kept.variant_ids.is_none() {
            let flat_group = regrouped.iter().find(|(id, _)| id == target_id).map(|(_, model)| model);
            merged.push((target_id.clone(), absorb_static_levels(current, flat_group)));
            continue;
        }
        let mut variant_ids = kept.variant_ids.clone().expect("checked above");
        let mut thinking_level_map: ThinkingLevelMap = current.thinking_level_map.clone().unwrap_or_default();
        let mut changed = false;
        for model in &legacy {
            let Some(alias) = derived.get(&model.id) else { continue };
            if &alias.target_id != target_id {
                continue;
            }
            let Some(level) = alias.level else { continue };
            if let Some(existing) = variant_ids.get(&level)
                && existing != &model.id
            {
                continue;
            }
            variant_ids.insert(level, model.id.clone());
            thinking_level_map.insert(level, parse_cursor_variant_id(&model.id).level);
            changed = true;
        }
        if changed {
            let mut next_reasoning = kept.clone();
            next_reasoning.variant_ids = Some(variant_ids);
            let mut compat = current.compat.clone().map(|compat| compat.0).unwrap_or_default();
            compat.insert(
                "cursorReasoning".to_owned(),
                serde_json::to_value(&next_reasoning).unwrap_or(serde_json::Value::Null),
            );
            let mut next = current.clone();
            next.thinking_level_map = Some(thinking_level_map);
            next.compat = Some(ModelCompat(compat));
            merged.push((target_id.clone(), next));
        } else {
            merged.push((target_id.clone(), current.clone()));
        }
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<Model> = Vec::new();
    for model in models {
        let legacy_model = is_legacy(model);
        let alias: Option<CursorVariantAlias> = if legacy_model {
            get_cursor_variant_alias(&model.id).or_else(|| derived.get(&model.id).cloned())
        } else {
            None
        };
        let target_id = alias
            .map(|alias| alias.target_id)
            .or_else(|| {
                if legacy_model {
                    represented_target_by_id.get(&model.id).cloned()
                } else {
                    None
                }
            })
            .unwrap_or_else(|| model.id.clone());
        let coalesced = merged.iter().find(|(id, _)| id == &target_id).map(|(_, model)| model);
        if let Some(coalesced) = coalesced {
            let retained_ids = cursor_reasoning(coalesced).and_then(|reasoning| reasoning.variant_ids);
            // Static groups absorb every flat alias row; derived groups keep conflicting variants flat.
            let absorbed = model.id == target_id
                || match &retained_ids {
                    None => legacy_model,
                    Some(ids) => ids.values().any(|id| id == &model.id),
                };
            if !absorbed {
                if !seen.contains(&model.id) {
                    out.push(model.clone());
                }
                seen.insert(model.id.clone());
                continue;
            }
            if !seen.contains(&target_id) {
                out.push(coalesced.clone());
            }
            seen.insert(target_id);
            continue;
        }
        if !legacy_model {
            if !seen.contains(&model.id) {
                out.push(model.clone());
            }
            seen.insert(model.id.clone());
            continue;
        }
        if seen.contains(&target_id) {
            continue;
        }
        seen.insert(target_id.clone());
        let regrouped_model = regrouped.iter().find(|(id, _)| id == &target_id).map(|(_, model)| model.clone());
        out.push(regrouped_model.unwrap_or_else(|| model.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{InputModality, ThinkingSelection, ThinkingSelectionSource};

    /// The 204-id live `GetUsableModels` capture (2026-08-18).
    #[derive(serde::Deserialize)]
    struct FixtureEntry {
        id: String,
        name: String,
        input: Vec<String>,
        #[serde(default)]
        reasoning: bool,
        #[serde(default, rename = "contextWindow")]
        context_window: u64,
        #[serde(default, rename = "cursorMaxMode")]
        cursor_max_mode: bool,
    }

    #[derive(serde::Deserialize)]
    struct UnlistedFixture {
        models: Vec<FixtureEntry>,
    }

    /// The fable family base id, split so the tool layer's display filter cannot corrupt this source file.
    const FABLE1: &str = concat!("claude", "-fable-5", "-1");
    const FABLE1_THINKING: &str = concat!("claude", "-fable-5", "-1", "-thinking");
    const OPUS55: &str = concat!("claude", "-opus-5", "-5");
    const FABLE_THINKING: &str = concat!("claude", "-fable-5", "-thinking");

    fn fixture_204() -> Vec<FixtureEntry> {
        serde_json::from_str(include_str!("../../tests/fixtures/cursor-usable-models-20260818.json"))
            .expect("204 fixture")
    }

    fn unlisted_fixture() -> UnlistedFixture {
        serde_json::from_str(include_str!("../../tests/fixtures/cursor-usable-models-unlisted-20260923.json"))
            .expect("unlisted fixture")
    }

    fn legacy_model(id: &str) -> Model {
        let source = fixture_204().into_iter().find(|entry| entry.id == id);
        Model {
            id: id.to_owned(),
            name: source.as_ref().map(|entry| entry.name.clone()).unwrap_or_else(|| id.to_owned()),
            api: "cursor-agent".to_owned(),
            provider: "cursor".to_owned(),
            base_url: "https://api2.cursor.sh".to_owned(),
            reasoning: source.as_ref().is_some_and(|entry| entry.reasoning),
            thinking_level_map: None,
            input: source
                .as_ref()
                .map(|entry| entry.input.iter().filter_map(|value| to_modality(value)).collect())
                .unwrap_or_else(|| vec![InputModality::Text]),
            cost: ModelCost::default(),
            context_window: source.as_ref().map(|entry| entry.context_window).unwrap_or(200_000),
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

    fn grouped_model() -> Model {
        Model {
            id: "kimi-k3".to_owned(),
            reasoning: true,
            thinking_level_map: Some(level_map(&[
                (ModelThinkingLevel::Off, None),
                (ModelThinkingLevel::Minimal, None),
                (ModelThinkingLevel::Low, Some("low")),
                (ModelThinkingLevel::Medium, None),
                (ModelThinkingLevel::High, Some("high")),
                (ModelThinkingLevel::Xhigh, None),
                (ModelThinkingLevel::Max, Some("max")),
            ])),
            compat: Some(compat_value(&CursorAgentCompat {
                cursor_max_mode: None,
                cursor_reasoning: Some(CursorReasoning {
                    capability_id: "kimi-k3".to_owned(),
                    thinking_mode: None,
                    representative_variant_id: "kimi-k3-high".to_owned(),
                    variant_ids: None,
                }),
            })),
            ..legacy_model("kimi-k3")
        }
    }

    /// A total seven-key map: every level the caller omits is explicitly unsupported.
    fn level_map(observed: &[(ModelThinkingLevel, Option<&str>)]) -> ThinkingLevelMap {
        let mut map: ThinkingLevelMap = ModelThinkingLevel::ALL.into_iter().map(|level| (level, None)).collect();
        for (level, value) in observed {
            map.insert(*level, value.map(str::to_owned));
        }
        map
    }

    fn variant_map(entries: &[(ModelThinkingLevel, &str)]) -> BTreeMap<ModelThinkingLevel, String> {
        entries.iter().map(|(level, id)| (*level, (*id).to_owned())).collect()
    }

    fn stored_flat(id: &str) -> Model {
        Model {
            id: id.to_owned(),
            name: id.to_owned(),
            reasoning: false,
            input: vec![InputModality::Text],
            context_window: 200_000,
            max_tokens: 64_000,
            compat: Some(ModelCompat::default()),
            ..legacy_model(id)
        }
    }

    fn raw_entry(id: &str) -> CursorCatalogRawEntry {
        CursorCatalogRawEntry {
            id: id.to_owned(),
            name: id.to_owned(),
            input: vec!["text".to_owned()],
            cursor_max_mode: false,
        }
    }

    fn normalized_unlisted() -> Vec<CursorCatalogEntry> {
        normalize_cursor_catalog(
            &unlisted_fixture()
                .models
                .iter()
                .map(|entry| CursorCatalogRawEntry {
                    id: entry.id.clone(),
                    name: entry.name.clone(),
                    input: entry.input.clone(),
                    cursor_max_mode: entry.cursor_max_mode,
                })
                .collect::<Vec<_>>(),
        )
    }

    fn find_derived(id: &str) -> CursorCatalogEntry {
        normalized_unlisted()
            .into_iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("expected {id} to be derived as a grouped identity"))
    }

    fn entry_to_cursor_model(entry: &CursorCatalogEntry) -> Model {
        entry_to_model(entry, &BTreeMap::new())
    }

    /// `resolveCursorSelectionDescriptor(model, { level, source: "explicit" }).modelId`.
    fn resolve_cursor_descriptor(model: &Model, level: ModelThinkingLevel) -> String {
        crate::cursor::selection_descriptor::resolve_cursor_selection_descriptor(
            model,
            Some(&ThinkingSelection {
                level,
                source: ThinkingSelectionSource::Explicit,
                legacy_variant_id: None,
            }),
        )
        .model_id
    }

    fn ids(models: &[Model]) -> Vec<String> {
        models.iter().map(|model| model.id.clone()).collect()
    }

    fn stored_grok_batch() -> Vec<Model> {
        ["grok-4.7-low", "grok-4.7-medium", "grok-4.7-high", "grok-4.7-xhigh"]
            .iter()
            .map(|id| stored_flat(id))
            .collect()
    }

    #[test]
    fn regroups_a_full_legacy_204_variant_store_to_the_grouped_catalog_shape() {
        let legacy: Vec<Model> = fixture_204().iter().map(|entry| legacy_model(&entry.id)).collect();
        let out = regroup_stored_cursor_models(&legacy);
        assert_eq!(out.len(), 113);
        assert_eq!(out.iter().filter(|model| model.reasoning).count(), 32);
        let kimi = out.iter().find(|model| model.id == "kimi-k3").expect("kimi-k3");
        assert_eq!(kimi.context_window, 1_048_576);
        assert_eq!(cursor_reasoning(kimi).expect("reasoning").capability_id, "kimi-k3");
    }

    #[test]
    fn is_idempotent_applying_it_twice_yields_the_same_list() {
        let legacy: Vec<Model> = fixture_204().iter().map(|entry| legacy_model(&entry.id)).collect();
        let once = regroup_stored_cursor_models(&legacy);
        let twice = regroup_stored_cursor_models(&once);
        assert_eq!(ids(&twice), ids(&once));
        assert_eq!(
            twice.iter().map(|model| serde_json::to_string(&model.thinking_level_map).expect("json")).collect::<Vec<_>>(),
            once.iter().map(|model| serde_json::to_string(&model.thinking_level_map).expect("json")).collect::<Vec<_>>()
        );
        assert_eq!(
            twice.iter().map(|model| model.context_window).collect::<Vec<_>>(),
            once.iter().map(|model| model.context_window).collect::<Vec<_>>()
        );
        assert_eq!(
            twice.iter().map(|model| model.max_tokens).collect::<Vec<_>>(),
            once.iter().map(|model| model.max_tokens).collect::<Vec<_>>()
        );
    }

    #[test]
    fn handles_mixed_old_new_stores_and_preserves_unknown_entries_in_order() {
        let unknown = Model { id: "zzz-custom-thing".to_owned(), ..legacy_model("zzz-custom-thing") };
        let legacy_entry = legacy_model(&format!("{FABLE_THINKING}-xhigh"));
        let input = vec![unknown, legacy_entry, grouped_model()];
        let out = regroup_stored_cursor_models(&input);
        let out_ids = ids(&out);
        assert!(out_ids.contains(&"zzz-custom-thing".to_owned()));
        assert!(out_ids.contains(&"kimi-k3".to_owned()));
        assert!(out_ids.contains(&FABLE_THINKING.to_owned()));
        assert!(!out_ids.contains(&format!("{FABLE_THINKING}-xhigh")));
        let unknown_out = out.iter().find(|model| model.id == "zzz-custom-thing").expect("unknown kept");
        assert_eq!(unknown_out.context_window, 200_000);
    }

    #[test]
    fn deduplicates_repeated_legacy_ids_deterministically() {
        let input = vec![legacy_model("gpt-5.5-extra-high"), legacy_model("gpt-5.5-extra-high")];
        let out = regroup_stored_cursor_models(&input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "gpt-5.5");
    }

    #[test]
    fn keeps_retained_fast_variant_identities_distinct() {
        let legacy: Vec<Model> = fixture_204().iter().map(|entry| legacy_model(&entry.id)).collect();
        let out = regroup_stored_cursor_models(&legacy);
        let fast_ids: Vec<String> =
            ids(&out).into_iter().filter(|id| id.ends_with("-fast")).collect();
        assert!(fast_ids.len() > 60, "expected more than 60 fast identities, got {}", fast_ids.len());
    }

    #[test]
    fn regroups_stored_flat_grok_variants_into_one_identity_with_variant_ids() {
        let out = regroup_stored_cursor_models(&stored_grok_batch());
        assert_eq!(out.len(), 1);
        let grok = &out[0];
        assert_eq!(grok.id, "grok-4.7");
        assert!(grok.reasoning);
        assert_eq!(grok.upstream_model_id.as_deref(), Some("grok-4.7-medium"));
        assert_eq!(
            grok.thinking_level_map,
            Some(level_map(&[
                (ModelThinkingLevel::Low, Some("low")),
                (ModelThinkingLevel::Medium, Some("medium")),
                (ModelThinkingLevel::High, Some("high")),
                (ModelThinkingLevel::Xhigh, Some("xhigh")),
            ]))
        );
        let reasoning = cursor_reasoning(grok).expect("reasoning");
        assert_eq!(reasoning.capability_id, "grok-4.7");
        assert_eq!(reasoning.representative_variant_id, "grok-4.7-medium");
        assert_eq!(
            reasoning.variant_ids,
            Some(variant_map(&[
                (ModelThinkingLevel::Low, "grok-4.7-low"),
                (ModelThinkingLevel::Medium, "grok-4.7-medium"),
                (ModelThinkingLevel::High, "grok-4.7-high"),
                (ModelThinkingLevel::Xhigh, "grok-4.7-xhigh"),
            ]))
        );
    }

    #[test]
    fn coalesces_a_complete_derived_identity_and_flat_members_in_either_order_at_their_first_position() {
        let complete = entry_to_cursor_model(&find_derived("grok-4.7"));
        let partial = Model {
            thinking_level_map: Some(level_map(&[
                (ModelThinkingLevel::Medium, Some("medium")),
                (ModelThinkingLevel::Xhigh, Some("xhigh")),
            ])),
            compat: Some(compat_value(&CursorAgentCompat {
                cursor_max_mode: None,
                cursor_reasoning: Some(CursorReasoning {
                    capability_id: "grok-4.7".to_owned(),
                    thinking_mode: None,
                    representative_variant_id: "grok-4.7-medium".to_owned(),
                    variant_ids: Some(variant_map(&[
                        (ModelThinkingLevel::Medium, "grok-4.7-medium"),
                        (ModelThinkingLevel::Xhigh, "grok-4.7-xhigh"),
                    ])),
                }),
            })),
            ..complete.clone()
        };
        for existing in [&partial, &complete] {
            for batch in [
                vec![existing.clone(), stored_flat("grok-4.7-low"), stored_flat("grok-4.7-high")],
                vec![stored_flat("grok-4.7-low"), stored_flat("grok-4.7-high"), existing.clone()],
            ] {
                let mut input = vec![stored_flat("before")];
                input.extend(batch);
                input.push(stored_flat("after"));
                let out = regroup_stored_cursor_models(&input);
                assert_eq!(ids(&out), vec!["before".to_owned(), "grok-4.7".to_owned(), "after".to_owned()]);
                let grouped = &out[1];
                assert_eq!(
                    cursor_reasoning(grouped).expect("reasoning").variant_ids,
                    Some(variant_map(&[
                        (ModelThinkingLevel::Low, "grok-4.7-low"),
                        (ModelThinkingLevel::Medium, "grok-4.7-medium"),
                        (ModelThinkingLevel::High, "grok-4.7-high"),
                        (ModelThinkingLevel::Xhigh, "grok-4.7-xhigh"),
                    ]))
                );
                assert_eq!(
                    grouped.thinking_level_map,
                    Some(level_map(&[
                        (ModelThinkingLevel::Low, Some("low")),
                        (ModelThinkingLevel::Medium, Some("medium")),
                        (ModelThinkingLevel::High, Some("high")),
                        (ModelThinkingLevel::Xhigh, Some("xhigh")),
                    ]))
                );
                assert_eq!(regroup_stored_cursor_models(&out), out);
            }
        }
    }

    #[test]
    fn merges_a_single_new_flat_level_into_an_existing_derived_identity() {
        let complete = entry_to_cursor_model(&find_derived("grok-4.7"));
        let out = regroup_stored_cursor_models(&[stored_flat("grok-4.7-minimal"), complete]);
        assert_eq!(ids(&out), vec!["grok-4.7".to_owned()]);
        assert_eq!(
            cursor_reasoning(&out[0]).expect("reasoning").variant_ids.expect("variant ids").get(&ModelThinkingLevel::Minimal),
            Some(&"grok-4.7-minimal".to_owned())
        );
        assert_eq!(regroup_stored_cursor_models(&out), out);
    }

    #[test]
    fn retains_conflicting_xhigh_variants_as_flat_models_while_merging_only_represented_grok_members() {
        let existing = entry_to_cursor_model(
            &normalize_cursor_catalog(&[raw_entry("grok-4.7-low"), raw_entry("grok-4.7-extra-high")])[0],
        );
        let batch = vec![
            existing.clone(),
            stored_flat("grok-4.7-xhigh"),
            stored_flat("grok-4.7-high"),
            stored_flat("grok-4.7-xhigh-fast"),
        ];
        let mut reversed = batch.clone();
        reversed.reverse();
        for listed in [batch, reversed] {
            let out = regroup_stored_cursor_models(&listed);
            let out_ids = ids(&out);
            if listed[0].id == "grok-4.7" {
                assert_eq!(
                    out_ids,
                    vec!["grok-4.7".to_owned(), "grok-4.7-xhigh".to_owned(), "grok-4.7-xhigh-fast".to_owned()]
                );
            } else {
                assert_eq!(
                    out_ids,
                    vec!["grok-4.7-xhigh-fast".to_owned(), "grok-4.7".to_owned(), "grok-4.7-xhigh".to_owned()]
                );
            }
            let group = out.iter().find(|model| model.id == "grok-4.7").expect("group");
            assert_eq!(
                cursor_reasoning(group).expect("reasoning").variant_ids,
                Some(variant_map(&[
                    (ModelThinkingLevel::Low, "grok-4.7-low"),
                    (ModelThinkingLevel::High, "grok-4.7-high"),
                    (ModelThinkingLevel::Xhigh, "grok-4.7-extra-high"),
                ]))
            );
            assert_eq!(
                resolve_cursor_descriptor(group, ModelThinkingLevel::Xhigh),
                "grok-4.7-extra-high".to_owned()
            );
            let flat = out.iter().find(|model| model.id == "grok-4.7-xhigh").expect("flat");
            assert!(!flat.reasoning);
            assert_eq!(resolve_cursor_descriptor(flat, ModelThinkingLevel::High), "grok-4.7-xhigh".to_owned());
            assert_eq!(regroup_stored_cursor_models(&out), out);
        }
    }

    #[test]
    fn coalesces_the_thinking_identity_even_when_every_incoming_level_conflicts() {
        let existing = entry_to_cursor_model(
            &normalize_cursor_catalog(&[
                raw_entry(&format!("{FABLE1_THINKING}-low")),
                raw_entry(&format!("{FABLE1_THINKING}-high")),
            ])[0],
        );
        let batch = vec![
            existing.clone(),
            stored_flat(&format!("{FABLE1}-low-thinking")),
            stored_flat(&format!("{FABLE1}-high-thinking")),
        ];
        let mut reversed = batch.clone();
        reversed.reverse();
        for listed in [batch, reversed] {
            let out = regroup_stored_cursor_models(&listed);
            let out_ids = ids(&out);
            if listed[0].id == FABLE1_THINKING {
                assert_eq!(
                    out_ids,
                    vec![
                        FABLE1_THINKING.to_owned(),
                        format!("{FABLE1}-low-thinking"),
                        format!("{FABLE1}-high-thinking")
                    ]
                );
            } else {
                assert_eq!(
                    out_ids,
                    vec![
                        format!("{FABLE1}-high-thinking"),
                        format!("{FABLE1}-low-thinking"),
                        FABLE1_THINKING.to_owned()
                    ]
                );
            }
            let group = out.iter().find(|model| model.id == FABLE1_THINKING).expect("group");
            assert_eq!(
                cursor_reasoning(group).expect("reasoning").variant_ids,
                Some(variant_map(&[
                    (ModelThinkingLevel::Low, &format!("{FABLE1_THINKING}-low")),
                    (ModelThinkingLevel::High, &format!("{FABLE1_THINKING}-high")),
                ]))
            );
            assert_eq!(
                resolve_cursor_descriptor(group, ModelThinkingLevel::Low),
                format!("{FABLE1_THINKING}-low")
            );
            for id in [format!("{FABLE1}-low-thinking"), format!("{FABLE1}-high-thinking")] {
                let flat = out.iter().find(|model| model.id == id).expect("flat");
                assert!(!flat.reasoning);
                assert_eq!(resolve_cursor_descriptor(flat, ModelThinkingLevel::High), id.clone());
            }
            assert_eq!(regroup_stored_cursor_models(&out), out);
        }
    }

    #[test]
    fn coalesces_duplicate_conflicting_flat_ids_without_consuming_them_into_the_retained_group() {
        let existing = entry_to_cursor_model(
            &normalize_cursor_catalog(&[raw_entry("grok-4.7-low"), raw_entry("grok-4.7-extra-high")])[0],
        );
        let conflicting = stored_flat("grok-4.7-xhigh");
        let duplicate = Model { name: "duplicate".to_owned(), ..conflicting.clone() };
        let out = regroup_stored_cursor_models(&[existing, conflicting.clone(), duplicate]);
        assert_eq!(ids(&out), vec!["grok-4.7".to_owned(), "grok-4.7-xhigh".to_owned()]);
        assert_eq!(out[1], conflicting);
        assert_eq!(regroup_stored_cursor_models(&out), out);
    }

    #[test]
    fn coalesces_duplicate_stored_derived_identities_without_any_flat_entries_or_new_metadata() {
        let existing = entry_to_cursor_model(&find_derived("grok-4.7"));
        let duplicate = Model { name: "second copy".to_owned(), ..existing.clone() };
        for listed in [
            vec![
                stored_flat("before"),
                existing.clone(),
                stored_flat("between"),
                duplicate.clone(),
                stored_flat("after"),
            ],
            vec![
                stored_flat("before"),
                duplicate.clone(),
                stored_flat("between"),
                existing.clone(),
                stored_flat("after"),
            ],
        ] {
            let out = regroup_stored_cursor_models(&listed);
            assert_eq!(
                ids(&out),
                vec![
                    "before".to_owned(),
                    "grok-4.7".to_owned(),
                    "between".to_owned(),
                    "after".to_owned()
                ]
            );
            assert_eq!(out[1], listed[1]);
            assert_eq!(regroup_stored_cursor_models(&out), out);
        }
    }

    #[test]
    fn is_idempotent_on_its_own_output() {
        let once = regroup_stored_cursor_models(&stored_grok_batch());
        let twice = regroup_stored_cursor_models(&once);
        assert_eq!(
            twice.iter().map(|model| serde_json::to_string(model).expect("json")).collect::<Vec<_>>(),
            once.iter().map(|model| serde_json::to_string(model).expect("json")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn keeps_unknown_flat_entries_in_stable_order_beside_the_derived_identity() {
        let mut input = vec![stored_flat("zzz-custom-thing")];
        input.extend(stored_grok_batch());
        input.push(stored_flat("grok-4.7-high-fast"));
        let out = regroup_stored_cursor_models(&input);
        assert_eq!(
            ids(&out),
            vec!["zzz-custom-thing".to_owned(), "grok-4.7".to_owned(), "grok-4.7-high-fast".to_owned()]
        );
    }

    /// `regroupStoredCursorModels(ids.map(storedFlat))[0]` for a batch that groups.
    fn static_group(group_ids: &[&str]) -> Model {
        let out = regroup_stored_cursor_models(&group_ids.iter().map(|id| stored_flat(id)).collect::<Vec<_>>());
        let group = out.first().cloned().expect("a grouped identity");
        assert!(cursor_reasoning(&group).is_some(), "expected {} to group", group_ids.join(","));
        group
    }

    #[test]
    fn absorbs_a_level_the_static_group_lacks_without_replacing_its_representative() {
        let partial = static_group(&["kimi-k3-low"]);
        for stored in
            [vec![partial.clone(), stored_flat("kimi-k3-max")], vec![stored_flat("kimi-k3-max"), partial.clone()]]
        {
            let out = regroup_stored_cursor_models(&stored);
            assert_eq!(ids(&out), vec!["kimi-k3".to_owned()]);
            let mut expected = partial.thinking_level_map.clone().expect("partial map");
            expected.insert(ModelThinkingLevel::Max, Some("max".to_owned()));
            assert_eq!(out[0].thinking_level_map, Some(expected));
            assert_eq!(out[0].compat, partial.compat);
            assert_eq!(regroup_stored_cursor_models(&out), out);
        }
    }

    /// Every stored unlisted family regrouped into a derived identity (variantIds present).
    fn derived_models() -> Vec<Model> {
        let models: Vec<Model> =
            unlisted_fixture().models.iter().map(|entry| stored_flat(&entry.id)).collect();
        regroup_stored_cursor_models(&models)
            .into_iter()
            .filter(|model| cursor_reasoning(model).and_then(|reasoning| reasoning.variant_ids).is_some())
            .collect()
    }

    #[test]
    fn advertises_grok_4_7_levels_exactly_as_listed_clamping_unlisted_ones_onto_a_listed_wire_id() {
        let grok = derived_models().into_iter().find(|model| model.id == "grok-4.7").expect("derived grok-4.7");
        assert_eq!(
            crate::models::get_supported_thinking_levels(&grok),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Xhigh
            ]
        );
        assert_eq!(crate::models::clamp_thinking_level(&grok, ModelThinkingLevel::Off), ModelThinkingLevel::Low);
        assert_eq!(crate::models::clamp_thinking_level(&grok, ModelThinkingLevel::Minimal), ModelThinkingLevel::Low);
        assert_eq!(crate::models::clamp_thinking_level(&grok, ModelThinkingLevel::Max), ModelThinkingLevel::Xhigh);
        let effective = crate::models::clamp_thinking_level(&grok, ModelThinkingLevel::Off);
        assert_eq!(resolve_cursor_descriptor(&grok, effective), "grok-4.7-low".to_owned());
    }

    #[test]
    fn never_offers_or_sends_a_level_the_catalog_did_not_list_for_every_derived_family() {
        let models = derived_models();
        assert_eq!(
            ids(&models),
            vec![
                FABLE1.to_owned(),
                FABLE1_THINKING.to_owned(),
                OPUS55.to_owned(),
                "gemini-3.8-flash".to_owned(),
                "grok-4.7".to_owned(),
                "muse-spark-1.3".to_owned(),
            ]
        );
        for model in &models {
            let variant_ids = cursor_reasoning(model).and_then(|reasoning| reasoning.variant_ids).unwrap_or_default();
            let listed: Vec<ModelThinkingLevel> = ModelThinkingLevel::ALL
                .into_iter()
                .filter(|level| variant_ids.contains_key(level))
                .collect();
            assert_eq!(crate::models::get_supported_thinking_levels(model), listed, "{}", model.id);
            for requested in ModelThinkingLevel::ALL {
                let effective = crate::models::clamp_thinking_level(model, requested);
                assert!(listed.contains(&effective), "{}:{requested:?}", model.id);
                assert_eq!(
                    resolve_cursor_descriptor(model, effective),
                    variant_ids.get(&effective).cloned().expect("listed level has a wire id"),
                    "{}:{requested:?}",
                    model.id
                );
            }
        }
    }

    #[test]
    fn maps_every_derived_level_to_its_exact_server_listed_variant_id() {
        let model = entry_to_cursor_model(&find_derived("grok-4.7"));
        for level in [ModelThinkingLevel::Low, ModelThinkingLevel::Medium, ModelThinkingLevel::High, ModelThinkingLevel::Xhigh]
        {
            assert_eq!(resolve_cursor_descriptor(&model, level), format!("grok-4.7-{}", level.as_str()));
        }
    }

    #[test]
    fn falls_back_to_the_representative_for_a_level_missing_from_variant_ids() {
        let model = entry_to_cursor_model(&find_derived("grok-4.7"));
        for level in [ModelThinkingLevel::Off, ModelThinkingLevel::Minimal, ModelThinkingLevel::Max] {
            assert_eq!(resolve_cursor_descriptor(&model, level), "grok-4.7-medium".to_owned());
        }
    }

    #[test]
    fn accepts_derived_member_ids_as_legacy_variant_selections_and_rejects_unknown_ones() {
        let model = entry_to_cursor_model(&find_derived("grok-4.7"));
        let accepted = ThinkingSelection {
            level: ModelThinkingLevel::Xhigh,
            source: ThinkingSelectionSource::LegacyVariant,
            legacy_variant_id: Some("grok-4.7-xhigh".to_owned()),
        };
        assert_eq!(
            crate::cursor::selection_descriptor::resolve_cursor_selection_descriptor(&model, Some(&accepted)).model_id,
            "grok-4.7-xhigh"
        );
        let rejected = ThinkingSelection {
            level: ModelThinkingLevel::Low,
            source: ThinkingSelectionSource::LegacyVariant,
            legacy_variant_id: Some("grok-4.7-turbo".to_owned()),
        };
        assert_eq!(
            crate::cursor::selection_descriptor::resolve_cursor_selection_descriptor(&model, Some(&rejected)).model_id,
            "grok-4.7-medium"
        );
    }
}
