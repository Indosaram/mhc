//! `tools/task/description.test.ts`

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{Value, json};

use crate::agents::AgentDefinition;
use crate::tools::task::description::{
    DescriptionInput, TASK_PROMPT_GUIDELINES, TASK_PROMPT_SNIPPET, build_task_tool_description,
};

fn agents() -> BTreeMap<String, AgentDefinition> {
    let mut momus = AgentDefinition::named("momus");
    momus.description = "Deep reasoning".to_string().into();
    [("momus".to_string(), momus)].into_iter().collect()
}

fn empty_config() -> Value {
    json!({ "categories": {}, "agents": {} })
}

fn build(config: &Value) -> String {
    let agents = agents();
    build_task_tool_description(&DescriptionInput {
        omo_config: config,
        agents: &agents,
    })
}

#[test]
fn given_custom_omo_json_category_when_built_then_it_lists_that_category_dynamically() {
    let config = json!({
        "categories": { "release-crew": { "description": "Ships the release train" } },
        "agents": {},
    });

    let description = build(&config);

    assert!(description.contains("release-crew"));
    assert!(description.contains("Ships the release train"));
}

#[test]
fn given_description_when_built_then_it_enforces_category_xor_subagent_type() {
    let description = build(&empty_config());
    assert!(description.contains("EITHER category OR subagent_type"));
}

#[test]
fn given_description_when_built_then_it_describes_spawn_only_task_and_task_send_continuation() {
    let description = build(&empty_config());
    assert!(description.contains("task_send"));
    assert!(!description.contains("task(task_id"));
    assert!(description.contains("run_in_background"));
}

#[test]
fn given_loaded_agents_when_built_then_it_lists_available_agent_types() {
    let description = build(&empty_config());
    assert!(description.contains("momus"));
}

#[test]
fn given_guidelines_when_read_then_task_summary_usage_is_advertised() {
    assert!(
        TASK_PROMPT_GUIDELINES
            .iter()
            .any(|guideline| guideline.contains("task_summary"))
    );
}

#[test]
fn given_prompt_surfaces_when_read_then_snippet_and_guidelines_are_present() {
    assert!(!TASK_PROMPT_SNIPPET.is_empty());
    assert!(!TASK_PROMPT_GUIDELINES.is_empty());
}

#[test]
fn given_task_prompt_surfaces_when_inspected_then_target_selection_belongs_to_description_only() {
    let description = build(&empty_config());
    let target_rule =
        Regex::new(r"(?i)category.*subagent_type|subagent_type.*category").expect("valid regex");
    let duplicated_target_rule = TASK_PROMPT_GUIDELINES
        .iter()
        .any(|guideline| target_rule.is_match(guideline));

    assert!(Regex::new(r"\bprompt\b").expect("valid regex").is_match(&description));
    assert!(Regex::new(r"\btasks\b").expect("valid regex").is_match(&description));
    assert!(!duplicated_target_rule);
}

#[test]
fn given_description_when_built_then_it_forbids_category_with_model_and_names_config_escape() {
    let description = build(&empty_config());
    assert!(description.contains("omo.json"));
    assert!(description.contains("subagent_type"));
}

#[test]
fn given_prompt_guidelines_when_read_then_they_carry_category_model_exclusivity_rule() {
    let joined = TASK_PROMPT_GUIDELINES.join("\n");
    assert!(joined.contains("model"));
    assert!(joined.contains("category"));
    assert!(joined.contains("omo.json"));
}
