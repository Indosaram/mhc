//! `tools/task/categories.test.ts`

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::agents::AgentDefinition;
use crate::tools::task::categories::{
    TaskAgentInfo, TaskCategoryInfo, list_task_agents, list_task_categories,
};

#[test]
fn given_empty_omo_config_when_listed_then_builtin_categories_carry_descriptions() {
    let config = json!({ "categories": {}, "agents": {} });

    let categories = list_task_categories(&config);

    let quick = categories
        .iter()
        .find(|entry| entry.name == "quick")
        .expect("quick listed");
    assert!(quick.description.as_deref().is_some_and(|text| !text.is_empty()));
}

#[test]
fn given_custom_omo_json_category_when_listed_then_it_appears_with_description() {
    let config = json!({
        "categories": { "release-crew": { "description": "Ships the release train" } },
        "agents": {},
    });

    let categories = list_task_categories(&config);

    let custom = categories
        .into_iter()
        .find(|entry| entry.name == "release-crew");
    assert_eq!(
        custom,
        Some(TaskCategoryInfo {
            name: "release-crew".to_string(),
            description: Some("Ships the release train".to_string()),
        })
    );
}

#[test]
fn given_disabled_category_when_listed_then_it_is_omitted() {
    let config = json!({ "categories": { "quick": { "disable": true } }, "agents": {} });

    let names: Vec<String> = list_task_categories(&config)
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    assert!(!names.iter().any(|name| name == "quick"));
}

#[test]
fn given_loaded_agent_definitions_when_listed_then_names_and_descriptions_surface_disabled_excluded()
{
    let mut momus = AgentDefinition::named("momus");
    momus.description = "Deep reasoning".to_string().into();
    let mut hidden = AgentDefinition::named("hidden");
    hidden.description = "n/a".to_string().into();
    hidden.disable = true.into();
    let agents: BTreeMap<String, AgentDefinition> = [
        ("momus".to_string(), momus),
        ("hidden".to_string(), hidden),
    ]
    .into_iter()
    .collect();

    let listed = list_task_agents(&agents);

    assert!(listed.contains(&TaskAgentInfo {
        name: "momus".to_string(),
        description: Some("Deep reasoning".to_string()),
    }));
    assert!(!listed.iter().any(|entry| entry.name == "hidden"));
}
