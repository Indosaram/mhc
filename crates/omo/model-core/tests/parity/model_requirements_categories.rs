use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::FallbackEntry;
use pretty_assertions::assert_eq;

use crate::support::entry;

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
fn deep_low_leads_with_gpt_6_1_sol_then_gpt_6_1_sol_fast_medium_on_the_openai_lanes() {
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
fn the_deep_lanes_and_artistry_no_longer_hard_require_primary_models() {
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["deep-low"].requires_model, None);
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["deep-high"].requires_model, None);
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["artistry"].requires_model, None);
}
