//! `tools/task/description-plan-gated.test.ts`

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::agents::AgentDefinition;
use crate::tools::task::description::{DescriptionInput, build_task_tool_description};

fn config() -> Value {
    json!({ "categories": {}, "agents": {} })
}

fn agent(name: &str, description: &str) -> AgentDefinition {
    let mut definition = AgentDefinition::named(name);
    definition.description = description.to_string().into();
    definition
}

fn agent_set() -> BTreeMap<String, AgentDefinition> {
    [
        ("explore", "Codebase search"),
        ("librarian", "Docs research"),
        ("metis", "Pre-planning consultant"),
        ("momus", "Plan reviewer"),
    ]
    .into_iter()
    .map(|(name, description)| (name.to_string(), agent(name, description)))
    .collect()
}

fn build(agents: &BTreeMap<String, AgentDefinition>) -> String {
    let config = config();
    build_task_tool_description(&DescriptionInput {
        omo_config: &config,
        agents,
    })
}

#[test]
fn given_gated_and_plain_agents_when_built_then_plan_gated_agents_are_classified_separately() {
    let description = build(&agent_set());

    assert!(description.contains("Plan-gated agents"));
    assert!(description.contains("ulw-plan"));
    assert!(description.contains("start-work"));
    assert!(description.contains("user explicitly request"));
    assert!(description.contains(".omo/plans"));
    assert!(description.contains("metis"));
    assert!(description.contains("momus"));
}

#[test]
fn given_gated_and_plain_agents_when_built_then_available_agents_line_excludes_gated_names() {
    let description = build(&agent_set());

    let available_line = description
        .split('\n')
        .find(|line| line.contains("Available agents:"))
        .unwrap_or_default();
    assert!(available_line.contains("explore"));
    assert!(available_line.contains("librarian"));
    assert!(!available_line.contains("metis"));
    assert!(!available_line.contains("momus"));
}

#[test]
fn given_only_plain_agents_when_built_then_no_plan_gated_section_is_rendered() {
    let agents: BTreeMap<String, AgentDefinition> =
        [("explore".to_string(), agent("explore", "Codebase search"))]
            .into_iter()
            .collect();

    let description = build(&agents);

    assert!(!description.contains("Plan-gated agents"));
}
