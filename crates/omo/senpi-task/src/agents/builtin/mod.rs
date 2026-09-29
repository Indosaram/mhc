//! Curated builtin agents and their fallback chains (`agents/builtin/*`).

mod prompts;

use std::collections::BTreeSet;
use std::sync::LazyLock;

use chrono::Datelike;

use super::types::{AgentDefinition, AgentToolRule};
use crate::delegate_adapter::DelegateFallbackEntry;

pub(crate) use prompts::MOMUS_SEND_DENIAL_REMINDER;

const READ_ONLY_TOOLS: [&str; 9] = [
    "read",
    "find",
    "grep",
    "ls",
    "bash",
    "lsp_diagnostics",
    "lsp_goto_definition",
    "lsp_find_references",
    "lsp_symbols",
];

fn builtin(name: &str, description: &str, prompt: String) -> AgentDefinition {
    AgentDefinition {
        description: Some(description.to_string()),
        prompt: Some(prompt),
        mode: Some("subagent".to_string()),
        execution_mode: Some("in-process".to_string()),
        tools: Some(
            READ_ONLY_TOOLS
                .iter()
                .map(|tool| AgentToolRule::new(tool, true))
                .collect(),
        ),
        ..AgentDefinition::named(name)
    }
}

/// The curated builtin definitions in registration order.
pub static BUILTIN_AGENT_DEFAULTS: LazyLock<Vec<AgentDefinition>> = LazyLock::new(|| {
    let year = chrono::Local::now().year();
    vec![
        builtin(
            "explore",
            prompts::EXPLORE_DESCRIPTION,
            prompts::EXPLORE_PROMPT.to_string(),
        ),
        builtin(
            "librarian",
            prompts::LIBRARIAN_DESCRIPTION,
            format!(
                "{}{year}{}",
                prompts::LIBRARIAN_PROMPT_BEFORE_YEAR,
                prompts::LIBRARIAN_PROMPT_AFTER_YEAR
            ),
        ),
        builtin(
            "metis",
            prompts::METIS_DESCRIPTION,
            prompts::METIS_PROMPT.to_string(),
        ),
        builtin(
            "momus",
            prompts::MOMUS_DESCRIPTION,
            prompts::MOMUS_PROMPT.to_string(),
        ),
    ]
});

/// The builtin definition registered under `name`.
pub fn builtin_agent(name: &str) -> Option<&'static AgentDefinition> {
    BUILTIN_AGENT_DEFAULTS
        .iter()
        .find(|definition| definition.name == name)
}

pub fn curated_readonly_agent_names() -> BTreeSet<&'static str> {
    BUILTIN_AGENT_DEFAULTS
        .iter()
        .map(|definition| definition.name.as_str())
        .collect()
}

fn entry(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    DelegateFallbackEntry::new(providers, model, variant)
}

fn fast_search_chain() -> Vec<DelegateFallbackEntry> {
    vec![
        entry(&["openai"], "gpt-5.6-luna-fast", Some("low")),
        entry(&["deepseek"], "deepseek-v4-flash", Some("max")),
        entry(
            &["opencode-go", "bailian-coding-plan"],
            "qwen3.5-plus",
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
    ]
}

/// Hand transcription of model-core's agent requirements, keyed by builtin agent name.
pub static AGENT_FALLBACK_CHAINS: LazyLock<Vec<(&'static str, Vec<DelegateFallbackEntry>)>> =
    LazyLock::new(|| {
        let claude = ["anthropic", "github-copilot", "opencode", "vercel"];
        vec![
            ("explore", fast_search_chain()),
            ("librarian", fast_search_chain()),
            (
                "metis",
                vec![
                    entry(&claude, "claude-sonnet-4-6", None),
                    entry(&claude, "claude-opus-5", Some("max")),
                    entry(
                        &["openai", "github-copilot", "opencode", "vercel"],
                        "gpt-5.6-sol",
                        Some("medium"),
                    ),
                    entry(&["opencode-go", "vercel"], "glm-5.2", None),
                    entry(&["kimi-for-coding"], "kimi-k3", None),
                ],
            ),
            (
                "momus",
                vec![
                    entry(&["openai", "vercel"], "gpt-5.6-terra", Some("high")),
                    entry(&["github-copilot"], "gpt-5.6-terra", Some("high")),
                    entry(
                        &["openai", "opencode", "vercel"],
                        "gpt-5.6-sol",
                        Some("xhigh"),
                    ),
                    entry(&["github-copilot"], "gpt-5.6-sol", Some("high")),
                    entry(&claude, "claude-opus-5", Some("max")),
                    entry(
                        &["google", "github-copilot", "opencode", "vercel"],
                        "gemini-3.1-pro",
                        Some("high"),
                    ),
                    entry(&["opencode-go", "vercel"], "glm-5.2", None),
                ],
            ),
        ]
    });

pub fn agent_fallback_chain(name: &str) -> Option<&'static [DelegateFallbackEntry]> {
    AGENT_FALLBACK_CHAINS
        .iter()
        .find(|(agent, _)| *agent == name)
        .map(|(_, chain)| chain.as_slice())
}
