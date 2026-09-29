use std::sync::LazyLock;

use crate::delegate_adapter::DelegateFallbackEntry;

const ANTHROPIC: &[&str] = &[
    "anthropic",
    "anthropic-api",
    "github-copilot",
    "opencode",
    "vercel",
];
const KIMI: &[&str] = &[
    "kimi-coding",
    "kimi-for-coding",
    "moonshotai",
    "opencode-go",
];
const OPENAI: &[&str] = &[
    "openai",
    "quotio-openai",
    "github-copilot",
    "opencode",
    "vercel",
];

fn rung(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    DelegateFallbackEntry::new(providers, model, variant)
}

/// Mirrors model-core `category-model-requirements.ts`; senpi's kimi rungs carry both the
/// `kimi-coding` registry id and the `kimi-for-coding` models.dev id.
pub static CATEGORY_FALLBACK_CHAINS: LazyLock<Vec<(&'static str, Vec<DelegateFallbackEntry>)>> =
    LazyLock::new(|| {
        vec![
            (
                "visual-engineering",
                vec![
                    rung(ANTHROPIC, "claude-opus-5", Some("max")),
                    rung(KIMI, "kimi-k3", Some("max")),
                    rung(
                        &["zai-coding-plan", "opencode-go", "vercel"],
                        "glm-5.2",
                        Some("max"),
                    ),
                    rung(OPENAI, "gpt-5.6-sol", Some("medium")),
                ],
            ),
            (
                "architect",
                vec![rung(ANTHROPIC, "claude-fable-5", Some("xhigh"))],
            ),
            (
                "ultrabrain",
                vec![
                    rung(
                        &["openai", "quotio-openai", "vercel"],
                        "gpt-5.6-sol",
                        Some("max"),
                    ),
                    rung(&["github-copilot"], "gpt-5.6-sol", Some("max")),
                    rung(
                        &["openai", "opencode", "vercel"],
                        "gpt-5.6-sol",
                        Some("max"),
                    ),
                ],
            ),
            ("deep", vec![rung(OPENAI, "gpt-5.6-sol", Some("medium"))]),
            (
                "artistry",
                vec![
                    rung(ANTHROPIC, "claude-fable-5", Some("xhigh")),
                    rung(KIMI, "kimi-k3", Some("max")),
                    rung(ANTHROPIC, "claude-opus-5", Some("xhigh")),
                ],
            ),
            (
                "quick",
                vec![
                    rung(
                        &["kimi-coding", "kimi-for-coding"],
                        "kimi-for-coding-highspeed",
                        None,
                    ),
                    rung(&["openai-codex"], "gpt-5.6-luna-fast", Some("low")),
                    rung(&["deepseek"], "deepseek-v4-flash", Some("off")),
                    rung(
                        &[
                            "qwen-token-plan",
                            "alibaba-token-plan",
                            "bailian-coding-plan",
                            "vercel",
                        ],
                        "qwen3.6-flash",
                        Some("low"),
                    ),
                    rung(&["opencode-go", "vercel"], "minimax-m3", Some("max")),
                    rung(&["opencode-go", "vercel"], "minimax-m2.7", Some("max")),
                    rung(&["xai"], "grok-4.20-0309-non-reasoning", None),
                    rung(
                        &["anthropic", "anthropic-api", "github-copilot", "vercel"],
                        "claude-haiku-4-5",
                        Some("off"),
                    ),
                ],
            ),
            (
                "unspecified-low",
                vec![
                    rung(
                        &["xai", "github-copilot", "opencode", "vercel"],
                        "grok-4.6",
                        Some("xhigh"),
                    ),
                    rung(OPENAI, "gpt-5.6-terra", Some("high")),
                    rung(ANTHROPIC, "claude-sonnet-5", Some("low")),
                    rung(
                        &[
                            "qwen-token-plan",
                            "alibaba-token-plan",
                            "qwen-token-plan-cn",
                            "alibaba-token-plan-cn",
                        ],
                        "qwen3.8-max-preview",
                        Some("max"),
                    ),
                    rung(
                        &["deepseek", "opencode-go", "vercel"],
                        "deepseek-v4-pro",
                        Some("max"),
                    ),
                    rung(
                        &["xiaomi", "opencode-go", "vercel"],
                        "mimo-v2.5-pro",
                        Some("max"),
                    ),
                ],
            ),
            (
                "unspecified-high",
                vec![
                    rung(KIMI, "kimi-k3", Some("max")),
                    rung(ANTHROPIC, "claude-opus-5", Some("xhigh")),
                    rung(OPENAI, "gpt-5.6-sol", Some("high")),
                ],
            ),
            (
                "writing",
                vec![
                    rung(KIMI, "kimi-k3", Some("low")),
                    rung(ANTHROPIC, "claude-opus-5", Some("low")),
                    rung(
                        &["google", "github-copilot", "opencode", "vercel"],
                        "gemini-3.1-pro",
                        None,
                    ),
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
