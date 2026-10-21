//! Translations of the upstream `model-requirements-*.test.ts` test suites:
//! - `model-requirements-agents.test.ts` (11 tests)
//! - `model-requirements-categories.test.ts` (14 tests)
//! - `model-requirements-invariants.test.ts` (5 tests)
//! - `model-requirements-deprecated-routing.test.ts` (1 test)

use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::FallbackEntry;
use model_core::ModelRequirement;
use pretty_assertions::assert_eq;

use crate::support::entry;
use crate::support::strings;

fn chain(agent: &str) -> &'static [FallbackEntry] {
    &AGENT_MODEL_REQUIREMENTS[agent].fallback_chain
}

// -----------------------------------------------------------------------------
// model-requirements-agents.test.ts (11 tests)
// -----------------------------------------------------------------------------

#[test]
fn oracle_has_gpt_5_6_sol_xhigh_as_primary() {
    let oracle = chain("oracle");

    assert!(!oracle.is_empty());
    assert_eq!(
        oracle[0],
        entry(
            &["openai", "chatgpt-subscription", "opencode"],
            "gpt-5.6-sol",
            Some("xhigh")
        )
    );
    assert_eq!(
        oracle[1],
        entry(&["github-copilot"], "gpt-5.6-sol", Some("high"))
    );
}

#[test]
fn sisyphus_keeps_opus_primary_before_kimi_k3_gpt_5_6_sol_glm_5_2_and_big_pickle_fallbacks() {
    let sisyphus = &AGENT_MODEL_REQUIREMENTS["sisyphus"];
    let chain = &sisyphus.fallback_chain;

    assert_eq!(chain.len(), 5);
    assert_eq!(sisyphus.requires_any_model, Some(true));
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-5-5",
            Some("max")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &[
                "opencode-go",
                "kimi-for-coding",
                "moonshotai",
                "opencode",
                "bailian-coding-plan",
                "moonshotai-cn",
                "firmware",
                "ollama-cloud",
                "aihubmix",
            ],
            "kimi-k3",
            None
        )
    );
    assert_eq!(
        chain[2],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
    assert_eq!(chain[3].providers[0], "zai-coding-plan");
    assert_eq!(chain[3].model, "glm-5.2");
    assert_eq!(chain[4].providers[0], "opencode");
    assert_eq!(chain[4].model, "big-pickle");
}

#[test]
fn librarian_runs_no_thinking_kimi_highspeed_luna_fast_deepseek_flash_qwen_m2_7_then_haiku() {
    let requirement = &AGENT_MODEL_REQUIREMENTS["librarian"];

    assert_eq!(
        requirement.fallback_chain,
        vec![
            entry(
                &["kimi-for-coding"],
                "kimi-for-coding-highspeed",
                Some("off")
            ),
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-6-luna-fast",
                Some("low")
            ),
            entry(&["anthropic", "github-copilot"], "claude-haiku-5-5", Some("medium")),
            entry(&["deepseek"], "deepseek-flash", Some("max")),
            entry(
                &["opencode-go", "bailian-coding-plan"],
                "qwen3.7-plus",
                None
            ),
            entry(&["opencode-go"], "minimax-m2.7", None),
        ]
    );
}

#[test]
fn explore_runs_no_thinking_kimi_highspeed_luna_fast_deepseek_flash_qwen_m2_7_then_haiku() {
    let requirement = &AGENT_MODEL_REQUIREMENTS["explore"];

    assert_eq!(
        requirement.fallback_chain,
        vec![
            entry(
                &["kimi-for-coding"],
                "kimi-for-coding-highspeed",
                Some("off")
            ),
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-6-luna-fast",
                Some("low")
            ),
            entry(&["anthropic", "github-copilot"], "claude-haiku-5-5", Some("medium")),
            entry(&["deepseek"], "deepseek-flash", Some("max")),
            entry(
                &["opencode-go", "bailian-coding-plan"],
                "qwen3.7-plus",
                None
            ),
            entry(&["opencode-go"], "minimax-m2.7", None),
        ]
    );
}

