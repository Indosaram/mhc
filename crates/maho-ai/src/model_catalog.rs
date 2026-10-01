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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_generated::{MODELS, get_builtin_provider_models};

    // senpi test/model-catalog-types.test.ts has 4 `it()` cases. The `expectTypeOf` half of
    // "derives model API, ID, and provider literals from grouped model data" is TS-only
    // literal-type narrowing with no Rust runtime analogue (Rust's static typing enforces the
    // same narrowing structurally at compile time via the `Model` struct's typed fields). The
    // runtime `expect()` half of every case is ported here against `flatten_model_catalog`'s own
    // output (via the generated per-provider shards it backs), matching the TS assertions on
    // `XAI_MODELS`/`GITHUB_COPILOT_MODELS`/`XIAOMI_MODELS`.

    fn model_in<'a>(provider: &str, id: &'a str) -> &'a crate::types::Model {
        MODELS[provider].get(id).unwrap_or_else(|| panic!("{provider}/{id} missing from flattened catalog"))
    }

    #[test]
    fn derives_model_api_id_and_provider_literals_from_grouped_model_data() {
        let title = "derives model API, ID, and provider literals from grouped model data";
        for (provider, id, api) in [
            ("xai", "grok-4.5", "openai-responses"),
            ("xai", "grok-4.6", "openai-responses"),
            ("xai", "grok-4.7", "openai-responses"),
            ("xai", "grok-4.3", "openai-responses"),
            ("github-copilot", "grok-4.7", "openai-responses"),
            ("xiaomi", "mimo-v2.5-pro", "openai-completions"),
            ("xiaomi", "mimo-v2.6-pro", "openai-completions"),
        ] {
            let model = model_in(provider, id);
            assert_eq!((model.id.as_str(), model.provider.as_str(), model.api.as_str()), (id, provider, api), "{title}");
        }
    }

    #[test]
    fn routes_github_copilot_grok_4_5_through_the_responses_api() {
        let title = "routes GitHub Copilot Grok 4.5 through the Responses API";
        assert_eq!(model_in("github-copilot", "grok-4.5").api, "openai-responses", "{title}");
    }

    // Regression test for https://github.com/earendil-works/pi/issues/9209
    #[test]
    fn routes_all_github_copilot_gpt_models_through_the_responses_api() {
        let title = "routes all GitHub Copilot GPT models through the Responses API";
        let gpt_models: Vec<&crate::types::Model> =
            MODELS["github-copilot"].values().filter(|m| m.id.starts_with("gpt-")).collect();
        assert!(!gpt_models.is_empty(), "{title}");
        assert!(gpt_models.iter().all(|m| m.api == "openai-responses"), "{title}");
        assert_eq!(model_in("github-copilot", "gpt-6-astra").api, "openai-responses", "{title}");
    }

    // Grok 4.7 and MiMo V2.6 Pro must be reachable on their DIRECT provider shards
    // (xai.json / xiaomi.json), not only through aggregator catalogs — and through
    // the builtin registry consumers actually read. The grok-4.6 / mimo-v2.5-pro
    // checks are controls proving the addition did not replace an existing entry.
    #[test]
    fn ships_grok_4_7_and_mimo_v2_6_pro_on_their_direct_provider_shards_and_the_builtin_registry() {
        let title = "ships grok-4.7 and mimo-v2.6-pro on their direct provider shards and the builtin registry";
        assert_eq!(model_in("xai", "grok-4.6").id, "grok-4.6", "{title}");
        assert_eq!(model_in("xai", "grok-4.7").id, "grok-4.7", "{title}");
        assert_eq!(model_in("xiaomi", "mimo-v2.5-pro").id, "mimo-v2.5-pro", "{title}");
        assert_eq!(model_in("xiaomi", "mimo-v2.6-pro").id, "mimo-v2.6-pro", "{title}");

        let xai_ids: Vec<&str> = get_builtin_provider_models("xai").expect("xai").iter().map(|m| m.id.as_str()).collect();
        assert!(xai_ids.contains(&"grok-4.6"), "{title}");
        assert!(xai_ids.contains(&"grok-4.7"), "{title}");
        let xiaomi_ids: Vec<&str> =
            get_builtin_provider_models("xiaomi").expect("xiaomi").iter().map(|m| m.id.as_str()).collect();
        assert!(xiaomi_ids.contains(&"mimo-v2.5-pro"), "{title}");
        assert!(xiaomi_ids.contains(&"mimo-v2.6-pro"), "{title}");
    }

    // `flattenModelCatalog`'s own `Object.assign` merge semantics (later groups win, ids from
    // different groups in the same catalog coexist) exercised directly against `ModelGroups`,
    // independent of the generated data — this is the unit the TS suite's runtime assertions
    // exercise only indirectly through the generated shards above.
    #[test]
    fn flattens_grouped_models_with_later_groups_overwriting_earlier_ids() {
        fn model(id: &str, api: &str) -> Model {
            let mut m = model_in("xai", "grok-4.5").clone();
            m.id = id.into();
            m.api = api.into();
            m
        }
        let mut group_a = IndexMap::new();
        group_a.insert("shared".to_owned(), model("shared", "api-a"));
        group_a.insert("only-a".to_owned(), model("only-a", "api-a"));
        let mut group_b = IndexMap::new();
        group_b.insert("shared".to_owned(), model("shared", "api-b"));
        group_b.insert("only-b".to_owned(), model("only-b", "api-b"));
        let mut groups: ModelGroups = IndexMap::new();
        groups.insert("a".to_owned(), group_a);
        groups.insert("b".to_owned(), group_b);

        let flat = flatten_model_catalog("xai", &groups);
        assert_eq!(flat.len(), 3);
        assert_eq!(flat["shared"].api, "api-b");
        assert_eq!(flat["only-a"].api, "api-a");
        assert_eq!(flat["only-b"].api, "api-b");
    }
}
