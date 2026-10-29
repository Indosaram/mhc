//! `agents/category-shadow.ts`: `categories.<name>` keys that equal a known agent name.
//!
//! A `task(subagent_type: <name>)` spawn never reads `categories.*`; only `agents.<name>` reaches
//! agent resolution, so such a key is silently dead for that agent. Diagnose only: resolution does
//! not alias the category onto the agent.

use serde_json::Value;

/// Prompt-free builtin agent names (`agents/builtin/names.ts`).
const BUILTIN_AGENT_NAMES: [&str; 7] = [
    "explore",
    "librarian",
    "plan-consultant",
    "plan-reviewer",
    "omo-native-code-reviewer",
    "omo-native-qa-executor",
    "omo-native-gate-reviewer",
];

fn keys(config: &Value, section: &str) -> Vec<String> {
    config
        .get(section)
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// Only the agent's own key counts: a chain under a retired alias key never reaches the canonical
/// agent and must not silence the warning.
fn owns_chain(config: &Value, name: &str) -> bool {
    config
        .get("agents")
        .and_then(|agents| agents.get(name))
        .is_some_and(|agent| agent.get("model").is_some() || agent.get("models").is_some())
}

pub fn category_shadowed_agent_names<'a>(
    config: &Value,
    agent_names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut agents: std::collections::BTreeSet<String> = BUILTIN_AGENT_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    agents.extend(keys(config, "agents"));
    agents.extend(agent_names.into_iter().map(str::to_string));
    keys(config, "categories")
        .into_iter()
        .filter(|name| agents.contains(name) && !owns_chain(config, name))
        .collect()
}

pub fn category_shadow_message(config: &Value, name: &str) -> String {
    let category = config
        .get("categories")
        .and_then(|categories| categories.get(name));
    let chain = category
        .and_then(|category| category.get("models").cloned())
        .or_else(|| {
            category
                .and_then(|category| category.get("model"))
                .map(|model| Value::Array(vec![model.clone()]))
        });
    let value = chain.map_or_else(|| "[...]".to_string(), |chain| chain.to_string());
    format!(
        "categories.\"{name}\" is not read for subagent_type \"{name}\" spawns (categories only route task(category: ...)), so that agent runs its default model chain. Move the chain to the agent: \"agents\": {{ \"{name}\": {{ \"models\": {value} }} }}. If you also use task(category: \"{name}\") on purpose, the key keeps working there; giving the agent its own models silences this warning"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retired_alias_key_does_not_silence_the_warning() {
        let config = json!({
            "categories": { "omo-native-code-reviewer": { "model": "a/b" } },
            "agents": { "omo-senpi-code-reviewer": { "model": "x/y" } },
        });
        assert_eq!(
            category_shadowed_agent_names(&config, []),
            vec!["omo-native-code-reviewer"]
        );
    }

    #[test]
    fn agent_with_own_model_is_not_reported() {
        let config = json!({
            "categories": { "omo-native-code-reviewer": { "model": "a/b" } },
            "agents": { "omo-native-code-reviewer": { "model": "x/y" } },
        });
        assert!(category_shadowed_agent_names(&config, []).is_empty());
    }

    #[test]
    fn message_embeds_chain_and_suffix() {
        let config = json!({ "categories": { "explore": { "model": "a/b" } } });
        let message = category_shadow_message(&config, "explore");
        assert!(message.contains("\"agents\": { \"explore\": { \"models\": [\"a/b\"] } }"));
        assert!(message.ends_with("giving the agent its own models silences this warning"));
    }
}