#[test]
fn multimodal_looker_keeps_vision_capable_fallback_order() {
    let chain = chain("multimodal-looker");

    assert_eq!(chain.len(), 4);
    assert_eq!(
        chain[0],
        entry(
            &["openai", "chatgpt-subscription", "opencode"],
            "gpt-5.6-sol",
            Some("low")
        )
    );
    assert_eq!(chain[1], entry(&["opencode-go"], "kimi-k3", None));
    assert_eq!(chain[2].model, "glm-4.6v");
    assert_eq!(
        chain[3],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-5-nano",
            None
        )
    );
}

#[test]
fn prometheus_uses_fable_5_1_xhigh_then_opus_5_5_max_before_kimi_k3_max() {
    let chain = chain("prometheus");

    assert_eq!(chain.len(), 3);
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-fable-5-1",
            Some("xhigh")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-5-5",
            Some("max")
        )
    );
    assert_eq!(
        chain[2],
        entry(
            &[
                "opencode-go",
                "kimi-for-coding",
                "moonshotai",
                "opencode"
            ],
            "kimi-k3",
            Some("max")
        )
    );
}

#[test]
fn metis_uses_fable_5_1_max_then_opus_5_max_then_kimi_k3_max() {
    let chain = chain("metis");

    assert_eq!(chain.len(), 3);
    assert_eq!(
        chain[0],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-fable-5-1",
            Some("max")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-5-5",
            Some("max")
        )
    );
    assert_eq!(
        chain[2],
        entry(
            &[
                "opencode-go",
                "kimi-for-coding",
                "moonshotai",
                "opencode"
            ],
            "kimi-k3",
            Some("max")
        )
    );
}

#[test]
fn momus_leads_with_native_gpt_6_astra_xhigh_before_copilot_and_opencode_astra_rungs() {
    let chain = chain("momus");

    assert!(chain.len() > 1);
    assert_eq!(
        chain[0],
        entry(
            &["openai", "chatgpt-subscription"],
            "gpt-6-astra",
            Some("xhigh")
        )
    );
    assert_eq!(
        chain[1],
        entry(&["github-copilot"], "gpt-6-astra", Some("high"))
    );
    assert_eq!(
        chain[2],
        entry(
            &["openai", "chatgpt-subscription", "opencode"],
            "gpt-6-astra",
            Some("high")
        )
    );
    assert_eq!(
        chain[3],
        entry(
            &["anthropic", "github-copilot", "opencode"],
            "claude-opus-5-5",
            Some("max")
        )
    );
    assert_eq!(
        chain[5],
        entry(
            &["google", "github-copilot", "opencode"],
            "gemini-3.1-pro",
            Some("high")
        )
    );
    assert_eq!(chain[4], entry(&["google", "google-vertex", "opencode"], "gemini-4-argon", Some("high")));
    assert_eq!(chain[6], entry(&["opencode-go"], "glm-5.2", None));
    assert!(!chain.iter().any(|entry| entry.model.starts_with("gpt-5.6-")));
}

