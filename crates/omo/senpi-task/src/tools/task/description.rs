//! `tools/task/description.ts`: the task tool description, prompt snippet, and guidelines.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::agents::{AgentDefinition, plan_gated_agent_names};
use crate::tools::task::categories::{list_task_agents, list_task_categories};

pub const TASK_PROMPT_SNIPPET: &str =
    "Spawn one child or fan out a batch; use task_send to continue an existing child.";

pub const TASK_PROMPT_GUIDELINES: [&str; 5] = [
    "Use run_in_background=true only for parallel independent work; the default waits and returns the result.",
    "NEVER pass model together with category: category-routed tasks take their model from omo.json (categories.<name>.models).",
    "Continue an existing child with task_send(to=\"st_...\", message=\"...\"); task always spawns.",
    "Use task_output for one midpoint status or transcript peek; use task_cancel to end a child.",
    "Pass task_summary (one line, <=80 chars) on every spawn: the user's footer/widget UI shows it instead of the raw prompt, so it should say WHAT was delegated.",
];

pub struct DescriptionInput<'a> {
    pub omo_config: &'a Value,
    pub agents: &'a BTreeMap<String, AgentDefinition>,
}

fn render_list<'a>(entries: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> String {
    let lines: Vec<String> = entries
        .into_iter()
        .map(|(name, description)| match description {
            Some(description) if !description.is_empty() => format!("  - {name}: {description}"),
            _ => format!("  - {name}"),
        })
        .collect();
    if lines.is_empty() {
        "  (none configured)".to_string()
    } else {
        lines.join("\n")
    }
}

pub fn build_task_tool_description(input: &DescriptionInput<'_>) -> String {
    let categories = list_task_categories(input.omo_config);
    let agents = list_task_agents(input.agents);
    let gated_names = plan_gated_agent_names();
    let (gated_agents, plain_agents): (Vec<_>, Vec<_>) = agents
        .iter()
        .partition(|agent| gated_names.contains(agent.name.as_str()));
    let agent_names = plain_agents
        .iter()
        .map(|agent| agent.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let agent_names = if agent_names.is_empty() {
        "none loaded".to_string()
    } else {
        agent_names
    };
    let (gated_line, momus_notice) = if gated_agents.is_empty() {
        (String::new(), "")
    } else {
        let gated = gated_agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        (
            format!(
                "\n  Plan-gated agents (spawnable only after the user explicitly requests the ulw-plan workflow, a .omo/plans/*.md plan artifact was touched in this session, and start-work was never invoked): {gated}"
            ),
            "\n  momus is one-shot: spawn it, read task_output, optionally task_cancel; task_send is always refused. The harness replaces the momus spawn prompt with the canonical plan-review contract (one .omo/plans/*.md path only) - any other prompt content is discarded, so pass the plan path and nothing else.",
        )
    };
    let category_list = render_list(
        categories
            .iter()
            .map(|entry| (entry.name.as_str(), entry.description.as_deref())),
    );
    format!(
        r#"Spawn one child task or fan out a batch.

Choose exactly one input form:
- Single: prompt
- Batch: tasks (1-16 items); top-level target, model, and skills are inherited when an item omits them. An inherited model is rejected when the item's effective target is a category.

Each spawn MUST provide EITHER category OR subagent_type after inheritance. DO NOT provide both.

- category routes through Sisyphus-Junior. Available categories:
{category_list}
- subagent_type invokes a loaded agent directly. Available agents: {agent_names}{gated_line}{momus_notice}

Blank provider padding is normalized automatically; do not add filler values.
load_skills prepends named skills. run_in_background=true returns task ids for parallel work; false waits for results.
name is an optional stable handle. model is an explicit override for subagent_type spawns ONLY.
NEVER combine model with category: a category-routed task always takes its model from omo.json (categories.<name>.models), so passing both fails with invalid_arguments.
  CORRECT: task(subagent_type="momus", model="openai/gpt-5.6-sol", prompt="...")
  INCORRECT: task(category="architect", model="quotio-openai/gpt-5.6-luna-fast", prompt="...")
task_send continues an existing child; task always spawns.
Prompts MUST be in English."#
    )
}
