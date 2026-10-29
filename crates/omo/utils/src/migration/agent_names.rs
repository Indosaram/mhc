use serde_json::{Map, Value};

use super::ObjectMigration;

pub const AGENT_NAME_MAP: [(&str, &str); 31] = [
    ("omo", "sisyphus"),
    ("OmO", "sisyphus"),
    ("Sisyphus", "sisyphus"),
    ("Sisyphus (Ultraworker)", "sisyphus"),
    ("sisyphus", "sisyphus"),
    ("Hephaestus (Deep Agent)", "hephaestus"),
    ("OmO-Plan", "prometheus"),
    ("omo-plan", "prometheus"),
    ("Planner-Sisyphus", "prometheus"),
    ("planner-sisyphus", "prometheus"),
    ("Prometheus - Plan Builder", "prometheus"),
    ("Prometheus (Plan Builder)", "prometheus"),
    ("prometheus", "prometheus"),
    ("orchestrator-sisyphus", "atlas"),
    ("Atlas", "atlas"),
    ("Atlas (Plan Executor)", "atlas"),
    ("atlas", "atlas"),
    ("plan-consultant", "metis"),
    ("Metis - Plan Consultant", "metis"),
    ("Metis (Plan Consultant)", "metis"),
    ("metis", "metis"),
    ("Momus - Plan Critic", "momus"),
    ("Momus (Plan Critic)", "momus"),
    ("momus", "momus"),
    ("Sisyphus-Junior", "sisyphus-junior"),
    ("sisyphus-junior", "sisyphus-junior"),
    ("build", "build"),
    ("oracle", "oracle"),
    ("librarian", "librarian"),
    ("explore", "explore"),
    ("multimodal-looker", "multimodal-looker"),
];

pub const BUILTIN_AGENT_NAMES: [&str; 10] = [
    "sisyphus",
    "oracle",
    "librarian",
    "explore",
    "multimodal-looker",
    "metis",
    "momus",
    "prometheus",
    "atlas",
    "build",
];

fn lookup(key: &str) -> Option<&'static str> {
    AGENT_NAME_MAP
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, name)| *name)
}

/// Canonical agent name: lowercase lookup first, then exact lookup, else the input.
pub fn agent_name_for(key: &str) -> String {
    lookup(&key.to_lowercase())
        .or_else(|| lookup(key))
        .map_or_else(|| key.to_string(), str::to_string)
}

pub fn migrate_agent_names(agents: &Map<String, Value>) -> ObjectMigration {
    let mut migrated = Map::new();
    let mut changed = false;
    for (key, value) in agents {
        let new_key = agent_name_for(key);
        changed |= new_key != *key;
        migrated.insert(new_key, value.clone());
    }
    ObjectMigration { migrated, changed }
}