#[test]
fn atlas_keeps_sonnet_kimi_gpt_5_6_sol_and_minimax_fallback_order() {
    let chain = chain("atlas");

    assert_eq!(chain.len(), 6);
    assert_eq!(chain[0].model, "claude-sonnet-5");
    assert_eq!(chain[0].providers[0], "anthropic");
    assert_eq!(chain[1].model, "kimi-k3");
    assert_eq!(chain[1].providers[0], "opencode-go");
    assert_eq!(
        chain[2],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
    assert_eq!(chain[3].model, "minimax-m3");
    assert_eq!(chain[3].providers[0], "opencode-go");
    assert_eq!(
        chain[4],
        entry(
            &["minimax-coding-plan", "minimax-cn-coding-plan"],
            "MiniMax-M3",
            None
        )
    );
    assert_eq!(chain[5].model, "minimax-m2.7");
    assert_eq!(chain[5].providers[0], "opencode-go");
}

#[test]
fn sisyphus_junior_keeps_sonnet_kimi_minimax_and_big_pickle_fallbacks() {
    let model_ids: Vec<&str> = chain("sisyphus-junior")
        .iter()
        .map(|entry| entry.model.as_str())
        .collect();

    assert_eq!(
        model_ids,
        vec![
            "claude-sonnet-5",
            "kimi-k3",
            "gpt-5.6-sol",
            "minimax-m3",
            "MiniMax-M3",
            "minimax-m2.7",
            "big-pickle"
        ]
    );
    assert!(!model_ids.contains(&"gpt-5.5"));
}

#[test]
fn hephaestus_supports_openai_chatgpt_subscription_github_copilot_and_opencode_providers() {
    let hephaestus = &AGENT_MODEL_REQUIREMENTS["hephaestus"];

    assert_eq!(
        hephaestus.requires_provider,
        Some(strings(&[
            "openai",
            "chatgpt-subscription",
            "github-copilot",
            "opencode"
        ]))
    );
    assert!(
        !hephaestus.requires_provider
            .as_ref()
            .unwrap()
            .iter()
            .any(|p| p == "venice")
    );
    assert!(
        !hephaestus.fallback_chain[0]
            .providers
            .iter()
            .any(|p| p == "venice")
    );
    assert_eq!(hephaestus.requires_model, None);
    assert_eq!(hephaestus.requires_any_model, Some(true));
}

#[test]
fn hephaestus_leads_with_one_merged_gpt_6_sol_medium_rung_over_its_gpt_5_6_sol_predecessor() {
    let chain = chain("hephaestus");

    assert_eq!(chain.len(), 2);
    assert_eq!(
        chain[0],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-6-sol",
            Some("medium")
        )
    );
    assert_eq!(
        chain[1],
        entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
}

// -----------------------------------------------------------------------------
// model-requirements-categories.test.ts (14 tests)
// -----------------------------------------------------------------------------

#[test]
fn writing_only_activates_when_one_of_its_own_chain_models_is_available() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["writing"].requires_any_model,
        Some(true)
    );
}

#[test]
fn ultrabrain_routes_gpt_6_astra_max_before_the_existing_sol_max_fallbacks() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["ultrabrain"].fallback_chain,
        vec![
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-6-astra",
                Some("max")
            ),
            entry(&["github-copilot"], "gpt-6-astra", Some("max")),
            entry(
                &["openai", "chatgpt-subscription", "opencode"],
                "gpt-6-astra",
                Some("max")
            ),
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-5.6-sol",
                Some("max")
            ),
            entry(&["github-copilot"], "gpt-5.6-sol", Some("max")),
            entry(
                &["openai", "chatgpt-subscription", "opencode"],
                "gpt-5.6-sol",
                Some("max")
            ),
        ]
    );
}

#[test]
fn ultrabrain_keeps_gpt_5_6_sol_max_on_every_fallback_rung() {
    let requirement = &CATEGORY_MODEL_REQUIREMENTS["ultrabrain"];
    let chain: Vec<&FallbackEntry> = requirement
        .fallback_chain
        .iter()
        .filter(|entry| entry.model == "gpt-5.6-sol")
        .collect();

    assert_eq!(
        chain,
        vec![
            &entry(
                &["openai", "chatgpt-subscription"],
                "gpt-5.6-sol",
                Some("max")
            ),
            &entry(&["github-copilot"], "gpt-5.6-sol", Some("max")),
            &entry(
                &["openai", "chatgpt-subscription", "opencode"],
                "gpt-5.6-sol",
                Some("max")
            ),
        ]
    );
}

