use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::FallbackEntry;
use model_core::ModelRequirement;
use pretty_assertions::assert_eq;

const EXPECTED_AGENTS: [&str; 11] = [
    "sisyphus",
    "hephaestus",
    "oracle",
    "librarian",
    "explore",
    "multimodal-looker",
    "prometheus",
    "metis",
    "momus",
    "atlas",
    "sisyphus-junior",
];

const EXPECTED_CATEGORIES: [&str; 8] = [
    "visual-engineering",
    "ultrabrain",
    "deep",
    "artistry",
    "quick",
    "unspecified-low",
    "unspecified-high",
    "writing",
];

fn assert_valid_chain(name: &str, requirement: &ModelRequirement) {
    assert!(!requirement.fallback_chain.is_empty(), "{name}");
    for entry in &requirement.fallback_chain {
        assert!(!entry.providers.is_empty(), "{name}");
        assert!(!entry.model.is_empty(), "{name}");
    }
}

fn all_entries() -> Vec<&'static FallbackEntry> {
    AGENT_MODEL_REQUIREMENTS
        .values()
        .chain(CATEGORY_MODEL_REQUIREMENTS.values())
        .flat_map(|requirement| &requirement.fallback_chain)
        .collect()
}

#[test]
fn all_builtin_agents_have_non_empty_fallback_chains_with_valid_entries() {
    assert_eq!(AGENT_MODEL_REQUIREMENTS.len(), EXPECTED_AGENTS.len());
    for agent in EXPECTED_AGENTS {
        assert_valid_chain(agent, &AGENT_MODEL_REQUIREMENTS[agent]);
    }
}

#[test]
fn all_categories_have_non_empty_fallback_chains_with_valid_entries() {
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS.len(), EXPECTED_CATEGORIES.len());
    for category in EXPECTED_CATEGORIES {
        assert_valid_chain(category, &CATEGORY_MODEL_REQUIREMENTS[category]);
    }
}

#[test]
fn fallback_chain_model_ids_do_not_include_provider_prefixes() {
    for entry in all_entries() {
        assert!(!entry.model.contains('/'), "{}", entry.model);
    }
}

#[test]
fn builtin_kimi_fallback_entries_use_kimi_k3_instead_of_retired_k2_ids() {
    let entries = all_entries();

    let retired: Vec<_> = entries
        .iter()
        .filter(|entry| ["k2p5", "kimi-k2.5", "kimi-k2.6"].contains(&entry.model.as_str()))
        .collect();
    let kimi = entries
        .iter()
        .filter(|entry| entry.model == "kimi-k3")
        .count();

    assert_eq!(retired, Vec::<&&FallbackEntry>::new());
    assert!(kimi > 0);
}

#[test]
fn builtin_fallback_chains_contain_no_gpt_5_5_entries() {
    let retired: Vec<_> = all_entries()
        .into_iter()
        .filter(|entry| entry.model == "gpt-5.5")
        .collect();

    assert_eq!(retired, Vec::<&FallbackEntry>::new());
}
