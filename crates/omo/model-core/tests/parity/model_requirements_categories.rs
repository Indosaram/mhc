use model_core::CATEGORY_MODEL_REQUIREMENTS;
use pretty_assertions::assert_eq;

use crate::support::entry;

#[test]
fn ultrabrain_is_gpt_5_6_sol_max_on_every_rung() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["ultrabrain"].fallback_chain,
        vec![
            entry(
                &["openai", "quotio-openai", "vercel"],
                "gpt-5.6-sol",
                Some("max")
            ),
            entry(&["github-copilot"], "gpt-5.6-sol", Some("max")),
            entry(
                &["openai", "opencode", "vercel"],
                "gpt-5.6-sol",
                Some("max")
            ),
        ]
    );
}

#[test]
fn deep_is_a_single_sol_family_medium_rung() {
    let deep = &CATEGORY_MODEL_REQUIREMENTS["deep"];
    assert_eq!(deep.fallback_chain.len(), 1);
    assert_eq!(
        deep.fallback_chain[0],
        entry(
            &[
                "openai",
                "quotio-openai",
                "github-copilot",
                "opencode",
                "vercel"
            ],
            "gpt-5.6-sol",
            Some("medium")
        )
    );
}

#[test]
fn visual_engineering_follows_the_approved_4_rung_chain() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["visual-engineering"].fallback_chain,
        vec![
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode",
                    "vercel"
                ],
                "claude-opus-5",
                Some("max")
            ),
            entry(
                &[
                    "kimi-for-coding",
                    "moonshotai",
                    "opencode-go",
                    "opencode",
                    "vercel"
                ],
                "kimi-k3",
                Some("max")
            ),
            entry(
                &["zai-coding-plan", "opencode-go", "vercel"],
                "glm-5.2",
                Some("max")
            ),
            entry(
                &[
                    "openai",
                    "quotio-openai",
                    "github-copilot",
                    "opencode",
                    "vercel"
                ],
                "gpt-5.6-sol",
                Some("medium")
            ),
        ]
    );
}

#[test]
fn quick_follows_the_approved_8_rung_chain() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["quick"].fallback_chain,
        vec![
            entry(&["kimi-for-coding"], "kimi-for-coding-highspeed", None),
            entry(&["openai-codex"], "gpt-5.6-luna-fast", Some("low")),
            entry(&["deepseek"], "deepseek-v4-flash", Some("off")),
            entry(
                &[
                    "qwen-token-plan",
                    "alibaba-token-plan",
                    "bailian-coding-plan",
                    "vercel"
                ],
                "qwen3.6-flash",
                Some("low")
            ),
            entry(&["opencode-go", "vercel"], "minimax-m3", Some("max")),
            entry(&["opencode-go", "vercel"], "minimax-m2.7", Some("max")),
            entry(&["xai"], "grok-4.20-0309-non-reasoning", None),
            entry(
                &["anthropic", "anthropic-api", "github-copilot", "vercel"],
                "claude-haiku-4-5",
                Some("off")
            ),
        ]
    );
}

#[test]
fn unspecified_low_follows_the_approved_6_rung_chain_headed_by_grok_4_6_xhigh() {
    let chain = &CATEGORY_MODEL_REQUIREMENTS["unspecified-low"].fallback_chain;
    assert!(!chain.iter().any(|entry| entry.model == "gpt-5.6-luna"));
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["unspecified-low"].fallback_chain,
        vec![
            entry(
                &["xai", "github-copilot", "opencode", "vercel"],
                "grok-4.6",
                Some("xhigh")
            ),
            entry(
                &[
                    "openai",
                    "quotio-openai",
                    "github-copilot",
                    "opencode",
                    "vercel"
                ],
                "gpt-5.6-terra",
                Some("high")
            ),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode",
                    "vercel"
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
                &["deepseek", "opencode-go", "vercel"],
                "deepseek-v4-pro",
                Some("max")
            ),
            entry(
                &["xiaomi", "opencode-go", "vercel"],
                "mimo-v2.5-pro",
                Some("max")
            ),
        ]
    );
}

#[test]
fn unspecified_high_follows_the_approved_3_rung_chain() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["unspecified-high"].fallback_chain,
        vec![
            entry(
                &[
                    "kimi-for-coding",
                    "moonshotai",
                    "opencode-go",
                    "opencode",
                    "vercel"
                ],
                "kimi-k3",
                Some("max")
            ),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode",
                    "vercel"
                ],
                "claude-opus-5",
                Some("xhigh")
            ),
            entry(
                &[
                    "openai",
                    "quotio-openai",
                    "github-copilot",
                    "opencode",
                    "vercel"
                ],
                "gpt-5.6-sol",
                Some("high")
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
                    "opencode",
                    "vercel"
                ],
                "claude-fable-5",
                Some("xhigh")
            ),
            entry(
                &[
                    "kimi-for-coding",
                    "moonshotai",
                    "opencode-go",
                    "opencode",
                    "vercel"
                ],
                "kimi-k3",
                Some("max")
            ),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode",
                    "vercel"
                ],
                "claude-opus-5",
                Some("xhigh")
            ),
        ]
    );
}

#[test]
fn writing_follows_the_approved_3_rung_chain() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["writing"].fallback_chain,
        vec![
            entry(
                &[
                    "kimi-for-coding",
                    "moonshotai",
                    "opencode-go",
                    "opencode",
                    "vercel"
                ],
                "kimi-k3",
                Some("low")
            ),
            entry(
                &[
                    "anthropic",
                    "anthropic-api",
                    "github-copilot",
                    "opencode",
                    "vercel"
                ],
                "claude-opus-5",
                Some("low")
            ),
            entry(
                &["google", "github-copilot", "opencode", "vercel"],
                "gemini-3.6-flash",
                None
            ),
        ]
    );
}

#[test]
fn deep_and_artistry_no_longer_hard_require_primary_models() {
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["deep"].requires_model, None);
    assert_eq!(CATEGORY_MODEL_REQUIREMENTS["artistry"].requires_model, None);
}
