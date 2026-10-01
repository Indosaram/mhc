//! Port of senpi packages/ai/src/cursor/catalog-grouping.ts.
// ported by todo 13

use std::collections::{BTreeMap, BTreeSet};

use crate::cursor::model_capabilities::{
    get_cursor_variant_alias, parse_cursor_variant_id, CursorVariantAlias, CursorVariantParse,
    CURSOR_MODEL_CAPABILITIES,
};
use crate::types::{ModelThinkingLevel, ThinkingLevelMap};

pub use crate::cursor::model_capabilities::parse_cursor_variant_id as parse_cursor_variant_id_reexport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorCatalogRawEntry {
    pub id: String,
    pub name: String,
    pub input: Vec<String>,
    pub cursor_max_mode: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CursorCatalogEntry {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub window: f64,
    pub max_window: Option<f64>,
    pub input: Vec<String>,
    pub cursor_max_mode: bool,
    pub capability_id: Option<String>,
    pub thinking_mode: Option<bool>,
    pub representative_variant_id: Option<String>,
    pub legacy_aliases: Vec<String>,
    /// Derived-group variant ids: normalized thinking level -> the exact server-listed
    /// variant id the live catalog serves. Present only on identities derived at
    /// runtime from ids the static alias table does not list (senpi#2038).
    pub variant_ids: Option<BTreeMap<ModelThinkingLevel, String>>,
}

const ALL_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];
const FALLBACK_WINDOW: f64 = 200_000.0;

fn static_targets() -> BTreeSet<String> {
    crate::cursor::model_capabilities::all_cursor_variant_aliases().into_iter().map(|alias| alias.target_id).collect()
}

struct GroupMember {
    raw: CursorCatalogRawEntry,
    alias: CursorVariantAlias,
    level: Option<String>,
    thinking: Option<bool>,
    fast: bool,
}

fn is_claude(base_id: &str) -> bool {
    base_id.starts_with("claude-")
}

fn clean_name(members: &[GroupMember], base_id: &str, thinking_mode: Option<bool>) -> String {
    let level_words = regex::Regex::new(r"(Minimal|Low|Medium|High|Extra High|XHigh|Max|None)").expect("regex");
    let representative = members
        .iter()
        .find(|m| m.level.as_deref() == Some("high"))
        .or_else(|| members.iter().find(|m| m.level.as_deref() == Some("medium")))
        .or_else(|| members.iter().find(|m| m.level.as_deref() == Some("low")))
        .unwrap_or(&members[0]);
    let mut name = representative.raw.name.clone();
    name = regex::Regex::new(r"\s*\(NO ZDR\)\s*").expect("regex").replace_all(&name, " \u{1}").trim().to_owned();
    let word_re = regex::Regex::new(&format!(r"\s+{}\b", level_words.as_str())).expect("regex");
    name = word_re.replace_all(&name, "").into_owned();
    name = regex::Regex::new(r"\s+Fast\b").expect("regex").replace_all(&name, "").into_owned();
    if thinking_mode != Some(true) {
        name = regex::Regex::new(r"\s+Thinking\b").expect("regex").replace_all(&name, "").into_owned();
    }
    name = name.replace('\u{1}', "(NO ZDR)");
    name = regex::Regex::new(r"\s+").expect("regex").replace_all(&name, " ").trim().to_owned();
    if name.is_empty() {
        name = base_id.to_owned();
    }
    name
}

fn build_level_map(members: &[GroupMember], capability: Option<&crate::cursor::model_capabilities::CursorModelCapability>) -> ThinkingLevelMap {
    let observed: BTreeSet<String> = members.iter().filter_map(|m| m.level.clone()).collect();
    let mut map: ThinkingLevelMap = BTreeMap::new();
    for level in ALL_LEVELS {
        if level == ModelThinkingLevel::Off {
            let off_spec = capability.and_then(|c| c.levels.get(&ModelThinkingLevel::Off));
            let value = if off_spec.is_some() && observed.contains("none") {
                off_spec.map(|s| s.value.clone())
            } else {
                None
            };
            map.insert(level, value);
            continue;
        }
        let spec = capability.and_then(|c| c.levels.get(&level));
        let value = match spec {
            Some(spec) if observed.contains(level.as_str()) || observed.contains(&spec.value) => Some(spec.value.clone()),
            _ => None,
        };
        map.insert(level, value);
    }
    map
}

