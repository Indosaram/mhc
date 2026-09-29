//! Hardcoded agent and category fallback chains, copied value-for-value from the TypeScript tables.

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

/// Agent name -> requirement, in declaration order.
pub static AGENT_MODEL_REQUIREMENTS: LazyLock<IndexMap<&'static str, ModelRequirement>> =
    LazyLock::new(|| {
        IndexMap::from([
            (
                "sisyphus",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["anthropic", "github-copilot", "opencode", "vercel"],
                            "claude-opus-5",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "opencode-go",
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode",
                                "vercel",
                                "bailian-coding-plan",
                                "moonshotai-cn",
                                "firmware",
                                "ollama-cloud",
                                "aihubmix",
                            ],
                            "kimi-k3",
                            None,
                        ),
                        entry(
                            &["openai", "github-copilot", "opencode", "vercel"],
                            "gpt-5.6-sol",
                            Some("medium"),
                        ),
                        entry(
                            &[
                                "zai-coding-plan",
                                "opencode",
                                "bailian-coding-plan",
                                "vercel",
                            ],
                            "glm-5.2",
                            None,
                        ),
                        entry(&["opencode"], "big-pickle", None),
                    ],
                    requires_any_model: Some(true),
                    ..ModelRequirement::default()
                },
            ),
            (
                "hephaestus",
                ModelRequirement {
                    fallback_chain: vec![entry(
                        &["openai", "github-copilot", "vercel", "opencode"],
                        "gpt-5.6-sol",
                        Some("medium"),
                    )],
                    requires_any_model: Some(true),
                    requires_provider: Some(strings(&[
                        "openai",
                        "github-copilot",
                        "opencode",
                        "vercel",
                    ])),
                    ..ModelRequirement::default()
                },
            ),
            (
                "oracle",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["openai", "opencode", "vercel"],
                            "gpt-5.6-sol",
                            Some("xhigh"),
                        ),
                        entry(&["github-copilot"], "gpt-5.6-sol", Some("high")),
                        entry(
                            &["google", "github-copilot", "opencode", "vercel"],
                            "gemini-3.1-pro",
                            Some("high"),
                        ),
                        entry(
                            &["anthropic", "github-copilot", "opencode", "vercel"],
                            "claude-opus-5",
                            Some("max"),
                        ),
                        entry(&["opencode-go", "vercel"], "glm-5.2", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "librarian",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(&["openai"], "gpt-5.6-luna-fast", Some("low")),
                        entry(&["deepseek"], "deepseek-v4-flash", Some("max")),
                        entry(
                            &["opencode-go", "bailian-coding-plan"],
                            "qwen3.7-plus",
                            None,
                        ),
                        entry(&["vercel"], "minimax-m2.7-highspeed", None),
                        entry(&["opencode-go", "vercel"], "minimax-m3", None),
                        entry(
                            &["minimax-coding-plan", "minimax-cn-coding-plan"],
                            "MiniMax-M3",
                            None,
                        ),
                        entry(&["opencode-go", "vercel"], "minimax-m2.7", None),
                        entry(
                            &["anthropic", "github-copilot", "vercel"],
                            "claude-haiku-4-5",
                            None,
                        ),
                        entry(&["openai", "vercel"], "gpt-5.4-nano", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "explore",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(&["openai"], "gpt-5.6-luna-fast", Some("low")),
                        entry(&["deepseek"], "deepseek-v4-flash", Some("max")),
                        entry(
                            &["opencode-go", "bailian-coding-plan"],
                            "qwen3.7-plus",
                            None,
                        ),
                        entry(&["vercel"], "minimax-m2.7-highspeed", None),
                        entry(&["opencode-go", "vercel"], "minimax-m3", None),
                        entry(
                            &["minimax-coding-plan", "minimax-cn-coding-plan"],
                            "MiniMax-M3",
                            None,
                        ),
                        entry(&["opencode-go", "vercel"], "minimax-m2.7", None),
                        entry(
                            &["anthropic", "github-copilot", "vercel"],
                            "claude-haiku-4-5",
                            None,
                        ),
                        entry(&["openai", "vercel"], "gpt-5.4-nano", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "multimodal-looker",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["openai", "opencode", "vercel"],
                            "gpt-5.6-sol",
                            Some("low"),
                        ),
                        entry(&["opencode-go", "vercel"], "kimi-k3", None),
                        entry(&["zai-coding-plan", "vercel"], "glm-4.6v", None),
                        entry(
                            &["openai", "github-copilot", "opencode", "vercel"],
                            "gpt-5-nano",
                            None,
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "prometheus",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["anthropic", "github-copilot", "opencode", "vercel"],
                            "claude-fable-5",
                            Some("xhigh"),
                        ),
                        entry(
                            &[
                                "opencode-go",
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode",
                                "vercel",
                            ],
                            "kimi-k3",
                            Some("max"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "metis",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["anthropic", "github-copilot", "opencode", "vercel"],
                            "claude-opus-5",
                            Some("high"),
                        ),
                        entry(
                            &[
                                "opencode-go",
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode",
                                "vercel",
                            ],
                            "kimi-k3",
                            Some("low"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "momus",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(&["openai", "vercel"], "gpt-5.6-terra", Some("high")),
                        entry(&["github-copilot"], "gpt-5.6-terra", Some("high")),
                        entry(
                            &["openai", "opencode", "vercel"],
                            "gpt-5.6-sol",
                            Some("xhigh"),
                        ),
                        entry(&["github-copilot"], "gpt-5.6-sol", Some("high")),
                        entry(
                            &["anthropic", "github-copilot", "opencode", "vercel"],
                            "claude-opus-5",
                            Some("max"),
                        ),
                        entry(
                            &["google", "github-copilot", "opencode", "vercel"],
                            "gemini-3.1-pro",
                            Some("high"),
                        ),
                        entry(&["opencode-go", "vercel"], "glm-5.2", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "atlas",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["anthropic", "github-copilot", "opencode", "vercel"],
                            "claude-sonnet-5",
                            None,
                        ),
                        entry(&["opencode-go", "vercel"], "kimi-k3", None),
                        entry(
                            &["openai", "github-copilot", "opencode", "vercel"],
                            "gpt-5.6-sol",
                            Some("medium"),
                        ),
                        entry(&["opencode-go", "vercel"], "minimax-m3", None),
                        entry(
                            &["minimax-coding-plan", "minimax-cn-coding-plan"],
                            "MiniMax-M3",
                            None,
                        ),
                        entry(&["opencode-go", "vercel"], "minimax-m2.7", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "sisyphus-junior",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["anthropic", "github-copilot", "opencode", "vercel"],
                            "claude-sonnet-5",
                            None,
                        ),
                        entry(&["opencode-go", "vercel"], "kimi-k3", None),
                        entry(
                            &["openai", "github-copilot", "opencode", "vercel"],
                            "gpt-5.6-sol",
                            Some("medium"),
                        ),
                        entry(&["opencode-go", "vercel"], "minimax-m3", None),
                        entry(
                            &["minimax-coding-plan", "minimax-cn-coding-plan"],
                            "MiniMax-M3",
                            None,
                        ),
                        entry(&["opencode-go", "vercel"], "minimax-m2.7", None),
                        entry(&["opencode"], "big-pickle", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
        ])
    });

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
                                "vercel",
                            ],
                            "claude-opus-5",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode-go",
                                "opencode",
                                "vercel",
                            ],
                            "kimi-k3",
                            Some("max"),
                        ),
                        entry(
                            &["zai-coding-plan", "opencode-go", "vercel"],
                            "glm-5.2",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "openai",
                                "quotio-openai",
                                "github-copilot",
                                "opencode",
                                "vercel",
                            ],
                            "gpt-5.6-sol",
                            Some("medium"),
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
                            &["openai", "quotio-openai", "vercel"],
                            "gpt-5.6-sol",
                            Some("max"),
                        ),
                        entry(&["github-copilot"], "gpt-5.6-sol", Some("max")),
                        entry(
                            &["openai", "opencode", "vercel"],
                            "gpt-5.6-sol",
                            Some("max"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "deep",
                ModelRequirement {
                    fallback_chain: vec![entry(
                        &[
                            "openai",
                            "quotio-openai",
                            "github-copilot",
                            "opencode",
                            "vercel",
                        ],
                        "gpt-5.6-sol",
                        Some("medium"),
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
                                "vercel",
                            ],
                            "claude-fable-5",
                            Some("xhigh"),
                        ),
                        entry(
                            &[
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode-go",
                                "opencode",
                                "vercel",
                            ],
                            "kimi-k3",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                                "vercel",
                            ],
                            "claude-opus-5",
                            Some("xhigh"),
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "quick",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(&["kimi-for-coding"], "kimi-for-coding-highspeed", None),
                        entry(&["openai-codex"], "gpt-5.6-luna-fast", Some("low")),
                        entry(&["deepseek"], "deepseek-v4-flash", Some("off")),
                        entry(
                            &[
                                "qwen-token-plan",
                                "alibaba-token-plan",
                                "bailian-coding-plan",
                                "vercel",
                            ],
                            "qwen3.6-flash",
                            Some("low"),
                        ),
                        entry(&["opencode-go", "vercel"], "minimax-m3", Some("max")),
                        entry(&["opencode-go", "vercel"], "minimax-m2.7", Some("max")),
                        entry(&["xai"], "grok-4.20-0309-non-reasoning", None),
                        entry(
                            &["anthropic", "anthropic-api", "github-copilot", "vercel"],
                            "claude-haiku-4-5",
                            Some("off"),
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
                            &["xai", "github-copilot", "opencode", "vercel"],
                            "grok-4.6",
                            Some("xhigh"),
                        ),
                        entry(
                            &[
                                "openai",
                                "quotio-openai",
                                "github-copilot",
                                "opencode",
                                "vercel",
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
                                "vercel",
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
                            &["deepseek", "opencode-go", "vercel"],
                            "deepseek-v4-pro",
                            Some("max"),
                        ),
                        entry(
                            &["xiaomi", "opencode-go", "vercel"],
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
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode-go",
                                "opencode",
                                "vercel",
                            ],
                            "kimi-k3",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                                "vercel",
                            ],
                            "claude-opus-5",
                            Some("xhigh"),
                        ),
                        entry(
                            &[
                                "openai",
                                "quotio-openai",
                                "github-copilot",
                                "opencode",
                                "vercel",
                            ],
                            "gpt-5.6-sol",
                            Some("high"),
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
                                "kimi-for-coding",
                                "moonshotai",
                                "opencode-go",
                                "opencode",
                                "vercel",
                            ],
                            "kimi-k3",
                            Some("low"),
                        ),
                        entry(
                            &[
                                "anthropic",
                                "anthropic-api",
                                "github-copilot",
                                "opencode",
                                "vercel",
                            ],
                            "claude-opus-5",
                            Some("low"),
                        ),
                        entry(
                            &["google", "github-copilot", "opencode", "vercel"],
                            "gemini-3.6-flash",
                            None,
                        ),
                    ],
                    ..ModelRequirement::default()
                },
            ),
        ])
    });