#[test]
fn deep_high_routes_gpt_6_astra_high_only() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["deep-high"].fallback_chain,
        vec![entry(
            &[
                "openai",
                "chatgpt-subscription",
                "github-copilot",
                "opencode"
            ],
            "gpt-6-astra",
            Some("high")
        )]
    );
}

#[test]
fn deep_low_leads_with_gpt_6_1_sol_then_gpt_6_1_sol_fast_medium_on_openai_lanes() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["deep-low"].fallback_chain,
        vec![
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-6.1-sol",
                Some("medium")
            ),
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-6.1-sol-fast",
                Some("medium")
            ),
            entry(
                &[
                    "openai",
                    "chatgpt-subscription",
                    "github-copilot",
                    "opencode"
                ],
                "gpt-5.6-sol",
                Some("medium")
            ),
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-5.6-sol-fast",
                Some("medium")
            ),
        ]
    );
}

#[test]
fn neither_deep_lane_carries_the_other_lanes_model() {
    let low_models: Vec<&str> = CATEGORY_MODEL_REQUIREMENTS["deep-low"]
        .fallback_chain
        .iter()
        .map(|entry| entry.model.as_str())
        .collect();
    let high_models: Vec<&str> = CATEGORY_MODEL_REQUIREMENTS["deep-high"]
        .fallback_chain
        .iter()
        .map(|entry| entry.model.as_str())
        .collect();

    assert_eq!(
        low_models,
        vec![
            "gpt-6.1-sol",
            "gpt-6.1-sol-fast",
            "gpt-5.6-sol",
            "gpt-5.6-sol-fast"
        ]
    );
    assert_eq!(high_models, vec!["gpt-6-astra"]);
}

#[test]
fn visual_engineering_starts_with_argon_low() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["visual-engineering"].fallback_chain.first(),
        Some(&entry(&["google", "google-vertex", "opencode"], "gemini-4-argon", Some("low")))
    );
}

#[test]
fn visual_engineering_follows_the_approved_4_rung_chain() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["visual-engineering"].fallback_chain,
        vec![
            entry(&["google", "google-vertex", "opencode"], "gemini-4-argon", Some("low")),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-fable-5-1",
                Some("max")
            ),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-opus-5-5",
                Some("max")
            ),
            entry(
                &[
                    "kimi-for-coding",
                    "moonshotai",
                    "opencode-go",
                    "opencode"
                ],
                "kimi-k3",
                Some("max")
            ),
        ]
    );
}

#[test]
fn quick_follows_the_approved_9_rung_chain() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["quick"].fallback_chain,
        vec![
            entry(
                &["openai", "chatgpt-subscription"],
                "gpt-6-luna-fast",
                Some("low")
            ),
            entry(&["anthropic", "anthropic-api", "github-copilot"], "claude-haiku-5-5", Some("medium")),
            entry(&["deepseek"], "deepseek-flash", Some("off")),
            entry(
                &[
                    "qwen-token-plan",
                    "alibaba-token-plan",
                    "bailian-coding-plan"
                ],
                "qwen3.6-flash",
                Some("low")
            ),
            entry(&["opencode-go"], "minimax-m3", Some("max")),
            entry(&["opencode-go"], "minimax-m2.7", Some("max")),
            entry(&["xai"], "grok-4.20-0309-non-reasoning", None),
            entry(&["zai-coding-plan"], "glm-5.3-flash", Some("low")),
            entry(&["xiaomi"], "mimo-v2.6-flash", Some("low")),
        ]
    );
}

