use model_core::CATEGORY_MODEL_REQUIREMENTS;
use pretty_assertions::assert_eq;

use crate::support::entry;

#[test]
fn visual_engineering_prioritizes_opus_max_kimi_k3_max_glm_5_2_max_then_sol_medium() {
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
fn deep_is_limited_to_a_single_sol_family_medium_rung() {
    assert_eq!(
        CATEGORY_MODEL_REQUIREMENTS["deep"].fallback_chain,
        vec![entry(
            &[
                "openai",
                "quotio-openai",
                "github-copilot",
                "opencode",
                "vercel"
            ],
            "gpt-5.6-sol",
            Some("medium")
        ),]
    );
}

#[test]
fn quick_prioritizes_kimi_high_speed_luna_low_deepseek_off_then_the_speed_tier() {
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
fn unspecified_high_artistry_and_writing_follow_the_approved_kimi_for_coding_chains() {
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
