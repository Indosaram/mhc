//! Hardcoded category fallback chains, copied value-for-value from the TypeScript tables.

use std::sync::LazyLock;

use indexmap::IndexMap;

use crate::model_requirement_types::FallbackEntry;
use crate::model_requirement_types::ModelRequirement;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn entry(providers: &[&str], model: &str, variant: Option<&str>) -> FallbackEntry {
    FallbackEntry {
        providers: strings(providers),
        model: model.to_string(),
        variant: variant.map(str::to_string),
        ..FallbackEntry::default()
    }
}

/// Category name -> requirement, in declaration order.
pub static CATEGORY_MODEL_REQUIREMENTS: LazyLock<IndexMap<&'static str, ModelRequirement>> =
    LazyLock::new(|| {
        IndexMap::from([
            (
                "visual-engineering",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-fable-5-1",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-opus-5-5",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode-go",
                                "opencode",
                            ],
                            "kimi-k3",
                            Some("max"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "ultrabrain",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-6-astra",
                            Some("max"),
                        ),
                        entry(
                            &["github-copilot"],
                            "gpt-6-astra",
                            Some("max"),
                        ),
                        entry(
                            &["openai", "chatgpt-subscription", "opencode"],
                            "gpt-6-astra",
                            Some("max"),
                        ),
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-5.6-sol",
                            Some("max"),
                        ),
                        entry(
                            &["github-copilot"],
                            "gpt-5.6-sol",
                            Some("max"),
                        ),
                        entry(
                            &["openai", "chatgpt-subscription", "opencode"],
                            "gpt-5.6-sol",
                            Some("max"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "deep-low",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-6.1-sol",
                            Some("medium"),
                        ),
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-6.1-sol-fast",
                            Some("medium"),
                        ),
                        entry(
                            &[
                                "openai",
                                "chatgpt-subscription",
                                "github-copilot",
                                "opencode",
                            ],
                            "gpt-5.6-sol",
                            Some("medium"),
                        ),
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-5.6-sol-fast",
                            Some("medium"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "deep-high",
                ModelRequirement {
                    fallback_chain: vec![entry(
                        &[
                            "openai",
                            "chatgpt-subscription",
                            "github-copilot",
                            "opencode",
                        ],
                        "gpt-6-astra",
                        Some("high"),
                    )],
                    ..ModelRequirement::default()
                },
            ),
            (
                "artistry",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-fable-5-1",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-opus-5-5",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode-go",
                                "opencode",
                            ],
                            "kimi-k3",
                            Some("max"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "quick",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-6-luna-fast",
                            Some("low"),
                        ),
                        entry(
                            &["deepseek"],
                            "deepseek-flash",
                            Some("off"),
                        ),
                        entry(
                            &[
                                "qwen-token-plan",
                                "alibaba-token-plan",
                                "bailian-coding-plan",
                            ],
                            "qwen3.6-flash",
                            Some("low"),
                        ),
                        entry(
                            &["opencode-go"],
                            "minimax-m3",
                            Some("max"),
                        ),
                        entry(
                            &["opencode-go"],
                            "minimax-m2.7",
                            Some("max"),
                        ),
                        entry(
                            &["xai"],
                            "grok-4.20-0309-non-reasoning",
                            None,
                        ),
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                            ],
                            "claude-haiku-4-5",
                            Some("off"),
                        ),
                        entry(
                            &["zai-coding-plan"],
                            "glm-5.3-flash",
                            Some("low"),
                        ),
                        entry(
                            &["xiaomi"],
                            "mimo-v2.6-flash",
                            Some("low"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "unspecified-low",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-sonnet-5-5",
                            Some("medium"),
                        ),
                        entry(
                            &["xiaomi", "opencode-go"],
                            "mimo-v2.6-pro",
                            Some("max"),
                        ),
                        entry(
                            &["xai", "github-copilot", "opencode-go"],
                            "grok-4.7",
                            Some("xhigh"),
                        ),
                        entry(
                            &[
                                "openai",
                                "chatgpt-subscription",
                                "github-copilot",
                                "opencode",
                            ],
                            "gpt-5.6-terra",
                            Some("high"),
                        ),
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-sonnet-5",
                            Some("low"),
                        ),
                        entry(
                            &[
                                "qwen-token-plan",
                                "alibaba-token-plan",
                                "qwen-token-plan-cn",
                                "alibaba-token-plan-cn",
                            ],
                            "qwen3.8-max-preview",
                            Some("max"),
                        ),
                        entry(
                            &["deepseek", "opencode-go"],
                            "deepseek-v4-pro",
                            Some("max"),
                        ),
                        entry(
                            &["xiaomi", "opencode-go"],
                            "mimo-v2.5-pro",
                            Some("max"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "unspecified-high",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-opus-5-5",
                            Some("medium"),
                        ),
                        entry(
                            &["zai-coding-plan", "opencode-go"],
                            "glm-5.3",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode-go",
                                "opencode",
                            ],
                            "kimi-k3",
                            Some("max"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "writing",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-opus-5-5",
                            Some("low"),
                        ),
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                            ],
                            "claude-opus-4-6",
                            Some("max"),
                        ),
                    ],
                    requires_any_model: Some(true),
                    ..ModelRequirement::default()
                },
            ),
        ])
    });