#[test]
fn unspecified_low_follows_the_approved_10_rung_chain_headed_by_claude_sonnet_5_5_medium() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["unspecified-low"].fallback_chain,
        vec![
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-sonnet-5-5",
                Some("medium")
            ),
            entry(
                &["xiaomi", "opencode-go"],
                "mimo-v2.6-pro",
                Some("max")
            ),
            entry(
                &["xai", "github-copilot", "opencode-go"],
                "grok-4.7",
                Some("xhigh")
            ),
            entry(&["zai-coding-plan", "opencode-go"], "glm-5.3", Some("high")),
            entry(&["openai", "chatgpt-subscription"], "gpt-6.1-sol", Some("medium")),
            entry(
                &[
                    "openai",
                    "chatgpt-subscription",
                    "github-copilot",
                    "opencode"
                ],
                "gpt-5.6-sol",
                Some("medium")
            ),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-sonnet-5",
                Some("low")
            ),
            entry(
                &[
                    "qwen-token-plan",
                    "alibaba-token-plan",
                    "qwen-token-plan-cn",
                    "alibaba-token-plan-cn"
                ],
                "qwen3.8-max-preview",
                Some("max")
            ),
            entry(
                &["deepseek", "opencode-go"],
                "deepseek-v4-pro",
                Some("max")
            ),
            entry(
                &["xiaomi", "opencode-go"],
                "mimo-v2.5-pro",
                Some("max")
            ),
        ]
    );
}

#[test]
fn unspecified_high_follows_the_approved_opus_first_3_rung_chain() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["unspecified-high"].fallback_chain,
        vec![
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-opus-5-5",
                Some("medium")
            ),
            entry(
                &["zai-coding-plan", "opencode-go"],
                "glm-5.3",
                Some("max")
            ),
            entry(
                &[
                    "kimi-for-coding",
                    "moonshotai",
                    "opencode-go",
                    "opencode"
                ],
                "kimi-k3",
                Some("max")
            ),
        ]
    );
}

#[test]
fn artistry_follows_the_approved_3_rung_chain() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["artistry"].fallback_chain,
        vec![
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-fable-5-1",
                Some("max")
            ),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-opus-5-5",
                Some("max")
            ),
            entry(
                &[
                    "kimi-for-coding",
                    "moonshotai",
                    "opencode-go",
                    "opencode"
                ],
                "kimi-k3",
                Some("max")
            ),
        ]
    );
}

#[test]
fn writing_leads_with_argon_low_then_opus_without_fable() {
    let chain = &CATEGORY_MODEL_REQUIREMENTS["writing"].fallback_chain;

    assert!(!chain.iter().any(|entry| entry.model == "claude-fable-5-1"));
    assert_eq!(
        chain,
        &vec![
            entry(&["google", "google-vertex", "opencode"], "gemini-4-argon", Some("low")),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-opus-5-5",
                Some("low")
            ),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode"
                ],
                "claude-opus-4-6",
                Some("max")
            ),
        ]
    );
}

#[test]
fn deep_lanes_and_artistry_no_longer_hard_require_primary_models() {
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["deep-low"].requires_model, None);
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["deep-high"].requires_model, None);
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["artistry"].requires_model, None);
}

// -----------------------------------------------------------------------------
// model-requirements-invariants.test.ts (5 tests)
// -----------------------------------------------------------------------------

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

const EXPECTED_CATEGORIES: [&str; 9] = [
    "visual-engineering",
    "ultrabrain",
    "deep-low",
    "deep-high",
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

// -----------------------------------------------------------------------------
// model-requirements-deprecated-routing.test.ts (1 test)
// -----------------------------------------------------------------------------

#[test]
fn no_deprecated_haiku_or_gpt_nano_fallback_entry_routes_through_opencode() {
    let deprecated_models = ["claude-haiku-4-5", "gpt-5.4-nano"];

    let deprecated_opencode_entries: Vec<&FallbackEntry> = AGENT_MODEL_REQUIREMENTS
        .values()
        .chain(CATEGORY_MODEL_REQUIREMENTS.values())
        .flat_map(|requirement| &requirement.fallback_chain)
        .filter(|entry| {
            deprecated_models.contains(&entry.model.as_str())
                && entry.providers.iter().any(|p| p == "opencode")
        })
        .collect();

    assert_eq!(deprecated_opencode_entries, Vec::<&FallbackEntry>::new());
}
