use model_core::latest_models::{
    CLAUDE_FABLE, CLAUDE_HAIKU, CLAUDE_OPUS, CLAUDE_SONNET, GEMINI_PRO, GLM, GLM_FLASH, GPT_ASTRA,
    GPT_LUNA_FAST, GPT_SOL, KIMI,
};
use std::sync::LazyLock;

use crate::delegate_adapter::DelegateFallbackEntry;

const ANTHROPIC: &[&str] = &[
    "anthropic-subscription",
    "anthropic",
    "anthropic-api",
    "github-copilot",
    "opencode",
];
const KIMI_PROVIDERS: &[&str] = &[
    "kimi-coding",
    "kimi-for-coding",
    "moonshotai",
    "opencode-go",
];
const OPENAI: &[&str] = &["chatgpt-subscription", "openai"];
const OPENAI_ZEN: &[&str] = &["chatgpt-subscription", "openai", "opencode"];

fn rung(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    crate::delegate_adapter::fallback_entry(providers, model, variant)
}

/// senpi-task category routing: provider order prefers the subscription over a metered API.
pub static CATEGORY_FALLBACK_CHAINS: LazyLock<Vec<(&'static str, Vec<DelegateFallbackEntry>)>> =
    LazyLock::new(|| {
        vec![
            (
                "visual-engineering",
                vec![
                    rung(
                        &["google", "google-vertex", "opencode"],
                        GEMINI_PRO,
                        Some("low"),
                    ),
                    rung(ANTHROPIC, CLAUDE_FABLE, Some("max")),
                    rung(ANTHROPIC, "claude-fable-5-1", Some("max")),
                    rung(ANTHROPIC, CLAUDE_OPUS, Some("max")),
                    rung(KIMI_PROVIDERS, KIMI, Some("max")),
                ],
            ),
            (
                "architect",
                vec![
                    rung(ANTHROPIC, CLAUDE_FABLE, Some("xhigh")),
                    rung(ANTHROPIC, "claude-fable-5-1", Some("xhigh")),
                ],
            ),
            ("ultrabrain", vec![rung(OPENAI_ZEN, GPT_ASTRA, Some("max"))]),
            ("deep-low", vec![rung(OPENAI, GPT_SOL, Some("medium"))]),
            ("deep-high", vec![rung(OPENAI_ZEN, GPT_ASTRA, Some("high"))]),
            (
                "artistry",
                vec![
                    rung(ANTHROPIC, CLAUDE_FABLE, Some("max")),
                    rung(ANTHROPIC, "claude-fable-5-1", Some("max")),
                    rung(ANTHROPIC, CLAUDE_OPUS, Some("max")),
                    rung(KIMI_PROVIDERS, KIMI, Some("max")),
                ],
            ),
            (
                "quick",
                vec![
                    rung(OPENAI, GPT_LUNA_FAST, Some("low")),
                    rung(
                        &[
                            "anthropic-subscription",
                            "anthropic",
                            "anthropic-api",
                            "github-copilot",
                        ],
                        CLAUDE_HAIKU,
                        Some("medium"),
                    ),
                    rung(&["deepseek"], "deepseek-flash", Some("off")),
                    rung(
                        &[
                            "qwen-token-plan",
                            "alibaba-token-plan",
                            "bailian-coding-plan",
                        ],
                        "qwen3.6-flash",
                        Some("low"),
                    ),
                    rung(&["opencode-go"], "minimax-m3", None),
                    rung(&["opencode-go"], "minimax-m2.7", None),
                    rung(&["xai"], "grok-4.20-0309-non-reasoning", None),
                    rung(&["zai", "zai-coding-cn"], GLM_FLASH, Some("low")),
                    rung(&["xiaomi"], "mimo-v2.6-flash", Some("low")),
                ],
            ),
            (
                "unspecified-low",
                vec![
                    rung(ANTHROPIC, CLAUDE_SONNET, Some("medium")),
                    rung(&["xiaomi", "opencode-go"], "mimo-v2.6-pro", Some("high")),
                    rung(
                        &["xai", "github-copilot", "opencode-go"],
                        "grok-4.7",
                        Some("xhigh"),
                    ),
                    rung(&["zai", "zai-coding-cn", "opencode-go"], GLM, Some("high")),
                    rung(OPENAI, GPT_SOL, Some("medium")),
                    rung(OPENAI, "gpt-6.1-sol", Some("medium")),
                    rung(
                        &[
                            "chatgpt-subscription",
                            "openai",
                            "github-copilot",
                            "opencode",
                        ],
                        "gpt-5.6-sol",
                        Some("medium"),
                    ),
                    rung(ANTHROPIC, "claude-sonnet-5", Some("low")),
                    rung(
                        &[
                            "qwen-token-plan",
                            "alibaba-token-plan",
                            "qwen-token-plan-cn",
                            "alibaba-token-plan-cn",
                        ],
                        "qwen3.8-max-preview",
                        Some("high"),
                    ),
                    rung(&["deepseek", "opencode-go"], "deepseek-v4-pro", Some("max")),
                    rung(&["xiaomi", "opencode-go"], "mimo-v2.5-pro", Some("high")),
                ],
            ),
            (
                "unspecified-high",
                vec![
                    rung(ANTHROPIC, CLAUDE_OPUS, Some("medium")),
                    rung(&["zai", "zai-coding-cn", "opencode-go"], GLM, Some("max")),
                    rung(KIMI_PROVIDERS, KIMI, Some("max")),
                ],
            ),
            (
                "writing",
                vec![
                    rung(
                        &["google", "google-vertex", "opencode"],
                        GEMINI_PRO,
                        Some("low"),
                    ),
                    rung(ANTHROPIC, CLAUDE_OPUS, Some("low")),
                    rung(ANTHROPIC, "claude-opus-4-6", Some("max")),
                ],
            ),
        ]
    });

pub fn category_fallback_chain(category_name: &str) -> Option<&'static [DelegateFallbackEntry]> {
    CATEGORY_FALLBACK_CHAINS
        .iter()
        .find(|(name, _)| *name == category_name)
        .map(|(_, chain)| chain.as_slice())
}