fn pick_representative(members: &[GroupMember]) -> String {
    let with_levels: Vec<&GroupMember> =
        members.iter().filter(|m| m.level.is_some() && m.level.as_deref() != Some("none")).collect();
    let pool: Vec<&GroupMember> = if with_levels.is_empty() { members.iter().collect() } else { with_levels };
    let order = ["medium", "low", "minimal", "high", "xhigh", "extra-high", "max", "none"];
    let mut sorted = pool;
    sorted.sort_by(|a, b| {
        let ai = order.iter().position(|&o| o == a.level.as_deref().unwrap_or("none")).unwrap_or(order.len());
        let bi = order.iter().position(|&o| o == b.level.as_deref().unwrap_or("none")).unwrap_or(order.len());
        if ai != bi { ai.cmp(&bi) } else { a.alias.legacy_variant_id.cmp(&b.alias.legacy_variant_id) }
    });
    sorted[0].alias.legacy_variant_id.clone()
}

fn normalize_derived_level(token: &str) -> Option<ModelThinkingLevel> {
    match token {
        "none" => Some(ModelThinkingLevel::Off),
        "extra-high" => Some(ModelThinkingLevel::Xhigh),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

/// Every level is explicit: an unobserved level maps to `None` (unsupported), because the
/// shared model contract treats an absent ordinary level as supported and would otherwise
/// offer levels the server never listed.
fn build_derived_level_map(members: &[GroupMember]) -> ThinkingLevelMap {
    let mut map: ThinkingLevelMap = ALL_LEVELS.into_iter().map(|level| (level, None)).collect();
    for member in members {
        if let (Some(level), Some(observed_level)) = (member.alias.level, member.level.clone())
            && map.get(&level) == Some(&None)
        {
            map.insert(level, Some(observed_level));
        }
    }
    map
}

fn build_derived_variant_ids(members: &[GroupMember]) -> BTreeMap<ModelThinkingLevel, String> {
    let mut map = BTreeMap::new();
    for member in members {
        if let Some(level) = member.alias.level {
            map.entry(level).or_insert_with(|| member.raw.id.clone());
        }
    }
    map
}

/// Derive variant-group aliases for raw ids the static alias table does not list.
/// A family (shared base id, with the `-thinking` infix as a separate identity for
/// Claude-style ids) is derived only when at least two distinct levels are observed
/// among its unlisted non-fast members in the same batch; `-fast` variants,
/// single-level families, and level-less ids stay flat exactly as today. Ids the
/// static table already covers are never re-derived, so static output stays
/// byte-identical (senpi#2038).
pub fn derive_cursor_variant_aliases(ids: &[String]) -> BTreeMap<String, CursorVariantAlias> {
    struct Family {
        target_id: String,
        levels: BTreeSet<ModelThinkingLevel>,
        members: Vec<(String, ModelThinkingLevel, String)>,
    }
    let static_targets = static_targets();
    let mut families: BTreeMap<String, Family> = BTreeMap::new();
    let raw_ids: BTreeSet<&String> = ids.iter().collect();
    for id in ids {
        if get_cursor_variant_alias(id).is_some() {
            continue;
        }
        let parsed: CursorVariantParse = parse_cursor_variant_id(id);
        if parsed.fast || parsed.level.is_none() || parsed.base_id.is_empty() {
            continue;
        }
        let Some(level) = normalize_derived_level(parsed.level.as_deref().expect("checked")) else { continue };
        let target_id =
            if parsed.thinking == Some(true) { format!("{}-thinking", parsed.base_id) } else { parsed.base_id.clone() };
        let family = families.entry(target_id.clone()).or_insert_with(|| Family {
            target_id: target_id.clone(),
            levels: BTreeSet::new(),
            members: Vec::new(),
        });
        family.levels.insert(level);
        family.members.push((id.clone(), level, parsed.level.clone().expect("checked")));
    }
    let mut derived: BTreeMap<String, CursorVariantAlias> = BTreeMap::new();
    for family in families.values() {
        if family.levels.len() < 2
            || static_targets.contains(&family.target_id)
            || get_cursor_variant_alias(&family.target_id).is_some()
            || raw_ids.contains(&family.target_id)
        {
            continue;
        }
        let mut chosen: BTreeMap<ModelThinkingLevel, String> = BTreeMap::new();
        for (id, level, suffix) in &family.members {
            match chosen.get(level) {
                None => {
                    chosen.insert(*level, id.clone());
                }
                Some(previous) => {
                    let previous_suffix = parse_cursor_variant_id(previous).level;
                    let level_token = level_to_token(*level);
                    let suffix_matches = *suffix == level_token;
                    let previous_suffix_matches = previous_suffix.as_deref() == Some(level_token.as_str());
                    if (suffix_matches && !previous_suffix_matches)
                        || (suffix_matches == previous_suffix_matches && *id < *previous)
                    {
                        chosen.insert(*level, id.clone());
                    }
                }
            }
        }
        for (level, id) in chosen {
            derived.insert(
                id.clone(),
                CursorVariantAlias {
                    target_id: family.target_id.clone(),
                    legacy_variant_id: id,
                    encoding: "legacy-variant",
                    level: Some(level),
                },
            );
        }
    }
    derived
}

fn level_to_token(level: ModelThinkingLevel) -> String {
    match level {
        ModelThinkingLevel::Off => "off".to_owned(),
        other => other.as_str().to_owned(),
    }
}

/// Normalize a raw Cursor catalog (live discovery, CLI scrape, or stored cache) into selectable identities.
pub fn normalize_cursor_catalog(raw_entries: &[CursorCatalogRawEntry]) -> Vec<CursorCatalogEntry> {
    let ids: Vec<String> = raw_entries.iter().map(|r| r.id.clone()).collect();
    let derived = derive_cursor_variant_aliases(&ids);
    let mut groups: BTreeMap<String, Vec<GroupMember>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();

    for raw in raw_entries {
        let alias = get_cursor_variant_alias(&raw.id).or_else(|| derived.get(&raw.id).cloned());
        match alias {
            None => {
                let parsed = parse_cursor_variant_id(&raw.id);
                let key = format!("unknown{}{}", parsed.base_id, raw.id);
                if !groups.contains_key(&key) {
                    order.push(key.clone());
                }
                let member = GroupMember {
                    raw: raw.clone(),
                    alias: CursorVariantAlias {
                        target_id: raw.id.clone(),
                        legacy_variant_id: raw.id.clone(),
                        encoding: "legacy-variant",
                        level: None,
                    },
                    level: parsed.level,
                    thinking: parsed.thinking,
                    fast: parsed.fast,
                };
                groups.insert(key, vec![member]);
            }
            Some(alias) => {
                let parsed = parse_cursor_variant_id(&raw.id);
                let key = format!("{}{}", alias.target_id, parsed.fast);
                if !groups.contains_key(&key) {
                    order.push(key.clone());
                    groups.insert(key.clone(), Vec::new());
                }
                groups.get_mut(&key).expect("just inserted").push(GroupMember {
                    raw: raw.clone(),
                    alias,
                    level: parsed.level,
                    thinking: parsed.thinking,
                    fast: parsed.fast,
                });
            }
        }
    }

    let mut out = Vec::new();
    for key in order {
        let members = groups.remove(&key).expect("key exists");
        let first = &members[0];
        let base_parsed = parse_cursor_variant_id(&first.alias.target_id);
        let derived_group = members.iter().all(|m| derived.contains_key(&m.raw.id));
        let base_id = if derived_group { parse_cursor_variant_id(&first.raw.id).base_id } else { base_parsed.base_id };
        let capability = CURSOR_MODEL_CAPABILITIES.get(base_id.as_str());
        let is_grouped = members.len() > 1 || first.alias.target_id != first.raw.id;
        let efforts: Vec<&GroupMember> =
            members.iter().filter(|m| m.level.is_some() && m.level.as_deref() != Some("none")).collect();

        if is_grouped && !efforts.is_empty() && !first.fast {
            let thinking_mode = if is_claude(&base_id) { Some(first.thinking == Some(true)) } else { None };
            let mut input: Vec<String> = Vec::new();
            for member in &members {
                for modality in &member.raw.input {
                    if !input.contains(modality) {
                        input.push(modality.clone());
                    }
                }
            }
            let mut legacy_aliases: Vec<String> = members.iter().map(|m| m.alias.legacy_variant_id.clone()).collect();
            legacy_aliases.sort();
            out.push(CursorCatalogEntry {
                id: first.alias.target_id.clone(),
                name: clean_name(&members, &base_id, thinking_mode),
                reasoning: true,
                thinking_level_map: Some(if derived_group {
                    build_derived_level_map(&members)
                } else {
                    build_level_map(&members, capability)
                }),
                window: capability.map(|c| c.window).unwrap_or(FALLBACK_WINDOW),
                max_window: capability.and_then(|c| c.max_window),
                input,
                cursor_max_mode: members.iter().any(|m| m.raw.cursor_max_mode),
                capability_id: Some(base_id.clone()),
                thinking_mode,
                representative_variant_id: Some(pick_representative(&members)),
                legacy_aliases,
                variant_ids: if derived_group { Some(build_derived_variant_ids(&members)) } else { None },
            });
            continue;
        }

        for member in &members {
            let member_base = parse_cursor_variant_id(&member.raw.id).base_id;
            let member_capability = CURSOR_MODEL_CAPABILITIES.get(member_base.as_str());
            out.push(CursorCatalogEntry {
                id: member.raw.id.clone(),
                name: member.raw.name.clone(),
                reasoning: false,
                thinking_level_map: None,
                window: member_capability.map(|c| c.window).unwrap_or(FALLBACK_WINDOW),
                max_window: member_capability.and_then(|c| c.max_window),
                input: member.raw.input.clone(),
                cursor_max_mode: member.raw.cursor_max_mode,
                capability_id: None,
                thinking_mode: None,
                representative_variant_id: None,
                legacy_aliases: vec![member.alias.legacy_variant_id.clone()],
                variant_ids: None,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct FixtureEntry {
        id: String,
        name: String,
        input: Vec<String>,
        #[serde(rename = "cursorMaxMode")]
        cursor_max_mode: bool,
    }

    fn fixture_204() -> Vec<CursorCatalogRawEntry> {
        let entries: Vec<FixtureEntry> = serde_json::from_str(include_str!(
            "../../tests/fixtures/cursor-usable-models-20260818.json"
        ))
        .expect("fixture");
        entries
            .into_iter()
            .map(|e| CursorCatalogRawEntry { id: e.id, name: e.name, input: e.input, cursor_max_mode: e.cursor_max_mode })
            .collect()
    }

    #[test]
    fn collapses_204_variants_to_113_outputs_with_32_reasoning_identities() {
        let out = normalize_cursor_catalog(&fixture_204());
        assert_eq!(out.len(), 113);
        assert_eq!(out.iter().filter(|e| e.reasoning).count(), 32);
    }

    #[test]
    fn splits_the_claude_boolean_axis_into_separate_selectable_identities() {
        let out = normalize_cursor_catalog(&fixture_204());
        let plain = out.iter().find(|e| e.id == "claude-fable-5").expect("plain");
        let thinking = out.iter().find(|e| e.id == "claude-fable-5-thinking").expect("thinking");
        assert!(plain.reasoning);
        assert!(thinking.reasoning);
        assert_eq!(plain.thinking_mode, Some(false));
        assert_eq!(thinking.thinking_mode, Some(true));
        for identity in [plain, thinking] {
            let map = identity.thinking_level_map.as_ref().expect("map");
            assert_eq!(map.get(&ModelThinkingLevel::Low).cloned().flatten(), Some("low".to_owned()));
            assert_eq!(map.get(&ModelThinkingLevel::Max).cloned().flatten(), Some("max".to_owned()));
        }
    }

    #[test]
    fn assigns_total_seven_key_maps_with_exact_wire_values_to_every_reasoning_identity() {
        let out = normalize_cursor_catalog(&fixture_204());
        for entry in out.iter().filter(|e| e.reasoning) {
            let map = entry.thinking_level_map.as_ref().expect("map");
            for level in ALL_LEVELS {
                assert!(map.contains_key(&level), "{} missing key {:?}", entry.id, level);
            }
        }
    }

    #[test]
    fn pins_per_family_maps_and_translations() {
        let out = normalize_cursor_catalog(&fixture_204());
        let by_id: BTreeMap<&str, &CursorCatalogEntry> = out.iter().map(|e| (e.id.as_str(), e)).collect();
        let get = |id: &str, level: ModelThinkingLevel| -> Option<String> {
            by_id.get(id).and_then(|e| e.thinking_level_map.as_ref()).and_then(|m| m.get(&level).cloned().flatten())
        };
        assert_eq!(get("gpt-5.5", ModelThinkingLevel::Xhigh), Some("extra-high".to_owned()));
        assert_eq!(get("gpt-5.3-codex", ModelThinkingLevel::Xhigh), Some("extra-high".to_owned()));
        assert_eq!(get("gpt-5.6-sol", ModelThinkingLevel::Xhigh), Some("xhigh".to_owned()));
        assert_eq!(get("cursor-grok-4.5", ModelThinkingLevel::Xhigh), None);
        assert_eq!(get("cursor-grok-4.6", ModelThinkingLevel::Xhigh), Some("xhigh".to_owned()));
        assert_eq!(get("glm-5.2", ModelThinkingLevel::High), Some("high".to_owned()));
        assert_eq!(get("glm-5.2", ModelThinkingLevel::Max), Some("max".to_owned()));
        assert_eq!(get("glm-5.2", ModelThinkingLevel::Low), None);
        assert_eq!(get("kimi-k3", ModelThinkingLevel::Low), Some("low".to_owned()));
        assert_eq!(get("kimi-k3", ModelThinkingLevel::High), Some("high".to_owned()));
        assert_eq!(get("kimi-k3", ModelThinkingLevel::Max), Some("max".to_owned()));
        assert_eq!(get("kimi-k3", ModelThinkingLevel::Medium), None);
        assert_eq!(get("gemini-3.6-flash", ModelThinkingLevel::Minimal), Some("minimal".to_owned()));
        assert_eq!(get("gemini-3.7-flash", ModelThinkingLevel::Minimal), None);
    }

    #[test]
    fn exposes_off_equals_none_only_where_the_descriptor_declares_it() {
        let out = normalize_cursor_catalog(&fixture_204());
        let by_id: BTreeMap<&str, &CursorCatalogEntry> = out.iter().map(|e| (e.id.as_str(), e)).collect();
        for id in ["gpt-5.5", "gpt-5.6-sol", "gpt-5.6-luna", "gpt-5.6-terra", "gpt-5.4-mini", "gpt-5.4-nano"] {
            let value = by_id.get(id).and_then(|e| e.thinking_level_map.as_ref()).and_then(|m| m.get(&ModelThinkingLevel::Off).cloned().flatten());
            assert_eq!(value, Some("none".to_owned()), "{id}");
        }
        for id in ["claude-fable-5", "kimi-k3", "glm-5.2", "cursor-grok-4.6", "gemini-3.7-flash", "gpt-5.3-codex"] {
            let value = by_id.get(id).and_then(|e| e.thinking_level_map.as_ref()).and_then(|m| m.get(&ModelThinkingLevel::Off).cloned().flatten());
            assert_eq!(value, None, "{id}");
        }
    }

    #[test]
    fn keeps_fast_variants_raw_and_level_less_twins_untouched() {
        let out = normalize_cursor_catalog(&fixture_204());
        let ids: BTreeSet<&str> = out.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains("claude-opus-4-7-thinking-high-fast"));
        assert!(ids.contains("composer-2.5-fast"));
        assert!(ids.contains("claude-4-sonnet-thinking"));
        assert!(ids.contains("claude-4.5-sonnet-thinking"));
        for raw_id in ["claude-opus-4-7-thinking-high-fast", "composer-2.5-fast", "claude-4-sonnet-thinking"] {
            let entry = out.iter().find(|e| e.id == raw_id).expect(raw_id);
            assert!(!entry.reasoning, "{raw_id}");
        }
    }

    #[test]
    fn assigns_static_windows_and_keeps_display_qualifiers() {
        let out = normalize_cursor_catalog(&fixture_204());
        let by_id: BTreeMap<&str, &CursorCatalogEntry> = out.iter().map(|e| (e.id.as_str(), e)).collect();
        assert_eq!(by_id.get("kimi-k3").expect("kimi").window, 1_048_576.0);
        assert_eq!(by_id.get("claude-fable-5").expect("fable").window, 1_000_000.0);
        assert_eq!(by_id.get("claude-fable-5").expect("fable").max_window, Some(1_000_000.0));
        assert!(by_id.get("claude-fable-5").expect("fable").name.contains("1M"));
        assert!(by_id.get("claude-fable-5").expect("fable").name.contains("NO ZDR"));
    }

    #[test]
    fn records_legacy_aliases_for_grouped_identities() {
        let out = normalize_cursor_catalog(&fixture_204());
        let fable = out.iter().find(|e| e.id == "claude-fable-5-thinking").expect("fable");
        assert!(fable.legacy_aliases.contains(&"claude-fable-5-thinking-xhigh".to_owned()));
        assert!(fable.legacy_aliases.contains(&"claude-fable-5-thinking-low".to_owned()));
        assert!(!fable.legacy_aliases.contains(&"claude-fable-5-low".to_owned()));
    }

    #[test]
    fn parses_every_fixture_id_and_round_trips_via_original_id() {
        for entry in fixture_204() {
            let parsed = parse_cursor_variant_id(&entry.id);
            assert_eq!(parsed.original_id, entry.id);
            assert!(!parsed.base_id.is_empty());
        }
    }

    #[test]
    fn handles_both_thinking_level_orders() {
        let p = parse_cursor_variant_id("claude-fable-5-thinking-xhigh");
        assert_eq!(p.base_id, "claude-fable-5");
        assert_eq!(p.level, Some("xhigh".to_owned()));
        assert_eq!(p.thinking, Some(true));
        assert!(!p.fast);

        let p = parse_cursor_variant_id("claude-4.5-opus-high-thinking");
        assert_eq!(p.base_id, "claude-4.5-opus");
        assert_eq!(p.level, Some("high".to_owned()));
        assert_eq!(p.thinking, Some(true));
        assert!(!p.fast);

        let p = parse_cursor_variant_id("gpt-5.5-extra-high");
        assert_eq!(p.base_id, "gpt-5.5");
        assert_eq!(p.level, Some("extra-high".to_owned()));
        assert_eq!(p.thinking, Some(false));

        let p = parse_cursor_variant_id("gpt-5.6-luna-none");
        assert_eq!(p.base_id, "gpt-5.6-luna");
        assert_eq!(p.level, Some("none".to_owned()));
        assert_eq!(p.thinking, Some(false));

        let p = parse_cursor_variant_id("composer-2.5-fast");
        assert_eq!(p.base_id, "composer-2.5");
        assert_eq!(p.level, None);
        assert!(p.fast);
    }

    #[test]
    fn never_authorizes_migration_for_syntactically_plausible_but_unknown_ids() {
        for id in ["custom-high", "custom-none", "some-model-thinking-low", "gpt-5.5[effort=high]"] {
            assert!(get_cursor_variant_alias(id).is_none(), "{id}");
        }
    }
}
