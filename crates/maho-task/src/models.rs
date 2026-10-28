//! Model choice for a delegated task: an explicit model, a configured category or the agent's own
//! models, resolved with `maho_model_select::resolve_delegate_model` (spec model-selection §5.4).
use crate::agents::AgentDefinition;
use maho_model_select::{DelegateChoice, DelegateInput, normalize_reasoning, resolve_delegate_model};
use serde_json::Value;

/// Thinking levels the child CLI accepts (`--thinking`).
pub const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// What the caller asked for.
#[derive(Clone, Debug, Default)]
pub struct ModelRequest<'a> {
    pub explicit_model: Option<&'a str>,
    pub category: Option<&'a str>,
}

/// Inputs that do not come from the call.
#[derive(Clone, Debug, Default)]
pub struct ModelContext<'a> {
    /// The harness configuration view (`maho_config::load_native(..).view`).
    pub view: Option<&'a Value>,
    /// `provider/id` of every model with usable credentials (empty = unknown).
    pub available: &'a [String],
    /// `provider/id` of the delegating session's model.
    pub parent_model: Option<&'a str>,
}

/// The model a child runs on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelChoice {
    /// `provider/id` (or a bare id); `None` lets the child use its own default.
    pub model: Option<String>,
    pub thinking: Option<String>,
    /// Text appended to the child's system prompt (category `prompt_append`).
    pub prompt_append: Option<String>,
    /// Tools the category disables (`tools: { name: false }`).
    pub disabled_tools: Vec<String>,
    /// How the model was chosen (`explicit`, `category:<name>`, `agent`, `fallback`, `parent`).
    pub source: String,
}

/// Names of the configured, enabled categories.
pub fn category_names(view: Option<&Value>) -> Vec<String> {
    view.and_then(|view| view.get("categories"))
        .and_then(Value::as_object)
        .map(|categories| {
            categories
                .iter()
                .filter(|(_, fields)| fields.get("disable") != Some(&Value::Bool(true)))
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// A configured category's description, for the tool description.
pub fn category_description(view: Option<&Value>, name: &str) -> Option<String> {
    view?
        .get("categories")?
        .get(name)?
        .get("description")?
        .as_str()
        .map(str::to_owned)
}

fn thinking_of(value: Option<&str>) -> Option<String> {
    let level = normalize_reasoning(value?)?;
    THINKING_LEVELS.contains(&level.as_str()).then_some(level)
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) => vec![text.trim().to_owned()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .or_else(|| item.get("model").and_then(Value::as_str))
            })
            .map(|text| text.trim().to_owned())
            .collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|text| !text.is_empty())
    .collect()
}

/// Mapping of one category definition (§5.4): `models` first entry + rest, else `model` +
/// `fallback_models`; reasoning from the selected entry, then `reasoning`, then legacy
/// `reasoningEffort`/`variant`.
fn category_input(fields: &Value) -> (Option<String>, Vec<String>, Option<String>) {
    let entries = fields.get("models").and_then(Value::as_array);
    if let Some(entries) = entries.filter(|entries| !entries.is_empty()) {
        let models = string_list(Some(&Value::Array(entries.clone())));
        let first_reasoning = entries[0]
            .get("reasoning")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut models = models.into_iter();
        return (models.next(), models.collect(), first_reasoning);
    }
    (
        fields
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        string_list(fields.get("fallback_models")),
        None,
    )
}

/// Agent model references usable here: `@role` selectors (an oh-my-pi convention) are skipped.
fn usable(models: &[String]) -> Vec<String> {
    models
        .iter()
        .filter(|model| !model.starts_with('@'))
        .cloned()
        .collect()
}

