//! Hardcoded agent fallback chains, copied value-for-value from the TypeScript tables.

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
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-opus-5-5",
                            Some("max"),
                        ),
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
                            None,
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
                            &[
                                "zai-coding-plan",
                                "opencode",
                                "bailian-coding-plan",
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
                    fallback_chain: vec![
                        entry(
                            &[
                                "openai",
                                "chatgpt-subscription",
                                "github-copilot",
                                "opencode",
                            ],
                            "gpt-6-sol",
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
                    ],
                    requires_provider: Some(strings(&[
                        "openai",
                        "chatgpt-subscription",
                        "github-copilot",
                        "opencode",
                    ])),
                    requires_any_model: Some(true),
                    ..ModelRequirement::default()
                },
            ),
            (
                "oracle",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["openai", "chatgpt-subscription", "opencode"],
                            "gpt-5.6-sol",
                            Some("xhigh"),
                        ),
                        entry(
                            &["github-copilot"],
                            "gpt-5.6-sol",
                            Some("high"),
                        ),
                        entry(
                            &["google", "google-vertex", "opencode"],
                            "gemini-4-argon",
                            Some("high"),
                        ),
                        entry(
                            &["google", "github-copilot", "opencode"],
                            "gemini-3.1-pro",
                            Some("high"),
                        ),
                        entry(
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-opus-5-5",
                            Some("max"),
                        ),
                        entry(&["opencode-go"], "glm-5.2", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "librarian",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["kimi-for-coding"],
                            "kimi-for-coding-highspeed",
                            Some("off"),
                        ),
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-6-luna-fast",
                            Some("low"),
                        ),
                        entry(
                            &["anthropic", "github-copilot"],
                            "claude-haiku-5-5",
                            Some("medium"),
                        ),
                        entry(
                            &["deepseek"],
                            "deepseek-flash",
                            Some("max"),
                        ),
                        entry(
                            &["opencode-go", "bailian-coding-plan"],
                            "qwen3.7-plus",
                            None,
                        ),
                        entry(&["opencode-go"], "minimax-m2.7", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "explore",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["kimi-for-coding"],
                            "kimi-for-coding-highspeed",
                            Some("off"),
                        ),
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-6-luna-fast",
                            Some("low"),
                        ),
                        entry(
                            &["anthropic", "github-copilot"],
                            "claude-haiku-5-5",
                            Some("medium"),
                        ),
                        entry(
                            &["deepseek"],
                            "deepseek-flash",
                            Some("max"),
                        ),
                        entry(
                            &["opencode-go", "bailian-coding-plan"],
                            "qwen3.7-plus",
                            None,
                        ),
                        entry(&["opencode-go"], "minimax-m2.7", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "multimodal-looker",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["openai", "chatgpt-subscription", "opencode"],
                            "gpt-5.6-sol",
                            Some("low"),
                        ),
                        entry(&["opencode-go"], "kimi-k3", None),
                        entry(&["zai-coding-plan"], "glm-4.6v", None),
                        entry(
                            &[
                                "openai",
                                "chatgpt-subscription",
                                "github-copilot",
                                "opencode",
                            ],
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
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-fable-5-1",
                            Some("xhigh"),
                        ),
                        entry(
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-opus-5-5",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "opencode-go",
                                "kimi-for-coding",
                                "moonshotai",
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
                "metis",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-fable-5-1",
                            Some("max"),
                        ),
                        entry(
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-opus-5-5",
                            Some("max"),
                        ),
                        entry(
                            &[
                                "opencode-go",
                                "kimi-for-coding",
                                "moonshotai",
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
                "momus",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["openai", "chatgpt-subscription"],
                            "gpt-6-astra",
                            Some("xhigh"),
                        ),
                        entry(
                            &["github-copilot"],
                            "gpt-6-astra",
                            Some("high"),
                        ),
                        entry(
                            &["openai", "chatgpt-subscription", "opencode"],
                            "gpt-6-astra",
                            Some("high"),
                        ),
                        entry(
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-opus-5-5",
                            Some("max"),
                        ),
                        entry(
                            &["google", "google-vertex", "opencode"],
                            "gemini-4-argon",
                            Some("high"),
                        ),
                        entry(
                            &["google", "github-copilot", "opencode"],
                            "gemini-3.1-pro",
                            Some("high"),
                        ),
                        entry(&["opencode-go"], "glm-5.2", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "atlas",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-sonnet-5",
                            None,
                        ),
                        entry(&["opencode-go"], "kimi-k3", None),
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
                        entry(&["opencode-go"], "minimax-m3", None),
                        entry(
                            &["minimax-coding-plan", "minimax-cn-coding-plan"],
                            "MiniMax-M3",
                            None,
                        ),
                        entry(&["opencode-go"], "minimax-m2.7", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
            (
                "sisyphus-junior",
                ModelRequirement {
                    fallback_chain: vec![
                        entry(
                            &["anthropic", "github-copilot", "opencode"],
                            "claude-sonnet-5",
                            None,
                        ),
                        entry(&["opencode-go"], "kimi-k3", None),
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
                        entry(&["opencode-go"], "minimax-m3", None),
                        entry(
                            &["minimax-coding-plan", "minimax-cn-coding-plan"],
                            "MiniMax-M3",
                            None,
                        ),
                        entry(&["opencode-go"], "minimax-m2.7", None),
                        entry(&["opencode"], "big-pickle", None),
                    ],
                    ..ModelRequirement::default()
                },
            ),
        ])
    });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon_high_precedes_previous_pro_without_copilot() {
        for (agent, index) in [("oracle", 2), ("momus", 4)] {
            let chain = &AGENT_MODEL_REQUIREMENTS[agent].fallback_chain;
            assert_eq!(chain[index], entry(&["google", "google-vertex", "opencode"], "gemini-4-argon", Some("high")));
            assert_eq!(chain[index + 1], entry(&["google", "github-copilot", "opencode"], "gemini-3.1-pro", Some("high")));
        }
        let argon: Vec<_> = AGENT_MODEL_REQUIREMENTS.values()
            .flat_map(|requirement| &requirement.fallback_chain)
            .filter(|rung| rung.model == "gemini-4-argon")
            .collect();
        assert_eq!(argon.len(), 2);
        for rung in argon {
            assert!(matches!(rung.variant.as_deref(), Some("low" | "medium" | "high")));
            assert!(!rung.providers.iter().any(|provider| provider == "github-copilot"));
        }
    }

    #[test]
    fn fast_agents_use_haiku_medium_as_third_rung() {
        for agent in ["librarian", "explore"] {
            assert_eq!(AGENT_MODEL_REQUIREMENTS[agent].fallback_chain, vec![
                entry(&["kimi-for-coding"], "kimi-for-coding-highspeed", Some("off")),
                entry(&["openai", "chatgpt-subscription"], "gpt-6-luna-fast", Some("low")),
                entry(&["anthropic", "github-copilot"], "claude-haiku-5-5", Some("medium")),
                entry(&["deepseek"], "deepseek-flash", Some("max")),
                entry(&["opencode-go", "bailian-coding-plan"], "qwen3.7-plus", None),
                entry(&["opencode-go"], "minimax-m2.7", None),
            ]);
        }
    }
}
