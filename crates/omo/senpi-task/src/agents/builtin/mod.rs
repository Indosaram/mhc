//! Curated builtin agents and their fallback chains (`agents/builtin/*`).

mod prompts;

use std::collections::BTreeSet;
use std::sync::LazyLock;

use chrono::Datelike;

use super::types::{AgentDefinition, AgentToolRule};
use crate::delegate_adapter::DelegateFallbackEntry;

pub(crate) const PLAN_REVIEWER_SEND_DENIAL_REMINDER: &str = prompts::PLAN_REVIEWER_SEND_DENIAL_REMINDER;

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
    let mut tools: Vec<_> = READ_ONLY_TOOLS.iter().map(|tool| AgentToolRule::new(tool, true)).collect();
    if name == "librarian" {
        tools.push(AgentToolRule::new("x_search", true));
    } else if name == "explore" {
        tools.push(AgentToolRule::new("x_search", false));
    }
    AgentDefinition {
        description: Some(description.to_string()),
        prompt: Some(prompt),
        mode: Some("subagent".to_string()),
        execution_mode: Some("in-process".to_string()),
        tools: Some(tools),
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
            "plan-consultant",
            prompts::METIS_DESCRIPTION,
            prompts::METIS_PROMPT.to_string(),
        ),
        builtin(
            "plan-reviewer",
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

pub fn ulw_reviewer_agent_names() -> BTreeSet<&'static str> {
    ["omo-native-code-reviewer", "omo-native-gate-reviewer", "omo-native-qa-executor"].into_iter().collect()
}

pub fn canonical_agent_name(name: &str) -> String {
    match name.trim() {
        "omo-senpi-code-reviewer" => "omo-native-code-reviewer".to_string(),
        "omo-senpi-gate-reviewer" => "omo-native-gate-reviewer".to_string(),
        "omo-senpi-qa-executor" => "omo-native-qa-executor".to_string(),
        name => name.to_string(),
    }
}

fn entry(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    crate::delegate_adapter::fallback_entry(providers, model, variant)
}

fn fast_search_chain() -> Vec<DelegateFallbackEntry> {
    vec![
        entry(&["kimi-coding", "kimi-for-coding"], "kimi-for-coding-highspeed", Some("off")),
        entry(&["chatgpt-subscription", "openai"], "gpt-6-luna-fast", Some("low")),
        entry(&["anthropic-subscription", "anthropic", "github-copilot"], "claude-haiku-5-5", Some("medium")),
        entry(&["deepseek"], "deepseek-flash", Some("max")),
        entry(&["opencode-go", "bailian-coding-plan"], "qwen3.7-plus", None),
        entry(&["opencode-go"], "minimax-m2.7", None),
    ]
}

/// Hand transcription of model-core's agent requirements, keyed by builtin agent name.
pub static AGENT_FALLBACK_CHAINS: LazyLock<Vec<(&'static str, Vec<DelegateFallbackEntry>)>> =
    LazyLock::new(|| {
        let claude = ["anthropic-subscription", "anthropic", "github-copilot", "opencode"];
        vec![
            ("explore", fast_search_chain()),
            ("librarian", fast_search_chain()),
            (
                "plan-consultant",
                vec![
                    entry(&claude, "claude-fable-5-1", Some("max")),
                    entry(&claude, "claude-opus-5-5", Some("max")),
                    entry(&["opencode-go", "kimi-for-coding", "moonshotai", "opencode"], "kimi-k3", Some("max")),
                ],
            ),
            (
                "plan-reviewer",
                vec![
                    entry(&["chatgpt-subscription", "openai"], "gpt-6-astra", Some("xhigh")),
                    entry(&["github-copilot"], "gpt-6-astra", Some("high")),
                    entry(
                        &["chatgpt-subscription", "openai", "opencode"],
                        "gpt-6-astra",
                        Some("high"),
                    ),
                    entry(&["github-copilot"], "gpt-6-astra", Some("high")),
                    entry(&claude, "claude-opus-5-5", Some("max")),
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
            ),
        ]
    });

pub fn agent_fallback_chain(name: &str) -> Option<&'static [DelegateFallbackEntry]> {
    AGENT_FALLBACK_CHAINS
        .iter()
        .find(|(agent, _)| *agent == name)
        .map(|(_, chain)| chain.as_slice())
}