/// Chooses the model, thinking level and category extras for one task.
///
/// # Errors
/// An unknown or disabled category (the message lists the configured ones).
pub fn choose_model(
    agent: &AgentDefinition,
    request: &ModelRequest<'_>,
    context: &ModelContext<'_>,
) -> Result<ModelChoice, String> {
    let mut choice = ModelChoice::default();
    let mut input = DelegateInput {
        available: context.available.to_vec(),
        system_default: context.parent_model.map(str::to_owned),
        has_model_cache: !context.available.is_empty(),
        ..DelegateInput::default()
    };
    let mut reasoning = None;
    if let Some(model) = request.explicit_model.filter(|m| !m.trim().is_empty()) {
        input.user_model = Some(model.trim().to_owned());
        choice.source = "explicit".into();
    } else if let Some(category) = request.category.filter(|c| !c.trim().is_empty()) {
        let category = category.trim();
        let fields = context
            .view
            .and_then(|view| view.get("categories"))
            .and_then(|categories| categories.get(category))
            .filter(|fields| fields.get("disable") != Some(&Value::Bool(true)));
        let Some(fields) = fields else {
            let known = category_names(context.view);
            return Err(if known.is_empty() {
                format!("Unknown category \"{category}\": no categories are configured (`categories` in omo.jsonc)")
            } else {
                format!(
                    "Unknown category \"{category}\". Configured categories: {}",
                    known.join(", ")
                )
            });
        };
        let (user_model, fallbacks, entry_reasoning) = category_input(fields);
        input.user_model = user_model;
        input.user_fallbacks = fallbacks;
        reasoning = entry_reasoning
            .or_else(|| {
                fields
                    .get("reasoning")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                ["reasoningEffort", "variant"]
                    .iter()
                    .find_map(|key| fields.get(key).and_then(Value::as_str))
                    .map(str::to_owned)
            });
        choice.prompt_append = fields
            .get("prompt_append")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(Value::Object(tools)) = fields.get("tools") {
            choice.disabled_tools = tools
                .iter()
                .filter(|(_, enabled)| **enabled == Value::Bool(false))
                .map(|(name, _)| name.clone())
                .collect();
        }
        choice.source = format!("category:{category}");
    } else {
        let mut models = usable(&agent.models).into_iter();
        input.user_model = models.next();
        input.user_fallbacks = models.collect();
        choice.source = if input.user_model.is_some() {
            "agent".into()
        } else {
            "parent".into()
        };
    }
    if reasoning.is_none() {
        reasoning.clone_from(&agent.thinking_level);
    }
    let resolved = resolve_delegate_model(&input);
    match resolved {
        DelegateChoice::Resolved(resolved) => {
            if resolved.from_fallback {
                choice.source = format!("{} (fallback)", choice.source);
            }
            choice.model = Some(resolved.model);
            choice.thinking = thinking_of(resolved.variant.as_deref()).or(thinking_of(reasoning.as_deref()));
        }
        DelegateChoice::Deferred | DelegateChoice::None => {
            choice.model = context.parent_model.map(str::to_owned);
            choice.thinking = thinking_of(reasoning.as_deref());
            choice.source = "parent".into();
        }
    }
    Ok(choice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::bundled_agents;
    use serde_json::json;

    fn task_agent() -> AgentDefinition {
        bundled_agents().remove(0)
    }

    #[test]
    fn parent_model_without_preferences() {
        let choice = choose_model(
            &task_agent(),
            &ModelRequest::default(),
            &ModelContext {
                parent_model: Some("p/main"),
                ..ModelContext::default()
            },
        )
        .expect("choice");
        assert_eq!(choice.model.as_deref(), Some("p/main"));
        assert_eq!(choice.source, "parent");
    }

    #[test]
    fn explicit_model_wins() {
        let choice = choose_model(
            &task_agent(),
            &ModelRequest {
                explicit_model: Some("x/y"),
                category: Some("ignored"),
            },
            &ModelContext::default(),
        )
        .expect("choice");
        assert_eq!(choice.model.as_deref(), Some("x/y"));
        assert_eq!(choice.source, "explicit");
    }

    #[test]
    fn category_models_fallback_and_extras() {
        let view = json!({"categories": {
            "fast": {"models": [{"model": "a/one", "reasoning": "low"}, "b/two"],
                     "prompt_append": "Be quick.", "tools": {"bash": false, "read": true}},
            "off": {"model": "c/three", "disable": true}
        }});
        let available = vec!["b/two".to_owned()];
        let choice = choose_model(
            &task_agent(),
            &ModelRequest {
                category: Some("fast"),
                ..ModelRequest::default()
            },
            &ModelContext {
                view: Some(&view),
                available: &available,
                parent_model: Some("p/main"),
            },
        )
        .expect("choice");
        assert_eq!(choice.model.as_deref(), Some("b/two"));
        assert_eq!(choice.thinking.as_deref(), Some("low"));
        assert_eq!(choice.prompt_append.as_deref(), Some("Be quick."));
        assert_eq!(choice.disabled_tools, ["bash"]);
        assert_eq!(choice.source, "category:fast (fallback)");
        let error = choose_model(
            &task_agent(),
            &ModelRequest {
                category: Some("off"),
                ..ModelRequest::default()
            },
            &ModelContext {
                view: Some(&view),
                ..ModelContext::default()
            },
        )
        .expect_err("disabled");
        assert!(error.contains("Configured categories: fast"), "{error}");
        assert_eq!(category_names(Some(&view)), ["fast"]);
    }

    #[test]
    fn agent_models_skip_role_selectors() {
        let mut agent = task_agent();
        agent.models = vec!["@smol".into(), "m/agent".into()];
        agent.thinking_level = Some("high".into());
        let choice = choose_model(&agent, &ModelRequest::default(), &ModelContext::default())
            .expect("choice");
        assert_eq!(choice.model.as_deref(), Some("m/agent"));
        assert_eq!(choice.thinking.as_deref(), Some("high"));
        assert_eq!(choice.source, "agent");
    }
}
